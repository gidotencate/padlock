// padlock-cli/src/paths.rs
//
// Utilities for collecting StructLayouts from one or more paths.
//
// Paths are processed as follows:
//   - Directories: walked recursively for source files (.c, .cpp, .rs, .go).
//     Hidden directories and well-known build artifact directories are skipped.
//   - Source files: parsed via padlock_source.
//   - Anything else: treated as a compiled binary and analyzed via padlock_dwarf.
//
// Errors from individual files are printed as warnings and skipped rather than
// aborting the whole run, so a single unparseable file doesn't block analysis
// of the rest of the project.

use std::path::{Path, PathBuf};

use padlock_core::ir::StructLayout;

/// Collect StructLayouts from a list of paths and return them together with
/// the list of paths that were actually analyzed (for reporting purposes).
///
/// Directories are expanded recursively. Errors from individual files are
/// printed as warnings and skipped.
pub fn collect_layouts(paths: &[PathBuf]) -> anyhow::Result<(Vec<StructLayout>, Vec<String>)> {
    let mut all_layouts: Vec<StructLayout> = Vec::new();
    let mut analyzed: Vec<String> = Vec::new();

    for path in paths {
        if path.is_dir() {
            let files = walk_source_files(path);
            if files.is_empty() {
                eprintln!(
                    "padlock: warning: no source files found in {}",
                    path.display()
                );
                continue;
            }
            let arch = padlock_dwarf::reader::detect_arch_from_host();
            for file in files {
                match padlock_source::parse_source(&file, arch) {
                    Ok(layouts) => {
                        analyzed.push(file.display().to_string());
                        all_layouts.extend(layouts);
                    }
                    Err(e) => eprintln!("padlock: warning: {}: {e}", file.display()),
                }
            }
        } else if padlock_source::detect_language(path).is_some() {
            let arch = padlock_dwarf::reader::detect_arch_from_host();
            let layouts = padlock_source::parse_source(path, arch)?;
            analyzed.push(path.display().to_string());
            all_layouts.extend(layouts);
        } else {
            // Binary — try BTF first (eBPF objects), then fall back to DWARF.
            let data = std::fs::read(path)?;
            let arch = padlock_dwarf::reader::detect_arch(&data)
                .unwrap_or_else(|_| padlock_dwarf::reader::detect_arch_from_host());
            let layouts = if has_btf_section(&data) {
                extract_btf_layouts(&data, arch)?
            } else {
                let dwarf = padlock_dwarf::reader::load(&data)?;
                padlock_dwarf::extractor::Extractor::new(&dwarf, arch).extract_all()?
            };
            analyzed.push(path.display().to_string());
            all_layouts.extend(layouts);
        }
    }

    Ok((all_layouts, analyzed))
}

/// Walk `dir` recursively and return paths of all source files found.
///
/// Skips:
///   - Hidden directories (names starting with `.`)
///   - `target/`, `node_modules/`, `_build/`, `dist/`, `build/`, `vendor/`
///
/// Results are returned in a stable (sorted) order.
pub fn walk_source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_inner(dir, &mut files);
    files
}

fn walk_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read.filter_map(|e| e.ok()).collect();
    // Stable order: sort by file name so output is deterministic.
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if should_skip_dir(&name) {
                continue;
            }
            walk_inner(&path, out);
        } else if padlock_source::detect_language(&path).is_some() {
            out.push(path);
        }
    }
}

/// Returns `true` if the binary data contains a `.BTF` ELF section.
fn has_btf_section(data: &[u8]) -> bool {
    use object::Object;
    object::File::parse(data)
        .map(|f| f.section_by_name(".BTF").is_some())
        .unwrap_or(false)
}

/// Extract struct layouts from the `.BTF` section of a binary.
fn extract_btf_layouts(
    data: &[u8],
    arch: &'static padlock_core::arch::ArchConfig,
) -> anyhow::Result<Vec<padlock_core::ir::StructLayout>> {
    use object::{Object, ObjectSection};
    let obj = object::File::parse(data)?;
    let section = obj
        .section_by_name(".BTF")
        .ok_or_else(|| anyhow::anyhow!("no .BTF section"))?;
    let btf_data = section.data()?;
    padlock_dwarf::btf::extract_from_btf(btf_data, arch)
}

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        "target" | "node_modules" | "_build" | "dist" | "build" | "vendor"
    ) || name.starts_with('.')
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn walk_finds_rust_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("foo.rs"), "struct Foo { x: i32 }").unwrap();
        fs::write(dir.path().join("bar.txt"), "not a source file").unwrap();
        let files = walk_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].to_string_lossy().ends_with("foo.rs"));
    }

    #[test]
    fn walk_finds_c_and_go_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.c"), "struct A { int x; };").unwrap();
        fs::write(dir.path().join("b.go"), "type B struct { X int }").unwrap();
        let files = walk_source_files(dir.path());
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn walk_skips_target_dir() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("foo.rs"), "struct Foo { x: i32 }").unwrap();
        let files = walk_source_files(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn walk_skips_node_modules() {
        let dir = TempDir::new().unwrap();
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        fs::write(nm.join("foo.rs"), "struct Foo { x: i32 }").unwrap();
        let files = walk_source_files(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn walk_skips_hidden_dirs() {
        let dir = TempDir::new().unwrap();
        let hidden = dir.path().join(".hidden");
        fs::create_dir(&hidden).unwrap();
        fs::write(hidden.join("foo.rs"), "struct Foo { x: i32 }").unwrap();
        let files = walk_source_files(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn walk_is_recursive() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("src");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("lib.rs"), "struct Lib { x: i32 }").unwrap();
        let files = walk_source_files(dir.path());
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn walk_output_is_stable() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("b.rs"), "struct B { x: i32 }").unwrap();
        fs::write(dir.path().join("a.rs"), "struct A { x: i32 }").unwrap();
        let files = walk_source_files(dir.path());
        assert_eq!(files.len(), 2);
        // a.rs should come before b.rs (sorted)
        assert!(files[0].file_name().unwrap() < files[1].file_name().unwrap());
    }

    #[test]
    fn collect_layouts_from_rust_source() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("s.rs");
        fs::write(&file, "struct S { x: i32, y: i32 }").unwrap();
        let (layouts, analyzed) = collect_layouts(&[file]).unwrap();
        assert!(!layouts.is_empty());
        assert_eq!(analyzed.len(), 1);
        assert!(layouts.iter().any(|l| l.name == "S"));
    }

    #[test]
    fn collect_layouts_from_directory() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.rs"), "struct A { x: i32 }").unwrap();
        fs::write(dir.path().join("b.rs"), "struct B { y: i64 }").unwrap();
        let (layouts, analyzed) = collect_layouts(&[dir.path().to_path_buf()]).unwrap();
        assert!(layouts.iter().any(|l| l.name == "A"));
        assert!(layouts.iter().any(|l| l.name == "B"));
        assert_eq!(analyzed.len(), 2);
    }

    #[test]
    fn collect_layouts_multiple_explicit_files() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        fs::write(&a, "struct A { x: i32 }").unwrap();
        fs::write(&b, "struct B { y: i64 }").unwrap();
        let (layouts, _) = collect_layouts(&[a, b]).unwrap();
        assert!(layouts.iter().any(|l| l.name == "A"));
        assert!(layouts.iter().any(|l| l.name == "B"));
    }
}
