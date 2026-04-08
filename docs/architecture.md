# padlock Architecture

## Overview

padlock is a Cargo workspace of six crates. The data flows in one direction:

```
  Source / Binary input
         │
         ▼
  ┌──────────────┐   ┌──────────────┐
  │padlock-source│   │padlock-dwarf │
  │ (C/C++/Rust/ │   │ (DWARF/PDB   │
  │  Go source)  │   │  binaries)   │
  └──────┬───────┘   └──────┬───────┘
         └────────┬──────────┘
                  ▼
         padlock-core IR
         (StructLayout, Field)
                  │
                  ▼
          Analysis Passes
          (padding, reorder,
           false_sharing, locality,
           scorer)
                  │
                  ▼
         Report / Findings
                  │
                  ▼
         ┌────────────────┐
         │padlock-output  │
         │ terminal / JSON│
         │ SARIF / diff   │
         └────────────────┘
                  │
                  ▼
         padlock-cli
         (padlock binary + cargo-padlock subcommand)


  Compile-time path (separate):
  ┌────────────────┐
  │padlock-macros  │  proc-macro crate — no runtime dependency
  │ #[assert_no_padding]              │
  │ #[assert_size(N)]                 │
  └────────────────┘
```

---

## Crates

### `padlock-core`

Central dependency for all other crates. Contains:

- **`ir.rs`** — Intermediate representation. Key types:
  - `StructLayout` — a fully-laid-out struct: name, total size, alignment, `Vec<Field>`, arch
  - `Field` — one field: name, `TypeInfo`, offset, size, align, `AccessPattern`
  - `AccessPattern` — `Unknown | Concurrent { guard, is_atomic } | ReadMostly | Padding`
  - `find_padding(layout)` — returns all `PaddingGap` objects between fields
  - `optimal_order(layout)` — returns fields sorted by descending alignment

- **`arch.rs`** — `ArchConfig` constants for each supported target (pointer size, cache line size). Statics: `X86_64_SYSV`, `AARCH64`, `AARCH64_APPLE`, `WASM32`, `RISCV64`.

- **`findings.rs`** — `Finding` enum, `StructReport` (includes `num_fields`, `num_holes`, source location), `Report` (includes `analyzed_paths`). `Report::from_layouts` is the single entry point that runs all passes and returns the full report.

- **`analysis/`** — One module per analysis pass:
  - `padding` — re-exports `ir::find_padding`
  - `reorder` — computes optimal field order and savings via `reorder_savings` / `optimal_order`
  - `false_sharing` — `find_sharing_conflicts` (any cache-line groups) and `has_false_sharing` (confirmed: different guards)
  - `locality` — `has_locality_issue` and `partition_hot_cold`
  - `scorer` — assigns a 0–100 score based on finding severity and padding percentage
  - `impact` — `estimate_impact(savings, current_size, optimal_size, cache_line) -> ImpactEstimate`. Pure computation: calculates extra bytes and extra cache lines at 1K and 1M instance scales, plus whether the current layout spans more cache lines per instance than the optimal layout. Used by the output layer to render concrete at-scale hints.

---

### `padlock-source`

Source analysis backend. No compiler invoked — sizes are approximated from a built-in type table.

- **`lib.rs`** — public API: `parse_source(path, arch)`, `detect_language(path)`, `SourceLanguage` enum. `parse_source` sets `layout.source_file` (the file path string) and `layout.source_line` (from the AST) on every returned layout.

- **`frontends/`**:
  - `c_cpp.rs` — tree-sitter-c / tree-sitter-cpp parser. Walks the AST, extracts `struct_specifier` and `type_definition` nodes, simulates layout with `simulate_layout`. Handles C primitive types, C++ qualified types (`std::mutex`), template types (`std::atomic<T>`), unions. **Bit-field structs are skipped entirely** (`is_bitfield_type` detects `:N` suffixes; any struct with a bitfield member returns `None`) — bit-field packing cannot be accurately modelled without a compiler. Detects `__attribute__((packed))` on struct/class nodes: sets `is_packed = true` and simulates packed layout (no inter-field alignment padding, struct alignment forced to 1). Extracts `GUARDED_BY(mu)` / `__attribute__((guarded_by(mu)))` / `PT_GUARDED_BY(mu)` from field source text and sets `AccessPattern::Concurrent { guard }` directly. Sets `source_line` from the struct node's `start_position().row + 1`.
  - `rust.rs` — syn + `proc-macro2` (with `span-locations` feature) parser. Detects `#[repr(C)]` and `#[repr(packed)]`; uses `syn::visit` to walk item structs. **Generic struct definitions (`struct Foo<T>`) are skipped** — layout is unknowable without concrete type arguments. `primitive_size_align` covers all Rust primitives plus common stdlib types: `Vec`/`String` (3×pointer), `Box`/`Arc`/`Rc` (1×pointer), all `AtomicXxx` (exact sizes), `PhantomData` (0), `Duration` (16B), and more — generic type arguments are stripped before lookup so `Vec<T>` correctly resolves as `Vec`. Reads explicit guard attributes per field: `#[lock_protected_by = "mu"]`, `#[protected_by = "mu"]`, `#[guarded_by("mu")]`, `#[guarded_by(mu)]`, `#[pt_guarded_by(...)]` — sets `AccessPattern::Concurrent { guard }` before the heuristic pass runs. Sets `source_line` from `ident.span().start().line`.
  - `go.rs` — tree-sitter-go parser. Maps Go built-in types; handles `sync.Mutex`, `sync.RWMutex`, and imported type names. **`interface{}` and `any` are treated as two-word fat pointers** (16 bytes on 64-bit), matching the Go runtime representation (type pointer + data pointer). Reads trailing line comments for guard annotation: `// padlock:guard=mu`, `// guarded_by: mu`, `// +checklocksprotects:mu` (gVisor-style). Sets `source_line` from the `type_declaration` node's `start_position().row + 1`.

- **`concurrency.rs`** — Heuristic pass: annotates `Field.access` to `Concurrent` for known synchronisation types (`Mutex<T>`, `std::mutex`, `sync.Mutex`, `AtomicU64`, etc.). Runs after the frontend; skips fields already set to non-`Unknown` (i.e. those with explicit guard annotations). Uses field name as guard proxy so type-name-detected fields with different names get different guard identifiers for false-sharing detection.

- **`fixgen.rs`** — Generates reorder patches: produces a `Vec<(usize, Vec<String>)>` of (struct-start-line, optimal-field-names) for the `diff` subcommand.

---

### `padlock-dwarf`

Binary analysis backend.

- **`reader.rs`** — Reads DWARF debug information from ELF/Mach-O binaries (via `gimli` + `object`). Extracts `DW_TAG_structure_type` entries and their `DW_TAG_member` children into `padlock-core` IR. **Bit-field members** (those carrying `DW_AT_bit_size`) are silently skipped — they share byte offsets and cannot be represented in the byte-level IR without losing accuracy.
- **`extractor.rs`** — `Extractor::extract_all` drives the DWARF walk. `extract_field` maps each `DW_TAG_member` to a `Field`; fields with `DW_AT_bit_size` are dropped. `detect_arch` maps the ELF/Mach-O machine code to an `ArchConfig` (Apple Mach-O AArch64 maps to `AARCH64_APPLE` with 128-byte cache lines).

- **`pdb.rs`** — Reads PDB files (Windows) via the `pdb` crate. Extracts struct and class type records.

- **`detect_arch_from_host()`** — Returns the `ArchConfig` for the current build target; used as the default arch for source analysis.

---

### `padlock-output`

Output formatters. All functions take `padlock-core` types as input.

- **`summary.rs`** — Human-readable terminal output. `render_report` prints the analysis header and struct findings. When multiple files were analyzed (`analyzed_paths.len() > 1`), structs are grouped under `── filename ──` separator headers, and each struct shows only `:line` (the filename is already in the header). For single-file runs the full `(file:line)` location is shown inline per struct. High-severity `ReorderSuggestion` findings include a compact scale hint: `(~8 MB/1M instances)`.
- **`explain.rs`** — `render_explain(&layout)` renders a visual field layout table with offset/size/align columns and inline padding gap rows. Trailing padding is labelled `<padding> (trailing)`. When a reorder would save bytes, an impact block is appended below the summary line showing extra KB/MB at 1K and 1M instance scales and extra cache lines per sequential scan. If reordering reduces the number of cache lines per instance, an additional note is shown.
- **`json.rs`** — Serialises `Report` to JSON via `serde_json`.
- **`sarif.rs`** — Emits SARIF 2.1.0 (`sarifVersion`, `runs[0].results`) for GitHub/GitLab code-scanning integration.
- **`diff.rs`** — Renders a unified diff of current vs optimal field order using `similar::TextDiff`.

---

### `padlock-cli`

Two binaries. Wires all other crates together.

- **`main.rs`** — `clap` derive API; subcommand dispatch for `padlock`. `--version` flag auto-populated from `Cargo.toml`. Subcommands: `analyze`, `list`, `diff`, `fix`, `report`, `watch`, `explain`, `check`.
- **`filter.rs`** — `FilterArgs` (shared CLI flags: `--filter`, `--exclude`, `--min-holes`, `--min-size`, `--packable`, `--sort-by`) and `SortBy` enum. Applies pre-analysis layout filtering and post-analysis report sorting.
- **`paths.rs`** — `collect_layouts` (loads layouts from multiple paths, expands directories) and `walk_source_files` (recursive directory walker, skips `target/`, `.git/`, etc.).
- **`commands/analyze.rs`** — Collects layouts from all paths, applies config + CLI filters, runs `Report::from_layouts`, dispatches to the right formatter.
- **`commands/list.rs`** — Prints a summary table of all structs (size, fields, holes, wasted bytes, score, location). Accepts filter and sort flags.
- **`commands/diff.rs`** — Accepts multiple paths/dirs, applies `--filter`, calls `padlock_output::render_diff` per layout.
- **`commands/fix.rs`** — Accepts multiple paths/dirs, applies `--filter`, shows reorder diff and (non-dry-run) writes `.bak` backup then rewrites in-place.
- **`commands/report.rs`** — Alias for analyze.
- **`commands/watch.rs`** — File/directory watcher using `notify`. Debounces change events (250 ms) and re-runs analysis on each change. Clears the terminal between runs.
- **`commands/explain.rs`** — Prints a visual field-by-field memory layout table (offset, size, align, padding gaps inline) for each struct. Accepts `--filter`.
- **`commands/check.rs`** — Baseline/ratchet mode. `--save-baseline FILE` snapshots current findings as JSON. `--baseline FILE` compares current findings against the snapshot and fails only on regressions (new structs with High findings, score drops, severity increases). Supports `--json` output.
- **`bin/cargo_padlock.rs`** — The `cargo-padlock` binary, installed as a cargo subcommand. Reads `Cargo.toml` for the default binary name, runs `cargo build`, locates the built binary in `target/{profile}/`, and runs DWARF analysis. Exits non-zero on high-severity findings.

---

### `padlock-macros`

Proc-macro crate (`proc-macro = true`). No runtime dependency on any padlock crate.

- **`#[assert_no_padding]`** — Attribute macro applied to a struct. Emits a `const` block that asserts `size_of::<Struct>() == sum(size_of::<FieldType>())`. The assertion fails at compile time if any padding bytes are present.
- **`#[assert_size(N)]`** — Attribute macro that asserts `size_of::<Struct>() == N`. Fails at compile time if the struct grows (e.g. from a field addition) or shrinks unexpectedly.

Both macros pass through the struct definition unchanged — they only append a hidden `const` item.

---

## Key Design Decisions

### `&'static ArchConfig`

`StructLayout` holds a `&'static ArchConfig` rather than an owned copy. The arch constants are module-level statics (`X86_64_SYSV`, etc.), so all layouts for a given target share the same pointer-sized reference. This avoids copying arch config into every struct and makes cross-arch comparisons straightforward.

### `AccessPattern::Concurrent { guard }`

Each concurrently-accessed field carries an optional `guard` string identifying which lock protects it. Two fields with **different** guards on the same cache line are a confirmed false-sharing hazard.

Guard assignment has two layers, applied in order:

1. **Explicit annotation** (highest priority) — the source frontend reads language-specific guard attributes (`#[lock_protected_by = "mu"]`, `GUARDED_BY(mu)`, `// padlock:guard=mu`) and sets `Concurrent { guard: Some("mu") }` directly on the field.
2. **Heuristic type-name inference** — `concurrency.rs::annotate_concurrency` matches well-known synchronisation type names. It only runs on fields still `Unknown` after the frontend pass, so explicit annotations always win.

For fields where neither applies, `guard` is `None` and the field is not considered a false-sharing candidate.

### `Report::from_layouts` as the single analysis entry point

All five analysis passes (`padding`, `reorder`, `false_sharing`, `locality`, `scorer`) are invoked in a fixed order by `analyze_one` inside `findings.rs`. Neither the frontends nor the output layer run any analysis — they only produce/consume IR and findings respectively.

### `test-helpers` feature

`padlock-core` exposes its test fixture layouts (`connection_layout()`, `packed_layout()`) under the `test-helpers` Cargo feature. This allows `padlock-output`, `padlock-source`, and other crates to use them in `#[cfg(test)]` without duplicating fixture code. Declare `padlock-core = { path = "../padlock-core", features = ["test-helpers"] }` under `[dev-dependencies]` to use them.

---

## Adding a New Analysis Pass

1. Create `crates/padlock-core/src/analysis/my_pass.rs`.
2. Implement the detection function over `&StructLayout`.
3. Add a new `Finding` variant to `findings.rs`.
4. Call the new pass from `analyze_one` in `findings.rs`.
5. Add a formatter arm in `padlock-output/src/summary.rs` (and JSON/SARIF if needed).

---

## Adding a New Language Frontend

1. Add a tree-sitter grammar crate (or a dedicated parser) to `padlock-source/Cargo.toml`.
2. Create `crates/padlock-source/src/frontends/my_lang.rs` with a `parse_my_lang(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>>` function.
3. Add a `SourceLanguage::MyLang` variant to `lib.rs` and wire up `detect_language` and `parse_source`.
4. Extend `concurrency.rs` with `is_concurrent_type` entries for the language's sync primitives.
