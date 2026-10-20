# Fuzzing padlock-source

`cargo-fuzz` harnesses for every source frontend plus the BTF binary parser.
Goal: these must never panic on arbitrary input — a parse error (`Err`) is
an expected, acceptable outcome; a panic is not. The non-ASCII char-boundary
panic fixed in 0.19.0 (a fixed-size byte-offset lookback window in the C/C++
frontend) is exactly the bug class this exists to catch pre-emptively.

## Setup (one-time)

```bash
rustup toolchain install nightly   # cargo-fuzz needs nightly for sanitizer instrumentation
cargo install cargo-fuzz
```

## Running

```bash
cd crates/padlock-source
cargo +nightly fuzz run fuzz_parse_c          # or fuzz_parse_cpp, fuzz_parse_go, fuzz_parse_zig, fuzz_parse_rust, fuzz_btf
cargo +nightly fuzz run fuzz_parse_c -- -max_total_time=60   # time-boxed run
```

A crash writes the failing input to `fuzz/artifacts/<target>/` — reproduce with:

```bash
cargo +nightly fuzz run fuzz_parse_c fuzz/artifacts/fuzz_parse_c/<crash-file>
```

## Targets

| Target | Exercises |
|---|---|
| `fuzz_parse_c` | `padlock_source::frontends::c_cpp::parse_c` |
| `fuzz_parse_cpp` | `padlock_source::frontends::c_cpp::parse_cpp` (templates, inheritance, bitfields, `alignas`, `#pragma pack`) |
| `fuzz_parse_go` | `padlock_source::frontends::go::parse_go` |
| `fuzz_parse_zig` | `padlock_source::frontends::zig::parse_zig` |
| `fuzz_parse_rust` | `padlock_source::frontends::rust::parse_rust` (via `syn`) |
| `fuzz_btf` | `padlock_dwarf::btf::extract_from_btf` — raw bytes, no UTF-8 coercion (BTF is a binary format) |

The five source-frontend targets coerce arbitrary bytes to valid UTF-8 via
`String::from_utf8_lossy` before parsing, since every `parse_*` function
takes `&str` — this still produces plenty of multi-byte characters to
exercise the same code paths invalid/unusual real-world input would hit.

## Not run in CI

This is a manual/local tool for now, not wired into `ci.yml` — a fuzz run's
duration doesn't fit a normal CI budget. Run it periodically, or after
touching a frontend's byte/offset-handling logic specifically.
