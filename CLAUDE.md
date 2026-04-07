# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is padlock

`padlock` is a struct memory layout analyzer for C, C++, Rust, and Go. It finds padding waste, false sharing, and cache locality problems in struct/type definitions — ranks issues by impact, auto-fixes field ordering, and flags concurrency risks. It is CLI-first and CI-ready, targeting multiple CPU architectures.

## Commands

```bash
# Build
cargo build

# Build release binary
cargo build --release

# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p padlock-core

# Run a single test by name
cargo test -p padlock-core test_name

# Check (faster than build, no codegen)
cargo check

# Lint
cargo clippy

# Format
cargo fmt

# Run the CLI (after build)
./target/debug/padlock
```

## Crate architecture

This is a Cargo workspace with five crates:

- **`padlock-core`** — Shared IR types, analysis passes, and findings. The central dependency for all other crates. Contains:
  - `ir.rs` — Intermediate representation of struct layouts
  - `arch.rs` — Architecture-specific alignment/size constants
  - `findings.rs` — Finding types (padding waste, false sharing, locality issues)
  - `analysis/` — Analysis passes: `padding`, `reorder`, `locality`, `false_sharing`, `scorer`

- **`padlock-dwarf`** — Binary analysis backend. Reads DWARF debug info (via `gimli` + `object`) and PDB files (via `pdb`) to extract struct layout data from compiled binaries. Produces `padlock-core` IR.

- **`padlock-source`** — Source analysis backend. Parses source files using `syn` (Rust) and `tree-sitter` (C/C++/Go) to extract struct definitions. Also handles concurrency annotation detection (`concurrency.rs`) and fix generation (`fixgen.rs`).

- **`padlock-output`** — Output formatters: JSON, SARIF (for CI/tooling integration), human-readable summary, and diff format.

- **`padlock-cli`** — The `padlock` binary. Wires together the other crates via subcommands: `analyze`, `fix`, `list`, `diff`, `report`. Uses `clap` (derive API) for CLI parsing, `owo-colors` for terminal color, `comfy-table` for table output. The `fix` subcommand rewrites struct fields in-place (with `.bak` backup) using span-finding + fix-generation from `padlock-source`. The `diff` subcommand shows a before/after unified diff without modifying files.

## Data flow

Source/binary input → (`padlock-dwarf` or `padlock-source`) → `padlock-core` IR → analysis passes → findings → (`padlock-output`) → terminal/JSON/SARIF

## Key implementation details

- **Supported architectures** (`padlock-core/src/arch.rs`): `X86_64_SYSV`, `AARCH64`, `AARCH64_APPLE` (128-byte cache line), `WASM32`, `RISCV64`. The default throughout the codebase is `X86_64_SYSV`.
- **Test fixtures**: `padlock-core` has a `test-helpers` feature that exposes `ir::test_fixtures` (e.g. `connection_layout()`, `packed_layout()`) for use in other crates: `cargo test -p padlock-core --features test-helpers`.
- **`Report::from_layouts`** in `findings.rs` is the single entry point that runs all analysis passes and produces `StructReport` results with scored findings.
- **Severity thresholds**: PaddingWaste ≥30% waste → High, ≥10% → Medium, <10% → Low. ReorderSuggestion savings ≥8 bytes → High, else Medium. FalseSharing is always High.
