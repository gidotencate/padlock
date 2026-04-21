# padlock-core

Core library for [padlock](https://github.com/gidotencate/padlock) — a struct memory layout analyzer for C, C++, Rust, Go, and Zig.

This crate provides:

- **Intermediate representation** (`StructLayout`, `Field`, `AccessPattern`) — the shared data model all frontends and backends produce and consume
- **Architecture constants** (`ArchConfig`) for x86-64, AArch64, Apple Silicon, WASM32, RISC-V 64
- **Analysis passes**: padding detection, reorder suggestions, false-sharing detection, locality analysis, and impact scoring
- **Report types** (`Report`, `StructReport`, `Finding`, `SkippedStruct`) — the output of running all analysis passes over a set of layouts. `Report::skipped` carries types that were encountered but not analyzed (generics/templates); `StructReport::uncertain_fields` lists fields whose sizes could not be accurately determined from source alone.

## Usage

`padlock-core` is an internal library crate. If you want to analyze struct layouts, install the CLI:

```bash
cargo install padlock-cli
```

To use `padlock-core` in your own tool, add it as a dependency and call `Report::from_layouts(&layouts)`.

The `test-helpers` feature exposes fixture layouts (`connection_layout()`, `packed_layout()`) for use in tests of crates that depend on `padlock-core`.

## Part of padlock

- [`padlock-cli`](https://crates.io/crates/padlock-cli) — CLI (`padlock` + `cargo-padlock` binaries)
- [`padlock-core`](https://crates.io/crates/padlock-core) — IR, analysis passes, findings *(this crate)*
- [`padlock-source`](https://crates.io/crates/padlock-source) — Source analysis (C/C++/Rust/Go)
- [`padlock-dwarf`](https://crates.io/crates/padlock-dwarf) — Binary analysis (DWARF/PDB)
- [`padlock-output`](https://crates.io/crates/padlock-output) — Output formatters (terminal/JSON/SARIF/diff)
- [`padlock-macros`](https://crates.io/crates/padlock-macros) — Compile-time layout assertions
