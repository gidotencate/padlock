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

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use padlock_core::findings::SkippedStruct;
use padlock_core::ir::StructLayout;
use rayon::prelude::*;

/// Collect StructLayouts from a list of paths and return them together with
/// the list of paths that were actually analyzed and any skipped types.
///
/// Directories are expanded recursively and parsed in parallel.  Source files
/// are served from an on-disk mtime cache when possible, skipping unchanged
/// files.  Errors from individual files are printed as warnings and skipped.
///
/// When `include_generated` is `false` (the default), files detected as
/// machine-generated are silently excluded from analysis.  Pass `true` to
/// include them (e.g. when analysing a code generator's own output).
pub fn collect_layouts(
    paths: &[PathBuf],
    include_generated: bool,
) -> anyhow::Result<(Vec<StructLayout>, Vec<String>, Vec<SkippedStruct>)> {
    let mut all_layouts: Vec<StructLayout> = Vec::new();
    let mut analyzed: Vec<String> = Vec::new();
    let mut all_skipped: Vec<SkippedStruct> = Vec::new();
    let mut cache = crate::cache::ParseCache::load();

    for path in paths {
        if path.is_dir() {
            let mut files = walk_source_files(path);
            if !include_generated {
                files.retain(|f| !is_generated_file(f));
            }
            if files.is_empty() {
                eprintln!(
                    "padlock: warning: no source files found in {}",
                    path.display()
                );
                continue;
            }
            let arch = padlock_dwarf::reader::detect_arch_from_host();

            // Partition files into cache hits and misses.
            let mut hits: Vec<(
                PathBuf,
                Vec<StructLayout>,
                Vec<padlock_core::findings::SkippedStruct>,
            )> = Vec::new();
            let mut misses: Vec<PathBuf> = Vec::new();
            for file in &files {
                if let Some((layouts, skipped)) = cache.get(file) {
                    hits.push((file.clone(), layouts, skipped));
                } else {
                    misses.push(file.clone());
                }
            }

            // Parse cache-miss files in parallel, with a progress indicator on
            // stderr when there are enough misses to be worth showing (≥20) and
            // stderr is a terminal.  The counter starts at the number of cache
            // hits so the displayed N/M reflects all files, not just misses.
            let total_files = files.len();
            let n_hits = hits.len();
            let parsed_count = Arc::new(AtomicUsize::new(n_hits));
            let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

            let progress_thread = if misses.len() >= 20 && std::io::stderr().is_terminal() {
                let counter = parsed_count.clone();
                let stop = stop_flag.clone();
                Some(std::thread::spawn(move || {
                    use std::io::Write;
                    loop {
                        let n = counter.load(Ordering::Relaxed);
                        eprint!("\r  padlock: scanning {n} / {total_files} files…");
                        let _ = std::io::stderr().flush();
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    // Clear the progress line.
                    eprint!("\r{:<60}\r", "");
                    let _ = std::io::stderr().flush();
                }))
            } else {
                None
            };

            let counter_ref = parsed_count.clone();
            let miss_results: Vec<_> = misses
                .par_iter()
                .map(|file| {
                    let r = padlock_source::parse_source(file, arch);
                    counter_ref.fetch_add(1, Ordering::Relaxed);
                    (file.clone(), r)
                })
                .collect();

            stop_flag.store(true, Ordering::Relaxed);
            if let Some(handle) = progress_thread {
                let _ = handle.join();
            }

            // Store new results in cache (layouts + skipped).
            for (file, result) in &miss_results {
                if let Ok(output) = result {
                    cache.insert(file, output.layouts.clone(), output.skipped.clone());
                }
            }

            // Build O(1) lookup maps before merging — avoids O(n²) linear scans
            // over hits/miss_results for each file in the 64 K-entry walk order.
            let hit_map: HashMap<&PathBuf, (&Vec<StructLayout>, &Vec<SkippedStruct>)> =
                hits.iter().map(|(p, l, s)| (p, (l, s))).collect();
            let mut miss_map: HashMap<PathBuf, _> = miss_results.into_iter().collect();

            // Merge hits and misses back in original file order.
            for file in &files {
                if let Some((layouts, skipped)) = hit_map.get(file) {
                    analyzed.push(file.display().to_string());
                    all_layouts.extend((*layouts).clone());
                    all_skipped.extend((*skipped).clone());
                } else if let Some(result) = miss_map.remove(file) {
                    match result {
                        Ok(output) => {
                            analyzed.push(file.display().to_string());
                            all_layouts.extend(output.layouts);
                            all_skipped.extend(output.skipped);
                        }
                        Err(e) => eprintln!("padlock: warning: {}: {e}", file.display()),
                    }
                }
            }
        } else if padlock_source::detect_language(path).is_some() {
            if !include_generated && is_generated_file(path) {
                continue;
            }
            let arch = padlock_dwarf::reader::detect_arch_from_host();
            // Try cache for single source files too.
            let (layouts, skipped) = if let Some((layouts, skipped)) = cache.get(path) {
                (layouts, skipped)
            } else {
                let output = padlock_source::parse_source(path, arch)?;
                cache.insert(path, output.layouts.clone(), output.skipped.clone());
                (output.layouts, output.skipped)
            };
            analyzed.push(path.display().to_string());
            all_layouts.extend(layouts);
            all_skipped.extend(skipped);
        } else {
            // Binary — route by format.
            let data = std::fs::read(path)?;
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let layouts = if ext == "pdb" {
                // PDB (Windows MSVC debug database)
                let arch = padlock_dwarf::reader::detect_arch_from_host();
                padlock_dwarf::pdb_reader::extract_from_pdb(&data, arch)?
            } else if is_raw_btf(&data) {
                // Raw BTF file (not an ELF container): `.btf` files produced by
                // `bpftool btf dump`, `/sys/kernel/btf/vmlinux`, etc.
                let arch = padlock_dwarf::reader::detect_arch_from_host();
                padlock_dwarf::btf::extract_from_btf(&data, arch)?
            } else if has_btf_section(&data) {
                let arch = padlock_dwarf::reader::detect_arch(&data)
                    .unwrap_or_else(|_| padlock_dwarf::reader::detect_arch_from_host());
                extract_btf_layouts(&data, arch)?
            } else {
                let arch = padlock_dwarf::reader::detect_arch(&data)
                    .unwrap_or_else(|_| padlock_dwarf::reader::detect_arch_from_host());
                let dwarf = padlock_dwarf::reader::load(&data)?;
                padlock_dwarf::extractor::Extractor::new(&dwarf, arch).extract_all()?
            };
            analyzed.push(path.display().to_string());
            all_layouts.extend(layouts);
        }
    }

    // Persist updated cache entries.
    cache.flush();

    // Deduplicate layouts by (source_file, source_line): the same struct parsed
    // from the same file at the same line is a duplicate.  This happens when a
    // header is found via multiple overlapping scan paths or passed twice.
    let mut seen: std::collections::HashSet<(String, u32)> = std::collections::HashSet::new();
    all_layouts.retain(|l| match (&l.source_file, l.source_line) {
        (Some(f), Some(line)) => seen.insert((f.clone(), line)),
        _ => true,
    });

    Ok((all_layouts, analyzed, all_skipped))
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

/// Returns `true` when `data` is a raw BTF blob (not an ELF container).
///
/// A raw BTF file starts with the BTF magic `0xEB9F` (little-endian u16) and is
/// *not* an ELF file (which starts with `\x7FELF`).  Raw BTF blobs are produced
/// by tools such as `bpftool btf dump file <obj> format raw` and are also exposed
/// by the kernel at `/sys/kernel/btf/vmlinux`.
fn is_raw_btf(data: &[u8]) -> bool {
    const BTF_MAGIC: u16 = 0xEB9F;
    const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
    if data.len() < 4 {
        return false;
    }
    let magic = u16::from_le_bytes([data[0], data[1]]);
    magic == BTF_MAGIC && data[..4] != ELF_MAGIC
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

/// Returns `true` when the file declares itself as machine-generated.
///
/// Checks (in order):
/// 1. Extension: `.pb.h`, `.pb.cc`, `.pb.c`, `.pb.cpp` (protobuf-generated C/C++)
/// 2. First 512 bytes for standard generation markers:
///    - Go:   `// Code generated` (official Go generator convention)
///    - Rust:  `// @generated` or `//! @generated` (prost, relay, etc.)
///    - C/C++: `// Generated by` or `/* Generated by`
pub fn is_generated_file(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if name.ends_with(".pb.h")
        || name.ends_with(".pb.cc")
        || name.ends_with(".pb.c")
        || name.ends_with(".pb.cpp")
    {
        return true;
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut buf = [0u8; 512];
    let n = f.read(&mut buf).unwrap_or(0);
    let Ok(head) = std::str::from_utf8(&buf[..n]) else {
        return false;
    };
    head.contains("// Code generated")
        || head.contains("// @generated")
        || head.contains("//! @generated")
        || head.contains("// Generated by")
        || head.contains("/* Generated by")
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
        let (layouts, analyzed, _skipped) = collect_layouts(&[file], false).unwrap();
        assert!(!layouts.is_empty());
        assert_eq!(analyzed.len(), 1);
        assert!(layouts.iter().any(|l| l.name == "S"));
    }

    #[test]
    fn collect_layouts_from_directory() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.rs"), "struct A { x: i32 }").unwrap();
        fs::write(dir.path().join("b.rs"), "struct B { y: i64 }").unwrap();
        let (layouts, analyzed, _skipped) =
            collect_layouts(&[dir.path().to_path_buf()], false).unwrap();
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
        let (layouts, _, _skipped) = collect_layouts(&[a, b], false).unwrap();
        assert!(layouts.iter().any(|l| l.name == "A"));
        assert!(layouts.iter().any(|l| l.name == "B"));
    }

    // ── is_raw_btf ────────────────────────────────────────────────────────────

    /// Build a minimal valid raw BTF header (just enough for magic detection).
    fn raw_btf_header() -> Vec<u8> {
        // BTF_MAGIC = 0xEB9F, stored little-endian → bytes [0x9F, 0xEB]
        let mut data = vec![0u8; 24];
        data[0] = 0x9F;
        data[1] = 0xEB;
        data
    }

    #[test]
    fn is_raw_btf_detects_btf_magic() {
        assert!(
            is_raw_btf(&raw_btf_header()),
            "valid BTF magic must be detected"
        );
    }

    #[test]
    fn is_raw_btf_rejects_elf_binary() {
        // ELF files start with \x7FELF — must never be treated as raw BTF even
        // if (by coincidence) the first two bytes happened to match the BTF magic.
        let elf_header = b"\x7FELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        assert!(
            !is_raw_btf(elf_header),
            "ELF binary must not be detected as raw BTF"
        );
    }

    #[test]
    fn is_raw_btf_rejects_arbitrary_data() {
        assert!(
            !is_raw_btf(b"not btf data at all"),
            "arbitrary data must not be detected as BTF"
        );
        assert!(
            !is_raw_btf(b"\x00\x00\x00\x00"),
            "zero bytes must not be detected as BTF"
        );
    }

    #[test]
    fn is_raw_btf_rejects_too_short_data() {
        assert!(!is_raw_btf(&[]), "empty data must return false");
        assert!(!is_raw_btf(&[0x9F]), "1 byte must return false");
        assert!(!is_raw_btf(&[0x9F, 0xEB]), "2 bytes must return false");
        assert!(
            !is_raw_btf(&[0x9F, 0xEB, 0x01]),
            "3 bytes must return false"
        );
    }

    #[test]
    fn is_raw_btf_accepts_minimal_4_byte_btf() {
        // Exactly 4 bytes with BTF magic is the minimum accepted (no ELF header possible)
        assert!(
            is_raw_btf(&[0x9F, 0xEB, 0x00, 0x00]),
            "4-byte BTF magic must be detected"
        );
    }

    // ── is_generated_file ─────────────────────────────────────────────────────

    fn write_tmp(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn generated_go_code_generated_marker() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(
            &dir,
            "gen.go",
            "// Code generated by protoc-gen-go. DO NOT EDIT.\npackage foo\n",
        );
        assert!(is_generated_file(&p));
    }

    #[test]
    fn generated_rust_at_generated_marker() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(
            &dir,
            "gen.rs",
            "// @generated\n// This file was generated.\npub struct Foo {}\n",
        );
        assert!(is_generated_file(&p));
    }

    #[test]
    fn generated_rust_bang_at_generated_marker() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(&dir, "gen.rs", "//! @generated\npub struct Foo {}\n");
        assert!(is_generated_file(&p));
    }

    #[test]
    fn generated_c_generated_by_line_comment() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(
            &dir,
            "gen.h",
            "// Generated by flatc. DO NOT EDIT.\n#pragma once\n",
        );
        assert!(is_generated_file(&p));
    }

    #[test]
    fn generated_c_generated_by_block_comment() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(&dir, "gen.h", "/* Generated by protoc. */\n#pragma once\n");
        assert!(is_generated_file(&p));
    }

    #[test]
    fn generated_pb_h_extension() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(&dir, "foo.pb.h", "#pragma once\nstruct Foo {};\n");
        assert!(is_generated_file(&p));
    }

    #[test]
    fn generated_pb_cc_extension() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(&dir, "foo.pb.cc", "#include \"foo.pb.h\"\n");
        assert!(is_generated_file(&p));
    }

    #[test]
    fn generated_pb_c_extension() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(&dir, "foo.pb.c", "/* protobuf output */\n");
        assert!(is_generated_file(&p));
    }

    #[test]
    fn generated_pb_cpp_extension() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(&dir, "foo.pb.cpp", "/* protobuf output */\n");
        assert!(is_generated_file(&p));
    }

    #[test]
    fn not_generated_normal_go_file() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(&dir, "main.go", "package main\ntype Foo struct { X int }\n");
        assert!(!is_generated_file(&p));
    }

    #[test]
    fn not_generated_normal_rust_file() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(
            &dir,
            "lib.rs",
            "// My hand-written library.\npub struct Bar {}\n",
        );
        assert!(!is_generated_file(&p));
    }

    #[test]
    fn not_generated_normal_c_file() {
        let dir = TempDir::new().unwrap();
        let p = write_tmp(&dir, "foo.h", "#pragma once\nstruct MyStruct { int x; };\n");
        assert!(!is_generated_file(&p));
    }
}
