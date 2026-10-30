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

/// Source roots of the crates `root` depends on by `path` (transitively),
/// from `[dependencies]`, `[build-dependencies]` and target-specific tables;
/// `dep = { workspace = true }` is looked up in the workspace's
/// `[workspace.dependencies]`. Registry and git dependencies are not followed,
/// and neither are path dependencies outside the workspace unless
/// `follow_dependencies` is on.
fn path_dependencies(root: &Path) -> Vec<PathBuf> {
    const MAX_DEPS: usize = 64;
    let mut found: Vec<PathBuf> = Vec::new();
    // Dependencies outside the workspace (or in a nested git repository) count
    // as external and are followed only with `follow_dependencies`.
    let boundary = workspace_dependencies(root)
        .map(|(w, _)| w)
        .unwrap_or_else(|| root.to_path_buf());
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        let Some(manifest) = read_manifest(&dir) else {
            continue;
        };
        let workspace = workspace_dependencies(&dir);
        let mut tables: Vec<&toml::Table> = Vec::new();
        for key in ["dependencies", "build-dependencies"] {
            if let Some(t) = manifest.get(key).and_then(|v| v.as_table()) {
                tables.push(t);
            }
        }
        if let Some(targets) = manifest.get("target").and_then(|v| v.as_table()) {
            for t in targets.values().filter_map(|v| v.as_table()) {
                for key in ["dependencies", "build-dependencies"] {
                    if let Some(t) = t.get(key).and_then(|v| v.as_table()) {
                        tables.push(t);
                    }
                }
            }
        }
        for table in tables {
            for (name, dep) in table {
                let Some(dep) = dep.as_table() else { continue };
                let path = if dep.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
                    workspace.as_ref().and_then(|(wroot, deps)| {
                        let p = deps.get(name)?.get("path")?.as_str()?;
                        Some(wroot.join(p))
                    })
                } else {
                    dep.get("path")
                        .and_then(|v| v.as_str())
                        .map(|p| dir.join(p))
                };
                let Some(path) = path.and_then(|p| std::fs::canonicalize(p).ok()) else {
                    continue;
                };
                if !crate::follow_dependencies() && crate::is_external(&path, &boundary) {
                    continue;
                }
                if path != root && !found.contains(&path) && path.join("src").is_dir() {
                    found.push(path.clone());
                    queue.push(path);
                }
            }
        }
        if found.len() >= MAX_DEPS {
            break;
        }
    }
    found.sort();
    found
}

fn read_manifest(dir: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(dir.join("Cargo.toml"))
        .ok()?
        .parse::<toml::Table>()
        .ok()
}

/// `[workspace.dependencies]` of the nearest enclosing workspace manifest, with
/// the directory it is relative to.
fn workspace_dependencies(dir: &Path) -> Option<(PathBuf, toml::Table)> {
    let mut cur = dir;
    loop {
        if let Some(m) = read_manifest(cur)
            && let Some(ws) = m.get("workspace").and_then(|v| v.as_table())
        {
            let deps = ws.get("dependencies")?.as_table()?.clone();
            return Some((cur.to_path_buf(), deps));
        }
        cur = cur.parent()?;
    }
}

/// Every `.rs` file under `root/src` and under the `src/` of each path
/// dependency, with mtime and size.
fn signature(root: &Path) -> Signature {
    let mut out = Vec::new();
    let mut stack = vec![root.join("src")];
    stack.extend(path_dependencies(root).into_iter().map(|d| d.join("src")));
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

fn build(root: &Path, signature: Signature, arch: &'static ArchConfig) -> Crate {
    build_with_ctx(root, signature, arch).0
}

fn build_with_ctx(
    root: &Path,
    signature: Signature,
    arch: &'static ArchConfig,
) -> (Crate, RustCtx) {
    // The crate's own files come first; files of path dependencies follow and
    // only supply declarations (laid out, but never reported for this crate).
    let own_src = root.join("src");
    let (own, external): (Vec<_>, Vec<_>) = signature
        .iter()
        .partition(|(p, _, _)| p.starts_with(&own_src));
    let files: Vec<(String, String)> = own
        .into_iter()
        .chain(external)
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
    let local_count = signature
        .iter()
        .filter(|(p, _, _)| p.starts_with(&own_src))
        .count()
        .min(files.len());
    let outer = crate::take_skipped();
    let (per_file, ctx) =
        crate::frontends::rust::parse_rust_crate(&files, local_count, Some(&own_src), arch);
    let skipped_all = crate::take_skipped();
    crate::restore_skipped(outer);
    let mut all: Vec<StructLayout> = Vec::new();
    for ((path, _), mut layouts) in files.iter().zip(per_file) {
        for l in &mut layouts {
            l.source_file = Some(path.clone());
        }
        all.extend(layouts);
    }
    // A dependency's type of the same name as one of this crate's is shadowed.
    let local_names: std::collections::HashSet<String> = all
        .iter()
        .filter(|l| {
            l.source_file
                .as_deref()
                .is_some_and(|f| Path::new(f).starts_with(&own_src))
        })
        .map(|l| l.name.clone())
        .collect();
    all.retain(|l| {
        l.source_file
            .as_deref()
            .is_some_and(|f| Path::new(f).starts_with(&own_src))
            || !local_names.contains(&l.name)
    });
    let layouts = resolve_per_file(
        all,
        ctx.ambiguous_names(),
        ctx.name_bindings(),
        &files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
    );
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

/// Group layouts by file and size nested types. A name declared in several
/// files (`ambiguous`) means different things in each, so a file is resolved
/// against its own layouts plus the unambiguous ones it reaches; an ambiguous
/// name declared elsewhere stays unresolved (and so uncertain).
fn resolve_per_file(
    mut all: Vec<StructLayout>,
    ambiguous: &std::collections::HashSet<String>,
    bindings: &[HashMap<String, usize>],
    paths: &[String],
) -> HashMap<PathBuf, Vec<StructLayout>> {
    let mut layouts: HashMap<PathBuf, Vec<StructLayout>> = HashMap::new();
    if ambiguous.is_empty() {
        crate::resolve_nested_structs(&mut all);
        for l in all {
            let key = PathBuf::from(l.source_file.clone().unwrap_or_default());
            layouts.entry(key).or_default().push(l);
        }
        return layouts;
    }
    for l in all {
        let key = PathBuf::from(l.source_file.clone().unwrap_or_default());
        layouts.entry(key).or_default().push(l);
    }
    let pool: HashMap<String, StructLayout> = layouts
        .values()
        .flatten()
        .filter(|l| !ambiguous.contains(&l.name))
        .map(|l| (l.name.clone(), l.clone()))
        .collect();
    let unresolved = layouts.clone();
    for own in layouts.values_mut() {
        *own = resolve_file(own, &pool, Vec::new());
    }
    // A file that uses an ambiguous name declared elsewhere (found through its
    // `use` items or qualified paths) sees that declaration, sized in its own
    // file's context and frozen.
    for (i, bound) in bindings.iter().enumerate() {
        if bound.is_empty() {
            continue;
        }
        let key = PathBuf::from(&paths[i]);
        let Some(own) = unresolved.get(&key) else {
            continue;
        };
        let mut extra = Vec::new();
        for (name, &target) in bound {
            if let Some(l) = layouts
                .get(&PathBuf::from(&paths[target]))
                .and_then(|ls| ls.iter().find(|l| &l.name == name))
            {
                extra.push(freeze(l));
            }
        }
        layouts.insert(key, resolve_file(own, &pool, extra));
    }
    layouts
}

/// A copy of `l` whose fields no longer name other types, so resolving it
/// again in another file's context keeps its size.
fn freeze(l: &StructLayout) -> StructLayout {
    let mut l = l.clone();
    for f in &mut l.fields {
        if let padlock_core::ir::TypeInfo::Opaque { size, align, .. } = &f.ty {
            f.ty = padlock_core::ir::TypeInfo::Primitive {
                name: "u8".to_string(),
                size: *size,
                align: *align,
            };
        }
    }
    l
}

/// Size `own` against the layouts of `extra` and the unambiguous `pool` types
/// reachable from it; only `own`'s layouts are returned.
fn resolve_file(
    own: &[StructLayout],
    pool: &HashMap<String, StructLayout>,
    extra: Vec<StructLayout>,
) -> Vec<StructLayout> {
    let referenced = |l: &StructLayout| -> Vec<String> {
        l.fields
            .iter()
            .filter_map(|f| match &f.ty {
                padlock_core::ir::TypeInfo::Primitive { name, .. }
                | padlock_core::ir::TypeInfo::Opaque { name, .. } => {
                    Some(crate::split_c_array_suffix(name).0.to_string())
                }
                _ => None,
            })
            .collect()
    };
    let n = own.len();
    let mut work: Vec<StructLayout> = own.to_vec();
    let mut seen: std::collections::HashSet<String> = work.iter().map(|l| l.name.clone()).collect();
    for l in extra {
        seen.insert(l.name.clone());
        work.push(l);
    }
    let mut next = 0;
    while next < work.len() {
        for name in referenced(&work[next]) {
            if !seen.contains(&name)
                && let Some(l) = pool.get(&name)
            {
                seen.insert(name);
                work.push(l.clone());
            }
        }
        next += 1;
    }
    crate::resolve_nested_structs(&mut work);
    work.truncate(n);
    work
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
    let c = Arc::new(build(root, sig, arch));
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
        let (c, ctx) = build_with_ctx(&root, sig, arch);
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

    fn crate_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        for (name, text) in files {
            std::fs::write(src.join(name), text).unwrap();
        }
        dir
    }

    #[test]
    fn same_name_in_two_files_resolves_per_file() {
        let dir = crate_dir(&[
            ("lib.rs", "mod a;\nmod b;\n"),
            (
                "a.rs",
                "pub struct Config { pub x: u64 }\npub struct UsesA { pub c: Config, pub f: u8 }\n",
            ),
            (
                "b.rs",
                "pub struct Config { pub x: u8, pub y: u8 }\npub struct UsesB { pub c: Config, pub f: u8 }\n",
            ),
        ]);
        let get = |file: &str, name: &str| {
            let p = dir.path().join("src").join(file);
            let text = std::fs::read_to_string(&p).unwrap();
            let l = parse_in_crate(&p, &text, &X86_64_SYSV).unwrap();
            l.into_iter().find(|l| l.name == name).unwrap()
        };
        let a = get("a.rs", "UsesA");
        let b = get("b.rs", "UsesB");
        assert_eq!(a.total_size, 16, "{:?}", a.fields);
        assert_eq!(b.total_size, 3, "{:?}", b.fields);
        assert!(a.uncertain_fields.is_empty() && b.uncertain_fields.is_empty());
    }

    #[test]
    fn cfg_disabled_definition_is_ignored() {
        let dir = crate_dir(&[(
            "lib.rs",
            "#[cfg(target_pointer_width = \"32\")]\npub struct Handle { pub a: u32 }\n\
             #[cfg(target_pointer_width = \"64\")]\npub struct Handle { pub a: u64 }\n\
             pub struct Uses { pub h: Handle, pub f: u8 }\n",
        )]);
        let lib = dir.path().join("src/lib.rs");
        let text = std::fs::read_to_string(&lib).unwrap();
        let l = parse_in_crate(&lib, &text, &X86_64_SYSV).unwrap();
        let uses = l.iter().find(|l| l.name == "Uses").unwrap();
        assert_eq!(uses.total_size, 16, "{:?}", uses.fields);
    }

    #[test]
    fn path_dependency_types_are_sized() {
        let ws = tempfile::TempDir::new().unwrap();
        let w = ws.path();
        std::fs::create_dir_all(w.join("app/src")).unwrap();
        std::fs::create_dir_all(w.join("core/src")).unwrap();
        std::fs::write(
            w.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"core\"]\n\n[workspace.dependencies]\ncore2 = { path = \"core\" }\n",
        )
        .unwrap();
        std::fs::write(
            w.join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ncore2 = { workspace = true }\n",
        )
        .unwrap();
        std::fs::write(
            w.join("core/Cargo.toml"),
            "[package]\nname = \"core2\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            w.join("core/src/lib.rs"),
            "pub struct Shared { pub a: u64, pub b: u64 }\n",
        )
        .unwrap();
        std::fs::write(
            w.join("app/src/lib.rs"),
            "use core2::Shared;\npub struct Uses { pub s: Shared, pub f: u8 }\n",
        )
        .unwrap();
        let lib = w.join("app/src/lib.rs");
        let text = std::fs::read_to_string(&lib).unwrap();
        let l = parse_in_crate(&lib, &text, &X86_64_SYSV).unwrap();
        // Only the app crate's own structs are reported for the file.
        assert!(l.iter().all(|l| l.name != "Shared"));
        let uses = l.iter().find(|l| l.name == "Uses").unwrap();
        assert_eq!(uses.total_size, 24, "{:?}", uses.fields);
        assert!(
            uses.uncertain_fields.is_empty(),
            "{:?}",
            uses.uncertain_fields
        );
    }

    #[test]
    fn dependency_outside_the_workspace_needs_follow_dependencies() {
        let dir = tempfile::TempDir::new().unwrap();
        let w = dir.path();
        std::fs::create_dir_all(w.join("app/src")).unwrap();
        std::fs::create_dir_all(w.join("vendor/src")).unwrap();
        std::fs::write(
            w.join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nvendor = { path = \"../vendor\" }\n",
        )
        .unwrap();
        std::fs::write(
            w.join("vendor/Cargo.toml"),
            "[package]\nname = \"vendor\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            w.join("vendor/src/lib.rs"),
            "pub struct Shared { pub a: u64, pub b: u64 }\n",
        )
        .unwrap();
        std::fs::write(
            w.join("app/src/lib.rs"),
            "use vendor::Shared;\npub struct Uses { pub s: Shared, pub f: u8 }\n",
        )
        .unwrap();
        let lib = w.join("app/src/lib.rs");
        let text = std::fs::read_to_string(&lib).unwrap();
        let uses = |l: Vec<StructLayout>| l.into_iter().find(|l| l.name == "Uses").unwrap();

        crate::set_follow_dependencies(false);
        let off = uses(parse_in_crate(&lib, &text, &X86_64_SYSV).unwrap());
        assert!(!off.uncertain_fields.is_empty(), "{:?}", off.fields);

        crate::set_follow_dependencies(true);
        let on = uses(parse_in_crate(&lib, &text, &X86_64_SYSV).unwrap());
        crate::set_follow_dependencies(false);
        assert_eq!(on.total_size, 24, "{:?}", on.fields);
        assert!(on.uncertain_fields.is_empty());
    }

    #[test]
    fn submodule_inside_the_workspace_is_external() {
        let dir = tempfile::TempDir::new().unwrap();
        let sub = dir.path().join("third_party/lib");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../../.git/modules/lib\n").unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let sub = std::fs::canonicalize(&sub).unwrap();
        assert!(crate::is_external(&sub, &root));
        assert!(!crate::is_external(&root.join("third_party"), &root));
    }

    #[test]
    fn qualified_and_imported_names_pick_the_right_file() {
        let dir = crate_dir(&[
            ("lib.rs", "mod a;\nmod b;\nmod c;\nmod d;\n"),
            ("a.rs", "pub struct Config { pub x: u64 }\n"),
            ("b.rs", "pub struct Config { pub x: u8, pub y: u8 }\n"),
            (
                "c.rs",
                "use crate::a::Config;\npub struct ViaUse { pub c: Config, pub f: u8 }\n",
            ),
            (
                "d.rs",
                "pub struct ViaPath { pub c: super::b::Config, pub f: u8 }\n",
            ),
        ]);
        let get = |file: &str, name: &str| {
            let p = dir.path().join("src").join(file);
            let text = std::fs::read_to_string(&p).unwrap();
            let l = parse_in_crate(&p, &text, &X86_64_SYSV).unwrap();
            l.into_iter().find(|l| l.name == name).unwrap()
        };
        let via_use = get("c.rs", "ViaUse");
        let via_path = get("d.rs", "ViaPath");
        assert_eq!(via_use.total_size, 16, "{:?}", via_use.fields);
        assert_eq!(via_path.total_size, 3, "{:?}", via_path.fields);
        assert!(
            via_use.uncertain_fields.is_empty(),
            "{:?}",
            via_use.uncertain_fields
        );
        assert!(
            via_path.uncertain_fields.is_empty(),
            "{:?}",
            via_path.uncertain_fields
        );
    }

    #[test]
    fn path_attribute_module_is_resolved() {
        let dir = crate_dir(&[
            (
                "lib.rs",
                "#[path = \"impls/one.rs\"] mod a;\nmod b;\nmod user;\n",
            ),
            ("b.rs", "pub struct Config { pub x: u8 }\n"),
            (
                "user.rs",
                "pub struct U { pub c: crate::a::Config, pub f: u8 }\n",
            ),
        ]);
        std::fs::create_dir(dir.path().join("src/impls")).unwrap();
        std::fs::write(
            dir.path().join("src/impls/one.rs"),
            "pub struct Config { pub x: u64 }\n",
        )
        .unwrap();
        let p = dir.path().join("src/user.rs");
        let text = std::fs::read_to_string(&p).unwrap();
        let l = parse_in_crate(&p, &text, &X86_64_SYSV).unwrap();
        let u = l.iter().find(|l| l.name == "U").unwrap();
        assert_eq!(u.total_size, 16, "{:?}", u.fields);
    }
}
