# Changelog

All notable changes to padlock are documented here.

## [0.9.0] — 2026-04-12

### Added
- **`.padlock.toml` filter defaults**: the config file now supports per-project filter defaults under `[padlock]`. CLI flags always take precedence; config values fill in fields not specified on the command line. New options: `filter` (regex include), `exclude` (regex exclude), `min_size` (minimum struct size in bytes), `min_holes` (minimum padding holes), `sort_by` (`score`|`size`|`waste`|`name`), `fail_on_severity` (`high`|`medium`|`low`). The `padlock init` template documents all new options.

## [0.8.9] — 2026-04-12

### Added
- **Field-level source locations**: all source frontends (C/C++, Go, Zig, Rust) now populate `source_line` on each `Field` in the IR. The line number is 1-based, matching the field declaration in the source file. This enables future tooling (IDE integration, diagnostic output) to pinpoint individual fields. DWARF-parsed layouts are unaffected (line info already comes from debug info).

## [0.8.8] — 2026-04-12

### Added
- **Bit-field skip warning**: when the C/C++ source frontend skips a struct because it contains bit-field members, it now prints a diagnostic note to stderr explaining why the struct is absent from output and advising use of binary analysis for accurate results. Previously the skip was silent.

## [0.8.7] — 2026-04-12

### Added
- **`padlock init`**: new subcommand that generates a `.padlock.toml` template in the current directory with all options commented out and documented. Errors if a `.padlock.toml` already exists.

## [0.8.6] — 2026-04-12

### Added
- **Per-finding suppression**: place `// padlock: ignore[Kind1, Kind2]` on the line immediately before a struct/type declaration to suppress specific finding types while keeping the struct in the report. Supports all languages (C/C++/Rust/Go/Zig). Valid kinds: `PaddingWaste`, `ReorderSuggestion`, `FalseSharing`, `LocalityIssue`. Multiple kinds can be comma-separated in a single directive.
  - Distinct from `// padlock:ignore` (which removes the struct from analysis entirely).
  - `StructLayout` carries a `suppressed_findings: Vec<String>` field (serde `default` + `skip_serializing_if` for backward compatibility).
  - Documented in `docs/findings.md` with per-language examples.

## [0.8.5] — 2026-04-12

### Changed
- **False sharing heuristic tightening**: `has_false_sharing` no longer flags two purely-atomic fields sharing a cache line as false sharing. The type-name heuristic assigns each field's own name as its guard, so two `AtomicU64` fields on the same line would previously always trigger this check — but that pattern is cache-line bouncing (a locality issue), not classical lock-based false sharing. A conflict is now only reported when **at least one** of the two fields has `is_atomic: false` (i.e. a mutex/lock type, or explicitly annotated via `GUARDED_BY` / `#[lock_protected_by]`). Explicit annotations always set `is_atomic: false`, so annotated conflicts are always reported as before.
- Tests added to verify: two pure atomics → no false sharing; atomic + mutex → false sharing; mixed two atomics + one mutex → false sharing.

## [0.8.4] — 2026-04-12

### Added
- **C++ stdlib type sizes**: the C/C++ frontend now resolves sizes for common standard library types instead of falling back to pointer-size:
  - `std::string` / `std::basic_string<char>` → 32B (libstdc++ layout)
  - `std::string_view` / `std::basic_string_view<char>` → 2×pointer (ptr + length)
  - `std::vector<T>` (any `T`) → 3×pointer (ptr + size + capacity)
  - `std::deque<T>` → 80B; `std::list<T>` → 3×pointer
  - `std::map<K,V>` / `std::set<T>` / `std::multimap` / `std::multiset` → 48B; unordered variants → 56B
  - `std::unique_ptr<T>` → pointer; `std::shared_ptr<T>` / `std::weak_ptr<T>` → 2×pointer
  - `std::optional<T>` → recursively resolved: `ceil(sizeof(T)+1)` rounded up to `alignof(T)`
  - `std::function<Sig>` → 32B; `std::any` → 32B; `std::error_code` → 16B
  - `std::span<T>` → 2×pointer; `std::atomic_flag` → 4B
- **`repr(Rust)` caveat**: Rust structs and enums without `#[repr(C)]`, `#[repr(packed)]`, or `#[repr(transparent)]` now carry an `is_repr_rust` flag. Terminal `analyze` output appends a note that the compiler may have already reordered fields; the `explain` layout table shows `[repr(Rust) — compiler may reorder]` in the header. The `is_repr_rust` flag is also present in JSON/SARIF output (`#[serde(default)]` for backward compatibility).
- Tests for all new C++ stdlib types and for `repr(Rust)` detection on structs and enums.

## [0.8.3] — 2026-04-12

### Added
- **Extended type coverage** — source frontends now recognise significantly more types without falling back to pointer-size:
  - **C/C++**: Linux kernel integer types (`u8`/`u16`/`u32`/`u64`, `s8`–`s64`, `__u8`–`__u64`, `__s8`–`__s64`); endian-annotated kernel types (`__be16`, `__le32`, `__be64`, …); C99 fast/least family (`uint_fast32_t` is correctly pointer-sized on 64-bit); `intmax_t`/`uintmax_t`; GCC/Clang `__int128`/`__uint128`; Windows SDK types (`BYTE`, `WORD`, `DWORD`, `QWORD`, `BOOL`, `HANDLE`, `LPVOID`, `UINT8`–`UINT64`, `INT8`–`INT64`, pointer aliases); half-precision floats (`_Float16`, `__fp16`, `__bf16`, `_Float128`); character types (`wchar_t` 4B on POSIX, `char8_t`, `char16_t`, `char32_t`); multi-word spellings (`long int`, `long long int`).
  - **Zig**: C interop types (`c_char`, `c_short`, `c_int`, `c_uint`, `c_long`, `c_ulong`, `c_longlong`, `c_ulonglong`, `c_float`, `c_double`, `c_longdouble`); arbitrary-width integers (`u1`–`u65535`, `i1`–`i65535`) — size is `ceil(N/8)` bytes aligned to next power-of-two (capped at 8).
  - **Rust**: All `NonZeroXxx` types (`NonZeroU8`–`NonZeroU128`, `NonZeroUsize`, signed variants) — now correctly sized as their underlying integer; `f16` (2B) and `f128` (16B).
- Tests for all new type additions in `c_cpp.rs`, `zig.rs`, and `rust.rs`.
- README "What source analysis gets right" table expanded to reflect the new coverage.

## [0.8.2] — 2026-04-12

### Added (VS Code extension)
- **Status bar health score**: the status bar now shows a live layout score (`$(lock) 67 D`) and letter grade (A–F) for the active file, weighted by struct size. Yellow warning background and `$(warning) N` count appear when High-severity findings are present. Spinner shown while analysis runs.
- **Hover popup**: hovering over a struct definition line shows a markdown popup with score bar (`██████░░░░`), wasted bytes, and each finding with severity icon (🔴/🟡/🔵).
- **Quick-fix lightbulb (CodeAction)**: `ReorderSuggestion` diagnostics now show a lightbulb (⚡) with two actions: reorder the specific struct in-place, or open the diff editor preview for the whole file.
- **Fix all with diff preview** (`padlock: Fix all (preview)`): copies the file to a temp location, runs `padlock fix`, and opens the VS Code diff editor so you can review every reordering before applying. Clicking Apply writes the changes with a `.bak` backup.
- Zig added to extension keywords, activation events, and README language table.
- README updated with full documentation for all finding types, concurrent-field annotation syntax (Rust/C++/Go), and how the extension fits with the CLI and CI.

## [0.8.1] — 2026-04-11

### Fixed
- `cargo clippy --workspace -- -D warnings` now passes cleanly. Fixes:
  - `explain.rs`: `map_or(true, …)` → `is_none_or(…)`
  - `rust.rs`: two nested `if`/`if-let` blocks collapsed to let-chain form
  - `c_cpp.rs`: `parse_anonymous_nested` return type aliased to `RawField` (was `type_complexity`); `#[allow(clippy::only_used_in_recursion)]` on the `arch` parameter that threads through the recursive call
  - `cache.rs`: nested `if let Some(dir)` / `if err` collapsed to let-chain
  - `commands/analyze.rs`: 8-argument `run` refactored — output and arch options moved into `AnalyzeOpts` struct, bringing the function back under the 7-argument limit

## [0.8.0] — 2026-04-11

### Added
- **`padlock summary`**: project health overview that fits in one terminal screen. Shows aggregate weighted score with letter grade (A–F), a severity bar chart (High / Medium / Low / Clean), the N worst files grouped by source file (with per-file score, High-finding count, and total wasted bytes), and the N worst structs (by score then wasted bytes). Ends with a next-step hint: `Run 'padlock analyze <worst-file>' for full detail.` Supports `--top N` (default 5), `--cache-line-size`, `--word-size`, and the same `--filter` / `--exclude` flags as `analyze`.
- **`--fail-on-severity high|medium|low`** flag on `padlock analyze`: exits non-zero when any finding meets or exceeds the specified severity. `medium` triggers on Medium and High; `low` triggers on any finding. Composable with `--fail-below` score threshold.
- **`repr(align(N))` in Rust source**: the Rust frontend now parses `#[repr(align(N))]` on struct definitions. The effective alignment is raised to `N` (when `N > natural_align`); trailing padding is adjusted so the total size is a multiple of the new alignment. Exposed as a new `repr_align` helper in `padlock-source`.
- **Rust tuple structs**: the Rust frontend now recognizes and parses tuple struct declarations (`struct Foo(u64, u8)`). Fields are named `_0`, `_1`, etc. Source-aware fix generation (`padlock fix`) reorders tuple fields inside the `(...)` body verbatim, preserving visibility modifiers and attribute annotations. IR-level fallback emits `struct Name(T0, T1, …);`.
- **C/C++ anonymous nested structs/unions**: anonymous `struct`/`union` members inside outer `struct`/`union` declarations are now flattened into the parent layout, matching how the C/C++ compiler treats them. Named nested structs (used as field types) are still treated as opaque fields.
- **Cache-line boundary markers in `explain`**: when a struct's fields span more than one cache line, `padlock explain` inserts a visual separator row (`╞── cache line N (offset O) ════╡`) at each boundary. Single-cache-line structs show no separator.
- **Parallel file parsing**: directory walks now parse source files in parallel using `rayon`, significantly reducing wall-clock time on large codebases.
- **On-disk parse cache**: unchanged source files are served from `.padlock-cache/layouts.json` (keyed by path + mtime) and skipped on repeat runs. The cache is silently invalidated on mtime change and ignored if corrupt. Binary and DWARF paths bypass the cache.

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
