// padlock-source/src/zig_imports.rs
//
// A Zig file names types of other files through `const a = @import("a.zig");`
// and then `a.Config`. Parsing a file alone leaves such a field at a guessed
// size. Here the imported files are laid out first (recursively, cached per
// file) and the sizes of their types are handed to the Zig frontend as
// `alias.Name`. Modules registered in the nearest `build.zig` with
// `addModule("name", …)` are followed too; `std`, `builtin` and packages from
// `build.zig.zon` are not. A file outside the package (the nearest `build.zig`
// directory) or in a nested git repository is followed only with
// `follow_dependencies`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use padlock_core::arch::ArchConfig;
use padlock_core::ir::StructLayout;

/// Imports are followed this deep; deeper chains are cut off (the types stay
/// uncertain), which also bounds cyclic imports.
const MAX_DEPTH: usize = 8;

type Stamp = Vec<(PathBuf, Option<SystemTime>, u64)>;

/// What an imported file offers: `Name` and `alias.Name` → (size, align).
struct Exports {
    sizes: HashMap<String, (usize, usize)>,
    /// Every file that went into `sizes`, to notice edits.
    stamp: Stamp,
}

type Key = (PathBuf, &'static str, u64);

static EXPORTS: LazyLock<Mutex<HashMap<Key, Arc<Exports>>>> = LazyLock::new(Default::default);

fn stamp_of(path: &Path) -> (PathBuf, Option<SystemTime>, u64) {
    let md = std::fs::metadata(path).ok();
    (
        path.to_path_buf(),
        md.as_ref().and_then(|m| m.modified().ok()),
        md.map(|m| m.len()).unwrap_or(0),
    )
}

fn is_current(stamp: &Stamp) -> bool {
    stamp.iter().all(|s| *s == stamp_of(&s.0))
}

/// The file an import string names, relative to the importing file's directory
/// or through a `build.zig` module.
fn resolve_import(from: &Path, import: &str) -> Option<PathBuf> {
    if matches!(import, "std" | "builtin" | "root" | "c") {
        return None;
    }
    let dir = from.parent()?;
    let candidate = if import.ends_with(".zig") {
        dir.join(import)
    } else {
        build_module(dir, import)?
    };
    let target = std::fs::canonicalize(candidate)
        .ok()
        .filter(|p| p.is_file())?;
    // Outside the package, or in a nested git repository: only on request.
    if !crate::follow_dependencies() && crate::is_external(&target, &package_root(from)) {
        return None;
    }
    Some(target)
}

/// The directory of the nearest `build.zig` above `file`, else its own directory.
fn package_root(file: &Path) -> PathBuf {
    let own = file.parent().unwrap_or(Path::new("")).to_path_buf();
    let mut cur = Some(own.as_path());
    while let Some(d) = cur {
        if d.join("build.zig").is_file() {
            return d.to_path_buf();
        }
        cur = d.parent();
    }
    own
}

/// `b.addModule("name", .{ .root_source_file = b.path("src/x.zig") })` in the
/// nearest `build.zig` above `dir`: the path of module `name`.
fn build_module(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut cur = Some(dir);
    for _ in 0..6 {
        let d = cur?;
        if let Ok(text) = std::fs::read_to_string(d.join("build.zig")) {
            let needle = format!("\"{name}\"");
            for (at, _) in text.match_indices(&needle) {
                let before = &text[..at];
                if !(before.ends_with("addModule(") || before.ends_with("createModule(")) {
                    continue;
                }
                let window = &text[at..text.len().min(at + 400)];
                for marker in ["path(\"", ".root_source_file = \"", "LazyPath = \""] {
                    if let Some(i) = window.find(marker) {
                        let rest = &window[i + marker.len()..];
                        if let Some(end) = rest.find('"') {
                            return Some(d.join(&rest[..end]));
                        }
                    }
                }
            }
            return None;
        }
        cur = d.parent();
    }
    None
}

thread_local! {
    static IN_PROGRESS: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Sizes of the imported types for `source` (the text of `path`), keyed
/// `alias.Name`, plus the files they came from.
fn imported_sizes(
    path: &Path,
    source: &str,
    arch: &'static ArchConfig,
) -> (HashMap<String, (usize, usize)>, Stamp) {
    let mut sizes = HashMap::new();
    let mut stamp = Vec::new();
    for (alias, import) in crate::frontends::zig::imports(source) {
        let Some(target) = resolve_import(path, &import) else {
            continue;
        };
        let Some(exports) = exports_of(&target, arch) else {
            continue;
        };
        for (name, sa) in &exports.sizes {
            sizes.insert(format!("{alias}.{name}"), *sa);
        }
        stamp.extend(exports.stamp.iter().cloned());
    }
    (sizes, stamp)
}

fn exports_of(path: &Path, arch: &'static ArchConfig) -> Option<Arc<Exports>> {
    let key = (path.to_path_buf(), arch.name, crate::settings_generation());
    if let Some(hit) = EXPORTS.lock().ok()?.get(&key)
        && is_current(&hit.stamp)
    {
        return Some(hit.clone());
    }
    let depth = IN_PROGRESS.with(|p| {
        let mut p = p.borrow_mut();
        if p.contains(&path.to_path_buf()) || p.len() >= MAX_DEPTH {
            return None;
        }
        p.push(path.to_path_buf());
        Some(p.len())
    });
    depth?;
    let built = build_exports(path, arch);
    IN_PROGRESS.with(|p| p.borrow_mut().pop());
    let built = Arc::new(built?);
    if let Ok(mut cache) = EXPORTS.lock() {
        cache.insert(key, built.clone());
    }
    Some(built)
}

fn build_exports(path: &Path, arch: &'static ArchConfig) -> Option<Exports> {
    let source = std::fs::read_to_string(path).ok()?;
    let (extern_sizes, mut stamp) = imported_sizes(path, &source, arch);
    stamp.push(stamp_of(path));
    let outer = crate::take_skipped();
    let parsed = crate::frontends::zig::parse_zig_with_imports(&source, arch, extern_sizes.clone());
    crate::restore_skipped(outer);
    let mut layouts = parsed.ok()?;
    crate::resolve_nested_structs(&mut layouts);
    crate::hide_anonymous_aggregates(&mut layouts);
    let mut sizes: HashMap<String, (usize, usize)> = layouts
        .iter()
        .map(|l| (l.name.clone(), (l.total_size, l.align)))
        .collect();
    // `pub const b = @import("b.zig");` re-exports b's types as `b.Name`.
    sizes.extend(extern_sizes);
    stamp.sort();
    stamp.dedup();
    Some(Exports { sizes, stamp })
}

/// Layouts of the file at `path` (text `source`) with imported types sized, or
/// `None` when it imports no file.
pub(crate) fn parse_with_imports(
    path: &Path,
    source: &str,
    arch: &'static ArchConfig,
) -> Option<anyhow::Result<Vec<StructLayout>>> {
    let (sizes, _) = imported_sizes(&std::fs::canonicalize(path).ok()?, source, arch);
    if sizes.is_empty() {
        return None;
    }
    let mut layouts = match crate::frontends::zig::parse_zig_with_imports(source, arch, sizes) {
        Ok(l) => l,
        Err(e) => return Some(Err(e)),
    };
    crate::resolve_nested_structs(&mut layouts);
    Some(Ok(layouts))
}

/// Newest mtime among the file and the files it imports, transitively.
pub(crate) fn newest_mtime(path: &Path, arch: &'static ArchConfig) -> Option<SystemTime> {
    let path = std::fs::canonicalize(path).ok()?;
    let source = std::fs::read_to_string(&path).ok()?;
    let (_, stamp) = imported_sizes(&path, &source, arch);
    stamp.into_iter().filter_map(|(_, t, _)| t).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn imported_struct_is_sized() {
        let dir = tempfile::TempDir::new().unwrap();
        write(
            dir.path(),
            "a.zig",
            "pub const Config = struct { x: u64, y: u64 };\n",
        );
        let main = "const a = @import(\"a.zig\");\n\
                    const Uses = struct { c: a.Config, f: u8 };\n";
        write(dir.path(), "main.zig", main);
        let l = parse_with_imports(&dir.path().join("main.zig"), main, &X86_64_SYSV)
            .unwrap()
            .unwrap();
        let uses = l.iter().find(|l| l.name == "Uses").unwrap();
        assert_eq!(uses.total_size, 24, "{:?}", uses.fields);
        assert!(
            uses.uncertain_fields.is_empty(),
            "{:?}",
            uses.uncertain_fields
        );
    }

    #[test]
    fn same_type_name_in_two_imports() {
        let dir = tempfile::TempDir::new().unwrap();
        write(
            dir.path(),
            "a.zig",
            "pub const Config = struct { x: u64 };\n",
        );
        write(
            dir.path(),
            "b.zig",
            "pub const Config = struct { x: u8 };\n",
        );
        let main = "const a = @import(\"a.zig\");\nconst b = @import(\"b.zig\");\n\
                    const Uses = struct { p: a.Config, q: b.Config };\n";
        write(dir.path(), "main.zig", main);
        let l = parse_with_imports(&dir.path().join("main.zig"), main, &X86_64_SYSV)
            .unwrap()
            .unwrap();
        let uses = l.iter().find(|l| l.name == "Uses").unwrap();
        assert_eq!(uses.total_size, 16, "{:?}", uses.fields);
    }

    #[test]
    fn transitive_and_cyclic_imports() {
        let dir = tempfile::TempDir::new().unwrap();
        write(
            dir.path(),
            "a.zig",
            "const b = @import(\"b.zig\");\npub const A = struct { inner: b.B, t: u8 };\n",
        );
        write(
            dir.path(),
            "b.zig",
            "const a = @import(\"a.zig\");\npub const B = struct { x: u64, y: u32 };\n",
        );
        let main = "const a = @import(\"a.zig\");\nconst U = struct { v: a.A, z: u8 };\n";
        write(dir.path(), "main.zig", main);
        let l = parse_with_imports(&dir.path().join("main.zig"), main, &X86_64_SYSV)
            .unwrap()
            .unwrap();
        let u = l.iter().find(|l| l.name == "U").unwrap();
        // A = B(16) + u8 → 24; U = 24 + u8 → 32.
        assert_eq!(u.total_size, 32, "{:?}", u.fields);
    }

    #[test]
    fn build_zig_module_is_followed() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        write(
            dir.path(),
            "build.zig",
            "pub fn build(b: *std.Build) void { _ = b.addModule(\"core\", .{ .root_source_file = b.path(\"src/core.zig\") }); }\n",
        );
        write(
            &dir.path().join("src"),
            "core.zig",
            "pub const Core = struct { x: u64, y: u64 };\n",
        );
        let main = "const core = @import(\"core\");\nconst U = struct { c: core.Core, f: u8 };\n";
        write(&dir.path().join("src"), "main.zig", main);
        let l = parse_with_imports(&dir.path().join("src/main.zig"), main, &X86_64_SYSV)
            .unwrap()
            .unwrap();
        let u = l.iter().find(|l| l.name == "U").unwrap();
        assert_eq!(u.total_size, 24, "{:?}", u.fields);
    }

    #[test]
    fn std_allocator_is_two_words() {
        let src = "const std = @import(\"std\");\nconst mem = std.mem;\n\
                   const S = struct { a: mem.Allocator, f: u8 };\n";
        let l = crate::frontends::zig::parse_zig(src, &X86_64_SYSV).unwrap();
        let s = l.iter().find(|l| l.name == "S").unwrap();
        assert_eq!(s.total_size, 24, "{:?}", s.fields);
        assert!(s.uncertain_fields.is_empty(), "{:?}", s.uncertain_fields);
    }

    #[test]
    fn import_leaving_the_package_needs_follow_dependencies() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("pkg")).unwrap();
        write(
            dir.path(),
            "shared.zig",
            "pub const Shared = struct { x: u64, y: u64 };\n",
        );
        let main =
            "const s = @import(\"../shared.zig\");\nconst U = struct { v: s.Shared, f: u8 };\n";
        let pkg = dir.path().join("pkg");
        write(&pkg, "main.zig", main);
        crate::set_follow_dependencies(false);
        assert!(parse_with_imports(&pkg.join("main.zig"), main, &X86_64_SYSV).is_none());
        crate::set_follow_dependencies(true);
        let l = parse_with_imports(&pkg.join("main.zig"), main, &X86_64_SYSV);
        crate::set_follow_dependencies(false);
        let l = l.unwrap().unwrap();
        let u = l.iter().find(|l| l.name == "U").unwrap();
        assert_eq!(u.total_size, 24, "{:?}", u.fields);
    }
}
