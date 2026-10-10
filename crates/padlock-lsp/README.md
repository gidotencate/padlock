# padlock-lsp

Standalone Language Server Protocol server for [padlock](https://github.com/gidotencate/padlock) — struct-layout diagnostics, hover, inlay hints, document symbols, and reorder quick-fixes for any LSP-capable editor (Neovim, Helix, Zed, JetBrains, Sublime).

Analyzes the live buffer in-process on open/change/save — no `padlock` CLI install required, no subprocess per keystroke. Honours the same `.padlock.toml` as the CLI (`ignore`, `min_severity`, per-struct overrides, `arch.override`), and accepts editor-pushed overrides via `initializationOptions` or `workspace/didChangeConfiguration`.

## Install

```bash
cargo install padlock-lsp
```

Point your editor's LSP client at the installed `padlock-lsp` binary for `.c`, `.cpp`, `.rs`, `.go`, and `.zig` files. Setup examples are in the [main README](https://github.com/gidotencate/padlock#lsp).

## Part of padlock

- [`padlock-cli`](https://crates.io/crates/padlock-cli) — CLI (`padlock` + `cargo-padlock` binaries)
- [`padlock-lsp`](https://crates.io/crates/padlock-lsp) — Language server *(this crate)*
- [`padlock-core`](https://crates.io/crates/padlock-core) — IR, analysis passes, findings
- [`padlock-source`](https://crates.io/crates/padlock-source) — Source analysis (C/C++/Rust/Go/Zig)
- [`padlock-dwarf`](https://crates.io/crates/padlock-dwarf) — Binary analysis (DWARF/PDB)
- [`padlock-output`](https://crates.io/crates/padlock-output) — Output formatters (terminal/JSON/SARIF/diff)
- [`padlock-macros`](https://crates.io/crates/padlock-macros) — Compile-time layout assertions
