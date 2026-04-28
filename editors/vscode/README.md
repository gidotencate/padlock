# padlock for VS Code

**The lint pass for struct memory layout** — finds padding waste, false sharing, and cache locality problems in C, C++, Rust, Go, and Zig, shown directly in the Problems panel.

## What it does

When you save a supported file, padlock runs in the background and populates the Problems panel with layout findings. No squiggles, no interruptions — just findings waiting when you want them.

```
WARNING  Connection: 10B wasted (41% of 24B). Run 'padlock explain' for the full layout.   connection.rs  line 4
WARNING  Connection: reordering fields saves 8B (24B → 16B). Use 'padlock: Apply fix' or the lightbulb (⚡) to reorder.   connection.rs  line 4
```

### Status bar health score

The status bar shows a live layout health score for the active file:

```
$(lock) 67 D  $(warning) 2
```

- **Score** (0–100) and **letter grade** (A–F) computed from all structs in the file, weighted by size.
- **$(warning) N** appears when there are High-severity findings; the status bar background turns yellow.
- **$(info) N** appears for Medium-only findings.
- Click the item to re-analyse the file.
- Shows a spinner `$(sync~spin) padlock` while analysis is running.

### Hover popup

Hover over any struct definition line to see a summary popup:

```
padlock — `Connection`
Score 67/100 `██████░░░░` · 24B · 10B wasted

🔴 PaddingWaste — 10B wasted (41% of 24B)
🟡 ReorderSuggestion — reorder saves 8B → 16B total
```

### Quick-fix lightbulb (CodeAction)

When a `ReorderSuggestion` diagnostic is active, the lightbulb (⚡) menu offers:

- **Reorder `StructName` fields (padlock)** — rewrites that struct in-place immediately (applied via WorkspaceEdit, so Ctrl+Z reverts it).
- **Preview reorder of `StructName` (padlock)** — opens the diff editor scoped to just that struct before applying.
- **Fix all reorder suggestions in file — preview (padlock)** — opens the diff editor showing every reordering in the file before applying.

## Requirements

padlock must be installed and on your `PATH`.

**macOS / Linux (Homebrew):**
```bash
brew tap gidotencate/padlock https://github.com/gidotencate/padlock
brew install padlock
```

**Any platform with Rust:**
```bash
cargo install padlock-cli
```

## Language support

| Language | File extensions | Run on save |
|---|---|---|
| Rust | `.rs` | ✓ |
| C | `.c`, `.h` | ✓ |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp` | ✓ |
| Go | `.go` | ✓ |
| Zig | `.zig` | ✓ |

## Commands

| Command | Description |
|---|---|
| `padlock: Analyze current file` | Run analysis on the open file |
| `padlock: Analyze workspace` | Run analysis across the entire workspace |
| `padlock: Apply fix (reorder all structs)` | Reorder all fixable structs in the current file immediately (undo-able) |
| `padlock: Preview fix (reorder all structs)` | Open a diff editor showing every reordering before applying |
| `padlock: Clear findings` | Remove all padlock diagnostics from the Problems panel |

All commands are also available from the **right-click context menu** when editing a supported file.

### Fix with preview

`padlock: Preview fix (reorder all structs)` runs the fix on a temporary copy of the file and opens the VS Code diff editor so you can review every reordering before committing. Clicking **Apply** applies the changes via WorkspaceEdit — no `.bak` file is created, and you can undo with Ctrl+Z. The same workflow is available per-struct via the lightbulb menu.

## Settings

| Setting | Default | Description |
|---|---|---|
| `padlock.runOnSave` | `true` | Analyze automatically on file save |
| `padlock.severity` | `"high"` | Minimum severity: `high`, `medium`, or `low` |
| `padlock.executable` | `"padlock"` | Path to the padlock binary |
| `padlock.extraArgs` | `[]` | Extra arguments passed to every analysis run |

### Example: show Medium and High findings, filter small structs

```json
{
  "padlock.severity": "medium",
  "padlock.executable": "/home/user/.cargo/bin/padlock",
  "padlock.extraArgs": ["--min-size", "16"]
}
```

### Example: include machine-generated files (e.g. protobuf output)

By default padlock skips files that declare themselves as machine-generated (`// Code generated`, `// @generated`, `.pb.h`/`.pb.cc`, etc.). To include them — for example when auditing generated protobuf code — add `--include-generated`:

```json
{
  "padlock.extraArgs": ["--include-generated"]
}
```

### Example: cross-architecture analysis (STM32F4 firmware)

Use `padlock.extraArgs` to set a target architecture. For Cortex-M4 firmware (32-byte cache lines, 4-byte pointers), false-sharing findings are active. For Cortex-M0/M3 (no cache), they are suppressed automatically.

```json
{
  "padlock.extraArgs": ["--target", "thumbv7em-none-eabi"]
}
```

### Example: focus on fixed-ABI types only

```json
{
  "padlock.extraArgs": ["--hide-repr-rust"]
}
```

## Severity mapping

padlock findings appear in the Problems panel with the following VS Code severity levels:

| padlock severity | VS Code level | Icon |
|---|---|---|
| High | Warning | ⚠ yellow |
| Medium | Information | ℹ blue |
| Low | Hint | subtle underline |

Layout issues are never shown as errors — they don't prevent your code from compiling.

## Understanding diagnostics

### PaddingWaste

The compiler inserts invisible padding bytes between fields to satisfy alignment requirements. These bytes are wasted memory that can push structs across cache lines.

Severity: **High** ≥ 30% wasted · **Medium** ≥ 10% · **Low** < 10%

### ReorderSuggestion

Fields can be reordered by descending alignment to eliminate most padding. padlock computes the optimal order and shows the exact byte savings. Use `padlock: Apply fix` or the lightbulb (⚡) to rewrite the file — all fixes are applied via WorkspaceEdit and land on the undo stack, so Ctrl+Z reverts them if needed.

Severity: **High** saves ≥ 8 bytes · **Medium** otherwise

### FalseSharing

Two or more fields accessed concurrently under different locks share the same cache line. Every write to one field invalidates the other core's cached copy, even though they protect independent data.

Fix: separate independently-locked groups onto their own cache lines with `#[repr(align(64))]` (or 128 bytes on Apple Silicon).

Severity: always **High**

**Evidence labels:** When a `FalseSharing` finding was derived from type-name heuristics (e.g. recognising `Mutex<T>` or `sync.Mutex`), the diagnostic message includes `(inferred from type names — add guard annotations or verify with profiling)`. When it was derived from explicit `GUARDED_BY` / `#[lock_protected_by]` / `// padlock:guard=` annotations, no label is shown — the finding is confirmed. See the annotation section below to convert inferred findings to confirmed ones.

### LocalityIssue

Hot fields (accessed concurrently or frequently) are interleaved with cold fields (set once, rarely read). This wastes cache lines and pollutes the hot-path working set.

Fix: group frequently-accessed fields at the start of the struct.

Severity: **Medium**

**Evidence labels:** When hot fields were identified by type-name heuristic, the diagnostic includes `(inferred from type names — verify with profiling)`. Explicit guard annotations remove this label.

## Annotating concurrent fields

By default padlock infers concurrency from type names (`Mutex`, `std::atomic`, `sync.Mutex`, …). For fields whose types don't reveal their guard, annotate them explicitly — this is the most accurate path to false-sharing detection.

**Rust** — field attributes:

```rust
struct HotPath {
    #[lock_protected_by = "mu_a"]
    readers: u64,
    #[lock_protected_by = "mu_b"]
    writers: u64,   // different guard, same cache line → FalseSharing High
    mu_a: Mutex<()>,
    mu_b: Mutex<()>,
}
```

Also accepted: `#[guarded_by("mu")]`, `#[protected_by = "mu"]`, `#[pt_guarded_by("mu")]`.

**C/C++** — Clang thread-safety macros:

```cpp
struct Cache {
    int64_t readers GUARDED_BY(lock_a);
    int64_t writers GUARDED_BY(lock_b);  // different guard → FalseSharing detected
    std::mutex lock_a;
    std::mutex lock_b;
};
```

Also accepted: `PT_GUARDED_BY(mu)`, `__attribute__((guarded_by(mu)))`.

**Go** — trailing line comments:

```go
type Cache struct {
    Readers int64      // padlock:guard=mu_a
    Writers int64      // padlock:guard=mu_b
    MuA     sync.Mutex
    MuB     sync.Mutex
}
```

Also accepted: `// guarded_by: mu`, `// +checklocksprotects:mu` (gVisor-style).

## How it fits with other tools

- **CI**: use the [padlock GitHub Action](https://github.com/gidotencate/padlock) to block PRs on High findings; `--fail-on-severity medium` tightens the gate
- **CLI**: `padlock summary src/` shows a project health overview (score, severity chart, worst files/structs) — useful before a refactor sprint
- **Pre-commit**: add padlock to your git hooks to catch issues before they reach the repo
- **Runtime profiling**: use padlock findings as a guide for where to focus with `perf c2c` or VTune
