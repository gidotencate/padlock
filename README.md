# padlock

Struct memory layout analyzer for C, C++, Rust, and Go. Finds padding waste, false sharing, and cache locality problems — ranks findings by impact, generates reorder suggestions, and flags concurrency risks. CLI-first and CI-ready.

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
| **Multi-language** | C, C++, Rust, Go source; compiled binaries via DWARF/PDB |
| **Multi-arch** | x86-64, AArch64, Apple Silicon (128-byte lines), WASM32, RISC-V 64 |
| **CI-ready** | SARIF output, `action.yml`, exit-code gating on high-severity findings |
| **`cargo padlock`** | Cargo subcommand — builds your project then analyses the binary |
| **Compile-time assertions** | `#[padlock::assert_no_padding]` / `#[padlock::assert_size(N)]` proc macros |
| **Watch mode** | `padlock watch <path>` re-analyses on every file change |

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

# Show field-reordering diff
padlock diff src/

# Show what fix would do (without writing)
padlock fix src/ --dry-run

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
- `--filter <PATTERN>` — include only structs whose names match this regex
- `--exclude <PATTERN>` — exclude structs whose names match this regex
- `--min-holes <N>` — only structs with ≥ N padding gaps
- `--min-size <N>` — only structs with total size ≥ N bytes
- `--packable` — only structs that have a reorder suggestion
- `--sort-by score|size|waste|name` — sort order (default: score, worst first)

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

| Language | Source Analysis | Binary (DWARF) |
|---|---|---|
| C | ✓ | ✓ |
| C++ | ✓ | ✓ |
| Rust | ✓ | ✓ |
| Go | ✓ | ✓ |

**Notes on source analysis:**
- Source analysis is approximate — no compiler is invoked; field sizes come from a built-in type table.
- Rust `#[repr(C)]` and `#[repr(packed)]` are detected and respected.
- C++ `alignas(N)` field annotations are currently ignored by the source frontend (use binary analysis for accurate C++ layout with alignment overrides).
- Plain Rust structs (`repr(Rust)`) may be reordered by the compiler; padlock analyzes declaration order, which is what you control.

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

Bit-field fields (`int flags : 3`) are detected and included as their full storage-unit size (e.g. 4 bytes for `int:3`). Layout is approximate for structs where multiple consecutive bit fields pack into a single storage unit.

---

## Scope and Limitations

padlock is a **layout waste detector and optimizer**. It focuses on padding, field ordering, false sharing, and cache locality. It is not:

- A full compiler — type sizes are approximated from a built-in table for source analysis.
- A profiler — it cannot measure actual cache miss rates.

**Known limitations:**
- C++ `alignas` and `__attribute__((aligned))` on individual fields are not modeled in source analysis (use DWARF analysis for accurate alignment override handling).
- Multiple consecutive bit fields that pack into one storage unit are each counted as a full storage unit, slightly overestimating struct size.
- The "padded" variants in false-sharing samples may still be flagged because const-expression padding (e.g. `[u8; 64 - size_of::<Mutex<u64>>()]`) is not evaluated by the source frontend.
- Plain Rust structs (`repr(Rust)`) may be reordered by the compiler; padlock analyzes declaration order, which is what the developer controls.

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
