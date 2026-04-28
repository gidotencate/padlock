# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is padlock

`padlock` is a struct memory layout analyzer for C, C++, Rust, Go, and Zig. It finds padding waste, false sharing, and cache locality problems in struct/type definitions — ranks issues by impact, auto-fixes field ordering, and flags concurrency risks. It is CLI-first and CI-ready, targeting multiple CPU architectures.

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

# Format (always run before committing — CI enforces this)
cargo fmt
cargo fmt --check   # verify clean

# Run the CLI (after build)
./target/debug/padlock
```

**Commit workflow**: `cargo fmt` → `cargo fmt --check` → `cargo clippy --workspace -- -D warnings` → `cargo test` → commit. Both `cargo fmt` and `cargo clippy -D warnings` are enforced by CI and will fail the build if skipped.

**Version bumps** touch six files: `Cargo.toml` (workspace), `crates/padlock-{cli,dwarf,output,source}/Cargo.toml` (inter-crate dep versions), and `editors/vscode/package.json`.

## Crate architecture

This is a Cargo workspace with six crates:

- **`padlock-core`** — Shared IR types, analysis passes, and findings. The central dependency for all other crates. Contains:
  - `ir.rs` — Intermediate representation of struct layouts
  - `arch.rs` — Architecture-specific alignment/size constants
  - `findings.rs` — Finding types (padding waste, false sharing, locality issues)
  - `analysis/` — Analysis passes: `padding`, `reorder`, `locality`, `false_sharing`, `scorer`

- **`padlock-dwarf`** — Binary analysis backend. Reads DWARF debug info (via `gimli` + `object`), raw BTF sections/files, and PDB files (Windows MSVC debug format, via `pdb` crate) to extract struct layout data from compiled binaries. Produces `padlock-core` IR.
  - **DWARF bitfields**: consecutive bitfield members at the same `DW_AT_data_member_location` are grouped into a single synthetic `[a:3|b:5]` field sized by `DW_AT_byte_size` on the member. Groups where `DW_AT_byte_size` is absent fall back to `uncertain_fields`.
  - **PDB support** (`pdb_reader.rs`): iterates the TPI stream, collects non-forward-reference `Class`, `Union`, and `Enumeration` records, resolves `FieldList` members. Bitfield members (where `field_type` is a `TypeData::Bitfield` record) are grouped the same way as DWARF; unknown storage-unit sizes are added to `uncertain_fields`. `Enumeration` types emit a single `__discriminant` field. Array sizes use `dimensions.last()` (cumulative byte lengths, not element counts). Source file/line is not available from PDB type records. Routes to this reader when the input file has a `.pdb` extension.

- **`padlock-source`** — Source analysis backend. Parses source files using `syn` (Rust) and `tree-sitter` (C/C++/Go/Zig) to extract struct definitions. Also handles concurrency annotation detection (`concurrency.rs`) and fix generation (`fixgen.rs`).
  - **Explicit guard annotations**: the Rust frontend reads `#[lock_protected_by = "mu"]`, `#[guarded_by("mu")]`, `#[protected_by = "mu"]`, `#[pt_guarded_by(...)]` on individual fields and sets `AccessPattern::Concurrent { guard }` directly — bypassing the heuristic type-name pass in `concurrency.rs` (which skips non-`Unknown` fields). The C/C++ frontend extracts `GUARDED_BY(mu)` / `__attribute__((guarded_by(mu)))` / `PT_GUARDED_BY(mu)` from field source text. The Go frontend reads `// padlock:guard=mu`, `// guarded_by: mu`, and `// +checklocksprotects:mu` (gVisor-style) as trailing line comments.
  - **Fix generation** (`fixgen.rs`): two-layer design. Source-aware generators (`generate_*_fix_from_source`) extract verbatim field chunks from original source (preserving `pub`, attributes, doc-comments, guard annotations) and reorder them. IR-based generators (`generate_*_fix`) synthesise from type names and serve as fallback. `apply_fixes_with_source` passes the found struct text to the source-aware generator; `find_*_struct_span` locates struct byte ranges for in-place rewriting.
  - **C/C++ typedef alias resolution**: `extract_structs_from_tree` does a phase-0 pre-scan via `collect_typedef_aliases` that walks `type_definition` nodes and builds a `HashMap<String, String>` of scalar alias → base type (e.g. `typedef uint32_t MyId` → `{"MyId": "uint32_t"}`). Struct/union/function-pointer typedefs are skipped. The map is threaded through `parse_struct_or_union_specifier` and `parse_class_specifier` and applied before calling `c_type_size_align`.
  - **Go interface sizing**: `extract_structs` does a phase-1 pre-scan via `collect_go_interface_names` that walks `type_spec` nodes with `interface_type` children, building a `HashSet<String>` of locally-declared interface names. In `parse_struct_type`, fields whose type matches a local interface are sized as 2-word fat pointers (16B on 64-bit). Fields with qualified types (e.g. `driver.Connector`, `io.Reader`) that cannot be resolved from source alone are added to `StructLayout::uncertain_fields`.
  - **Rust `dyn Trait` fat pointers**: `Box<dyn Trait>`, `Arc<dyn Trait>`, `Rc<dyn Trait>`, `Weak<dyn Trait>`, `&dyn Trait`, `&mut dyn Trait`, `*const dyn Trait`, `*mut dyn Trait` are all 2-word fat pointers (16B on 64-bit). The `rust_type_size_align` function in `rust.rs` detects `Type::TraitObject` inside smart pointer angle-bracket arguments and raw/reference pointer elements.
  - **Generics/templates are a fundamental limitation**: Generic Rust structs (`struct Foo<T>`), C++ templates (`template<typename T> struct Foo`), and Go generic structs (`type Pair[T any] struct`) cannot be accurately sized from source alone without monomorphisation. These are **fully skipped** and emit a `padlock: note: skipping '...'` message to **stderr** (not structured JSON/SARIF output). Zig comptime field types (`type`, `anytype`, `comptime_int`, `comptime_float`) cannot be runtime-sized — the field is kept at pointer-size and added to `uncertain_fields` so the output layer warns the user. Use binary analysis (DWARF/BTF) for exact measurements.
  - **C/C++ bitfields**: consecutive bitfields are grouped into storage-unit-sized synthetic fields (e.g. `[a:3|b:5]`) following GCC/Clang ABI rules. Anonymous padding bits (`int : 3`) consume bits in the current unit but are omitted from the label; all-anonymous units emit a `[__pad]` placeholder so total size is correct. MSVC mixed-type packing is not modelled. See `resolve_bitfield_groups` in `c_cpp.rs`.

- **`padlock-output`** — Output formatters: JSON, SARIF (for CI/tooling integration), human-readable summary, diff format, per-struct explanations, and project-level markdown summary.
  - `src/summary.rs` — human-readable multi-line report; groups structs by source file, renders findings with gap locations and severity labels, emits `uncertain_fields` notes.
  - `src/explain.rs` — verbose per-struct explanations with actionable advice for each finding type (used by the `explain` subcommand).
  - `src/markdown.rs` — project-level markdown summary (used by the `report` subcommand).
  - `src/project_summary.rs` — aggregated project-wide stats and top-issues summary.

- **`padlock-cli`** — The `padlock` binary plus `cargo-padlock`. Wires together the other crates via subcommands: `analyze`, `fix`, `list`, `diff`, `report`, `watch`, `explain`, `summary`, `check`, `bpf`, `init`. Uses `clap` (derive API) for CLI parsing, `owo-colors` for terminal color, `comfy-table` for table output. The `fix` subcommand rewrites struct fields in-place (with `.bak` backup). The `diff` subcommand shows a before/after unified diff. The `watch` subcommand uses `notify` to re-run analysis on every file change. The `explain` subcommand prints verbose per-struct explanations. The `check` subcommand exits non-zero when findings exceed a configured threshold (CI gate). The `bpf` subcommand analyzes eBPF object files and raw BTF files (including `/sys/kernel/btf/vmlinux`). The `init` subcommand scaffolds a `padlock.toml` config file.
  - `src/bin/cargo_padlock.rs` — the `cargo-padlock` binary; installed as a cargo subcommand (`cargo padlock`). Reads `Cargo.toml`, runs `cargo build`, locates the debug/release binary, and runs DWARF analysis. Supports `--json`, `--sarif`, `--release`, `--bin`, `--package`.

- **`padlock-macros`** — Proc-macro crate (`proc-macro = true`). Provides:
  - `#[padlock::assert_no_padding]` — compile-time assertion that `size_of::<Struct>() == sum(size_of::<FieldType>())`. Fails at compile time when padding is present.
  - `#[padlock::assert_size(N)]` — compile-time assertion that `size_of::<Struct>() == N`.

## Data flow

Source/binary input → (`padlock-dwarf` or `padlock-source`) → `padlock-core` IR → analysis passes → findings → (`padlock-output`) → terminal/JSON/SARIF

## Key implementation details

- **Supported architectures** (`padlock-core/src/arch.rs`): `X86_64_SYSV`, `AARCH64`, `AARCH64_APPLE` (128-byte cache line), `WASM32`, `RISCV64`, `CORTEX_M` (no cache, 4-byte ptrs), `CORTEX_M4` (32-byte lines, 4-byte ptrs), `AVR` (no cache, 2-byte ptrs). `arch_by_name()` resolves short names and Rust target triples via `arch_by_triple()`. `cache_line_size = 0` suppresses FalseSharing and LocalityIssue analysis. The default throughout the codebase is `X86_64_SYSV`.
- **Test fixtures**: `padlock-core` has a `test-helpers` feature that exposes `ir::test_fixtures` (e.g. `connection_layout()`, `packed_layout()`) for use in other crates: `cargo test -p padlock-core --features test-helpers`.
- **`Report::from_layouts`** in `findings.rs` is the single entry point that runs all analysis passes and produces `StructReport` results with scored findings.
- **Severity thresholds**: PaddingWaste ≥30% waste → High, ≥10% → Medium, <10% → Low. ReorderSuggestion savings ≥8 bytes → High, else Medium. FalseSharing is always High.
- **Rust enums**: unit-only enums emit a `__discriminant` field; data enums additionally emit a `__payload` field (sized to the largest variant) before the discriminant. Generic and empty enums are skipped.
- **Zig unions**: `union_declaration` nodes produce layouts with all fields at offset 0 (`is_union = true`). Tagged unions (`union(enum)`) get a synthetic `__tag` field after the payload. Empty `union {}` bodies produce a phantom tree-sitter node with an empty identifier — filtered by `parse_container_field` returning `None` for empty names.
- **Tree-sitter AST discovery**: when adding support for a new node kind, the pattern is to write a temporary test that prints the AST (`node.to_sexp()`) for a representative source snippet, then remove the test after the node kinds are confirmed.
- **`StructLayout::uncertain_fields`** (`ir.rs`): a `Vec<String>` of field names whose type size could not be accurately determined from source alone. Populated by: the **Go frontend** for qualified cross-package types (e.g. `driver.Connector`); the **Zig frontend** for comptime-only field types (`type`, `anytype`, `comptime_int`, `comptime_float`); the **`resolve_nested_structs` post-pass** in `lib.rs` for `Opaque` fields whose type name was not found among the parsed structs in the same file; the **DWARF extractor** for bitfield groups where `DW_AT_byte_size` is absent. Surfaced in `StructReport::uncertain_fields`, the human-readable summary (as a per-struct note), and JSON output. Serialized as a JSON array, omitted when empty.
- **`SkippedStruct` and `Report::skipped`** (`findings.rs`): a `Vec<SkippedStruct>` of types that were encountered but not analyzed (e.g. generics/templates). Populated via a thread-local side channel (`crate::record_skipped` in `padlock-source`) alongside the existing `eprintln!` notes. Collected by `parse_source` (returns `ParseOutput { layouts, skipped }`). Threaded through `collect_layouts` and stored in `report.skipped` by the CLI commands. Included in JSON output and SARIF `notifications`. The human-readable summary appends a "N types skipped" section when non-empty. C++, Rust, and Go generic/template types are **fully skipped** (no struct emitted); Zig comptime-generic functions (`fn Foo(comptime T: type) type`) are also detected and recorded. `SkippedStruct` derives `Deserialize` so it can be round-tripped through the on-disk parse cache (`.padlock-cache/layouts.json`), which now persists `skipped` per file entry.
- **BTF support** (`padlock-cli/src/paths.rs`): raw `.btf` files (BTF magic `0xEB9F`, not an ELF container) are detected by `is_raw_btf()` and routed directly to `padlock_dwarf::btf::extract_from_btf`. This enables `padlock analyze /sys/kernel/btf/vmlinux` and `padlock analyze foo.btf` without needing an ELF wrapper. ELF binaries with a `.BTF` section continue to work as before.
- **Generated-file skipping** (`padlock-cli/src/paths.rs`): `should_skip_source_file(path)` checks extensions (`.pb.h`, `.pb.cc`, `.pb.c`, `.pb.cpp`) and first 512 bytes for standard markers (`// Code generated` Go official, `// @generated` / `//! @generated` Rust/prost/relay, `// Generated by` / `/* Generated by` C/C++). Also skips any file over 500 KB unconditionally — files this large contain data tables or generated rule-rewrite code, never hand-tunable struct definitions. Called by `collect_layouts` when `include_generated` is `false` (the default). Controlled via `--include-generated` in `FilterArgs`.
- **Parallel directory walk** (`padlock-cli/src/paths.rs`): `walk_source_files` uses `collect_files_parallel` — a rayon `into_par_iter().flat_map()` DFS that fans out across subdirectories concurrently. Uses `DirEntry::file_type()` to avoid extra `stat()` per entry. Final `sort_unstable()` restores deterministic order.
- **Thread-local tree-sitter parsers**: Go (`go.rs`), Zig (`zig.rs`), C, and C++ (`c_cpp.rs`) frontends hold one `Parser` per rayon worker thread via `thread_local! { static PARSER: RefCell<Parser> }`. Each parse call uses `PARSER.with(|p| p.borrow_mut().parse(source, None))`. `old_tree = None` means every parse is a full fresh parse, so no reset is needed between files.
- **Parallel analysis passes** (`padlock-core/src/findings.rs`): `Report::from_layouts` uses `layouts.par_iter().map(analyze_one).collect()` — each struct's padding/reorder/locality/false-sharing analysis is independent and scales with available cores.
- **Parse cache anchored to analyzed root** (`padlock-cli/src/paths.rs`): `collect_layouts` derives `cache_root` from the first input path (using that path if it's a directory, or its parent if it's a file). `ParseCache::load(&cache_root)` ensures a consistent `.padlock-cache/` location regardless of the working directory.
- **Parse cache eviction and streaming write** (`padlock-cli/src/cache.rs`): `flush(&mut self)` prunes entries for files that no longer exist via `retain`, then serializes with `serde_json::to_writer(BufWriter::new(File::create(...)))` — streaming to disk without building the full JSON string in RAM.
