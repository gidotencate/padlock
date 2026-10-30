# padlock

[![crates.io](https://img.shields.io/crates/v/padlock-cli.svg)](https://crates.io/crates/padlock-cli)
[![CI](https://github.com/gidotencate/padlock/actions/workflows/ci.yml/badge.svg)](https://github.com/gidotencate/padlock/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/padlock-cli.svg)](#license)

**The lint pass for struct memory layout** — catches padding waste, false sharing, and cache locality problems at the source level, before they cost you at runtime.

Supports C, C++, Rust, Go, and Zig. Ranks findings by impact, generates reorder suggestions, flags concurrency risks. CLI-first and CI-ready.

**Accuracy:** checked against the compilers on real code with default settings (Go standard library, glibc, Python, OpenSSL and libuv headers, Rust crates; amd64, arm64, i386, riscv64): 83–100% of struct sizes exact, the rest flagged as uncertain, **none silently wrong**. See [How exact is it?](#how-exact-is-it).

```
$ padlock analyze src/connection.rs

Analyzed 2 structs — 10 bytes wasted across all structs

[✗] Connection (src/connection.rs:4)  24B  fields=4  holes=2  score=33
    [HIGH] Padding waste: 10B (41%) — 7B after `is_active` (offset 1), 3B after `is_tls` (offset 13)
    [HIGH] Reorder fields: 24B → 16B (saves 8B): timeout, port, is_active, is_tls  (~8 MB/1M instances)

[✓] ConnectionOptimal (src/connection.rs:22)  16B  fields=4  score=100
    (no issues found)
```

When analyzing a directory or multiple files, structs are grouped under `── file ──` headers with per-struct line numbers:

```
$ padlock analyze src/

Analyzed 3 files, 5 structs — 26 bytes wasted across all structs

── src/connection.rs ───────────────────────────────────────

[✗] Connection :4  24B  fields=4  holes=2  score=33
    [HIGH] Padding waste: 10B (41%) — 7B after `is_active` (offset 1), 3B after `is_tls` (offset 13)
    [HIGH] Reorder fields: 24B → 16B (saves 8B): timeout, port, is_active, is_tls

── src/stats.cpp ───────────────────────────────────────────

[~] Stats :5  96B  fields=4  score=70
    [LOW] False sharing: cache line 0: [read_mu, write_mu]  (inferred from type names — add guard annotations or verify with profiling)
    [MEDIUM] Locality: hot [read_mu, write_mu] mixed with cold [read_count, write_count] on same cache line(s)  (inferred from type names — verify with profiling)
```

---

## Features

| Capability | Details |
|---|---|
| **Padding waste** | Finds gaps from poor field ordering; shows per-gap offset and size so you know exactly where to look |
| **Reorder suggestions** | Computes optimal declaration order; shows before/after struct size and byte savings |
| **False sharing** | Detects concurrent fields with different guards on the same cache line (locks, `std::atomic`, C11 `_Atomic`, Go/Rust atomics); shows field names involved. High when a guard is annotated, Medium when inferred from type names with an atomic involved, Low when inferred between lock words alone |
| **Evidence labels** | Findings from explicit annotations are confirmed; findings from type-name inference are labeled `(inferred — verify with profiling)` |
| **Explicit guard annotation** | `#[lock_protected_by]`, `GUARDED_BY()`, `// padlock:guard=` — converts inferred findings to confirmed |
| **Locality** | Flags hot/cold field interleaving that hurts cache utilisation |
| **Scoring** | Each struct gets a 0–100 score (100 = no issues) |
| **Multi-language** | C, C++, Rust, Go, Zig source; compiled binaries via DWARF/BTF |
| **Multi-arch** | x86-64 (SysV and Windows/MSVC), x86 (i386), AArch64, Apple Silicon (128-byte lines), WASM32, RISC-V 64, Cortex-M, AVR; `--target <triple>` lays sources out for that target (data model, `long double`, alignment caps, MSVC bit-fields) |
| **Compiler-order layout** | `repr(Rust)` and plain Zig structs are laid out the way rustc/zig order them (largest alignment first), so their sizes match the compiled program; `--declared-order` shows source order instead; `--hide-repr-rust` excludes them |
| **Generics & templates** | C++ class templates, Rust/Go generic structs and Zig `fn(comptime T) type` are analyzed once per concrete instantiation used anywhere in the analyzed files (or listed under `[instantiate]` in `.padlock.toml`) |
| **Preprocessor & includes** | A C/C++ file is laid out with what its `#include`s declare (typedefs, constants, records, macros), found next to the file, in `-I DIR` / `include_dirs = [...]` and, for Linux targets, the system include paths. Macros are expanded (object-like and function-like, `#`/`##`), the target's predefined macros (`__x86_64__`, `__SIZEOF_LONG__`, …) are known, and `#if` branches decided for the target are resolved; `-D NAME[=VALUE]` / `defines = [...]` set project macros. A header is processed in the context of the file that includes it (umbrella headers such as `Python.h`). Members that still depend on undefined project macros, unknown typedefs or unparsable macro invocations are reported as uncertain instead of guessed |
| **C++ ABI** | Itanium inheritance: tail-padding reuse, empty-base optimisation, `[[no_unique_address]]`, virtual bases, primary base; namespaces and nested classes keep same-named types apart |
| **Exact modes** | `--compile-commands compile_commands.json` (C/C++ via the real compiler), `--go-exact` (Go toolchain via a virtual probe test), `--rust-exact` (rustc `-Zprint-type-sizes`) replace estimates with the compiler's own layout |
| **Verify against a binary** | `padlock verify --binary app src/` checks every source-level layout against the debug info of a build and fails CI on drift |
| **FFI check** | `padlock ffi src/` checks Rust `#[repr(C)]` structs against the C structs of the same name |
| **Measured hotness** | `--perf-types` reads perf's data-type profile so locality and false-sharing use measured accesses |
| **C++ stdlib variants** | `--stdlib libstdc++\|libc++\|msvc` selects the stdlib for type sizing; `libc++` `std::string` is 24B vs libstdc++ 32B |
| **Custom sync types** | `custom_sync_types = ["MyMutex", "ProtectedData"]` in `.padlock.toml` — project-specific lock types trigger false-sharing detection |
| **Rust niche optimization** | `Option<NonZeroXxx>`, `Option<&T>`, `Option<Box<T>>` sized as inner type (no discriminant overhead) |
| **MSVC `#pragma pack`** | Full pack-stack tracking (`push`/`pop`/`N`) for MSVC-style struct packing in C and C++ source |
| **Embedding hints** | When a struct with waste is embedded in others, output notes "fixing this would also shrink Foo, Bar" |
| **Generated-file skipping** | Machine-generated files (`// Code generated`, `// @generated`, `.pb.h`/`.pb.cc`, …) are silently skipped by default; `--include-generated` opts back in |
| **Path exclusions** | `exclude_paths = ["proto/**", "vendor/**"]` in `.padlock.toml` skips generated or third-party files |
| **ABI safety** | `padlock fix` warns before reordering fixed-layout structs (`repr(C)` Rust, C, Go, `extern`/`packed` Zig) that may break FFI or serialization — a plain (non-`extern`, non-`packed`) Zig struct is field-order-unstable like Rust's default `repr(Rust)` and does **not** get this warning, since the compiler may already reorder it |
| **CI-ready** | SARIF output, `action.yml`, exit-code gating on high-severity findings |
| **`cargo padlock`** | Cargo subcommand — builds your project then analyses the binary |
| **Compile-time assertions** | `#[padlock::assert_no_padding]`, `#[padlock::assert_size(N)]` proc macros; `#[padlock::assert_align(N)]` and `#[padlock::assert_offsets(field = N, …)]` from 0.47 |
| **Watch mode** | `padlock watch <path>` re-analyses on every file change |
| **Source-preserving fixes** | `padlock fix` reorders field chunks verbatim, keeping `pub`, `#[serde(...)]`, `/// doc-comments`, and guard annotations intact |
| **Project health summary** | `padlock summary` shows aggregate score, severity bar chart, worst files, and worst structs in one terminal screen |
| **Severity CI gate** | `--fail-on-severity medium\|low` exits non-zero when any finding meets or exceeds the threshold |
| **Parallel parsing** | Directory walks parse source files in parallel (rayon), with an on-disk mtime cache (`.padlock-cache/`) to skip unchanged files on repeat runs; live `scanning N / M files…` progress shown on stderr for large scans |
| **Source coverage** | Header shows `N of M types, X% source coverage` when types are skipped; `< 70%` triggers a "consider binary analysis" hint |
| **Cache-line visualization** | `padlock explain` adds a `CL` column (zero-indexed cache-line number per field/padding row) and inserts a separator row each time a field crosses into a new 64-byte (or 128-byte) cache line |
| **VS Code extension** | Findings in the Problems panel on save, status bar health score, hover popups, quick-fix lightbulb, and diff-preview fix-all |
| **`padlock-lsp`** | Standalone language server — diagnostics, hover, inlay hints, document symbols, and reorder quick-fixes for any LSP-capable editor (Neovim, Helix, Zed, JetBrains, Sublime); honours `.padlock.toml` |

---

## How exact is it?

Source analysis computes layouts the way the compiler would. Go files are laid out together with the rest of their package, Rust files with the rest of their crate, and C/C++ files with the headers they include (macros expanded), so types from other files are sized, not guessed. A Go file also reads the packages it imports — those of its module (`go.mod`, `go.work`) and the standard library — so a field typed `tspath.Path` or `ast.Kind` is sized from that package's source. What source alone cannot know, padlock marks as **uncertain** instead of guessing silently — a generic type of another Go package, a type from another Rust crate, a header it cannot find, a member written as an unknown macro, a branch on an undefined project macro.

**Measured on real code** with the default `padlock analyze <path>` — no options — against the layouts the compiler actually produced (its DWARF debug info, or rustc's `-Zprint-type-sizes`). *Exact* means the struct's size matches; *flagged* means padlock got it wrong but said so; *silently wrong* is the number that matters for trusting the output:

| Code | Target | Types | Exact | Flagged | Silently wrong |
|---|---|---:|---:|---:|---:|
| Go standard library (19 packages incl. `runtime`, `net/http`, `crypto/tls`; Go 1.27) | linux/amd64 | 708 | 95% | 5% | **0** |
| | linux/arm64 ¹ ³ | 394 | 83% | 17% | **0** |
| | linux/386 ¹ ³ | 391 | 83% | 17% | **0** |
| [microsoft/TypeScript](https://github.com/microsoft/TypeScript) (the Go port; non-test structs, `go test` probes as reference) | linux/amd64 | 1201 | 94% | 6% | **0** ⁴ |
| glibc headers (stdio, pthread, socket, signal, ucontext, elf, …) | x86_64 Linux | 153 | 91% | 9% | **0** |
| | aarch64 Linux ¹ | 181 | 96% | 4% | **0** |
| | riscv64 Linux ¹ | 169 | 93% | 7% | **0** |
| Python 3.12 + OpenSSL headers | x86_64 Linux | 152 | 95% | 5% | **0** |
| libuv headers | x86_64 Linux | 51 | 100% | 0% | **0** |
| padlock's own Rust crates | x86_64 Linux | 97 | 93% | 7% | **0** |
| V8 (C++ headers) | x86_64 Linux | — | 64% | 36% | **0** |
| Zig standard library | x86_64 Linux | — | 78% | 22% | **0** |
| Go standard library | windows/amd64 | — | 84% | 16% | **0** |
| Windows SDK C headers (mingw) ² | x86_64 Windows | 2199 | 67% | 31% | 35 |

¹ Cross targets need `--target`, and for C the target's headers (`-I`, here zig's bundled glibc headers).

Most flagged Go types are generics of another package (`collections.Set[T]`) or hold one; use `--go-exact` for those. ² The silent cases are headers whose packing comes from the file that includes them (`mciapi.h` via `mmsystem.h`), analyzed standalone. ³ Measured before padlock read imported Go packages (0.70); the amd64 row shows the effect. ⁴ `--go-exact` does not probe types declared inside functions or generic instances; the handful of such structs are not counted. Not yet measured on real code: macOS and Rust on 32-bit targets — for these the evidence is the conformance suite below.

Every change is also checked against the compilers (`gcc`, `g++`, `clang`, `rustc`, `go`, `zig`) with a conformance suite of probe structs per language — x86-64, aarch64, riscv64, i386, Cortex-M, AVR and Windows x64 for C; i386 for Go and Zig — plus differential fuzzing of random C/C++ structs (`tests/conformance/`). Differential fuzzing against g++ finds no mismatches in thousands of C++ seeds. The editor server (`padlock-lsp`) lays out the open file's live text against its Go package, Rust crate or C/C++ includes when it can find them (Go directories over 400 files are skipped), and the file alone otherwise.

When you need the compiler's own answer:

| Language | Exact mode | How |
|---|---|---|
| C / C++ | `--compile-commands build/compile_commands.json` | Recompiles each translation unit with its own flags plus debug info and reads the layouts the compiler produced — macros, `#if` configuration, templates and platform ABI included |
| Go | `--go-exact` | Adds a probe test to each package *virtually* (`go test -overlay`; nothing is written) and uses `reflect`'s sizes and offsets — exact even for generics of other packages |
| Rust | `--rust-exact` | Builds the crate once with `-Zprint-type-sizes` (via `RUSTC_BOOTSTRAP=1`, into `target/padlock-rust-exact`) and uses rustc's field order and sizes |
| Any | `padlock analyze <binary>` | Reads DWARF (including split DWARF, type units, compressed sections), BTF or PDB from a debug build. Build with `-fno-eliminate-unused-debug-types` (gcc/clang) so unused types are described too |

`padlock verify --binary <debug build> <sources>` turns the comparison into a CI check.

---

## Installation

### macOS / Linux (Homebrew)

```bash
brew tap gidotencate/padlock https://github.com/gidotencate/padlock
brew install padlock
```

### Any platform with Rust installed

```bash
cargo install padlock-cli
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/gidotencate/padlock/releases) for Linux (x86-64, ARM64), macOS (x86-64, Apple Silicon), and Windows (x86-64).

---

## Build from source

Requires a Rust toolchain (1.88+).

```bash
git clone <repo>
cd padlock
cargo build --release
# binary: target/release/padlock
```

Add to `PATH` or run directly:

```bash
export PATH="$PWD/target/release:$PATH"
```

Building for your own machine rather than for distribution? `RUSTFLAGS="-C target-cpu=native" cargo build --release` lets the compiler use your CPU's full instruction set. Not something the released binaries (or this repo's `Cargo.toml`) can use — they're built once on CI and run on arbitrary end-user CPUs, so baking in `target-cpu=native` there would break portability.

---

## Quick Start

```bash
# Analyze a source file
padlock analyze myfile.c

# Analyze an entire directory (recursive)
padlock analyze src/

# Analyze a compiled binary (DWARF)
padlock analyze target/debug/myapp

# Filter to only the worst structs
padlock analyze src/ --packable --sort-by waste

# Only structs with at least 2 padding holes, matching a name pattern
padlock analyze src/ --min-holes 2 --filter '^Hot'

# Cross-architecture analysis (e.g. checking Apple Silicon cache-line layout)
padlock analyze src/ --target aarch64-apple-darwin

# Focus on fixed-layout types only (hide repr(Rust) approximations)
padlock analyze src/ --hide-repr-rust

# Cargo subcommand — build + analyze in one step
cargo padlock
cargo padlock --bin myapp --sarif

# Analyze and output JSON
padlock analyze src/ --json

# Output SARIF for CI
padlock analyze myfile.cpp --sarif > padlock.sarif

# Project health summary (score, severity chart, worst files/structs)
padlock summary src/
padlock summary src/ --top 10

# Show field-reordering diff
padlock diff src/

# Show what fix would do (without writing)
padlock fix src/ --dry-run

# Stricter CI gate: fail on medium-severity or worse
padlock analyze src/ --fail-on-severity medium

# List all structs with sizes, holes, and scores
padlock list src/ --sort-by waste

# Live feedback — re-analyse on every save
padlock watch src/models.rs

# Show version
padlock --version
```

---

## Commands

### `padlock analyze <path>…`

Analyzes all structs in one or more files or directories and prints findings ranked by severity. Directories are walked recursively (skipping `target/`, `.git/`, etc.).

```
padlock analyze src/stats.rs
padlock analyze src/                      # entire directory
padlock analyze a.rs b.rs c.c            # multiple files
padlock analyze target/debug/myapp        # compiled binary (DWARF)
padlock analyze mylib.pdb                 # Windows PDB (MSVC debug database)
```

Flags:
- `--json` — emit JSON
- `--sarif` — emit SARIF 2.1.0 for CI tooling / GitHub code scanning
- `--markdown` — emit a GitHub-Flavored Markdown report (useful with `$GITHUB_STEP_SUMMARY`)
- `--filter <PATTERN>` — include only structs whose names match this regex
- `--exclude <PATTERN>` — exclude structs whose names match this regex
- `--min-holes <N>` — only structs with ≥ N padding gaps
- `--min-size <N>` — only structs with total size ≥ N bytes
- `--packable` — only structs that have a reorder suggestion
- `--sort-by score|size|waste|name` — sort order (default: score, worst first)
- `--cache-line-size <N>` — override the assumed cache-line size in bytes (default: 64, or 128 on Apple Silicon). Useful for comparing performance across architectures or analysing structs for embedded targets with non-standard cache geometries.
- `--word-size <N>` — override pointer/word size in bytes (e.g. `--word-size 4` for 32-bit targets). Affects all pointer-sized fields.
- `--target <TRIPLE>` — set the target architecture using a Rust target triple or short name. Common values: `aarch64-apple-darwin` (Apple Silicon, 128-byte cache lines), `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, `wasm32-unknown-unknown`. Overrides the `arch.override` setting in `.padlock.toml`.
- `--hide-repr-rust` — exclude `repr(Rust)` structs from output entirely. Useful when you want to focus on types with a fixed binary layout (C, `repr(C)`, Go, Zig) where findings are fully accurate and directly actionable.
- `--stdlib libstdc++|libc++|msvc` — set the C++ standard library variant for type sizing. Affects `std::string`, `std::mutex`, `std::shared_ptr`, and other stdlib types that differ in size across implementations. Default: `libstdc++` (GCC/Linux). Use `libc++` for macOS/iOS/Android Clang projects, `msvc` for Windows MSVC projects.
- `--fail-on-severity high|medium|low` — exit non-zero when any finding meets or exceeds this severity. `high` is the default CI gate (same as exit-on-high-finding behaviour); `medium` and `low` tighten the gate further.
- `--show-skipped` — print the full list of skipped types (generics/templates that could not be sized from source). By default only the count and a category breakdown are shown.
- `--include-generated` — include machine-generated source files in the analysis. By default, padlock skips files that declare themselves as generated (Go `// Code generated`, Rust `// @generated` / `//! @generated`, C/C++ `// Generated by` / `/* Generated by`, and protobuf `.pb.h`/`.pb.cc` files). These are typically not hand-tunable so findings in them are rarely actionable.

---

### `padlock summary <path>… [--top N]`

Shows a single-screen project health overview: aggregate weighted score + letter grade, severity bar chart, the N worst files, and the N worst structs. Designed for large codebases where `analyze` output is too verbose.

```
$ padlock summary src/

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Score   61 / 100   D    42 structs · 9 files · 384B wasted
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  🔴 High     ████████░░░░░░░░░░░░    14  (33%)
  🟡 Medium   ████░░░░░░░░░░░░░░░░     7  (16%)
  🔵 Low      ██░░░░░░░░░░░░░░░░░░     4  (10%)
  ✅ Clean    ██████░░░░░░░░░░░░░░    17  (40%)

  Worst files                              score    High   wasted
  ────────────────────────────────────────────────────────────────────
  src/network/connection.rs                   33       2     96B
  src/stats/metrics.rs                        42       1    128B

  Worst structs                   score   location
  ────────────────────────────────────────────────────────────────────
  Connection                         33   src/network/connection.rs:12
  Stats                              42   src/stats/metrics.rs:8

  Run `padlock analyze src/network/connection.rs` for full detail.
```

Flags:
- `--top <N>` — number of worst files and structs to show (default: 5)
- `--cache-line-size <N>` / `--word-size <N>` — arch overrides
- `--target <TRIPLE>` — set target architecture (see `analyze` flags)
- `--filter` / `--exclude` — same pattern filters as `analyze`

---

### `padlock list <path>…`

Lists every struct found with its size, field count, hole count, waste, and score. Accepts the same filtering and sorting flags as `analyze`.

```
$ padlock list src/server.rs --sort-by waste

Name               Size   Fields  Holes  Wasted  Score  Location
───────────────────────────────────────────────────────────────
Connection         24B    4       2      10B     33     src/server.rs:12
Stats              96B    4       1      8B      55     src/server.rs:28
ConnectionOptimal  16B    4       0      0B      100    src/server.rs:44
```

---

### `padlock diff <path>… [--filter PATTERN]`

Shows a unified diff of the current field order vs the optimal order. Accepts directories and multiple files.

```
$ padlock diff src/models.rs

--- Connection (current order)
+++ Connection (optimal order)
 Connection {
-    is_active: bool,
-    timeout: f64,
-    is_tls: bool,
-    port: i32,
+    timeout: f64,
+    port: i32,
+    is_active: bool,
+    is_tls: bool,
 }
```

### `padlock diff <path>… --against <git-ref> [--filter PATTERN]`

A different comparison: struct layout at a given git ref vs the current working tree, not declared-vs-optimal order. Catches ABI-relevant changes a plain source diff wouldn't call out — a reordered field, a new member, a size/alignment shift — useful as a pre-merge check on `#[repr(C)]`/`extern` structs shared across an FFI boundary.

```
$ padlock diff src/wire.rs --against main

~ Header  8B -> 12B  (waste 0B -> 3B, score 100 -> 72)
+ Footer  (new — 4B, score 100)
- LegacyTrailer  (removed — was 2B, score 100)
```

Requires the paths to be inside a git repository; `git_ref` is resolved via `git show <ref>:<path>`, so any ref `git` accepts (branch, tag, commit SHA, `HEAD~N`) works. A file that doesn't exist at `git_ref` is treated as new — every struct in it shows as added, not as an error.

---

### `padlock fix <path>… [--dry-run] [--backup] [--filter PATTERN]`

Shows the reorder diff and — without `--dry-run` — rewrites the source file in-place. Accepts directories and multiple files; `--filter` limits which structs are rewritten. Pass `--backup` to save a `.bak` copy of the original before rewriting (opt-in; use `--dry-run` to preview changes without touching any files).

**ABI safety warning:** Before rewriting any struct with a fixed binary layout (C structs, `repr(C)`, Go, Zig), padlock emits a warning to stderr:

```
padlock: warning: reordering fields in Connection, Stats will change the binary layout of a
fixed-ABI type. This may break FFI boundaries, serialized data compatibility, or any code that
assumes a specific field offset. Review all callers before applying.
```

`repr(Rust)` structs do not trigger this warning — the compiler already optimises their layout freely. Always audit callers and serialization code before applying fixes to fixed-ABI types.

---

### `padlock bpf <path>…`

Alias for `analyze` that targets eBPF object files and binaries containing a `.BTF` ELF section. Prints a brief orientation note (human output only) and runs the full analysis pipeline. Accepts `--json`, `--sarif`, `--fail-on-severity`, and all filter flags.

```bash
padlock bpf my_prog.bpf.o
padlock bpf my_prog.bpf.o --json
padlock bpf my_prog.bpf.o --sarif > padlock-bpf.sarif
```

See [docs/ebpf-btf.md](docs/ebpf-btf.md) for a full eBPF workflow including CI integration and comparison with `bpftool`.

---

### `padlock report <path>…`

Alias for `analyze`. Accepts the same flags.

---

### `padlock watch <path> [--json]`

Watches a file or directory and re-runs analysis on every change. Clears the terminal between runs for a live feedback loop. Works for both source files and compiled binaries.

```bash
# Watch a Rust source file while editing
padlock watch src/pool.rs

# Watch a binary — pair with cargo watch for a full rebuild loop
padlock watch target/debug/myapp
# In another terminal: cargo watch -x build
```

---

### `padlock explain <path>… [--filter PATTERN]`

Shows a visual field-by-field memory layout table with offset, size, alignment, and inline padding gap rows. When a reorder can reduce waste, an **impact block** is appended with concrete memory and cache estimates at 1K and 1M instance scales — turning an abstract percentage into a real number engineers can put in a code review.

```
$ padlock explain src/events.rs --filter ReadyEvent

ReadyEvent  (src/events.rs:42)
24 bytes  align=4  fields=3
┌────────┬──────┬───────┬────┬────────────────────────────────────┐
│ offset │ size │ align │ CL │ field                              │
├────────┼──────┼───────┼────┼────────────────────────────────────┤
│      0 │    1 │     1 │  0 │ tick: u8                           │
│      1 │    3 │     — │  0 │ <padding>                          │
│      4 │    4 │     4 │  0 │ ready: Ready                       │
│      8 │    1 │     1 │  0 │ is_shutdown: bool                  │
│      9 │   15 │     — │  0 │ <padding> (trailing)               │
└────────┴──────┴───────┴────┴────────────────────────────────────┘
14 bytes wasted (58%) — reorder: ready, tick, is_shutdown → 8 bytes
  ~8 KB extra per 1K instances · ~8 MB per 1M instances · ~125K extra cache lines/1M (seq. scan)
```

The impact line uses SI scaling: `savings × 1 000 ≈ KB`, `savings × 1 000 000 ≈ MB`. Cache-line estimates assume a sequential scan (64-byte lines). If the reorder also reduces the number of cache lines the struct spans per instance, an extra note is shown.

---

### `padlock verify --binary <BIN> <source>…`

Checks padlock's source-level layouts against the compiler's, read from a binary (or object file) built from those sources with debug info. Lists every struct whose size or field offsets differ and exits 1 when any does — use it in CI to prove the source analysis can be trusted for your code base. Names are matched across namespaces/modules (`ns::Conn`, `main.Conn`); Go types only match their own package.

```bash
gcc -g -fno-eliminate-unused-debug-types -c conn.c -o conn.o
padlock verify --binary conn.o conn.c
# 12/12 structs match the binary's debug info
```

C/C++ sources are read with the same headers and macros the build used: pass `-I DIR` / `-D NAME[=VALUE]`, or set `include_dirs` / `defines` in `.padlock.toml` (read from the first source's directory upward), exactly as for `analyze`. Blank Go fields (`_`) are not compared.

---

### `padlock ffi <path>…`

For every Rust `#[repr(C)]` struct that has a C/C++ struct of the same name among the analyzed files, checks that both describe the same memory — size, alignment, field count, and each field's offset and size by position. Exits 1 on any mismatch.

```bash
padlock ffi src/ include/
# 1/2 repr(C) structs match their C definition   (Packet: `len` at 8 (8B) vs `len` at 4 (4B))
```

---

### `padlock init [--force]`

Generates a `.padlock.toml` configuration file in the current directory with every supported option commented out and annotated. Use this when adopting padlock in an existing project to see all available settings at a glance.

```bash
padlock init               # writes .padlock.toml (fails if it already exists)
padlock init --force       # overwrites an existing config
```

---

## Configuration (`.padlock.toml`)

Place a `.padlock.toml` file at the root of your project to set defaults for all commands. CLI flags always take precedence over the config file.

```toml
[padlock]
# Skip generated or third-party files by glob (matched against source_file paths)
exclude_paths = ["proto/**", "vendor/**", "third_party/**", "generated/**"]

# Show only structs whose names match this regex (same as --filter)
filter = ""

# Exclude structs whose names match this regex (same as --exclude)
exclude = "^__"

# Skip structs smaller than this many bytes
min_size = 0

# Skip structs with fewer than this many padding gaps
min_holes = 0

# Default sort order: "score" | "size" | "waste" | "name"
sort_by = "score"

# Exit non-zero when any finding meets or exceeds this severity: "high" | "medium" | "low"
fail_on_severity = "high"

# Project-specific lock/sync wrapper type names.
# Fields whose type name contains any of these strings are classified as
# Concurrent, enabling false-sharing and locality detection for custom guards.
custom_sync_types = []
# Example: custom_sync_types = ["MyMutex", "ProtectedData", "RwLock"]

# C/C++ macros treated as defined when choosing #if/#ifdef branches (like -D).
defines = []
# Example: defines = ["CONFIG_SMP", "WORD_BITS=64"]

# Extra directories searched for C/C++ #includes (same as -I DIR).
include_dirs = []
# Example: include_dirs = ["include", "third_party/foo/include"]

# Show repr(Rust) and plain Zig structs in source order instead of the order
# the compiler uses (same as --declared-order).
declared_order = false

[instantiate]
# Generic types to analyze with these arguments, besides the instantiations
# found in the code (C++ templates, Rust/Go generics, Zig comptime functions).
# Buf  = ["char, 8", "int"]
# Pair = ["u32, String"]

[arch]
# Lay out source files for this target instead of the host.
# Accepted values: "x86_64", "x86_64-msvc", "x86", "aarch64", "aarch64_apple",
# "wasm32", "riscv64", "cortex_m", "cortex_m4", "avr", or a Rust target triple
# (e.g. "aarch64-apple-darwin", "x86_64-pc-windows-msvc").
# Takes effect when no --target flag is passed on the CLI.
override = ""
```

The `exclude_paths` globs are matched against the `source_file` field of each layout — relative paths as reported by the parser. Use `**` to match any number of path components, `*` for one component, and `?` for one character. Patterns are normalized to forward slashes before matching (so Windows paths work correctly).

---

### `padlock check [--baseline FILE] [--save-baseline] <path>…`

Baseline / ratchet mode for CI. First run saves a JSON snapshot of current findings; subsequent runs fail only on regressions — existing issues do not block merges.

```bash
# Step 1: save a baseline
padlock check src/ --save-baseline --baseline .padlock-baseline.json

# Step 2: every CI run (fails only on new regressions)
padlock check src/ --baseline .padlock-baseline.json
```

A struct is a regression if:
- Its worst finding severity increased (Low → Medium, Medium → High)
- Its score dropped by more than 1 point
- It is new (not in the baseline) and has at least one High finding

Every run prints a drift summary: `N new / M resolved / K unchanged` — where *resolved* counts structs that improved significantly since the baseline or that no longer appear (deleted/refactored).

Flags:
- `--baseline FILE` — path to baseline JSON (default: `.padlock-baseline.json`)
- `--save-baseline` — write current findings as the new baseline instead of comparing
- `--json` — emit comparison result as JSON

---

### `cargo padlock [--bin NAME] [--release] [--json] [--sarif]`

Installed as a cargo subcommand when padlock is on `PATH`. Reads `Cargo.toml` to determine the default binary name, runs `cargo build`, locates the built binary, and analyses it — all in one command.

```bash
cargo padlock                       # analyze default binary (debug)
cargo padlock --bin myapp           # specific binary target
cargo padlock --release             # build with --release profile
cargo padlock --sarif               # SARIF output for CI
```

Exits non-zero when high-severity findings exist, so it can gate CI directly.

---

## Understanding Findings

### PaddingWaste

The compiler inserts invisible padding bytes between fields to satisfy alignment requirements. These bytes are wasted memory that can push structs across cache lines.

```
struct Connection {
    is_active: bool,  // 1 byte, then 7 bytes padding
    timeout:   f64,   // 8 bytes
    is_tls:    bool,  // 1 byte, then 3 bytes padding
    port:      i32,   // 4 bytes
}                     // total: 24 bytes, 10 wasted (41.7%)
```

Severity: **High** ≥ 30% wasted · **Medium** ≥ 10% · **Low** < 10%

---

### ReorderSuggestion

Reordering fields by descending alignment eliminates most padding. padlock computes the optimal order and shows exact savings.

```
// Optimal: timeout (align 8) first, then port (align 4), then bools (align 1)
struct Connection {
    timeout:   f64,   // 8 bytes at offset 0
    port:      i32,   // 4 bytes at offset 8
    is_active: bool,  // 1 byte  at offset 12
    is_tls:    bool,  // 1 byte  at offset 13
}                     // total: 16 bytes — saves 8 bytes
```

Severity: **High** saves ≥ 8 bytes · **Medium** otherwise

---

### FalseSharing

When two or more fields are accessed concurrently under **different** locks (or independently), but share the same 64-byte cache line, every write to one field invalidates the other core's cached copy — even though they protect independent data.

```cpp
struct Stats {
    std::mutex read_mu;    // ┐ both on cache line 0 (offsets 0 and 48)
    int64_t    read_count; // │
    std::mutex write_mu;   // ┘ → false sharing between read_mu and write_mu
    int64_t    write_count;
};
```

Two atomics updated by different threads (`std::atomic<long> produced, consumed;`) are the same hazard and are reported too. Only the fields that conflict are listed.

Fix: pad each independently-locked group (or each hot atomic) to its own cache line.

Severity: **High** when a conflicting field carries a guard annotation (below); **Medium** when the guards are inferred from type names and an atomic is involved (independently updated atomics bounce the line on every write); **Low** when the inferred conflict is between lock words alone (two mutexes side by side only hurt when both are contended at the same moment). Inferred findings are likely but unconfirmed — verify with profiling before restructuring.

#### Explicit guard annotation

By default padlock infers concurrency from type names (`Mutex`, `std::atomic`, `sync.Mutex`, …). For fields whose types don't reveal their guard, annotate them explicitly — this is the most accurate path to false-sharing detection.

**Rust** — field attributes:

```rust
struct HotPath {
    #[lock_protected_by = "mu_a"]
    readers: u64,          // guarded by mu_a
    #[lock_protected_by = "mu_b"]
    writers: u64,          // guarded by mu_b — different guard, same cache line → High
    mu_a: Mutex<()>,
    mu_b: Mutex<()>,
}
```

Also accepted: `#[guarded_by("mu")]`, `#[guarded_by(mu)]`, `#[protected_by = "mu"]`, `#[pt_guarded_by("mu")]`.

**C/C++** — Clang thread-safety analysis macros:

```cpp
#include <mutex>
struct Cache {
    int64_t readers GUARDED_BY(lock_a);   // or __attribute__((guarded_by(lock_a)))
    int64_t writers GUARDED_BY(lock_b);   // different guard → false sharing detected
    std::mutex lock_a;
    std::mutex lock_b;
};
```

Also accepted: `PT_GUARDED_BY(mu)` (pointer targets), `__attribute__((pt_guarded_by(mu)))`.

**Go** — trailing line comments:

```go
type Cache struct {
    Readers int64      // padlock:guard=mu_a
    Writers int64      // padlock:guard=mu_b  ← different guard → false sharing
    MuA     sync.Mutex
    MuB     sync.Mutex
}
```

Also accepted: `// guarded_by: mu`, `// +checklocksprotects:mu` (gVisor-style).

---

### LocalityIssue

Hot fields (accessed concurrently / frequently) interleaved with cold fields (rarely accessed) waste cache lines and pollute the hot-path working set.

```c
struct Worker {
    pthread_mutex_t mu;   // hot — locked on every task
    int             id;   // cold — set once at startup
    int             tasks_done; // hot
    char            name[64];   // cold
};
```

Severity: **Medium**

---

## Scoring

Each struct receives a score from 0 (worst) to 100 (perfect packing, no concurrency issues).

| Score | Meaning |
|---|---|
| 100 | No findings |
| 80–99 | Minor issues (Low-severity padding) |
| 50–79 | Moderate issues (Medium findings) |
| 0–49 | Significant issues (High findings) |

---

## Language Support

Source analysis never invokes a compiler — field sizes come from a built-in type table, so how *accurate* that is depends on whether the language's layout is actually fixed by a spec/ABI, or left to the compiler's discretion. These are not equally reliable, even though all five "work":

| Language | Source analysis accuracy | Binary (DWARF/BTF) |
|---|---|---|
| C | **Accurate** — struct layout is ABI-fixed in declaration order | ✓ |
| Go | **Accurate** — the language spec guarantees declaration-order layout, never reordered by the compiler | ✓ |
| Zig (`extern`/`packed`) | **Accurate** — ABI-defined layout | via DWARF |
| Zig (plain struct) | **Sizes match the compiler** — laid out in the order zig uses (largest alignment first); field order is still implementation-defined, so findings are severity-downgraded, same as Rust `repr(Rust)` | via DWARF |
| Rust (`repr(C)`/`packed`/`transparent`) | **Accurate** | ✓ |
| Rust (`repr(Rust)`, the default) | **Sizes match the compiler** for the probed cases — laid out in rustc's order (largest alignment first); offsets are an estimate since rustc may move a niche field first, and there is no layout guarantee; `--rust-exact` gives rustc's own answer | ✓ |
| C++ | **Accurate** for the conformance suite — Itanium inheritance (primary base, tail-padding reuse, empty bases, virtual bases), vtables, bit-fields, namespaces, and templates once per concrete instantiation; STL internals depend on `--stdlib`, and `--compile-commands` gives the compiler's own answer | ✓ |
| eBPF (BTF) | — | ✓ (`.BTF` ELF section) |

So: if you only ever pass source files (no `padlock analyze <binary>`), every language gives compiler-matching sizes for the constructs the conformance suite covers. Plain Zig structs and Rust's default `repr(Rust)` still carry the "compiler may reorder" caveat — real, not a padlock limitation, since there is no fixed layout to promise. What source alone cannot see (types from unanalyzed headers or other packages, macro-generated members, template arguments never used in the analyzed files) is reported as `uncertain` or skipped; the exact modes in [How exact is it?](#how-exact-is-it) cover those.

- C++ `alignas(N)` field and struct-level annotations are extracted from source and applied to field alignment and struct trailing padding. For the most precise layout (e.g. complex template instantiations), binary (DWARF) analysis remains the authoritative path.

### Rust repr support

Rust's memory layout depends on which `repr` is in effect. padlock handles each case differently:

| repr | Layout guarantee | padlock accuracy | Notes |
|---|---|---|---|
| `repr(Rust)` (default) | None — compiler may reorder | Sizes match rustc | Laid out in rustc's order (largest alignment first); `--declared-order` shows the source order instead. Findings are severity-downgraded |
| `repr(C)` | C-compatible, declaration order | **Accurate** | Full analysis; best candidate for padding fixes |
| `repr(packed)` / `repr(packed(n))` | No padding, fields may be unaligned | Accurate for waste | Reorder suggestions suppressed — packing is intentional; note that unaligned field references can cause UB |
| `repr(align(n))` | Minimum alignment forced | Accurate | Struct alignment and trailing padding raised to `n`, also when the struct is nested in another |
| `repr(transparent)` | Same as inner field | Accurate | Single-field wrapper; padding findings correctly suppressed |
| `repr(u*)` / `repr(i*)` | Enum discriminant size | Accurate | Discriminant sized from the repr (or widened by explicit discriminant values); data enums get a synthetic payload field, and enums whose payload has spare bit patterns (`enum { A(Box<T>), B }`) are sized as the payload alone |

**Key points for Rust:**

- **`repr(C)` structs are the highest-value target.** Their layout is fixed in declaration order, they may cross FFI boundaries, and every wasted byte is a genuine cost. padlock's reorder suggestions for `repr(C)` structs are directly actionable.

- **Plain `repr(Rust)` structs** may already be optimally ordered by the compiler at compile time — the cost you pay is in source readability and the risk that adding a field in a "logical" position silently bloats the layout. padlock finds those risks.

- **`repr(packed)` trades padding waste for unaligned access.** padlock detects it and suppresses false-positive reorder suggestions. If padlock flags a padded struct and you add `repr(packed)` as a fix, verify that you never take a reference to a field — that can cause undefined behaviour on some architectures.

- **`repr(align(n))` is the correct fix for false sharing.** Instead of manual padding arrays, use `#[repr(align(64))]` (or 128 on Apple Silicon) on a wrapper struct. padlock's FalseSharing finding tells you *which* structs need this treatment; tokio's `CachePadded<T>` is the canonical Rust implementation of this pattern.

```rust
// What padlock flags:
struct WorkerState {
    task_count: AtomicU64,  // hot — modified on every task poll
    is_parked:  AtomicBool, // hot — different lock bucket
    name:       String,     // cold — set once at init
}

// One correct fix — separate hot fields onto their own cache line:
#[repr(align(64))]
struct WorkerState {
    task_count: AtomicU64,
    is_parked:  AtomicBool,
}
```

For exact compiler-verified layout of any repr, use `padlock analyze target/debug/myapp` (binary/DWARF mode).

---

## Architecture Support

| Architecture | Pointer | Cache Line | Notes |
|---|---|---|---|
| `x86_64` (SysV ABI) | 8 bytes | 64 bytes | Default |
| `aarch64` | 8 bytes | 64 bytes | Linux/Android |
| `aarch64_apple` | 8 bytes | 128 bytes | M-series Mac |
| `wasm32` | 4 bytes | 64 bytes | WebAssembly |
| `x86_64-msvc` | 8 bytes | 64 bytes | Windows x64 (LLP64 `long`, 2-byte `wchar_t`, MSVC bit-fields); also `msvc`, `windows` |
| `riscv64` | 8 bytes | 64 bytes | RISC-V 64-bit |
| `x86` | 4 bytes | 64 bytes | 32-bit x86 (i386 SysV: 8-byte types 4-byte aligned in structs); also `i686` |
| `cortex_m` | 4 bytes | — | Cortex-M0/M0+/M3/M23 — no cache; false-sharing suppressed |
| `cortex_m4` | 4 bytes | 32 bytes | Cortex-M4/M7/M33 — optional L1 cache |
| `avr` | 2 bytes | — | AVR 8-bit (ATmega etc.) — no cache, 1-byte alignment |

The architecture is auto-detected from the host when analyzing source files. For binary analysis it is read from the binary's ELF/Mach-O/PE header. Use `--target <triple>` to override for cross-compilation scenarios (e.g. `--target aarch64-apple-darwin` when building for Apple Silicon from a Linux CI host).

Targets with `cache_line_size = 0` (Cortex-M0/M3, AVR) automatically suppress `FalseSharing` and `LocalityIssue` findings — these are meaningless without a hardware cache. `PaddingWaste` and `ReorderSuggestion` still apply.

See **[docs/robotics-ros2.md](docs/robotics-ros2.md)** for a full embedded ARM workflow with STM32/ROS 2 examples.

---

## Real-World Findings

padlock run against popular open-source projects — layout issues that accumulate invisibly over time:

| Project | Language | Version | Structs | Wasted | Score | Notable finding |
|---|---|---|---|---|---|---|
| [tokio](https://tokio.rs) | Rust | 1.51.1 | 367 | 485B | 91/100 A¹ | `TraceStatus` — score 48, false sharing |
| [Redis](https://redis.io) | C | 7.0.15 | 282 | 892B | — | `multiState` — 20% waste, saves 8B |
| Go `net` + `database/sql` | Go | stdlib 1.22 | 607 | 1 236B | 86/100 B | `sql.DB` — false sharing, score 53; fix pending ([CL 767580](https://go-review.googlesource.com/c/go/+/767580)), 7–25× improvement under concurrent load |
| [grpc-go](https://github.com/grpc/grpc-go) | Go | latest | — | — | — | 4 structs fixed — 72B saved/RPC, false sharing on `mu` eliminated ([#9281](https://github.com/grpc/grpc-go/pull/9281), [#9359](https://github.com/grpc/grpc-go/pull/9359), [#9360](https://github.com/grpc/grpc-go/pull/9360), [#9361](https://github.com/grpc/grpc-go/pull/9361) — all merged) |
| Linux kernel `net/` | C | 6.x | 2 066 | 5 093B | 84/100 B | `virtio_vsock` — score 45, all 4 finding types |

¹ repr(Rust) structs are severity-downgraded (compiler may already reorder). Use `--hide-repr-rust` to focus on ABI-stable findings only.

**Rust / tokio — `TraceStatus`, false sharing (score 48):**
```
[HIGH] False sharing: spin_lock and waiter fields share a cache line
[HIGH] Reorder fields: 24B → 16B (saves 8B)
note: repr(Rust) — compiler may reorder; use binary analysis for confirmed layout
```

**C / Redis — `multiState`, 20% waste** (layout is deterministic — no compiler reordering in C):
```
[HIGH] Reorder fields: 40B → 32B (saves 8B): argv_len_sums, commands, alloc_count, ...
```

**Go / `database/sql.DB` — false sharing** (layout is deterministic — Go does not reorder fields):
```
[HIGH]   False sharing: cache line 0: [waitDuration, numClosed, mu]  (inferred from type names — add guard annotations or verify with profiling)
[MEDIUM] Locality: hot [waitDuration, numClosed, mu] interleaved with cold [connector, freeConn, ...]
```
`waitDuration` and `numClosed` are atomic counters updated on every query. They share a cache line with `mu` — under concurrent load, atomic writes invalidate the line that other goroutines need to lock. Fix submitted upstream ([CL 767580](https://go-review.googlesource.com/c/go/+/767580)): separating the atomics with `_ [48]byte` padding shows 7–25× improvement at 1–4 concurrent stressors in microbenchmarks.

**C / Linux kernel `net/` — `virtio_vsock`, score 45 (all 4 finding types):**
```
[MEDIUM] Padding waste: 24B (18%) across 4 gaps
[HIGH]   Reorder fields: 136B → 112B (saves 24B): event_lock, event_work, out_sgs, ...  (~24 MB/1M instances)
[HIGH]   False sharing: 2 cache-line conflicts (lock fields mixed with work structs)
[MEDIUM] Locality: hot [tx_lock, queued_replies, rx_lock, event_lock] interleaved with cold [vdev, vqs, ...]
```
`rcu_node` in `kernel/rcu/` is another example: score 52, 5 false-sharing conflicts across 39 fields — a concurrency-sensitive struct that has grown organically and accumulated layout debt.

See **[docs/real-world-examples.md](docs/real-world-examples.md)** for full field-by-field layouts and fix examples for each language.

---

## Compile-Time Assertions

`padlock-macros` provides proc-attribute macros that turn layout violations into **compile errors**. Add it to `Cargo.toml`:

```toml
[dependencies]
padlock-macros = "0.20"
```

### `#[padlock::assert_no_padding]`

Fails to compile if the struct has any padding bytes. The check is: `size_of::<Struct>() == sum(size_of::<FieldType>())`.

```rust
use padlock_macros::assert_no_padding;

#[assert_no_padding]        // ✓ compiles: 8 + 4 + 4 = 16 = size_of
struct WellOrdered {
    a: u64,
    b: u32,
    c: u32,
}

#[assert_no_padding]        // ✗ compile error: 1 + 8 = 9 ≠ 16 = size_of
struct Padded {
    a: u8,
    b: u64,
}
```

### `#[padlock::assert_size(N)]`

Fails to compile if the struct's size is not exactly `N` bytes. Useful for locking down hot-path structs against accidental growth.

```rust
use padlock_macros::assert_size;

#[assert_size(64)]          // ✓ exactly one cache line
struct CacheLine {
    data: [u8; 64],
}
```

---

## CI Integration

### GitHub Actions (recommended)

Use the bundled `action.yml` to analyse binaries or source files on every PR. Findings appear as inline annotations on the diff when SARIF is enabled.

```yaml
# .github/workflows/padlock.yml
name: Struct Layout Analysis
on: [push, pull_request]

permissions:
  contents: read
  security-events: write   # required for SARIF upload

jobs:
  padlock:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build
      - uses: gidotencate/padlock@v0.12.0
        with:
          path: target/debug/myapp
          output-format: sarif
          fail-on-severity: high
```

See `.github/workflows/padlock-example.yml` for a full reference workflow including all options.

### `cargo padlock` in CI

```yaml
- uses: dtolnay/rust-toolchain@stable
- run: cargo install padlock-cli
- run: cargo padlock --sarif   # exits non-zero on high-severity findings
```

### JSON output for scripting

```bash
padlock analyze src/ --json | jq '.structs[] | select(.score < 60)'
```

### Pre-commit hook

Run padlock before every commit so layout regressions never reach the repo.

**Plain git hook** — add to `.git/hooks/pre-commit` (and `chmod +x`):

```bash
#!/usr/bin/env bash
set -euo pipefail

# Collect staged source files padlock understands
FILES=$(git diff --cached --name-only --diff-filter=ACM \
  | grep -E '\.(c|cpp|cc|h|hpp|rs|go)$' || true)

if [ -z "$FILES" ]; then
  exit 0
fi

echo "padlock: checking struct layouts…"
padlock analyze $FILES --fail-on-severity high
```

**[pre-commit](https://pre-commit.com) framework** — add to `.pre-commit-config.yaml`:

```yaml
repos:
  - repo: local
    hooks:
      - id: padlock
        name: padlock struct layout check
        language: system
        entry: padlock analyze
        args: [--fail-on-severity, high]
        types_or: [c, c++, rust]   # pre-commit passes matched files as arguments
        pass_filenames: true
```

**[lefthook](https://github.com/evilmartians/lefthook)** — add to `lefthook.yml`:

```yaml
pre-commit:
  commands:
    padlock:
      glob: "*.{c,cpp,h,rs,go}"
      run: padlock analyze {staged_files} --fail-on-severity high
```

---

## VS Code Extension

Install from the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=gidotencate.padlock) or search for **padlock** in the Extensions panel.

Requires `padlock` on your `PATH` (`cargo install padlock-cli`).

- **Problems panel** — findings appear automatically on file save for Rust, C, C++, Go, and Zig files.
- **Status bar** — live health score and letter grade (`$(lock) 67 D`) for the active file; yellow when High findings are present.
- **Hover** — hover over a struct definition line to see score bar, wasted bytes, and each finding.
- **Quick-fix lightbulb** — `ReorderSuggestion` diagnostics offer an in-place fix for the struct or a diff-preview fix for the whole file.
- **`padlock: Fix all (preview)`** — opens the VS Code diff editor before writing any changes; saves a `.bak` backup on apply.
- **CodeLens** — a clickable `padlock: score N · MB wasted` line above each struct's declaration; click to apply the reorder fix directly (when one exists).
- **Line highlight** — each struct's declaration line gets a subtle background tint (red for a High finding, amber for Medium) so problem structs stand out while scrolling, without needing to read the Problems panel.

See [editors/vscode/README.md](editors/vscode/README.md) for the full extension documentation.

---

## Other Editors (LSP)

For editors without a bespoke padlock integration, `padlock-lsp` exposes the same diagnostics and hover findings over the Language Server Protocol:

```bash
cargo install padlock-lsp
```

Point your editor's LSP client at the `padlock-lsp` binary for `.c`, `.cpp`, `.rs`, `.go`, and `.zig` files. It analyzes the live buffer in-process on open/change/save — no `padlock` CLI install required, and no subprocess per keystroke. Diagnostics, hover, inlay hints (a compact `N B wasted` label inline at the end of each struct's declaration, with the full finding list in its tooltip), document symbols (an outline entry per struct with its score), and quick-fix code actions (reorder one struct, or all reorderable structs in the file) — no binary/DWARF analysis or workspace-wide scan; use the CLI for those. It honours the same `.padlock.toml` as the CLI (`ignore`, `min_severity`, per-struct overrides, `arch.override`) — a struct suppressed or re-severity'd for the CLI stays suppressed/re-severity'd in the editor. Editors that prefer pushing settings over editing a repo file can instead set `minSeverity`/`arch` via `initializationOptions` (or push a live update via `workspace/didChangeConfiguration`) — these take precedence over the file when present. Example Neovim setup (`nvim-lspconfig`-style, since padlock isn't a built-in config yet):

```lua
vim.lsp.start({
  name = 'padlock-lsp',
  cmd = { 'padlock-lsp' },
  root_dir = vim.fs.root(0, { '.git', 'Cargo.toml', 'go.mod' }),
})
```

---

## Supported Types

### SIMD

padlock knows the sizes and alignments of SIMD vector types:

| Type | Size | Align | ISA |
|---|---|---|---|
| `__m128`, `__m128d`, `__m128i` | 16 | 16 | SSE |
| `__m256`, `__m256d`, `__m256i` | 32 | 32 | AVX |
| `__m512`, `__m512d`, `__m512i` | 64 | 64 | AVX-512 |
| `float32x4_t`, `int8x16_t`, … | 16 | 16 | ARM NEON 128-bit |
| `float32x2_t`, `int8x8_t`, … | 8 | 8 | ARM NEON 64-bit |

A struct with a field placed before a SIMD type will be flagged for `PaddingWaste` as normal.

### Unions (C/C++)

Unions are parsed and simulated correctly — all fields at offset 0, total size = largest field. `PaddingWaste` and `ReorderSuggestion` are suppressed for unions since they are already compact by definition. `FalseSharing` and `LocalityIssue` still apply.

### Bit Fields (C/C++)

Structs containing bit-field members are parsed using GCC/Clang ABI rules: consecutive bitfields of the same storage-unit type are grouped into a single synthetic field (e.g. `[a:3|b:5]`), and the group is sized and aligned to the storage unit (`int` → 4 bytes, `uint8_t` → 1 byte). A change in storage-unit size, overflow of the current unit, or a zero-width flush (`int : 0`) starts a new group. Anonymous padding bits (`int : 3` with no name) consume bits within the current unit but are omitted from the label; all-anonymous groups emit a `[__pad]` placeholder so that the total size remains correct.

Bits are placed one by one under the GCC/Clang SysV rules (a field never straddles its own type's alignment boundary unless packed; `T : 0` aligns to `T`; `packed` and `#pragma pack` are honoured). The `x86_64-msvc` target uses MSVC's rules instead (a new unit whenever the declared type changes). For compiler-accurate bit-level offsets you can also use binary analysis (`padlock analyze target/debug/myapp`); the compiler encodes real offsets in DWARF/PDB. In binary mode (DWARF, PDB, BTF), bitfield members are grouped into synthetic `[a:3|b:5]` storage-unit fields using the same rules; groups where the storage-unit size cannot be determined are flagged as uncertain fields.

---

## Scope and Limitations

padlock is a **layout waste detector and optimizer**. It focuses on padding, field ordering, false sharing, and cache locality. It is not:

- A full compiler — type sizes are approximated from a built-in type table for source analysis. Use binary (DWARF) analysis for compiler-accurate results.
- A profiler — it cannot measure actual cache miss rates.

### What source analysis gets right

| Language | Accurate | Notes |
|---|---|---|
| C / C++ | All C primitives (`char`–`long long`, `float`/`double`/`long double`), `stdint.h` exact-width (`int8_t`–`uint64_t`), C99 fast/least family, `intmax_t`/`uintmax_t`, `size_t`/`ptrdiff_t`/`intptr_t`, `std::atomic<T>` | |
| C / C++ | Linux kernel types: `u8`–`u64`, `s8`–`s64`, `__u8`–`__u64`, `__s8`–`__s64`, endian-annotated `__be16`/`__le32` etc. | |
| C / C++ | Windows SDK: `BYTE`, `WORD`, `DWORD`, `QWORD`, `BOOL`, `HANDLE`, `LPVOID`, `UINT8`–`UINT64`, `INT8`–`INT64` and pointer aliases | |
| C / C++ | GCC/Clang extensions: `__int128`, `_Float16`, `__fp16`, `__bf16`, `_Float128` | |
| C / C++ | Character types: `wchar_t` (4B on POSIX), `char8_t`, `char16_t`, `char32_t` | |
| C++ | vtable pointers, single/multiple inheritance per the Itanium ABI (primary base, tail-padding reuse, empty-base optimisation, virtual bases placed last), `[[no_unique_address]]`, `alignas(N)` on fields and structs, namespace/class-qualified names | on `x86_64-msvc`, bases never share tail padding (MSVC rule); MSVC virtual-base (vbptr) layout is approximate |
| C++ (binary/DWARF) | Base-class subobjects extracted from `DW_TAG_inheritance` as synthetic `[BaseName]` fields at correct byte offsets; virtual bases skipped | — |
| C++ stdlib | `std::string`/`std::string_view`, `std::vector<T>`/`std::deque<T>`/`std::list<T>`, `std::map`/`std::set`/unordered variants, `std::unique_ptr`/`std::shared_ptr`/`std::weak_ptr`, `std::optional<T>` (recursive), `std::function`, `std::any`, `std::span<T>`, `std::error_code`, `std::atomic_flag`, `std::variant<T...>`, `std::pair<T1,T2>`, `std::tuple<T...>`, `std::array<T,N>`, `std::complex<T>`, `std::bitset<N>`, `std::chrono::duration`/`time_point`, `std::initializer_list<T>`, `std::filesystem::path` (32B), `std::thread` (8 words), `std::jthread` (9 words) | sizes vary by stdlib; use `--stdlib libstdc++\|libc++\|msvc` to select the correct variant (default: libstdc++) |
| C / C++ | `__attribute__((packed))` structs and classes; `#pragma pack(N)` / `#pragma pack(push, N)` / `#pragma pack(pop)` | no inter-field padding inserted (packed); `#pragma pack(N)` caps field alignment at N |
| Rust | All primitive types (`u8`–`u128`, `i8`–`i128`, `f16`, `f32`, `f64`, `f128`, `usize`, `isize`, `char`, `bool`), `repr(C)`, `repr(packed)`, `repr(transparent)`, `repr(align(N))` | |
| Rust stdlib | `Vec`, `String`, `Box`, `Arc`, `Rc`, all `AtomicXxx`, `PhantomData`, `Duration`, channels, smart pointers, all `NonZeroXxx` | size is independent of type parameter `T` |
| Rust stdlib | `VecDeque<T>` (4 words / 32B), `HashMap<K,V>` / `HashSet<V>` (6 words / 48B on 64-bit), `BTreeMap` / `BTreeSet` (3 words / 24B), `LinkedList` / `BinaryHeap` (3 words / 24B) | `HashMap`/`HashSet` sizes reflect hashbrown internals — use binary analysis to verify |
| Rust stdlib | `RefCell<T>` — modelled as `isize` borrow counter + `UnsafeCell<T>` at aligned offset | |
| Rust stdlib | Transparent newtypes: `Cell<T>`, `MaybeUninit<T>`, `UnsafeCell<T>`, `Wrapping<T>`, `Saturating<T>`, `ManuallyDrop<T>` | sized as inner `T` |
| Rust stdlib | Niche-optimized options: `Option<NonZeroU8/I8>` – `Option<NonZeroUsize/Isize>`, `Option<&T>`, `Option<&mut T>`, `Option<Box<T>>`, `Option<NonNull<T>>`, `Option<Arc<T>>`, `Option<Rc<T>>` | sized as inner type — no extra discriminant byte |
| Go | All primitives, `string` (2 words), `[]T` slices (3 words), `map[K]V` (1 word), `chan T` (1 word), `error`/`interface{}`/`any` (2 words), `complex128`, locally-declared named interfaces; types of imported packages of the module or the standard library, read from their source | generic types of other packages flagged as uncertain |
| Go stdlib | `sync.Mutex` (8B), `sync.RWMutex` (24B), `sync.Once` (12B), `sync.WaitGroup` (12B), `sync.Map` (48B with Go 1.24+, 32B before — follows the installed toolchain), `time.Time` (24B), `atomic.Bool`/`Int32`/`Uint32` (4B), `atomic.Int64`/`Uint64` (8B), `atomic.Value` (16B), `strings.Builder` (32B), `bytes.Buffer` (40B) | stable under Go 1 compatibility guarantee, except `sync.Map`; used only when the file imports the standard package of that name (`internal/runtime/atomic.Bool` is 1B, `sync/atomic.Bool` 4B) |
| Zig | All standard integer/float types, C interop types (`c_int`, `c_uint`, `c_long`, etc.), arbitrary-width integers (`u1`–`u65535`, `i1`–`i65535`) | in `packed struct`, arbitrary-width fields occupy exact bits; total = `ceil(bits/8)`; in normal structs, `ceil(N/8)` bytes aligned to next power-of-two; comptime-only field types (`type`, `anytype`, `comptime_int`, `comptime_float`) are flagged as uncertain; regular Zig structs (not `extern`/`packed`) have implementation-defined field order — findings are downgraded one severity level (like Rust `repr(Rust)`) |

### What source analysis skips (instead of showing wrong data)

| Case | Action | Accurate alternative |
|---|---|---|
| C/C++ structs with bit-field members | Parsed — bits placed per the target ABI and grouped into synthetic fields (`[a:3\|b:5]`) | Binary (DWARF) analysis for compiler-accurate bit offsets |
| C++ templates, Rust/Go generic structs, Zig `fn(comptime T: type) type` with no concrete use in the analyzed files | Skipped — note printed to stderr. Each concrete use (`Foo<int>`, `Pair[int, string]`, `List(u8)`) found in any analyzed file is analyzed as its own struct | List the arguments under `[instantiate]` in `.padlock.toml`, add a `// padlock:instantiate(Args...)` comment (C++), or use binary analysis |
| Rust structs with only lifetime parameters (`struct Cache<'a>`) | Analyzed normally — lifetime params do not affect layout | — |
| Forward-declared / incomplete structs | Skipped | Binary analysis |

### Analyzing specific C++ template instantiations

A bare `template<typename T> struct Foo { T x; };` can't be sized — `T`'s size is unknown until it's instantiated. padlock analyzes every instantiation it sees used in the analyzed files, plus those listed under `[instantiate]` in `.padlock.toml`. To name instantiations next to the template itself, for a known, fixed set (the common case for perf-sensitive template value types: SIMD/math vector types, intrusive container nodes, fixed-point types), name the concrete type arguments directly above the template with a `// padlock:instantiate(...)` comment — one line per desired instantiation:

```cpp
// padlock:instantiate(float)
// padlock:instantiate(double)
template<typename T>
struct Vector3 {
    T x, y, z;
};
```

produces two analyzed structs, `Vector3<float>` and `Vector3<double>`, each sized by substituting the concrete type for `T` and otherwise using padlock's normal field-layout logic — verified to match a real `g++ -g` compile bit-for-bit for ordinary aggregate types. For a multi-parameter template, list the arguments positionally: `// padlock:instantiate(int, double)` for `template<typename T, typename U>` maps `T → int, U → double`.

This substitution is textual, not a real template instantiator: it does not understand partial specializations, SFINAE/`enable_if`-conditional members, or variadic parameters. For an ordinary "struct holds some `T`s" template it's exact; for template metaprogramming where the compiler would pick a different specialization than the primary template body, it can be wrong. When in doubt, cross-check against binary analysis.

### Known remaining limitations (source analysis)

- **Rust enums** — data enums are modeled with a synthetic `__payload` field (sized to the largest variant) and a `__discriminant` field; niche-filled enums and common niche types (`Option<NonZeroXxx>`, `Option<&T>`, `Option<Box<T>>`) are sized correctly. Unusual niches may still be approximate — use `--rust-exact` or binary analysis.
- **Rust std sync types on macOS/Windows** — `Mutex`/`RwLock`/`Condvar` sizes differ per OS; on Apple and MSVC targets these fields are flagged as `uncertain`. Rust on 32-bit targets is not yet covered by the conformance suite.
- **Go types of packages outside the module and the standard library** (third-party modules) and **generic types of other packages** are not resolved from source; these fields are flagged as `uncertain` and sized as one word (an interface as two). Use `--go-exact` for certainty.
- **`repr(Rust)` reordering** — sizes follow rustc's order, but individual field offsets are an estimate (rustc may place a large-niche field first). `--rust-exact` reports rustc's own order.

---

## Crate Architecture

```
padlock-cli       — padlock binary + cargo-padlock subcommand; watch mode
├── padlock-source  — source frontend: tree-sitter (C/C++/Go/Zig), syn (Rust)
│                     explicit guard annotation: #[lock_protected_by], GUARDED_BY(), // padlock:guard=
├── padlock-dwarf   — binary frontend: DWARF via gimli+object, BTF via custom reader
├── padlock-output  — formatters: terminal, JSON, SARIF, diff
├── padlock-macros  — proc macros: #[assert_no_padding], #[assert_size(N)],
│                     #[assert_align(N)], #[assert_offsets(..)]
└── padlock-core    — IR types, analysis passes, findings, scoring

padlock-lsp        — standalone language server (lsp-server/lsp-types) for
                      editors without a bespoke integration (Neovim, Helix,
                      Zed, JetBrains via LSP4IJ, Sublime via LSP). Depends
                      only on padlock-source + padlock-core; analyzes the
                      live buffer in-process via parse_source_str.
```

See [docs/architecture.md](docs/architecture.md) for the full data-flow diagram and crate responsibilities.  
See [docs/findings.md](docs/findings.md) for detailed finding reference.  
See [docs/comparison.md](docs/comparison.md) for how padlock compares to pahole, -Wpadded, and runtime profilers.  
See [docs/publishing.md](docs/publishing.md) for crates.io publishing and GitHub Actions CI setup.  
See [docs/game-dev-ecs.md](docs/game-dev-ecs.md) for game development / DOD / ECS workflows.  
See [docs/ebpf-btf.md](docs/ebpf-btf.md) for eBPF and BTF analysis with `padlock bpf`.  
See [docs/robotics-ros2.md](docs/robotics-ros2.md) for embedded ARM and ROS 2 workflows.  
See [docs/extending.md](docs/extending.md) for adding analysis passes, frontends, output formats, and architectures.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
