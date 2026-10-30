// padlock-cli/src/cache.rs
//
// File-level parse cache.  Stores the parsed StructLayouts from each source
// file keyed by (path, mtime-secs) so that unchanged files are not re-parsed
// on consecutive padlock runs.
//
// The cache is a single JSON file at `.padlock-cache/layouts.json` relative to
// the directory where padlock is invoked.  It is silently ignored (and
// recreated) if it is missing, corrupt, or contains an unknown arch name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use padlock_core::findings::SkippedStruct;
use padlock_core::ir::StructLayout;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct CacheStore {
    /// padlock version that wrote the cache. Layouts depend on the parser, so a
    /// cache written by another version is discarded rather than served stale.
    #[serde(default)]
    version: String,
    entries: HashMap<String, CacheEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    mtime_secs: u64,
    layouts: Vec<StructLayout>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skipped: Vec<SkippedStruct>,
    /// Files the result also depends on (a C/C++ file's `#include`s), with
    /// their mtimes when it was parsed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deps: Vec<(String, u64)>,
}

/// Everything besides the file itself that changes what the parser produces:
/// the padlock version, the preprocessor defines, the C++ stdlib variant and
/// the Go toolchain's layout-relevant behaviour.
/// A cache written under a different fingerprint is discarded.
fn cache_fingerprint() -> String {
    let defines: Vec<String> = padlock_source::preprocessor_defines_fingerprint();
    format!(
        "{}|{}|{:?}|{:?}|{}|{}|{}|{}/{}|{}",
        env!("CARGO_PKG_VERSION"),
        defines.join(","),
        padlock_source::include_dirs(),
        padlock_source::cpp_stdlib(),
        padlock_source::declared_order(),
        padlock_source::follow_dependencies(),
        padlock_source::configured_instantiations_fingerprint().join(";"),
        crate::paths::source_arch().name,
        crate::paths::source_arch().pointer_size,
        padlock_source::go_toolchain_fingerprint()
    )
}

/// In-process parse cache backed by `.padlock-cache/layouts.json`.
pub struct ParseCache {
    store: CacheStore,
    cache_path: PathBuf,
    dirty: bool,
    disabled: bool,
}

impl ParseCache {
    /// Load the cache from `root/.padlock-cache/layouts.json`.
    ///
    /// `root` should be the root of the directory being analyzed (not CWD) so
    /// that repeated runs from different working directories share one cache.
    /// If the file is missing or corrupt, an empty cache is returned.
    pub fn load(root: &Path) -> Self {
        let cache_path = root.join(".padlock-cache").join("layouts.json");
        let store: CacheStore = std::fs::read(&cache_path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .filter(|s: &CacheStore| s.version == cache_fingerprint())
            .unwrap_or_default();
        ParseCache {
            store,
            cache_path,
            dirty: false,
            disabled: false,
        }
    }

    /// A cache that never reads or writes `.padlock-cache/` — every lookup
    /// misses and every insert is a no-op. Used for `--no-cache`, where
    /// re-parsing every file is cheaper than a disk round-trip that will
    /// never be reused (e.g. single-file on-save analysis).
    pub fn disabled() -> Self {
        ParseCache {
            store: CacheStore::default(),
            cache_path: PathBuf::new(),
            dirty: false,
            disabled: true,
        }
    }

    /// Return the cached layouts and skipped items for `path` if the file has
    /// not changed since the cache was written, `None` otherwise.
    pub fn get(&self, path: &Path) -> Option<(Vec<StructLayout>, Vec<SkippedStruct>)> {
        let mtime = file_mtime(path)?;
        let key = path.to_string_lossy().into_owned();
        let entry = self.store.entries.get(&key)?;
        let deps_unchanged = entry
            .deps
            .iter()
            .all(|(dep, t)| mtime_secs(Path::new(dep)) == Some(*t));
        if entry.mtime_secs == mtime && deps_unchanged {
            Some((entry.layouts.clone(), entry.skipped.clone()))
        } else {
            None
        }
    }

    /// Store parsed layouts and skipped items for `path` (uses current mtime).
    pub fn insert(&mut self, path: &Path, layouts: Vec<StructLayout>, skipped: Vec<SkippedStruct>) {
        if self.disabled {
            return;
        }
        let Some(mtime) = file_mtime(path) else {
            return;
        };
        let key = path.to_string_lossy().into_owned();
        let deps = padlock_source::source_dependencies(path, crate::paths::source_arch())
            .into_iter()
            .filter_map(|d| Some((d.to_string_lossy().into_owned(), mtime_secs(&d)?)))
            .collect();
        self.store.entries.insert(
            key,
            CacheEntry {
                mtime_secs: mtime,
                layouts,
                skipped,
                deps,
            },
        );
        self.dirty = true;
    }

    /// Write the cache back to disk if any entries were updated.
    ///
    /// Also prunes stale entries for files that no longer exist — prevents the
    /// cache from growing unboundedly when source files are deleted or moved.
    /// Caps total entries at MAX_CACHE_ENTRIES to bound RAM and disk usage.
    /// Uses a streaming writer to avoid building the full JSON string in memory.
    ///
    /// Silently ignores I/O errors so that cache failures never break analysis.
    pub fn flush(&mut self) {
        if self.disabled || !self.dirty {
            return;
        }
        // Prune entries whose source file no longer exists.
        self.store
            .entries
            .retain(|k, _| std::path::Path::new(k).exists());
        // Cap total entries to prevent unbounded growth on very large projects.
        // Entries over the limit are dropped; they'll be re-parsed on the next run.
        const MAX_CACHE_ENTRIES: usize = 100_000;
        if self.store.entries.len() > MAX_CACHE_ENTRIES {
            let mut keys: Vec<&String> = self.store.entries.keys().collect();
            keys.sort_unstable();
            let keep: std::collections::HashSet<String> =
                keys.into_iter().take(MAX_CACHE_ENTRIES).cloned().collect();
            self.store.entries.retain(|k, _| keep.contains(k));
        }
        if let Some(dir) = self.cache_path.parent()
            && std::fs::create_dir_all(dir).is_err()
        {
            return;
        }
        self.store.version = cache_fingerprint();
        if let Ok(file) = std::fs::File::create(&self.cache_path) {
            let writer = std::io::BufWriter::new(file);
            let _ = serde_json::to_writer(writer, &self.store);
        }
    }
}

fn file_mtime(path: &Path) -> Option<u64> {
    let own = mtime_secs(path)?;
    // A Rust file is laid out with the rest of its crate.
    if path.extension().is_some_and(|e| e == "rs")
        && let Some(root) = padlock_source::rust_crate_root(path)
    {
        let newest = padlock_source::rust_crate_newest_mtime(&root)
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return Some(own.max(newest));
    }
    // A Zig file is laid out with the files it imports.
    if path.extension().is_some_and(|e| e == "zig")
        && let Some(t) = padlock_source::zig_imports_newest_mtime(path, crate::paths::source_arch())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
    {
        return Some(own.max(t.as_secs()));
    }
    // A Go file is laid out with the other files of its package, so an edit,
    // addition or removal of a sibling `.go` file invalidates it too.
    if path.extension().is_some_and(|e| e == "go")
        && let Some(dir) = path.parent()
    {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        return Some(own.max(go_dir_mtime(dir)));
    }
    Some(own)
}

/// Newest mtime among a directory and its `.go` files. Memoized for a second:
/// every file of a package asks for the same directory, and listing it per
/// file is quadratic in the package size.
fn go_dir_mtime(dir: &Path) -> u64 {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};
    static MEMO: LazyLock<Mutex<HashMap<PathBuf, (u64, Instant)>>> =
        LazyLock::new(Default::default);
    if let Ok(memo) = MEMO.lock()
        && let Some((t, at)) = memo.get(dir)
        && at.elapsed() < Duration::from_secs(1)
    {
        return *t;
    }
    let newest = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "go"))
        .filter_map(|p| mtime_secs(&p))
        .chain(mtime_secs(dir))
        .max()
        .unwrap_or(0);
    if let Ok(mut memo) = MEMO.lock() {
        memo.insert(dir.to_path_buf(), (newest, Instant::now()));
    }
    newest
}

fn mtime_secs(path: &Path) -> Option<u64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;
    use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
    use std::fs;
    use tempfile::TempDir;

    fn simple_layout() -> StructLayout {
        StructLayout {
            name: "Foo".to_string(),
            total_size: 8,
            align: 8,
            fields: vec![Field {
                name: "x".to_string(),
                ty: TypeInfo::Primitive {
                    name: "u64".to_string(),
                    size: 8,
                    align: 8,
                },
                offset: 0,
                size: 8,
                align: 8,
                source_file: None,
                source_line: None,
                access: AccessPattern::Unknown,
            }],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
            is_repr_rust: false,
            suppressed_findings: Vec::new(),
            uncertain_fields: Vec::new(),
            forced_align: None,
            pack_limit: None,
            cpp: None,
            explicit_align: Vec::new(),
        }
    }

    #[test]
    fn cache_miss_on_fresh_cache() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.rs");
        fs::write(&src, "struct A { x: u64 }").unwrap();

        // A fresh (no file on disk) cache always misses.
        let cache_path = dir.path().join(".padlock-cache").join("layouts.json");
        let cache = ParseCache {
            store: CacheStore::default(),
            cache_path,
            dirty: false,
            disabled: false,
        };
        assert!(cache.get(&src).is_none());
    }

    #[test]
    fn cache_hit_after_insert_and_flush() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("b.rs");
        fs::write(&src, "struct B { x: u64 }").unwrap();

        let cache_path = dir.path().join(".padlock-cache").join("layouts.json");

        // Insert + flush
        {
            let mut cache = ParseCache {
                store: CacheStore::default(),
                cache_path: cache_path.clone(),
                dirty: false,
                disabled: false,
            };
            cache.insert(&src, vec![simple_layout()], vec![]);
            cache.flush();
        }

        // Load from disk and check hit
        let store: CacheStore = serde_json::from_slice(&fs::read(&cache_path).unwrap()).unwrap();
        let reload = ParseCache {
            store,
            cache_path,
            dirty: false,
            disabled: false,
        };
        let result = reload.get(&src);
        assert!(result.is_some());
        let (layouts, _skipped) = result.unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Foo");
    }

    #[test]
    fn cache_miss_when_file_modified() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("c.rs");
        fs::write(&src, "struct C { x: u64 }").unwrap();

        let cache_path = dir.path().join(".padlock-cache").join("layouts.json");

        // Store an entry with mtime = 0 (past time), simulating a stale entry.
        let key = src.to_string_lossy().into_owned();
        let mut store = CacheStore::default();
        store.entries.insert(
            key,
            CacheEntry {
                deps: Vec::new(),
                mtime_secs: 0, // will never match current file's mtime
                layouts: vec![simple_layout()],
                skipped: vec![],
            },
        );
        let cache = ParseCache {
            store,
            cache_path,
            dirty: false,
            disabled: false,
        };
        // File's actual mtime != 0, so this must be a miss.
        assert!(cache.get(&src).is_none());
    }

    #[test]
    fn cache_written_by_another_version_is_discarded() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("v.rs");
        fs::write(&src, "struct V { x: u64 }").unwrap();

        let mut cache = ParseCache::load(dir.path());
        cache.insert(&src, vec![simple_layout()], vec![]);
        cache.flush();
        assert!(
            ParseCache::load(dir.path()).get(&src).is_some(),
            "same-version cache is reused"
        );

        // Rewrite the file as if an older padlock produced it.
        let path = dir.path().join(".padlock-cache").join("layouts.json");
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        json["version"] = "0.0.1".into();
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(
            ParseCache::load(dir.path()).get(&src).is_none(),
            "layouts parsed by another padlock version must not be served"
        );
    }

    #[test]
    fn cache_flush_is_idempotent_when_not_dirty() {
        let dir = TempDir::new().unwrap();
        let cache_path = dir.path().join(".padlock-cache").join("layouts.json");
        let mut cache = ParseCache {
            store: CacheStore::default(),
            cache_path: cache_path.clone(),
            dirty: false,
            disabled: false,
        };
        cache.flush(); // should be a no-op
        assert!(!cache_path.exists(), "no file written when not dirty");
    }

    #[test]
    fn disabled_cache_never_reads_or_writes_disk() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("e.rs");
        fs::write(&src, "struct E { x: u64 }").unwrap();
        let cache_path = dir.path().join(".padlock-cache").join("layouts.json");

        let mut cache = ParseCache::disabled();
        assert!(cache.get(&src).is_none());
        cache.insert(&src, vec![simple_layout()], vec![]);
        // insert() on a disabled cache must not mark it dirty, or flush()
        // would still write a file despite the whole point being no disk I/O.
        assert!(cache.get(&src).is_none(), "disabled cache must never hit");
        cache.flush();
        assert!(
            !cache_path.exists(),
            "--no-cache must never create .padlock-cache/"
        );
    }

    #[test]
    fn cache_persists_skipped_items() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("d.rs");
        fs::write(&src, "struct D<T> { x: T }").unwrap();

        let cache_path = dir.path().join(".padlock-cache").join("layouts.json");
        let skipped = vec![SkippedStruct {
            name: "D".to_string(),
            reason: "generic struct".to_string(),
            source_file: Some(src.to_string_lossy().into_owned()),
        }];

        // Insert with skipped items and flush.
        {
            let mut cache = ParseCache {
                store: CacheStore::default(),
                cache_path: cache_path.clone(),
                dirty: false,
                disabled: false,
            };
            cache.insert(&src, vec![], skipped.clone());
            cache.flush();
        }

        // Reload from disk and check that skipped items survived.
        let store: CacheStore = serde_json::from_slice(&fs::read(&cache_path).unwrap()).unwrap();
        let reload = ParseCache {
            store,
            cache_path,
            dirty: false,
            disabled: false,
        };
        let (layouts, loaded_skipped) = reload.get(&src).expect("cache hit expected");
        assert!(layouts.is_empty());
        assert_eq!(loaded_skipped.len(), 1);
        assert_eq!(loaded_skipped[0].name, "D");
        assert_eq!(loaded_skipped[0].reason, "generic struct");
    }
}
