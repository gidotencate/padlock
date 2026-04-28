# Extending padlock

padlock is structured as a Cargo workspace with a clean separation between IR, analysis, frontends, and output. Each layer is independently extensible. This document describes the four most common extension points.

---

## 1. Adding a new analysis pass

Analysis passes live in `crates/padlock-core/src/analysis/`. Each pass takes a `&StructLayout` and returns findings or metadata.

**Step 1 — Create the pass module:**

```rust
// crates/padlock-core/src/analysis/my_pass.rs

use crate::ir::StructLayout;
use crate::findings::Finding;

/// Returns findings for structs that exceed a threshold you care about.
pub fn analyze(layout: &StructLayout) -> Vec<Finding> {
    // ... your logic ...
    vec![]
}
```

**Step 2 — Add a new `Finding` variant** (if the pass produces a new kind of finding):

```rust
// crates/padlock-core/src/findings.rs

pub enum Finding {
    PaddingWaste { .. },
    ReorderSuggestion { .. },
    FalseSharing { .. },
    LocalityIssue { .. },
    MyNewFinding {          // ← add here
        struct_name: String,
        severity: Severity,
        // ... fields your pass needs to report ...
    },
}
```

Implement `Finding::severity()` and `Finding::struct_name()` for the new variant — the compiler will tell you which match arms need updating.

**Step 3 — Wire into `analyze_one`:**

```rust
// crates/padlock-core/src/findings.rs — analyze_one()

let my_findings = my_pass::analyze(layout);
findings.extend(my_findings);
```

**Step 4 — Add output rendering** in `padlock-output`:
- `summary.rs`: add a match arm in `render_finding()`
- `markdown.rs`: add a match arm in `render_finding_md()`
- `sarif.rs`: add a rule in `rules()` and a match arm in `rule_id_for()` / `message_for()`

**Step 5 — Add tests** using the `connection_layout()` and `packed_layout()` fixtures from `padlock-core/src/ir.rs`:

```rust
#[test]
fn my_pass_detects_issue() {
    use padlock_core::ir::test_fixtures::connection_layout;
    let layout = connection_layout();
    let findings = my_pass::analyze(&layout);
    assert!(!findings.is_empty());
}
```

---

## 2. Adding a new language frontend

Source frontends live in `crates/padlock-source/src/frontends/`. Each frontend takes source text and returns `Vec<StructLayout>`.

All non-Rust frontends use tree-sitter. The pattern for discovering the right node kinds:

```rust
#[test]
fn print_ast_for_my_construct() {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_yourlang::language()).unwrap();
    let src = r#"type Foo struct { x int; y float64 }"#;
    let tree = parser.parse(src, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
    // Run this test, read the s-expression, find the node kinds you need.
    // Remove the test after you have confirmed the node kinds.
}
```

**Minimal frontend skeleton:**

```rust
// crates/padlock-source/src/frontends/mylang.rs

use padlock_core::ir::{StructLayout, Field, TypeInfo, AccessPattern};
use padlock_core::arch::X86_64_SYSV;

pub fn parse_source(source: &str, file_path: &str) -> Vec<StructLayout> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_mylang::language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();

    let mut layouts = Vec::new();
    // Walk root, find struct nodes, call parse_struct() for each.
    layouts
}

fn parse_struct(node: tree_sitter::Node<'_>, source: &[u8], file_path: &str)
    -> Option<StructLayout>
{
    // Extract name, fields, source_line from the node.
    // Return None for empty or unsupported structs.
    None
}
```

**Wire the frontend into the dispatcher:**

```rust
// crates/padlock-source/src/lib.rs

pub fn parse_file(path: &Path) -> anyhow::Result<Vec<StructLayout>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => frontends::rust::parse_file(path),
        Some("c" | "h" | "cpp" | "cc" | "cxx" | "hpp") => frontends::c_cpp::parse_file(path),
        Some("go") => frontends::go::parse_file(path),
        Some("zig") => frontends::zig::parse_file(path),
        Some("ml") => frontends::mylang::parse_file(path),  // ← add here
        _ => Ok(vec![]),
    }
}
```

**Wire into the VS Code extension** — add the file extension to `SUPPORTED_LANGS` in `editors/vscode/src/extension.ts` and to the `isSupportedFile()` regex.

---

## 3. Adding a new output format

Output formatters live in `crates/padlock-output/src/`. Each formatter takes a `&Report` and returns a `String` (or `anyhow::Result<String>` for formats that can fail, like SARIF JSON serialization).

```rust
// crates/padlock-output/src/myformat.rs

use padlock_core::findings::{Finding, Report, Severity};

pub fn to_myformat(report: &Report) -> String {
    let mut out = String::new();
    for sr in &report.structs {
        for finding in &sr.findings {
            // render each finding in your format
        }
    }
    out
}
```

Wire into `padlock-cli`:
1. Add `--myformat` flag to `AnalyzeOpts` and the `Analyze` subcommand in `main.rs`.
2. Call `padlock_output::myformat::to_myformat(&report)` in `commands/analyze.rs`.

---

## 4. Adding a new architecture

Architecture configs live in `crates/padlock-core/src/arch.rs`.

```rust
pub const MY_ARCH: ArchConfig = ArchConfig {
    name: "my_arch",
    pointer_size: 4,      // bytes
    cache_line_size: 32,  // bytes; 0 = no cache (suppresses FalseSharing)
    max_align: 8,         // maximum natural alignment for any type
    endianness: Endianness::Little,
};
```

Add a short name to `arch_by_name()` and target triple prefixes to `arch_by_triple()`:

```rust
"my_arch" => Some(&MY_ARCH),
// ...
} else if triple.starts_with("my_arch-") {
    Some(&MY_ARCH)
}
```

Add tests:

```rust
#[test]
fn my_arch_resolves() {
    assert_eq!(arch_by_name("my_arch"), Some(&MY_ARCH));
    assert_eq!(arch_by_name("my_arch-unknown-none"), Some(&MY_ARCH));
    assert_eq!(MY_ARCH.pointer_size, 4);
}
```

**`cache_line_size = 0` behaviour:** when set to zero, `find_sharing_conflicts()` and `has_false_sharing()` return empty results immediately, suppressing all false-sharing and locality findings. Padding waste and reorder findings are unaffected.

---

## Repository map

```
padlock/
├── crates/
│   ├── padlock-core/src/
│   │   ├── arch.rs          ← ArchConfig, arch_by_name/triple
│   │   ├── findings.rs      ← Finding enum, Report, analyze_one
│   │   ├── ir.rs            ← StructLayout, Field, TypeInfo
│   │   └── analysis/
│   │       ├── padding.rs   ← find_padding
│   │       ├── reorder.rs   ← optimal_order, ReorderSuggestion
│   │       ├── false_sharing.rs
│   │       ├── locality.rs
│   │       ├── scorer.rs
│   │       └── impact.rs    ← per-1K/1M instance estimates
│   ├── padlock-source/src/
│   │   ├── lib.rs           ← parse_source dispatcher, ParseOutput, record_skipped
│   │   ├── concurrency.rs   ← type-name heuristic for AccessPattern
│   │   ├── fixgen.rs        ← source-preserving field reorder
│   │   └── frontends/
│   │       ├── rust.rs      ← syn parser; generics/enums; guard annotations
│   │       ├── c_cpp.rs     ← tree-sitter-c/cpp; bitfields; GUARDED_BY
│   │       ├── go.rs        ← tree-sitter-go; interfaces; guard comments
│   │       └── zig.rs       ← tree-sitter-zig; packed/extern; comptime detection
│   ├── padlock-dwarf/src/
│   │   ├── reader.rs        ← ELF/Mach-O DWARF loader
│   │   ├── extractor.rs     ← DWARF walk; bitfield grouping; uncertain_fields
│   │   ├── btf.rs           ← BTF section/raw-file parser
│   │   └── pdb_reader.rs    ← Windows PDB type-info reader
│   ├── padlock-output/src/
│   │   ├── summary.rs           ← human terminal output
│   │   ├── project_summary.rs   ← aggregate score, letter grade, worst-N tables
│   │   ├── explain.rs           ← visual field layout table with CL markers
│   │   ├── diff.rs              ← unified diff of current vs optimal order
│   │   ├── markdown.rs          ← GitHub-Flavored Markdown report
│   │   ├── sarif.rs             ← SARIF 2.1.0 for code scanning
│   │   └── json.rs              ← (via serde on Report)
│   └── padlock-cli/src/
│       ├── main.rs          ← clap subcommands
│       ├── config.rs        ← .padlock.toml parsing
│       ├── filter.rs        ← FilterArgs, FailSeverity
│       ├── paths.rs         ← collect_layouts, walk_source_files, should_skip_source_file
│       └── commands/        ← one file per subcommand
└── crates/padlock-macros/   ← assert_no_padding, assert_size proc macros
```

---

## Testing conventions

- **Unit tests** live in the same file as the code under test (`#[cfg(test)] mod tests`).
- **Integration tests** that need `StructLayout` fixtures use `padlock-core`'s `test-helpers` feature: `padlock-core = { ..., features = ["test-helpers"] }` in `[dev-dependencies]`.
- **Frontend snapshot tests** parse a representative source snippet and assert on the field names, offsets, and types of the returned `StructLayout`.
- **Run a single test:** `cargo test -p padlock-core test_name`
- **Format before committing:** `cargo fmt && cargo fmt --check`
- **Lint before committing:** `cargo clippy --workspace -- -D warnings`
