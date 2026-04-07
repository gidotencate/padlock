# Publishing padlock

## License Choice

padlock uses the **MIT License** (see [LICENSE](../LICENSE)).

### Why MIT?

- **Maximum adoption**: MIT is the standard for Rust CLI tools and libraries. Anyone can use, fork, package, or embed padlock without restrictions.
- **Ecosystem fit**: The Rust ecosystem defaults to MIT or dual MIT/Apache-2.0. Deviating requires explicit justification.
- **Corporate-friendly**: Companies can use padlock in internal tooling without legal concerns.

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
cargo publish -p padlock-cli   # the binary, named "padlock"
```

Wait a minute between each to let the registry index update.

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

| Item | Priority | Effort |
|---|---|---|
| Inherit `[workspace.package]` in each crate's `Cargo.toml` | Required for publish | Low |
| Remove `libsource.a` from the repo (binary artifact, should not be committed) | Recommended | Low |
| In-place `fix` for bit-field and union structs | Low | Medium |
| Nested struct size resolution (field type = another struct in same file) | Medium | High |
| C++ inheritance / vtable padding | Medium | High |
| Configuration file (`.padlock.toml`) for per-project thresholds | Low | Medium |
| Ignore annotation (`// padlock:ignore`) | Medium | Low |

The tool is fully functional for its stated scope today. Start with publishing to GitHub (free), then crates.io once the package metadata is cleaned up.
