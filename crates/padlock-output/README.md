# padlock-output

Output formatters for [padlock](https://github.com/gidotencate/padlock) — a struct memory layout analyzer for C, C++, Rust, and Go.

This crate turns `padlock-core` `Report` and `StructLayout` types into human-readable or machine-readable output:

| Module | Function | Output |
|---|---|---|
| `summary` | `render_report(&report)` | Terminal output, grouped by file with `── filename ──` headers and `:line` locations |
| `json` | `to_json(&report)` | JSON serialization of the full report |
| `sarif` | `to_sarif(&report)` | SARIF 2.1.0 for GitHub/GitLab code-scanning integration |
| `diff` | `render_diff(&layout)` | Unified diff of current vs optimal field order |

## Usage

`padlock-output` is an internal library crate. To get formatted output, use the CLI:

```bash
padlock analyze src/             # terminal output, grouped by file
padlock analyze src/ --json      # JSON
padlock analyze src/ --sarif     # SARIF
padlock diff src/                # field reorder diff
```

## Part of padlock

- [`padlock-cli`](https://crates.io/crates/padlock-cli) — CLI (`padlock` + `cargo-padlock` binaries)
- [`padlock-core`](https://crates.io/crates/padlock-core) — IR, analysis passes, findings
- [`padlock-source`](https://crates.io/crates/padlock-source) — Source analysis (C/C++/Rust/Go)
- [`padlock-dwarf`](https://crates.io/crates/padlock-dwarf) — Binary analysis (DWARF/PDB)
- [`padlock-output`](https://crates.io/crates/padlock-output) — Output formatters (terminal/JSON/SARIF/diff) *(this crate)*
- [`padlock-macros`](https://crates.io/crates/padlock-macros) — Compile-time layout assertions
