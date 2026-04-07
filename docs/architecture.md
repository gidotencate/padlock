# padlock Architecture

## Overview

padlock is a Cargo workspace of five crates. The data flows in one direction:

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
         (user-facing binary)
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

- **`findings.rs`** — `Finding` enum, `StructReport`, `Report`. `Report::from_layouts` is the single entry point that runs all passes and returns the full report.

- **`analysis/`** — One module per analysis pass:
  - `padding` — re-exports `ir::find_padding`
  - `reorder` — computes optimal field order and savings via `reorder_savings` / `optimal_order`
  - `false_sharing` — `find_sharing_conflicts` (any cache-line groups) and `has_false_sharing` (confirmed: different guards)
  - `locality` — `has_locality_issue` and `partition_hot_cold`
  - `scorer` — assigns a 0–100 score based on finding severity and padding percentage

---

### `padlock-source`

Source analysis backend. No compiler invoked — sizes are approximated from a built-in type table.

- **`lib.rs`** — public API: `parse_source(path, arch)`, `detect_language(path)`, `SourceLanguage` enum

- **`frontends/`**:
  - `c_cpp.rs` — tree-sitter-c / tree-sitter-cpp parser. Walks the AST, extracts `struct_specifier` and `type_definition` nodes, simulates layout with `simulate_layout`. Handles C primitive types, C++ qualified types (`std::mutex`), and template types (`std::atomic<T>`).
  - `rust.rs` — syn-based parser. Detects `#[repr(C)]` and `#[repr(packed)]`; uses `syn::visit` to walk item structs. Handles pointer/reference/array type inference.
  - `go.rs` — tree-sitter-go parser. Maps Go built-in types; handles `sync.Mutex`, `sync.RWMutex`, and imported type names.

- **`concurrency.rs`** — Heuristic pass: annotates `Field.access` to `Concurrent` for known synchronisation types (`Mutex<T>`, `std::mutex`, `sync.Mutex`, `AtomicU64`, etc.). Uses field name as guard proxy so fields with different names generate different guard identifiers for false-sharing detection.

- **`fixgen.rs`** — Generates reorder patches: produces a `Vec<(usize, Vec<String>)>` of (struct-start-line, optimal-field-names) for the `diff` subcommand.

---

### `padlock-dwarf`

Binary analysis backend.

- **`reader.rs`** — Reads DWARF debug information from ELF/Mach-O binaries (via `gimli` + `object`). Extracts `DW_TAG_structure_type` entries and their `DW_TAG_member` children into `padlock-core` IR.

- **`pdb.rs`** — Reads PDB files (Windows) via the `pdb` crate. Extracts struct and class type records.

- **`detect_arch_from_host()`** — Returns the `ArchConfig` for the current build target; used as the default arch for source analysis.

---

### `padlock-output`

Output formatters. All functions take `padlock-core` types as input.

- **`terminal.rs`** — Human-readable table output with ANSI colors (`owo-colors`), severity badges, and `comfy-table` struct summary.
- **`json.rs`** — Serialises `Report` to JSON via `serde_json`.
- **`sarif.rs`** — Emits SARIF 2.1.0 (`sarifVersion`, `runs[0].results`) for GitHub/GitLab code-scanning integration.
- **`diff.rs`** — Renders a unified diff of current vs optimal field order using `similar::TextDiff`.

---

### `padlock-cli`

The `padlock` binary. Wires all other crates together.

- **`main.rs`** — `clap` derive API; subcommand dispatch.
- **`commands/analyze.rs`** — Detects source vs binary, calls the right frontend, runs `Report::from_layouts`, dispatches to the right formatter.
- **`commands/list.rs`** — Prints a summary table of all structs (size, fields, score).
- **`commands/diff.rs`** — Calls `padlock_output::render_diff` per layout and prints the unified diff.
- **`commands/fix.rs`** — Shows the reorder diff and (non-dry-run) writes a `.bak` backup.
- **`commands/report.rs`** — Alias for analyze; intended for extended report formats.

---

## Key Design Decisions

### `&'static ArchConfig`

`StructLayout` holds a `&'static ArchConfig` rather than an owned copy. The arch constants are module-level statics (`X86_64_SYSV`, etc.), so all layouts for a given target share the same pointer-sized reference. This avoids copying arch config into every struct and makes cross-arch comparisons straightforward.

### `AccessPattern::Concurrent { guard }`

Each concurrently-accessed field carries an optional `guard` string identifying which lock protects it. Two fields with **different** guards on the same cache line are a confirmed false-sharing hazard. For source analysis, the guard is set to the field's own name (treating each `Mutex`/`sync.Mutex`/`std::mutex` field as independently guarded). For DWARF analysis, explicit annotations can override this.

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
5. Add a formatter arm in `padlock-output/src/terminal.rs` (and JSON/SARIF if needed).

---

## Adding a New Language Frontend

1. Add a tree-sitter grammar crate (or a dedicated parser) to `padlock-source/Cargo.toml`.
2. Create `crates/padlock-source/src/frontends/my_lang.rs` with a `parse_my_lang(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>>` function.
3. Add a `SourceLanguage::MyLang` variant to `lib.rs` and wire up `detect_language` and `parse_source`.
4. Extend `concurrency.rs` with `is_concurrent_type` entries for the language's sync primitives.
