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

## `#[padlock::assert_offsets(field = N, …)]`

Fails to compile if a listed field is not at the given byte offset (uses `core::mem::offset_of!`, Rust 1.77+). Pins FFI / on-disk layouts, or the result of `padlock fix`, against silent drift:

```rust
use padlock_macros::assert_offsets;

#[assert_offsets(len = 0, cap = 8, flags = 16)]
#[repr(C)]
struct Header {
    len:   u64,
    cap:   u64,
    flags: u8,
}
```

## `#[padlock::assert_align(N)]`

Fails to compile if `align_of` is not exactly `N` (e.g. to keep a hot counter on its own cache line):

```rust
use padlock_macros::assert_align;

#[assert_align(64)]
#[repr(C, align(64))]
struct Counter { hits: u64 }
```

All macros pass the struct definition through unchanged — they only append a hidden `const` assertion block. No runtime cost.

## Part of padlock

- [`padlock-cli`](https://crates.io/crates/padlock-cli) — CLI (`padlock` + `cargo-padlock` binaries)
- [`padlock-core`](https://crates.io/crates/padlock-core) — IR, analysis passes, findings
- [`padlock-source`](https://crates.io/crates/padlock-source) — Source analysis (C/C++/Rust/Go/Zig)
- [`padlock-dwarf`](https://crates.io/crates/padlock-dwarf) — Binary analysis (DWARF/PDB)
- [`padlock-output`](https://crates.io/crates/padlock-output) — Output formatters (terminal/JSON/SARIF/diff)
- [`padlock-macros`](https://crates.io/crates/padlock-macros) — Compile-time layout assertions *(this crate)*
