// padlock-source/src/rust_crate.rs
//
// A Rust crate spans every `.rs` file under its `src/`: a field may use a
// struct, enum, alias or const from another module. Parsing a file on its own
// leaves such a field at a guessed size. Here a crate is parsed once (cached
// per crate root) with every file seeing the declarations of the others, and
// struct sizes are resolved crate-wide (see `frontends::rust::parse_rust_crate`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use padlock_core::arch::ArchConfig;
use padlock_core::findings::SkippedStruct;
use padlock_core::ir::StructLayout;

use crate::frontends::rust::RustCtx;

/// The directory holding the crate's `Cargo.toml`, for a file under its `src/`.
pub(crate) fn crate_root(path: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(path).ok()?;
    let mut dir = path.parent()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return path.starts_with(dir.join("src")).then(|| dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

type Signature = Vec<(PathBuf, Option<SystemTime>, u64)>;

/// Every `.rs` file under `root/src`, with mtime and size.
fn signature(root: &Path) -> Signature {
    let mut out = Vec::new();
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                let md = entry.metadata().ok();
                out.push((
                    p,
                    md.as_ref().and_then(|m| m.modified().ok()),
                    md.map(|m| m.len()).unwrap_or(0),
                ));
            }
        }
    }
    out.sort();
    out
}

/// Newest mtime among a crate's source files (for parse-cache invalidation).
pub(crate) fn newest_mtime(root: &Path) -> Option<SystemTime> {
    signature(root).into_iter().filter_map(|(_, t, _)| t).max()
}

struct Crate {
    signature: Signature,
    /// Layouts per file, resolved crate-wide, `source_file` set.
    layouts: HashMap<PathBuf, Vec<StructLayout>>,
    skipped: HashMap<PathBuf, Vec<SkippedStruct>>,
}

type Slot = Mutex<Option<(Arc<Crate>, Instant)>>;

type Crates = Mutex<HashMap<(PathBuf, &'static str, u64), Arc<Slot>>>;

static CRATES: LazyLock<Crates> = LazyLock::new(Default::default);

/// How long a crate is trusted before its files' timestamps are checked again.
const RECHECK_AFTER: Duration = Duration::from_secs(2);

/// Larger crates than this are parsed file by file (a crate-wide parse holds
/// every syntax tree in memory at once).
const MAX_FILES: usize = 5_000;

fn build(signature: Signature, arch: &'static ArchConfig) -> Crate {
    build_with_ctx(signature, arch).0
}

fn build_with_ctx(signature: Signature, arch: &'static ArchConfig) -> (Crate, RustCtx) {
    let files: Vec<(String, String)> = signature
        .iter()
        .filter_map(|(p, _, len)| {
            // The CLI's generated-file limit: tables, not hand-written types.
            if *len > 500_000 {
                return None;
            }
            Some((
                p.to_string_lossy().into_owned(),
                std::fs::read_to_string(p).ok()?,
            ))
        })
        .collect();
    let outer = crate::take_skipped();
    let (per_file, ctx) = crate::frontends::rust::parse_rust_crate(&files, arch);
    let skipped_all = crate::take_skipped();
    crate::restore_skipped(outer);
    let mut all: Vec<StructLayout> = Vec::new();
    for ((path, _), mut layouts) in files.iter().zip(per_file) {
        for l in &mut layouts {
            l.source_file = Some(path.clone());
        }
        all.extend(layouts);
    }
    crate::resolve_nested_structs(&mut all);
    let mut layouts: HashMap<PathBuf, Vec<StructLayout>> = HashMap::new();
    for l in all {
        let key = PathBuf::from(l.source_file.clone().unwrap_or_default());
        layouts.entry(key).or_default().push(l);
    }
    // Skip notes are not attributed per file by the crate-wide parse; keep
    // them with the crate's first file so they are reported once.
    let mut skipped = HashMap::new();
    if let Some((first, _)) = files.first() {
        skipped.insert(PathBuf::from(first), skipped_all);
    }
    (
        Crate {
            signature,
            layouts,
            skipped,
        },
        ctx,
    )
}

fn crate_for(root: &Path, arch: &'static ArchConfig) -> Option<Arc<Crate>> {
    let key = (root.to_path_buf(), arch.name, crate::settings_generation());
    let slot = CRATES.lock().ok()?.entry(key).or_default().clone();
    let mut state = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((c, checked)) = state.as_mut() {
        if checked.elapsed() < RECHECK_AFTER {
            return Some(c.clone());
        }
        if c.signature == signature(root) {
            *checked = Instant::now();
            return Some(c.clone());
        }
    }
    let sig = signature(root);
    if sig.len() > MAX_FILES {
        return None;
    }
    let c = Arc::new(build(sig, arch));
    *state = Some((c.clone(), Instant::now()));
    Some(c)
}

/// Build the crate containing `path` into the cache (for callers that parse
/// many files in parallel).
pub(crate) fn prepare(path: &Path, arch: &'static ArchConfig) {
    if let Some(root) = crate_root(path) {
        crate_for(&root, arch);
    }
}

/// Layouts of `path` (whose text is `source`) resolved against its whole
/// crate, or `None` when it is not under a crate's `src/` (or was edited
/// since it was read from disk).
pub(crate) fn parse_in_crate(
    path: &Path,
    source: &str,
    arch: &'static ArchConfig,
) -> Option<Vec<StructLayout>> {
    if std::fs::read_to_string(path).ok()? != source {
        return None;
    }
    let root = crate_root(path)?;
    let c = crate_for(&root, arch)?;
    let canonical = std::fs::canonicalize(path).ok()?;
    let file = path.to_string_lossy().into_owned();
    let found = c.signature.iter().any(|(p, _, _)| *p == canonical);
    if !found {
        return None;
    }
    crate::restore_skipped(c.skipped.get(&canonical).cloned().unwrap_or_default());
    Some(
        c.layouts
            .get(&canonical)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut l| {
                l.source_file = Some(file.clone());
                l
            })
            .collect(),
    )
}

/// A crate as an editor session sees it: its layouts, the declarations buffers
/// are parsed against, and when it was last checked against the disk.
type BufferCrate = (Arc<Crate>, RustCtx, Instant);

thread_local! {
    /// The crate-wide declarations an editor session parses buffers against.
    /// A `RustCtx` holds syntax trees (`!Send`), so it lives per thread — the
    /// language server is single-threaded; the CLI never uses this.
    static BUFFER_CRATES: std::cell::RefCell<HashMap<PathBuf, BufferCrate>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Layouts of an editor buffer (`source` is the live text of `path`, possibly
/// unsaved) resolved against the crate on disk: the buffer's own declarations
/// win over the saved version of the same file. `None` when `path` is not a
/// file under a crate's `src/`.
pub(crate) fn parse_buffer_in_crate(
    path: &Path,
    source: &str,
    arch: &'static ArchConfig,
) -> Option<Vec<StructLayout>> {
    let root = crate_root(path)?;
    let (krate, ctx) = BUFFER_CRATES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let fresh = cache.get(&root).and_then(|(c, ctx, checked)| {
            (checked.elapsed() < RECHECK_AFTER || c.signature == signature(&root))
                .then(|| (c.clone(), ctx.clone()))
        });
        if let Some(hit) = fresh {
            if let Some(entry) = cache.get_mut(&root) {
                entry.2 = Instant::now();
            }
            return Some(hit);
        }
        let sig = signature(&root);
        if sig.len() > MAX_FILES {
            return None;
        }
        let (c, ctx) = build_with_ctx(sig, arch);
        let c = Arc::new(c);
        cache.insert(root.clone(), (c.clone(), ctx.clone(), Instant::now()));
        Some((c, ctx))
    })?;
    let mut own = crate::frontends::rust::parse_rust_in_ctx(source, ctx).ok()?;
    let n = own.len();
    // The crate's layouts are final; only those the buffer names are needed,
    // and not the saved versions of types it (re)defines.
    let defined: std::collections::HashSet<String> = own.iter().map(|l| l.name.clone()).collect();
    let named: std::collections::HashSet<String> = own
        .iter()
        .flat_map(|l| &l.fields)
        .filter_map(|f| match &f.ty {
            padlock_core::ir::TypeInfo::Primitive { name, .. }
            | padlock_core::ir::TypeInfo::Opaque { name, .. } => {
                Some(crate::split_c_array_suffix(name).0.to_string())
            }
            _ => None,
        })
        .collect();
    let context: Vec<StructLayout> = krate
        .layouts
        .values()
        .flatten()
        .filter(|l| named.contains(&l.name) && !defined.contains(&l.name))
        .cloned()
        .collect();
    own.extend(context);
    crate::resolve_nested_structs(&mut own);
    own.truncate(n);
    Some(own)
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn types_from_other_modules_are_sized() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "mod a;\npub struct Uses { pub x: a::Inner, pub s: Option<a::Level>, pub f: u8 }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("a.rs"),
            "pub struct Inner { pub a: u64, pub b: u32 }\npub enum Level { Low, High }\n",
        )
        .unwrap();
        let lib = src.join("lib.rs");
        let text = std::fs::read_to_string(&lib).unwrap();
        let layouts = parse_in_crate(&lib, &text, &X86_64_SYSV).unwrap();
        let uses = layouts.iter().find(|l| l.name == "Uses").unwrap();
        assert_eq!(uses.total_size, 24, "{:?}", uses.fields);
        assert!(
            uses.uncertain_fields.is_empty(),
            "{:?}",
            uses.uncertain_fields
        );
    }
}
