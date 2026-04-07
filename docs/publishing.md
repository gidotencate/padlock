# Publishing padlock

## License Choice

padlock is dual-licensed under **MIT OR Apache-2.0** (user's choice). This is the standard for Rust tooling and matches the Rust compiler itself.

- **MIT** — permissive, short, widely understood.
- **Apache-2.0** — includes an explicit patent grant, which matters for enterprise users.

The dual license means users pick whichever suits their project. Both are corporate-friendly and allow unrestricted use, modification, and redistribution.

### If You Want Copyleft

Use **GPL v3** (not v2). Reasons to prefer v3 over v2:
- GPL v3 includes patent retaliation clauses (important for software tools)
- GPL v3 addresses anti-tivoization (hardware lockdown)
- GPL v2 is from 1991 and has known gaps; v2-only is incompatible with GPL v3

GPL v3 makes sense if you want to ensure that anyone who ships a modified padlock must also release their changes. It doesn't affect users who just run the tool.

---

## Publishing to crates.io

### Prerequisites

1. Create a crates.io account at [crates.io](https://crates.io)
2. Generate an API token: Settings → API Tokens
3. Log in:
   ```bash
   cargo login <your-token>
   ```

### Before First Publish

1. **Fill in `Cargo.toml` fields** in each crate (or inherit from workspace):
   ```toml
   # In workspace Cargo.toml:
   [workspace.package]
   version     = "0.1.0"
   edition     = "2021"
   license     = "MIT"
   repository  = "https://github.com/YOUR_USERNAME/padlock"
   description = "Struct memory layout analyzer for C, C++, Rust, and Go"
   keywords    = ["performance", "memory", "layout", "analysis", "cache"]
   categories  = ["development-tools", "development-tools::profiling"]
   ```

2. **Add `[package]` to each crate Cargo.toml** with `workspace = true` to inherit:
   ```toml
   [package]
   name = "padlock"   # the CLI crate
   version.workspace    = true
   edition.workspace    = true
   license.workspace    = true
   repository.workspace = true
   description.workspace = true
   ```

3. **Check what will be published** (dry run):
   ```bash
   cargo publish --dry-run -p padlock-core
   cargo publish --dry-run -p padlock
   ```

4. **Verify the README renders correctly** on crates.io. They use CommonMark; the current README is compatible.

### Publish Order

crates.io requires dependencies to be published before dependents. Publish in this order:

```bash
cargo publish -p padlock-core
cargo publish -p padlock-dwarf
cargo publish -p padlock-source
cargo publish -p padlock-output
cargo publish -p padlock-macros   # proc-macro crate; no runtime deps on other crates
cargo publish -p padlock           # facade crate re-exporting padlock-macros
cargo publish -p padlock-cli      # the binary, named "padlock" (as binary, not lib)
```

Wait ~30 seconds between each to let the registry index propagate. The `cargo-padlock` binary is part of `padlock-cli` and is published alongside it automatically.

> **Note**: The `padlock` facade crate and `padlock-cli` both claim the name "padlock" in different senses (one is a lib, one provides a binary). On crates.io, only one crate can occupy a given name. You have two options:
> - Publish only `padlock-cli` as "padlock" (users get the binary via `cargo install padlock`) and rename the facade to `padlock-lib` or `padlock-macros-prelude`
> - Publish the facade as "padlock" and rename the CLI crate to "padlock-cli" on crates.io (users install via `cargo install padlock-cli`)

### After Publishing

Users can install with:
```bash
cargo install padlock
```

---

## Publishing to GitHub

### Recommended Repository Structure

```
padlock/
├── .github/
│   └── workflows/
│       ├── ci.yml          # build + test on push/PR
│       └── release.yml     # build binaries on tag push
├── CLAUDE.md
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── README.md
├── .gitignore
├── crates/
└── docs/
```

### CI Workflow (`.github/workflows/ci.yml`)

```yaml
name: CI
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test
      - run: cargo clippy -- -D warnings
      - run: cargo fmt --check

  layout-check:
    runs-on: ubuntu-latest
    needs: test
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --release
      - run: |
          for f in padlock_samples/rust/*.rs padlock_samples/c/*.c; do
            ./target/release/padlock analyze "$f" --sarif >> padlock.sarif
          done
      - uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: padlock.sarif
```

### Release Workflow (pre-built binaries)

```yaml
name: Release
on:
  push:
    tags: ['v*']

jobs:
  build:
    strategy:
      matrix:
        include:
          - os: ubuntu-latest;  target: x86_64-unknown-linux-gnu
          - os: macos-latest;   target: aarch64-apple-darwin
          - os: windows-latest; target: x86_64-pc-windows-msvc
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: ${{ matrix.target }} }
      - run: cargo build --release --target ${{ matrix.target }}
      - uses: softprops/action-gh-release@v1
        with:
          files: target/${{ matrix.target }}/release/padlock*
```

---

## What Is Still Missing Before a Public Release?

Before you publish to crates.io or announce the tool, consider addressing:

| Item | Priority | Effort | Status |
|---|---|---|---|
| Inherit `[workspace.package]` in each crate's `Cargo.toml` | Required for publish | Low | Done |
| `padlock-macros` in workspace and publish order | Required for publish | Low | Done |
| `// padlock:ignore` suppression annotation | Medium | Low | Done |
| `cargo padlock` subcommand | High | Medium | Done |
| Explicit guard annotation (`#[lock_protected_by]`, `GUARDED_BY()`, etc.) | High | Medium | Done |
| Watch mode (`padlock watch`) | High | Medium | Done |
| `#[assert_no_padding]` proc macro | High | Medium | Done |
| GitHub Actions `action.yml` | High | Low | Done |
| `padlock` facade crate re-exporting `padlock-macros` | Medium | Low | Done |
| In-place `fix` for all source languages | Medium | Medium | Done |
| Nested struct size resolution | Medium | High | Done |
| C++ inheritance / vtable padding | Medium | High | Done |
| Configuration file (`.padlock.toml`) | Low | Medium | Done |
| Remove `libsource.a` from the repo (binary artifact) | Recommended | Low | Check before publish |
| Resolve crate name conflict (`padlock` facade vs `padlock-cli` binary) | Required for publish | Low | Pending decision |

The tool is feature-complete for its stated scope. Remaining pre-publish steps:
1. Check whether any binary artifact (`libsource.a`, `*.o`) is tracked in git and remove it
2. Decide on the crate name strategy (see note above about "padlock" name conflict)
3. Run `cargo publish --dry-run -p padlock-core` through all crates to catch metadata issues
