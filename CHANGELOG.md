# Changelog

All notable changes to padlock are documented here.

## [0.7.1] — 2026-04-11

### Fixed
- `cargo fmt`: reformatted `fixgen.rs` (compact `match` arms expanded to block style), `zig.rs` (long iterator chains split across lines, blank line removed before `#[test]`), and `c_cpp.rs` to satisfy `rustfmt` in CI

## [0.7.0] — 2026-04-11

### Added
- **Rust enum analysis**: unit-only enums emit a single `__discriminant` field sized to the variant count (1, 2, or 4 bytes). Enums with one or more data variants additionally emit a `__payload` field (sized to the largest variant payload), matching Rust's conservative union-then-discriminant layout. Generic enums and empty enums are skipped.
- **C++ `alignas(N)`**: field-level and struct-level `alignas` specifiers are now extracted from source. Field-level `alignas` overrides the natural alignment when `N > natural_align`; struct-level `alignas` raises the struct's minimum alignment and adjusts trailing padding accordingly. Handles both `alignas_qualifier` (tree-sitter-cpp node) and the `type_qualifier → alignas_qualifier` wrapping pattern used for field declarations.
- **Go embedded structs**: anonymous (embedded) struct fields such as `sync.Mutex` or a bare `Base` type reference are now detected and emitted as IR fields. The simple (unqualified) type name is used as the field name. When the embedded type is defined in the same file, nested-struct resolution fills in the correct size.
- **Zig `union` and tagged `union`**: the Zig frontend now parses `union_declaration` nodes. All fields are placed at offset 0 (`is_union = true`). Tagged unions (those with an `enum` keyword child in the tree-sitter AST) receive a synthetic `__tag` field appended after the payload. Empty `union {}` declarations are filtered correctly.
- **Source-preserving fix generation**: `padlock fix` now extracts field declarations verbatim from the original source and reorders them as chunks, preserving `pub`, `pub(crate)`, `#[serde(...)]`, `/// doc-comments`, inline comments (including `GUARDED_BY(mu)`), and field tags. IR-based generation is retained as a fallback when chunking fails or a field cannot be matched.

## [0.6.2] — 2026-04-10

### Fixed
- `cargo clippy`: mixed-case hex literal (`0xeB9F` → `0xEB9F`), unused `RawBtfType` fields, two `collapsible_if` patterns in `zig.rs`
- Release workflow: added a `create-release` job so the GitHub release is created once before the matrix build jobs run, eliminating the `already_exists` race condition when multiple platform builds finish simultaneously

### Changed
- VS Code extension version aligned to `0.6.2` to match the Rust crates

## [0.6.1] — 2026-04-10

### Changed
- VS Code extension version aligned to `0.6.1` to match the Rust crates

## [0.6.0] — 2026-04-10

### Added
- **Zig source frontend**: struct layouts extracted from `.zig` files via `tree-sitter-zig`. Handles regular, `packed`, and `extern` structs; resolves built-in types (`u8`–`u128`, `i8`–`i128`, `f16`–`f128`, `usize`, `isize`, `bool`, `void`), pointers, optionals (`?T`), slices (`[]T`), arrays (`[N]T`), and error unions. Concurrency heuristics detect `std.Thread.Mutex`, `std.Thread.RwLock`, `std.atomic.Value`, and `Atomic` wrappers. Fix generation not yet supported for Zig.
- **BTF binary frontend**: pure-Rust parser for the BPF Type Format (`.BTF` ELF section) present in Linux eBPF object files. Handles all stable BTF kinds (`INT`, `PTR`, `ARRAY`, `STRUCT`, `UNION`, `ENUM`, `TYPEDEF`, `VOLATILE`, `CONST`, `RESTRICT`, `FLOAT`, `ENUM64`) and gracefully skips modern kinds (`FUNC`, `FUNC_PROTO`, `VAR`, `DATASEC`, `DECL_TAG`, `TYPE_TAG`, `FWD`) without aborting. Bitfield members are represented as synthetic storage-unit fields (`flags__bits: u32`) so their byte-level footprint is preserved for padding analysis — consecutive bitfields sharing the same storage unit produce a single synthetic field. Packed structs detected automatically from size vs. natural-aligned size. Automatically preferred over DWARF when a `.BTF` section is present (`padlock analyze my.bpf.o`).
- **`--cache-line-size <N>`**: override the assumed cache-line size (bytes) for the analysis, independent of the target architecture. Useful for analysing structs for non-standard hardware or comparing AMD (64-byte) vs Apple Silicon (128-byte) behaviour.
- **`--word-size <N>`**: override pointer/word size (bytes) for cross-architecture analysis (e.g. `--word-size 4` for 32-bit targets).
- **`--markdown` output format**: `padlock analyze --markdown` emits a GitHub-Flavored Markdown report with score emoji, severity badges (🔴/🟡/🔵), and per-struct GFM tables.
- **GitHub Action `output-format: markdown`**: when set, the markdown report is appended to `$GITHUB_STEP_SUMMARY` so findings appear on the workflow summary page without leaving the Actions UI.

### Changed
- `tree-sitter` upgraded from `0.22` to `0.23`; grammar crates updated to matching versions (`tree-sitter-c 0.23`, `tree-sitter-cpp 0.23`, `tree-sitter-go 0.23`). Language initialisation now uses the `LANGUAGE.into()` API instead of the deprecated `language()` function.

## [0.5.3] — 2026-04-09

### Fixed
- Clippy `collapsible_if` errors introduced by the Rust 2024 edition — 19 nested `if` blocks collapsed into combined `if`/`if-let` chains across `extractor.rs`, `fixgen.rs`, `c_cpp.rs`, `go.rs`, `rust.rs`, `diff.rs`, and `fix.rs`
- `rust-version` bumped from `1.85` to `1.88` (let chains require Rust 1.88+)

## [0.5.2] — 2026-04-09

### Fixed
- GitHub Action (`action.yml`): version resolution curl no longer uses `-f`, so a 404 from the Releases API (no releases yet) falls back to `cargo install` instead of killing the script
- `padlock-example.yml`: trigger changed from `push`/`pull_request` to `workflow_dispatch` — it is a copy-paste template for users, not a live CI job for the padlock repo itself; eliminates the race with `release.yml`

### Changed
- Edition upgraded from `2021` to `2024` (zero code changes required; `cargo fix` confirmed clean migration)
- `rust-version = "1.85"` added to workspace to document the MSRV
- `toml` dependency bumped from `0.8` to `1` (same API, better error messages)
- `object` dependency bumped from `0.36` to `0.39` (same API, better Mach-O/PE support)

## [0.5.1] — 2026-04-09

### Fixed
- `detect_arch_on_real_object` integration test: added `"aarch64-apple"` to the list of valid arch names — Apple Silicon CI runners expose this name and the test was panicking
- `padlock-example.yml`: fixed `path: src/` → `path: crates/` (the padlock repo has no `src/` directory; source lives in `crates/`)

## [0.5.0] — 2026-04-09

### Added
- **Impact quantification**: `explain` subcommand now shows KB/MB overhead at 1K and 1M instance scales when reordering would save bytes; shows cache-line span reduction when applicable. `analyze`/`report` summary appends a compact `(~N MB/1M instances)` hint on High-severity `ReorderSuggestion` findings
- **DWARF integration test suite** (`crates/padlock-dwarf/tests/extractor_tests.rs`): 8 tests that compile C snippets at test time and verify extraction correctness (field names/offsets, typedef names, pointer sizes, arch detection, forward-declared struct skipping, analysis pipeline smoke test, bitfield skipping)
- **Architecture detection unit tests** in `reader.rs`: 6 tests using synthetic minimal ELF and Mach-O headers
- **GitHub composite action** (`action.yml`): pre-built binary download from GitHub Releases, SARIF upload, severity-gated exit, per-severity output counts (`high-count`, `medium-count`, `low-count`, `findings-count`)

### Fixed
- **DWARF**: bit-field members (`DW_AT_bit_size`) are now silently skipped instead of producing wrong byte-level IR; remaining non-bitfield fields still extracted
- **C/C++**: `__attribute__((packed))` on struct/class nodes is now detected from source text; packed layout simulation inserts no inter-field alignment padding and sets `is_packed = true`
- **Go**: `interface{}` and `any` fields are now correctly sized as 2 words (16 bytes on 64-bit), matching the Go runtime fat-pointer representation; `interface_type` AST nodes added to the tree-sitter field collector

### Changed
- Rust frontend: generic struct definitions (`struct Foo<T>`) are skipped (layout is unknowable without concrete type arguments)
- Rust stdlib type table expanded to ~55 entries: `Vec`/`String` (3×pointer), `Box`/`Arc`/`Rc` (1×pointer), all `AtomicXxx` (exact sizes), `PhantomData` (0), `Duration` (16 bytes), channels, smart pointers

## [0.4.0] — 2026-03-15

### Added
- `explain` subcommand: visual ASCII table of field layout with offset/size/align columns and inline padding gap rows
- `check` subcommand: baseline/ratchet mode — save a baseline JSON and fail only when new findings appear (CI regression detection)
- Per-struct suppression via `# padlock: ignore` / `// padlock: ignore` comments
- Rust `repr` documentation: explains the accuracy trade-offs between `repr(Rust)`, `repr(C)`, `repr(packed)`, and `repr(transparent)`
- `release.yml` workflow: builds pre-compiled binaries for Linux x64/ARM64, macOS x64/ARM64, and Windows x64 on tag push
- Repository badges (crates.io version, CI status, license)

## [0.3.0] — 2026-02-20

### Added
- Struct output grouped under `── filename ──` separator headers when analyzing multiple files
- `source_line` populated from AST node positions across all frontends
- Per-struct line numbers shown inline in grouped output

## [0.2.0] — 2026-02-01

### Added
- Multi-path support: `padlock analyze src/ lib/ include/` accepts any mix of files and directories
- `--filter` / `--exclude` flags: regex-based struct name filtering
- `--sort` flag: sort output by `score`, `size`, `waste`, or `name`
- `--min-size` and `--min-holes` thresholds to suppress noise
- `--version` flag

## [0.1.0] — 2026-01-15

### Added
- Initial implementation: padding waste detection, reorder suggestions, false sharing detection, locality issue detection
- Source frontends: C/C++ (tree-sitter), Rust (syn), Go (tree-sitter)
- Binary frontend: DWARF via gimli, PDB via pdb crate
- Output formats: human (coloured), JSON, SARIF 2.1.0, diff
- `fix` subcommand: in-place field reordering with `.bak` backup
- `watch` subcommand: re-runs analysis on file change
- `cargo padlock` subcommand
- Proc macros: `#[padlock::assert_no_padding]`, `#[padlock::assert_size(N)]`
- Architecture support: x86_64, aarch64, aarch64-apple (128-byte cache line), wasm32, riscv64
- `.padlock.toml` configuration file support
- Guard annotation support: Rust (`#[lock_protected_by]`), C/C++ (`GUARDED_BY()`), Go (`// padlock:guard=`)
