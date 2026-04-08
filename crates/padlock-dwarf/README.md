# padlock-dwarf

DWARF and PDB binary analysis backend for [padlock](https://github.com/gidotencate/padlock) — a struct memory layout analyzer for C, C++, Rust, and Go.

This crate reads compiled binaries and extracts exact struct layouts as produced by the compiler:

- **DWARF** (ELF on Linux, Mach-O on macOS) — reads `DW_TAG_structure_type` and `DW_TAG_member` entries via `gimli` + `object`
- **PDB** (Windows) — reads type records via the `pdb` crate
- **`detect_arch_from_host()`** — returns the `ArchConfig` for the current build target

Unlike source analysis, DWARF-based layouts are compiler-verified: offsets, sizes, and padding are exactly what the compiler produced. This is useful for structs with `alignas`, `__attribute__((packed))`, conditional compilation, or complex type aliases that source analysis cannot fully resolve.

## Usage

`padlock-dwarf` is an internal library crate. To analyze a binary, use the CLI:

```bash
padlock analyze target/debug/myapp
cargo padlock                        # build + analyze in one step
```

## Part of padlock

- [`padlock-cli`](https://crates.io/crates/padlock-cli) — CLI (`padlock` + `cargo-padlock` binaries)
- [`padlock-core`](https://crates.io/crates/padlock-core) — IR, analysis passes, findings
- [`padlock-source`](https://crates.io/crates/padlock-source) — Source analysis (C/C++/Rust/Go)
- [`padlock-dwarf`](https://crates.io/crates/padlock-dwarf) — Binary analysis (DWARF/PDB) *(this crate)*
- [`padlock-output`](https://crates.io/crates/padlock-output) — Output formatters (terminal/JSON/SARIF/diff)
- [`padlock-macros`](https://crates.io/crates/padlock-macros) — Compile-time layout assertions
