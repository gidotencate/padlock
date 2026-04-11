# padlock

[![crates.io](https://img.shields.io/crates/v/padlock-cli.svg)](https://crates.io/crates/padlock-cli)
[![CI](https://github.com/gidotencate/padlock/actions/workflows/ci.yml/badge.svg)](https://github.com/gidotencate/padlock/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/padlock-cli.svg)](LICENSE)

**The lint pass for struct memory layout** — catches padding waste, false sharing, and cache locality problems at the source level, before they cost you at runtime.

Supports C, C++, Rust, Go, and Zig. Ranks findings by impact, generates reorder suggestions, flags concurrency risks. CLI-first and CI-ready.

```
$ padlock analyze src/connection.rs

Analyzed 2 structs — 10 bytes wasted across all structs

[✗] Connection (src/connection.rs:4)  24B  fields=4  holes=2  score=33
    [HIGH] Padding waste: 10B (41%) across 2 gap(s)
    [HIGH] Reorder fields to save 8B → 16B: timeout, port, is_active, is_tls
    [HIGH] False sharing: 1 cache-line conflict(s)

[✓] ConnectionOptimal (src/connection.rs:22)  16B  fields=4  score=100
    (no issues found)
```

When analyzing a directory or multiple files, structs are grouped under `── file ──` headers with per-struct line numbers:

```
$ padlock analyze src/

Analyzed 3 files, 5 structs — 26 bytes wasted across all structs

── src/connection.rs ───────────────────────────────────────

[✗] Connection :4  24B  fields=4  holes=2  score=33
    [HIGH] Padding waste: 10B (41%) across 2 gap(s)
    [HIGH] Reorder fields to save 8B → 16B: timeout, port, is_active, is_tls

── src/stats.cpp ───────────────────────────────────────────

[✗] Stats :12  96B  fields=4  score=55
    [HIGH] False sharing: 1 cache-line conflict(s)
    [MEDIUM] Locality: hot [read_mu, write_mu] interleaved with cold [read_count, write_count]
```

---

## Features

| Capability | Details |
|---|---|
| **Padding waste** | Finds gaps from poor field ordering; shows exact bytes wasted |
| **Reorder suggestions** | Computes optimal declaration order; shows byte savings |
| **False sharing** | Detects concurrent fields with different guards on the same cache line |
| **Explicit guard annotation** | `#[lock_protected_by]`, `GUARDED_BY()`, `// padlock:guard=` — no more type-name guessing |
| **Locality** | Flags hot/cold field interleaving that hurts cache utilisation |
| **Scoring** | Each struct gets a 0–100 score (100 = no issues) |
| **Multi-language** | C, C++, Rust, Go, Zig source; compiled binaries via DWARF/PDB/BTF |
| **Multi-arch** | x86-64, AArch64, Apple Silicon (128-byte lines), WASM32, RISC-V 64 |
| **CI-ready** | SARIF output, `action.yml`, exit-code gating on high-severity findings |
| **`cargo padlock`** | Cargo subcommand — builds your project then analyses the binary |
| **Compile-time assertions** | `#[padlock::assert_no_padding]` / `#[padlock::assert_size(N)]` proc macros |
| **Watch mode** | `padlock watch <path>` re-analyses on every file change |
| **Source-preserving fixes** | `padlock fix` reorders field chunks verbatim, keeping `pub`, `#[serde(...)]`, `/// doc-comments`, and guard annotations intact |
| **Project health summary** | `padlock summary` shows aggregate score, severity bar chart, worst files, and worst structs in one terminal screen |
| **Severity CI gate** | `--fail-on-severity medium\|low` exits non-zero when any finding meets or exceeds the threshold |
| **Parallel parsing** | Directory walks parse source files in parallel (rayon), with an on-disk mtime cache (`.padlock-cache/`) to skip unchanged files on repeat runs |
| **Cache-line boundaries** | `padlock explain` inserts a visual separator row each time a field crosses into a new 64-byte (or 128-byte) cache line |

---

## Build

Requires a Rust toolchain (1.75+).

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
padlock analyze mylib.pdb                 # Windows PDB
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
- `--fail-on-severity high|medium|low` — exit non-zero when any finding meets or exceeds this severity. `high` is the default CI gate (same as exit-on-high-finding behaviour); `medium` and `low` tighten the gate further.

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

---

### `padlock fix <path>… [--dry-run] [--filter PATTERN]`

Shows the reorder diff and — without `--dry-run` — rewrites the source file in-place, saving a `.bak` backup first. Accepts directories and multiple files; `--filter` limits which structs are rewritten.

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
┌──────────┬──────┬───────┬────────────────────────────────────┐
│   offset │ size │ align │ field                              │
├──────────┼──────┼───────┼────────────────────────────────────┤
│        0 │    1 │     1 │ tick: u8                           │
│        1 │    3 │     — │ <padding>                          │
│        4 │    4 │     4 │ ready: Ready                       │
│        8 │    1 │     1 │ is_shutdown: bool                  │
│        9 │   15 │     — │ <padding> (trailing)               │
└──────────┴──────┴───────┴────────────────────────────────────┘
14 bytes wasted (58%) — reorder: ready, tick, is_shutdown → 8 bytes
  ~8 KB extra per 1K instances · ~8 MB per 1M instances · ~125K extra cache lines/1M (seq. scan)
```

The impact line uses SI scaling: `savings × 1 000 ≈ KB`, `savings × 1 000 000 ≈ MB`. Cache-line estimates assume a sequential scan (64-byte lines). If the reorder also reduces the number of cache lines the struct spans per instance, an extra note is shown.

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

Fix: pad each independently-locked group to its own cache line.

Severity: always **High**

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

| Language | Source Analysis | Binary (DWARF/BTF) |
|---|---|---|
| C | ✓ | ✓ |
| C++ | ✓ | ✓ |
| Rust | ✓ | ✓ |
| Go | ✓ | ✓ |
| Zig | ✓ | via DWARF |
| eBPF (BTF) | — | ✓ (`.BTF` ELF section) |

**Notes on source analysis:**
- Source analysis is approximate — no compiler is invoked; field sizes come from a built-in type table.
- C++ `alignas(N)` field and struct-level annotations are extracted from source and applied to field alignment and struct trailing padding. For the most precise layout (e.g. complex template instantiations), binary (DWARF) analysis remains the authoritative path.

### Rust repr support

Rust's memory layout depends on which `repr` is in effect. padlock handles each case differently:

| repr | Layout guarantee | padlock accuracy | Notes |
|---|---|---|---|
| `repr(Rust)` (default) | None — compiler may reorder | Approximate | Analyzes declaration order; use for finding issues to fix, not ABI verification |
| `repr(C)` | C-compatible, declaration order | **Accurate** | Full analysis; best candidate for padding fixes |
| `repr(packed)` / `repr(packed(n))` | No padding, fields may be unaligned | Accurate for waste | Reorder suggestions suppressed — packing is intentional; note that unaligned field references can cause UB |
| `repr(align(n))` | Minimum alignment forced | Partial | Source frontend infers standard field sizes; struct-level forced alignment not modeled — use binary analysis |
| `repr(transparent)` | Same as inner field | Accurate | Single-field wrapper; padding findings correctly suppressed |
| `repr(u*)` / `repr(i*)` | Enum discriminant size | Approximate | Applies to enums; padlock models the discriminant size and (for data enums) a synthetic payload field; exact niched layouts are not modeled |

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
| `riscv64` | 8 bytes | 64 bytes | RISC-V 64-bit |

The architecture is auto-detected from the host when analyzing source files. For binary analysis it is read from the binary's ELF/Mach-O/PE header.

---

## Real-World Findings: tokio 1.51

Running padlock against the tokio async runtime source reveals real layout issues in production code:

```
$ padlock analyze ~/.cargo/registry/src/.../tokio-1.51.1/src --sort-by waste --min-size 16
Analyzed 373 files, 273 structs — 348 bytes wasted across all structs
```

### `ReadyEvent` — 58% padding waste

```rust
// tokio/src/runtime/io/driver.rs  (as written)
pub(crate) struct ReadyEvent {
    pub(super) tick:        u8,     // offset 0,  1 byte
    //                              // 3 bytes padding
    pub(crate) ready:       Ready,  // offset 4,  4 bytes
    pub(super) is_shutdown: bool,   // offset 8,  1 byte
    //                              // 15 bytes trailing padding
}                                   // total: 24 bytes, 14 wasted (58%)
```

padlock suggests reordering to eliminate the waste:

```
[HIGH] Reorder fields to save 8B → 16B: ready, tick, is_shutdown
```

```rust
// After reorder: 16 bytes, 6 bytes wasted → 37% (unavoidable trailing alignment)
// With explicit packing possible to 6 bytes if repr(packed) is appropriate
pub(crate) struct ReadyEvent {
    pub(crate) ready:       Ready,  // offset 0, 4 bytes
    pub(super) tick:        u8,     // offset 4, 1 byte
    pub(super) is_shutdown: bool,   // offset 5, 1 byte
}                                   // total: 8 bytes (no padding at all)
```

### `DirBuilder` — 44% padding waste

```rust
// tokio/src/fs/dir_builder.rs  (as written)
pub struct DirBuilder {
    recursive: bool,        // offset 0,  1 byte
    //                      // 3 bytes padding
    mode: Option<u32>,      // offset 4,  8 bytes
}                           // total: 16 bytes, 7 wasted (44%)
```

```
[HIGH] Padding waste: 7B (44%) across 1 gap(s)
[HIGH] Reorder fields to save 4B → 12B: mode, recursive
```

### `Builder` (runtime builder) — 12% waste, 16 bytes recoverable

The runtime `Builder` struct (200 bytes, 27 fields) has 23 bytes of padding spread across 4 gaps — recoverable to 184 bytes by reordering, freeing a full cache line worth of space that every `tokio::runtime::Builder::new_multi_thread()` call allocates on the stack.

> **Note on repr:** `ReadyEvent` and `DirBuilder` are plain `repr(Rust)` structs. The Rust compiler *may* reorder their fields and eliminate the waste automatically at compile time — but it is not required to, and the declared order is what you see in the source, what code reviewers read, and what controls the layout if the struct is ever given `repr(C)`. padlock surfaces these issues so you can fix them intentionally rather than depending on compiler luck.

### `WorkerMetrics` — `repr(align(128))` done right

Tokio's worker metrics use `#[repr(align(128))]` to prevent false sharing across scheduler threads:

```rust
#[repr(align(128))]
pub(crate) struct WorkerMetrics {
    pub(crate) busy_duration_total: MetricAtomicU64,
    pub(crate) queue_depth:         MetricAtomicUsize,
    thread_id:                      Mutex<Option<ThreadId>>,
    pub(crate) park_count:          MetricAtomicU64,
    // ...
}
```

padlock correctly identifies this as a false-sharing concern at the source level (different atomic fields without guard separation), while the `repr(align(128))` at the struct level ensures each `WorkerMetrics` instance is on its own cache line at runtime. This is the recommended pattern: use `repr(align(64))` (or 128 on Apple/ARM big cores) on the struct rather than manual `[u8; N]` padding arrays.

These are not bugs — they are the kind of low-level layout details that accumulate invisibly over time. padlock surfaces them before they become performance regressions.

---

## Compile-Time Assertions

`padlock-macros` provides proc-attribute macros that turn layout violations into **compile errors**. Add it to `Cargo.toml`:

```toml
[dependencies]
padlock-macros = "0.1"
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
      - uses: gidotencate/padlock@v1
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

Structs containing bit-field members (`int flags : 3`) are **skipped** in source analysis. Bit-field packing is entirely compiler-controlled — which bits share a storage unit, and how padding works between them, cannot be correctly modelled without invoking a compiler. Showing wrong layout data is worse than showing nothing.

Use binary analysis (`padlock analyze target/debug/myapp`) for accurate layout data on structs that contain bit fields; the compiler encodes the real offsets and sizes in DWARF. In DWARF binary mode, bit-field members (those carrying `DW_AT_bit_size`) are also silently skipped — the remaining byte-aligned fields in the struct are still extracted and analyzed.

---

## Scope and Limitations

padlock is a **layout waste detector and optimizer**. It focuses on padding, field ordering, false sharing, and cache locality. It is not:

- A full compiler — type sizes are approximated from a built-in type table for source analysis. Use binary (DWARF) analysis for compiler-accurate results.
- A profiler — it cannot measure actual cache miss rates.

### What source analysis gets right

| Language | Accurate | Notes |
|---|---|---|
| C / C++ | Normal structs, unions, pointer fields, all primitive types, `std::atomic<T>` | |
| C++ | vtable pointer injection for `virtual` classes, single/multiple inheritance base slots | base-class sizes are approximate until nested-struct resolution |
| Rust | All primitive types, `repr(C)`, `repr(packed)`, `repr(transparent)` | |
| Rust stdlib | `Vec`, `String`, `Box`, `Arc`, `Rc`, all `AtomicXxx`, `PhantomData`, `Duration`, channels, smart pointers | size is independent of type parameter `T` |
| Go | All primitives, `string` (2 words), `[]T` slices (3 words), `map[K]V` (1 word), `chan T` (1 word), `error`/`interface{}`/`any` (2 words) | |
| C / C++ | `__attribute__((packed))` structs and classes | no inter-field padding inserted; struct alignment set to 1 |

### What source analysis skips (instead of showing wrong data)

| Case | Action | Accurate alternative |
|---|---|---|
| C/C++ structs with bit-field members | Skipped | Binary (DWARF) analysis |
| Rust generic struct definitions (`struct Foo<T>`) | Skipped | Binary analysis; or analyse concrete monomorphizations |
| Forward-declared / incomplete structs | Skipped | Binary analysis |

### Known remaining limitations (source analysis)

- **C++ `alignas(N)` / `__attribute__((aligned(N)))` on individual fields** — field alignment overrides are not modeled; use binary analysis for accuracy.
- **C++ templates** — unknown type parameters fall through to pointer-size; the struct is analyzed but may show approximate sizes.
- **Rust enums with data variants** (`enum Foo { A(u64), B { x: u32 } }`) — not modeled; only plain structs are analyzed.
- **Go named interface fields** (`io.Reader`, custom interfaces) — reported as 2 words (like `interface{}`/`any`), which is correct for the runtime representation. Named interfaces implemented by pointer types are always 2 words regardless.
- **`#pragma pack(N)` on C/C++ structs** — only `__attribute__((packed))` (GCC/Clang style) is detected from source; MSVC-style `#pragma pack` is not. Use binary analysis for accuracy on MSVC-compiled code.
- **Rust const-expression padding** (`[u8; 64 - size_of::<Mutex<u64>>()]`) — the expression is not evaluated; the field gets pointer-size as a default.
- **`repr(Rust)` reordering** — the compiler may reorder fields and eliminate padding automatically; padlock analyzes declaration order, which is what developers read and control.

---

## Crate Architecture

```
padlock-cli       — padlock binary + cargo-padlock subcommand; watch mode
├── padlock-source  — source frontend: tree-sitter (C/C++/Go), syn (Rust)
│                     explicit guard annotation: #[lock_protected_by], GUARDED_BY(), // padlock:guard=
├── padlock-dwarf   — binary frontend: DWARF via gimli+object, PDB via pdb
├── padlock-output  — formatters: terminal, JSON, SARIF, diff
├── padlock-macros  — proc macros: #[assert_no_padding], #[assert_size(N)]
└── padlock-core    — IR types, analysis passes, findings, scoring
```

See [docs/architecture.md](docs/architecture.md) for the full data-flow diagram and crate responsibilities.  
See [docs/findings.md](docs/findings.md) for detailed finding reference.  
See [docs/comparison.md](docs/comparison.md) for how padlock compares to pahole, -Wpadded, and runtime profilers.  
See [docs/publishing.md](docs/publishing.md) for crates.io publishing and GitHub Actions CI setup.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
