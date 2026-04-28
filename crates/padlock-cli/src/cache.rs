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
    entries: HashMap<String, CacheEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    mtime_secs: u64,
    layouts: Vec<StructLayout>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skipped: Vec<SkippedStruct>,
}

/// In-process parse cache backed by `.padlock-cache/layouts.json`.
pub struct ParseCache {
    store: CacheStore,
    cache_path: PathBuf,
    dirty: bool,
}

impl ParseCache {
    /// Load the cache from `root/.padlock-cache/layouts.json`.
    ///
    /// `root` should be the root of the directory being analyzed (not CWD) so
    /// that repeated runs from different working directories share one cache.
    /// If the file is missing or corrupt, an empty cache is returned.
    pub fn load(root: &Path) -> Self {
        let cache_path = root.join(".padlock-cache").join("layouts.json");
        let store = std::fs::read(&cache_path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        ParseCache {
            store,
            cache_path,
            dirty: false,
        }
    }

    /// Return the cached layouts and skipped items for `path` if the file has
    /// not changed since the cache was written, `None` otherwise.
    pub fn get(&self, path: &Path) -> Option<(Vec<StructLayout>, Vec<SkippedStruct>)> {
        let mtime = file_mtime(path)?;
        let key = path.to_string_lossy().into_owned();
        let entry = self.store.entries.get(&key)?;
        if entry.mtime_secs == mtime {
            Some((entry.layouts.clone(), entry.skipped.clone()))
        } else {
            None
        }
    }

    /// Store parsed layouts and skipped items for `path` (uses current mtime).
    pub fn insert(&mut self, path: &Path, layouts: Vec<StructLayout>, skipped: Vec<SkippedStruct>) {
        let Some(mtime) = file_mtime(path) else {
            return;
        };
        let key = path.to_string_lossy().into_owned();
        self.store.entries.insert(
            key,
            CacheEntry {
                mtime_secs: mtime,
                layouts,
                skipped,
            },
        );
        self.dirty = true;
    }

    /// Write the cache back to disk if any entries were updated.
    ///
    /// Also prunes stale entries for files that no longer exist — prevents the
    /// cache from growing unboundedly when source files are deleted or moved.
    /// Uses a streaming writer to avoid building the full JSON string in memory.
    ///
    /// Silently ignores I/O errors so that cache failures never break analysis.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        // Prune entries whose source file no longer exists.
        self.store
            .entries
            .retain(|k, _| std::path::Path::new(k).exists());
        if let Some(dir) = self.cache_path.parent()
            && std::fs::create_dir_all(dir).is_err()
        {
            return;
        }
        if let Ok(file) = std::fs::File::create(&self.cache_path) {
            let writer = std::io::BufWriter::new(file);
            let _ = serde_json::to_writer(writer, &self.store);
        }
    }
}

fn file_mtime(path: &Path) -> Option<u64> {
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
                mtime_secs: 0, // will never match current file's mtime
                layouts: vec![simple_layout()],
                skipped: vec![],
            },
        );
        let cache = ParseCache {
            store,
            cache_path,
            dirty: false,
        };
        // File's actual mtime != 0, so this must be a miss.
        assert!(cache.get(&src).is_none());
    }

    #[test]
    fn cache_flush_is_idempotent_when_not_dirty() {
        let dir = TempDir::new().unwrap();
        let cache_path = dir.path().join(".padlock-cache").join("layouts.json");
        let mut cache = ParseCache {
            store: CacheStore::default(),
            cache_path: cache_path.clone(),
            dirty: false,
        };
        cache.flush(); // should be a no-op
        assert!(!cache_path.exists(), "no file written when not dirty");
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
        };
        let (layouts, loaded_skipped) = reload.get(&src).expect("cache hit expected");
        assert!(layouts.is_empty());
        assert_eq!(loaded_skipped.len(), 1);
        assert_eq!(loaded_skipped[0].name, "D");
        assert_eq!(loaded_skipped[0].reason, "generic struct");
    }
}
