# padlock Architecture

## Overview

padlock is a Cargo workspace of six crates. The data flows in one direction:

```
  Source / Binary input
         │
         ▼
  ┌──────────────┐   ┌──────────────┐
  │padlock-source│   │padlock-dwarf │
  │ (C/C++/Rust/ │   │ (DWARF/PDB/  │
  │  Go/Zig src) │   │  BTF bins)   │
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
         │ Markdown (GFM) │
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

- **`arch.rs`** — `ArchConfig` constants for each supported target (pointer size, cache line size). Statics: `X86_64_SYSV`, `AARCH64`, `AARCH64_APPLE`, `WASM32`, `RISCV64`, `CORTEX_M` (no-cache, 4-byte ptrs), `CORTEX_M4` (32-byte lines, 4-byte ptrs), `AVR` (no-cache, 2-byte ptrs). `arch_by_name()` resolves short names and falls through to `arch_by_triple()` for Rust target triples. `with_overrides(base, cache_line_size, word_size)` creates a heap-leaked `&'static ArchConfig` with user-supplied overrides, used by `--cache-line-size` / `--word-size` CLI flags. When `cache_line_size = 0`, false-sharing and locality analysis is suppressed.

- **`findings.rs`** — `Finding` enum, `StructReport` (includes `num_fields`, `num_holes`, source location, `uncertain_fields`), `Report` (includes `analyzed_paths`, `skipped`), `SkippedStruct { name, reason, source_file }`. `Report::from_layouts` is the single entry point that runs all passes and returns the full report. `Report::skipped` carries types that were encountered but not analyzed (generics/templates/comptime-generic functions) — emitted in JSON output and as SARIF `notifications`.

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

- **`lib.rs`** — public API: `parse_source(path, arch) -> anyhow::Result<ParseOutput>`, `detect_language(path)`, `SourceLanguage` enum. `ParseOutput { layouts: Vec<StructLayout>, skipped: Vec<SkippedStruct> }` is the return type; `skipped` carries types that were encountered but not analyzed. `parse_source` sets `layout.source_file` and `layout.source_line` on every returned layout and stamps `source_file` on each `SkippedStruct`. Frontends call `crate::record_skipped(name, reason)` to report generics/templates via a thread-local side channel; `parse_source` drains it into `ParseOutput::skipped` after parsing. `resolve_nested_structs` (post-pass) flags fields of `TypeInfo::Opaque` whose type name does not resolve to any struct in the same file — they are added to `StructLayout::uncertain_fields` so the output layer can warn the user.

- **`frontends/`**:
  - `c_cpp.rs` — tree-sitter-c / tree-sitter-cpp parser. Walks the AST, extracts `struct_specifier` and `type_definition` nodes, simulates layout. Handles C primitive types, C++ qualified types (`std::mutex`), stdlib types (`std::string`, `std::vector`, etc.), unions. **Bitfield grouping**: consecutive bitfield members are grouped into storage-unit-sized synthetic `[a:3|b:5]` fields following GCC/Clang ABI rules (see `resolve_bitfield_groups` in `c_cpp.rs`); MSVC mixed-type packing is not modelled. **C++ template structs/classes/unions are skipped** — without monomorphisation the type parameter size is unknowable; non-template types in the same TU are unaffected; skipped templates are recorded in `ParseOutput::skipped`. **Typedef alias resolution**: a phase-0 pre-scan builds a `HashMap<String, String>` of scalar aliases (`typedef uint32_t UserId` → `{"UserId": "uint32_t"}`); struct/union/function-pointer typedefs are excluded from the map. Detects `__attribute__((packed))`: sets `is_packed = true` and simulates packed layout. **`alignas(N)` support**: field-level and struct-level `alignas` are both handled. Extracts `GUARDED_BY(mu)` / `__attribute__((guarded_by(mu)))` / `PT_GUARDED_BY(mu)` from field source text. Sets `source_line` from the struct node's `start_position().row + 1`.
  - `rust.rs` — syn + `proc-macro2` (with `span-locations` feature) parser. Detects `#[repr(C)]` and `#[repr(packed)]`; uses `syn::visit` to walk item structs and item enums. **Generic struct/enum definitions are skipped** — layout is unknowable without concrete type arguments. `primitive_size_align` covers all Rust primitives plus common stdlib types: `Vec`/`String` (3×pointer), `Box`/`Arc`/`Rc` (1×pointer), all `AtomicXxx` (exact sizes), `PhantomData` (0), `Duration` (16B), and more — generic type arguments are stripped before lookup so `Vec<T>` correctly resolves as `Vec`. **Transparent newtypes**: `Cell<T>`, `MaybeUninit<T>`, `UnsafeCell<T>`, `Wrapping<T>`, `Saturating<T>`, `ManuallyDrop<T>` recurse into the inner type argument — `Cell<u8>` is 1 byte, not pointer-size. **Enum support**: unit-only enums emit a single `__discriminant` field sized from the variant count (1 byte ≤ 256, 2 bytes ≤ 65 536, 4 bytes otherwise). Data enums additionally emit a `__payload` field (sized to the largest variant's simulated payload) placed before the discriminant, matching Rust's conservative union-then-discriminant layout. **Exact niched layouts** (e.g. `Option<NonZeroU8>` collapsing to 1 byte) are not modeled; use binary analysis. Reads explicit guard attributes per field: `#[lock_protected_by = "mu"]`, `#[protected_by = "mu"]`, `#[guarded_by("mu")]`, `#[guarded_by(mu)]`, `#[pt_guarded_by(...)]` — sets `AccessPattern::Concurrent { guard }` before the heuristic pass runs. Sets `source_line` from `ident.span().start().line`.
  - `go.rs` — tree-sitter-go parser. Maps Go built-in types; handles `sync.Mutex`, `sync.RWMutex`, and imported type names. **`interface{}`, `any`, and locally-declared named interfaces** are treated as two-word fat pointers (16 bytes on 64-bit). A phase-1 pre-scan (`collect_go_interface_names`) collects all `type X interface { ... }` declarations in the file; fields of those types are sized as fat pointers. **Qualified cross-package interface types** (`io.Reader`, `driver.Connector`, etc.) cannot be resolved from source alone — they are sized as 2 words and added to `StructLayout::uncertain_fields`. **Embedded struct support**: anonymous (embedded) fields are detected in `field_declaration` nodes that have a type identifier but no field identifier. Reads trailing line comments for guard annotation: `// padlock:guard=mu`, `// guarded_by: mu`, `// +checklocksprotects:mu` (gVisor-style). Sets `source_line` from the `type_declaration` node's `start_position().row + 1`.
  - `zig.rs` — tree-sitter-zig parser. Walks `variable_declaration` → `struct_declaration` / `union_declaration` → `container_field` nodes. Handles regular, `packed`, and `extern` struct modifiers. Resolves built-in types (`u8`–`u128`, `i8`–`i128`, `f16`–`f128`, `usize`, `isize`, `bool`, `void`), pointer types (`*T`), optional types (`?T`), slices (`[]T` as two words), arrays (`[N]T` as N × element size), and error unions (`E!T` as two words). **Union support**: `union_declaration` nodes produce layouts with all fields at offset 0 (`is_union = true`) and total size equal to the largest field. **Tagged unions** (those with an `"enum"` keyword as a direct child in the tree-sitter AST) receive a synthetic `__tag` field appended after the payload, sized to fit the discriminant (1 byte per ≤256 variants). Empty `union {}` declarations (which produce a phantom `container_field` with an empty identifier in the grammar) are filtered out. Concurrency heuristics: `std.Thread.Mutex`, `std.Thread.RwLock`, `std.atomic.Value`, `Atomic`. Field type source text is preserved in `TypeInfo::Primitive { name }` for fix generation (`const Name = struct { ... };` rewrite).

- **`concurrency.rs`** — Heuristic pass: annotates `Field.access` to `Concurrent` for known synchronisation types (`Mutex<T>`, `std::mutex`, `sync.Mutex`, `AtomicU64`, etc.). Runs after the frontend; skips fields already set to non-`Unknown` (i.e. those with explicit guard annotations). Uses field name as guard proxy so type-name-detected fields with different names get different guard identifiers for false-sharing detection.

- **`fixgen.rs`** — Generates and applies reorder patches. Two layers:
  - **Source-aware generation** (`generate_*_fix_from_source`): extracts verbatim field "chunks" from the original struct source (attributes, doc-comments, visibility modifiers, annotations, trailing commas) using language-specific splitters (`extract_rust_field_chunks`, `extract_c_field_chunks`, `extract_go_field_chunks`, `extract_zig_field_chunks`). Reorders chunks by the optimal IR field order, preserving all decoration. Used by `apply_fixes_rust`, `apply_fixes_c`, `apply_fixes_go`, `apply_fixes_zig` via `apply_fixes_with_source`.
  - **IR-based fallback** (`generate_*_fix`): synthesises field declarations from IR type names. Activated when chunk extraction fails or a field name in the optimal order cannot be matched to a source chunk.
  - Span finders (`find_rust_struct_span`, `find_c_struct_span`, `find_go_struct_span`, `find_zig_struct_span`) locate struct text ranges in source for in-place rewriting.
  - `unified_diff` produces a unified diff between original and fixed text for the `diff` subcommand.

---

### `padlock-dwarf`

Binary analysis backend.

- **`reader.rs`** — Reads DWARF debug information from ELF/Mach-O binaries (via `gimli` + `object`). Extracts `DW_TAG_structure_type` entries and their `DW_TAG_member` children into `padlock-core` IR. Delegates to `extractor.rs` for the actual walk.
- **`extractor.rs`** — `Extractor::extract_all` drives the DWARF walk. `extract_field` maps each `DW_TAG_member` to a `Field`. **Bitfield members** (`DW_AT_bit_size` present) are grouped into synthetic storage-unit fields: consecutive members sharing the same `DW_AT_data_member_location` (byte offset) are merged into a single `[a:3|b:5]` field sized from `DW_AT_byte_size` on the member. Groups where `DW_AT_byte_size` is absent are added to `StructLayout::uncertain_fields`. `detect_arch` maps the ELF/Mach-O machine code to an `ArchConfig` (Apple Mach-O AArch64 maps to `AARCH64_APPLE` with 128-byte cache lines).

- **`btf.rs`** — Pure-Rust parser for the BPF Type Format (BTF) embedded in Linux eBPF object files as a `.BTF` ELF section. `extract_from_btf(btf_data, arch)` parses the 24-byte `btf_header`, the type section, and the string section. Handles all stable BTF kinds: `INT`, `PTR`, `ARRAY`, `STRUCT`, `UNION`, `ENUM`, `TYPEDEF`, `VOLATILE`, `CONST`, `RESTRICT`, `FLOAT`, `ENUM64`. Gracefully skips modern kinds (`FUNC`, `FUNC_PROTO`, `VAR`, `DATASEC`, `DECL_TAG`, `TYPE_TAG`, `FWD`) without aborting the parse — subsequent types in the section are still processed. Bitfield members are represented as synthetic storage-unit fields (e.g. `flags__bits: u32`) at the storage unit's byte offset; consecutive bitfields sharing a storage unit produce a single synthetic field (deduplication by `covered_until` tracking). Packed struct detection via size comparison: if `total_size < natural_aligned_size`, `is_packed` is set. When a `.BTF` section is detected in `padlock-cli/src/paths.rs`, this parser is used instead of the DWARF extractor.

- **`pdb_reader.rs`** — Reads PDB files (Windows MSVC debug databases) via the `pdb` crate. Two passes over the TPI stream: (1) build a `TypeFinder` and collect non-forward-reference `Class`, `Union`, and `Enumeration` records; (2) build a `(size, align)` cache for fast field type resolution. `collect_fields` resolves each `FieldList`, grouping `TypeData::Bitfield` members the same way the DWARF extractor does. `Enumeration` types are emitted as a single `__discriminant` field. Array sizes are read from `dimensions.last()` (cumulative byte lengths, not element counts). Unknown bitfield storage units are surfaced in `uncertain_fields`. Source file/line is not available from PDB type records (they are stored in symbol records per function/variable, not per type definition).

- **`detect_arch_from_host()`** — Returns the `ArchConfig` for the current build target; used as the default arch for source analysis.

---

### `padlock-output`

Output formatters. All functions take `padlock-core` types as input.

- **`summary.rs`** — Human-readable terminal output. `render_report` prints the analysis header and struct findings. When multiple files were analyzed (`analyzed_paths.len() > 1`), structs are grouped under `── filename ──` separator headers, and each struct shows only `:line` (the filename is already in the header). For single-file runs the full `(file:line)` location is shown inline per struct. High-severity `ReorderSuggestion` findings include a compact scale hint: `(~8 MB/1M instances)`.
- **`project_summary.rs`** — `render_summary(input: &SummaryInput) -> String`. Single-screen project health overview: aggregate weighted score (weighted by struct `total_size`), letter grade (A ≥ 90, B ≥ 80, C ≥ 70, D ≥ 60, F < 60), severity bar chart (20-char █/░ bars for High / Medium / Low / Clean counts), worst-N files table (score, High-count, wasted bytes), worst-N structs table (score, source location), and a next-step hint. Exported as `padlock_output::render_project_summary`.
- **`explain.rs`** — `render_explain(&layout)` renders a visual field layout table with offset/size/align/**CL**/field columns and inline padding gap rows. The `CL` column shows the zero-indexed cache-line number for each field and padding row. Trailing padding is labelled `<padding> (trailing)`. When a reorder would save bytes, an impact block is appended below the summary line showing extra KB/MB at 1K and 1M instance scales and extra cache lines per sequential scan. If reordering reduces the number of cache lines per instance, an additional note is shown. **Cache-line markers**: when fields span more than one cache-line boundary (field offset / cache_line_size > previous), a `╞── cache line N (offset O) ════╡` separator row is inserted between field rows.
- **`json.rs`** — Serialises `Report` to JSON via `serde_json`.
- **`sarif.rs`** — Emits SARIF 2.1.0 (`sarifVersion`, `runs[0].results`) for GitHub/GitLab code-scanning integration.
- **`diff.rs`** — Renders a unified diff of current vs optimal field order using `similar::TextDiff`.
- **`markdown.rs`** — `to_markdown(report: &Report) -> String` emits a GitHub-Flavored Markdown report. Uses score emoji (✅ score=100, ⚠️ score≥60, ❌ otherwise), severity emoji (🔴 High, 🟡 Medium, 🔵 Low), and a GFM table per finding. Designed for use with `$GITHUB_STEP_SUMMARY` in GitHub Actions workflows.

---

### `padlock-cli`

Two binaries. Wires all other crates together.

- **`main.rs`** — `clap` derive API; subcommand dispatch for `padlock`. `--version` flag auto-populated from `Cargo.toml`. Subcommands: `analyze`, `summary`, `list`, `diff`, `fix`, `report`, `watch`, `explain`, `check`, `init`, `bpf`.
- **`config.rs`** — `Config`: reads `.padlock.toml` (searches from the first path upward, then `$HOME`). Parses `[padlock]` section keys: `ignore`, `filter`, `exclude`, `min_size`, `min_holes`, `sort_by`, `fail_on_severity`. `is_ignored(&name)` checks the ignore list. `for_path(p)` walks ancestor directories to find the nearest config file.
- **`filter.rs`** — `FilterArgs` (shared CLI flags: `--filter`, `--exclude`, `--min-holes`, `--min-size`, `--packable`, `--sort-by`) and `SortBy` enum. Applies pre-analysis layout filtering and post-analysis report sorting. `apply_config_defaults(&cfg)` fills any `None`/default `FilterArgs` fields from the loaded `Config` — CLI values always take precedence. Also contains `FailSeverity` enum (`High | Medium | Low`) with `matches(&self, sev: &Severity) -> bool` implementing ≥-semantics: `Low` matches any, `Medium` matches Medium and above, `High` matches only High.
- **`paths.rs`** — `collect_layouts` (loads layouts from multiple paths, expands directories) and `walk_source_files` (parallel recursive directory walker, skips `target/`, `.git/`, etc.). `walk_source_files` uses `collect_files_parallel` — a rayon `into_par_iter().flat_map()` DFS that fans out across subdirectories concurrently, followed by `sort_unstable()` for deterministic order. Cache-miss files are then parsed in parallel via rayon; unchanged files are served from the on-disk parse cache. `cache_root` is derived from the first analyzed path so the cache location is consistent across working directories. `should_skip_source_file` detects machine-generated files (magic markers + extensions) and files over 500 KB.
- **`cache.rs`** — `ParseCache`: on-disk mtime-keyed layout cache stored at `<analyzed-root>/.padlock-cache/layouts.json`. Keyed by absolute path + mtime-secs; `get(path)` returns `Some((layouts, skipped))` on hit or `None` on miss/staleness; `insert(path, layouts, skipped)` updates the entry; `flush(&mut self)` prunes entries for deleted files via `retain`, then writes with `serde_json::to_writer(BufWriter::new(...))` — streaming to disk without building the full JSON in RAM. Requires `StructLayout` to implement `Deserialize` (added in `padlock-core/src/ir.rs` with an `arch_serde` helper that stores/restores arch by name). `SkippedStruct` implements `Deserialize` as well so it survives cache round-trips.
- **`commands/analyze.rs`** — Collects layouts from all paths, applies config + CLI filters, runs `Report::from_layouts`, dispatches to the right formatter. Supports `--markdown` (calls `padlock_output::to_markdown`), `--cache-line-size`, `--word-size` (calls `padlock_core::arch::with_overrides`), and `--fail-on-severity` (exits non-zero when any finding meets or exceeds the threshold).
- **`commands/summary.rs`** — Collects layouts, applies config + CLI filters, calls `padlock_output::render_project_summary` with a `SummaryInput { report, top }` and prints the result. Accepts `--top N`, `--cache-line-size`, `--word-size`, and the shared filter flags.
- **`commands/list.rs`** — Prints a summary table of all structs (size, fields, holes, wasted bytes, score, location). Accepts filter and sort flags.
- **`commands/diff.rs`** — Accepts multiple paths/dirs, applies `--filter`, calls `padlock_output::render_diff` per layout.
- **`commands/fix.rs`** — Accepts multiple paths/dirs, applies `--filter`, shows reorder diff and (non-dry-run) writes `.bak` backup then rewrites in-place.
- **`commands/report.rs`** — Alias for analyze.
- **`commands/watch.rs`** — File/directory watcher using `notify`. Debounces change events (250 ms) and re-runs analysis on each change. Clears the terminal between runs.
- **`commands/explain.rs`** — Prints a visual field-by-field memory layout table (offset, size, align, CL, padding gaps inline) for each struct. The `CL` column shows the zero-indexed cache-line number per row. Accepts `--filter`.
- **`commands/init.rs`** — Generates a `.padlock.toml` configuration file in the current directory with all supported options commented out and annotated. `--force` overwrites an existing file.
- **`commands/check.rs`** — Baseline/ratchet mode. `--save-baseline FILE` snapshots current findings as JSON. `--baseline FILE` compares current findings against the snapshot and fails only on regressions (new structs with High findings, score drops, severity increases). Every run prints a drift summary: `N new / M resolved / K unchanged` (resolved = improved + disappeared from baseline). Supports `--json` output.
- **`commands/bpf.rs`** — Thin alias for `analyze`. Prints a one-line BTF orientation note (human output only) then delegates to `commands::analyze::run`. Accepts `--json`, `--sarif`, `--fail-on-severity`, and all filter flags. See `docs/ebpf-btf.md`.
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
