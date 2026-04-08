# padlock-source

Source analysis backend for [padlock](https://github.com/gidotencate/padlock) — a struct memory layout analyzer for C, C++, Rust, and Go.

This crate parses source files without invoking a compiler, using tree-sitter (C/C++/Go) and syn (Rust) to extract struct definitions and simulate their memory layout.

## Supported languages

| Language | Parser | Notes |
|---|---|---|
| C | tree-sitter-c | structs, unions, typedefs, bit-fields |
| C++ | tree-sitter-cpp | classes, inheritance, vtable pointer, templates |
| Rust | syn | `repr(C)`, `repr(packed)`, all field types |
| Go | tree-sitter-go | built-in types, `sync.Mutex`, slices, maps |

## Concurrency annotation

Guard annotations are read from source to enable accurate false-sharing detection:

- **Rust**: `#[lock_protected_by = "mu"]`, `#[guarded_by("mu")]`
- **C/C++**: `GUARDED_BY(mu)`, `__attribute__((guarded_by(mu)))`
- **Go**: `// padlock:guard=mu`, `// guarded_by: mu`, `// +checklocksprotects:mu`

Fields without explicit annotations are inferred from their type names (`Mutex`, `std::atomic`, `sync.Mutex`, …).

## Source locations

`parse_source(path, arch)` populates `StructLayout.source_file` and `StructLayout.source_line` for every returned layout, enabling line-level navigation in CLI output and SARIF reports.

## Usage

`padlock-source` is an internal library crate. To analyze source files, use the CLI:

```bash
padlock analyze src/
padlock analyze myfile.rs myfile.cpp
```

## Part of padlock

- [`padlock-cli`](https://crates.io/crates/padlock-cli) — CLI (`padlock` + `cargo-padlock` binaries)
- [`padlock-core`](https://crates.io/crates/padlock-core) — IR, analysis passes, findings
- [`padlock-source`](https://crates.io/crates/padlock-source) — Source analysis (C/C++/Rust/Go) *(this crate)*
- [`padlock-dwarf`](https://crates.io/crates/padlock-dwarf) — Binary analysis (DWARF/PDB)
- [`padlock-output`](https://crates.io/crates/padlock-output) — Output formatters (terminal/JSON/SARIF/diff)
- [`padlock-macros`](https://crates.io/crates/padlock-macros) — Compile-time layout assertions
