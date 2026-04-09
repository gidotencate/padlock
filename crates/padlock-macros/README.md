# padlock-macros

Compile-time struct layout assertions for [padlock](https://github.com/gidotencate/padlock) — the lint pass for struct memory layout.

This crate provides two proc-macro attributes that turn layout regressions into compile errors.

## `#[padlock::assert_no_padding]`

Fails to compile if the struct contains any padding bytes:

```rust
use padlock_macros::assert_no_padding;

#[assert_no_padding]
struct Connection {
    timeout: f64,   // 8 bytes, align 8
    port:    u16,   // 2 bytes
    active:  bool,  // 1 byte
    tls:     bool,  // 1 byte
    // no padding — passes
}
```

If you add a field that causes padding, the build fails with a clear message.

## `#[padlock::assert_size(N)]`

Fails to compile if the struct size is not exactly `N` bytes. Use this to prevent accidental growth or shrinkage:

```rust
use padlock_macros::assert_size;

#[assert_size(16)]
struct Connection {
    timeout: f64,
    port:    u16,
    active:  bool,
    tls:     bool,
}
```

Both macros pass the struct definition through unchanged — they only append a hidden `const` assertion block. No runtime cost.

## Part of padlock

- [`padlock-cli`](https://crates.io/crates/padlock-cli) — CLI (`padlock` + `cargo-padlock` binaries)
- [`padlock-core`](https://crates.io/crates/padlock-core) — IR, analysis passes, findings
- [`padlock-source`](https://crates.io/crates/padlock-source) — Source analysis (C/C++/Rust/Go)
- [`padlock-dwarf`](https://crates.io/crates/padlock-dwarf) — Binary analysis (DWARF/PDB)
- [`padlock-output`](https://crates.io/crates/padlock-output) — Output formatters (terminal/JSON/SARIF/diff)
- [`padlock-macros`](https://crates.io/crates/padlock-macros) — Compile-time layout assertions *(this crate)*
