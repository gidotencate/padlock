# padlock Findings Reference

padlock emits four finding types. Each finding has a **severity** (High / Medium / Low) that reflects its likely impact on real-world performance.

---

## PaddingWaste

**What it means**

The compiler inserts invisible padding bytes between fields (and after the last field) to keep each field at its required alignment offset. Those bytes are wasted memory: they increase struct size, push objects across cache lines, and increase memory bandwidth for bulk copies.

**How it is detected**

padlock simulates the platform's struct layout rules (C ABI / `repr(C)` for C and C++ source; `repr(C)` or declaration-order simulation for other languages) and computes the gap between every consecutive pair of fields.

> **Note for Rust:** Structs and enums without an explicit `#[repr(C)]`, `#[repr(packed)]`, or `#[repr(transparent)]` attribute use `repr(Rust)` — the compiler is free to reorder fields at compile time. padlock analyses the declared field order and appends a caveat note to the output. The findings remain useful for identifying issues to fix intentionally (especially before adding `#[repr(C)]` for FFI). For compiler-accurate layout, run `padlock analyze` on the compiled binary.

```
struct Connection {      offset  size  align
    is_active: bool       0      1     1
    // 7 bytes padding    1      7     —
    timeout:   f64        8      8     8
    is_tls:    bool       16     1     1
    // 3 bytes padding    17     3     —
    port:      i32        20     4     4
}                         total: 24 bytes, 10 bytes wasted (41.7%)
```

**Severity thresholds**

| Severity | Condition |
|---|---|
| High | ≥ 30% of total struct size is padding |
| Medium | ≥ 10% |
| Low | < 10% |

**How to fix**

Sort fields by descending alignment (largest alignment first). padlock always pairs this finding with a `ReorderSuggestion` that shows the optimal order.

---

## ReorderSuggestion

**What it means**

Reordering fields by descending alignment eliminates most padding gaps without changing the semantics of the struct. This finding includes the recommended field order and the exact byte savings.

**How it is detected**

padlock computes the optimal field order by sorting fields: highest alignment first, breaking ties by size descending, then name ascending. It then simulates the layout with that order and subtracts from the current total size.

```
// Before: 24 bytes
struct Connection { is_active, timeout, is_tls, port }

// After: 16 bytes — saves 8 bytes
struct Connection { timeout, port, is_active, is_tls }
```

The terminal output shows the before and after sizes explicitly:

```
[HIGH] Reorder fields: 24B → 16B (saves 8B): timeout, port, is_active, is_tls  (~8 MB/1M instances)
```

**Severity thresholds**

| Severity | Condition |
|---|---|
| High | Savings ≥ 8 bytes |
| Medium | Savings < 8 bytes |

**Interactions**

- Always co-occurs with `PaddingWaste` when reordering would help.
- Not emitted for `#[repr(packed)]` / `__attribute__((packed))` structs where reordering cannot reduce further.
- Not emitted when the struct is already in optimal order.

---

## FalseSharing

**What it means**

False sharing occurs when two or more fields that are accessed **independently** under **different** locks (or independently as atomic values) occupy the same 64-byte cache line (128 bytes on Apple Silicon). A write to one field invalidates the entire cache line on all other cores, even though they are not logically related — causing unnecessary cache coherence traffic and cache misses.

> **Architecture note**: on targets without a hardware cache (`cache_line_size = 0`, e.g. Cortex-M0/M3, AVR) this finding is automatically suppressed — cache-line conflicts are meaningless without a cache. Use `--target thumbv6m-none-eabi` (or another no-cache triple) to enable this behaviour.

**How it is detected**

For each struct padlock:

1. Identifies fields with a `Concurrent` access pattern. A field becomes `Concurrent` in one of two ways:
   - **Explicit annotation** — the developer annotates the field with a guard name (see below). This is the most reliable path.
   - **Heuristic type-name inference** — `concurrency.rs` recognises known synchronisation types (`Mutex<T>`, `std::mutex`, `sync.Mutex`, `AtomicU64`, `std::atomic<T>`, etc.) and assigns `Concurrent` with the field's own name as the guard.
2. Groups `Concurrent` fields by cache-line bucket (`field.offset / cache_line_size`).
3. If a bucket contains two or more concurrent fields with **different guard identifiers**, a `FalseSharing` finding is emitted.

**Explicit guard annotation**

When the field type does not reveal its synchronisation role, annotate it directly:

*Rust:*
```rust
struct Cache {
    #[lock_protected_by = "mu_a"]   // or: #[guarded_by("mu_a")]
    readers: u64,
    #[lock_protected_by = "mu_b"]
    writers: u64,
}
```

*C/C++ (Clang thread-safety analysis):*
```cpp
struct Cache {
    int64_t readers GUARDED_BY(mu_a);   // or __attribute__((guarded_by(mu_a)))
    int64_t writers GUARDED_BY(mu_b);
};
```

*Go:*
```go
type Cache struct {
    Readers int64 // padlock:guard=mu_a
    Writers int64 // padlock:guard=mu_b
}
```

Explicit annotations take precedence over type-name inference.

**Example (C++)**

```cpp
struct Stats {                  offset  size  cache line
    std::mutex  read_mu;         0      40    0   ← Concurrent("read_mu")
    int64_t     read_count;      40     8     0
    std::mutex  write_mu;        48     40    0   ← Concurrent("write_mu") — SAME LINE!
    int64_t     write_count;     88     8     1
};
// read_mu and write_mu are on cache line 0 with different guards → HIGH
```

**Example (Go)**

```go
type SharedCounters struct {
    ReaderMu    sync.Mutex   // offset 0,  Concurrent("ReaderMu")
    ReaderCount int64        // offset 8
    WriterMu    sync.Mutex   // offset 16, Concurrent("WriterMu") — same cache line!
    WriterCount int64        // offset 24
}
```

**Confirmed vs. inferred findings**

padlock distinguishes between two confidence levels:

- **Confirmed** — the field carries an explicit guard annotation (`GUARDED_BY`, `#[lock_protected_by]`, `// padlock:guard=`). The guard identifier is known exactly. Output shows the finding without qualification.
- **Inferred** — the field's type name was recognised as a synchronisation primitive by the heuristic pass (e.g. `Mutex<T>`, `sync.Mutex`, `AtomicU64`). The guard is assumed to be the field itself. Output appends:
  ```
  (inferred from type names — add guard annotations or verify with profiling)
  ```

Inferred findings are **still actionable** — they identify real false-sharing candidates — but they should be verified with profiling or by adding explicit guard annotations before committing to a fix.

To convert an inferred finding to a confirmed one, annotate the fields explicitly (see the annotation examples above). Once all concurrent fields in a cache-line conflict have explicit annotations, the `(inferred)` label disappears.

**Severity**

Always **High**. False sharing is a confirmed concurrency performance hazard.

**How to fix**

Pad each independently-locked group to its own cache line:

```rust
// Rust: explicit padding
struct SharedCountersPadded {
    reader_mu:    Mutex<u64>,
    _pad1:        [u8; 64 - size_of::<Mutex<u64>>()],
    writer_mu:    Mutex<u64>,
    _pad2:        [u8; 64 - size_of::<Mutex<u64>>()],
}

// C++: alignas on each group
struct alignas(64) StatsPadded {
    std::mutex  read_mu;
    int64_t     read_count;
    alignas(64)
    std::mutex  write_mu;
    int64_t     write_count;
};
```

**Limitations**

- Source analysis cannot evaluate compile-time constant expressions (e.g. `[u8; 64 - size_of::<T>()]`), so padded structs may still be flagged if the padding field is opaque.
- C++ `alignas` is extracted and applied in source analysis (field-level and struct-level). For the most precise layout with complex templates or dependent `alignas` expressions, binary (DWARF) analysis remains the authoritative path.

---

## LocalityIssue

**What it means**

A cache line loaded to access a hot field (concurrently modified or frequently read) also loads cold fields (rarely accessed) as unavoidable baggage. This wastes cache capacity and increases the miss rate on the hot path.

**How it is detected**

padlock looks for structs where `Concurrent`-access fields and `Unknown`-access (presumed cold or infrequent) fields are interleaved across different cache lines. Specifically: if the set of concurrent fields does not form a contiguous prefix or suffix of the struct, the hot/cold fields are interleaved.

**Example**

```c
struct Worker {
    pthread_mutex_t mu;        // hot (Concurrent) — cache line 0
    int             id;        // cold (Unknown)   — same cache line 0
    int             tasks_done; // hot (Unknown but frequently written) — line 0
    char            name[64];  // cold — lines 0–1
};
```

**Confirmed vs. inferred findings**

`LocalityIssue` follows the same evidence labeling as `FalseSharing`. When all hot fields were identified by type-name heuristic rather than explicit annotation, the output appends:

```
(inferred from type names — verify with profiling)
```

Annotating the hot fields explicitly (see the `FalseSharing` annotation examples) removes the `(inferred)` label and makes the finding confirmed.

**Severity**

**Medium** — interleaving is harmful but the impact is workload-dependent.

**How to fix**

Group hot fields together at the front of the struct, or separate hot and cold field groups with explicit cache-line padding.

> **Architecture note**: on targets without a hardware cache (`cache_line_size = 0`, e.g. Cortex-M0/M3, AVR) this finding is also suppressed along with `FalseSharing` — locality is only meaningful when cache pressure exists.

---

## Per-Finding Suppression

When a finding is intentional or impossible to fix in context, you can suppress specific finding types for a struct without hiding the struct from analysis entirely.

**Syntax** — place a comment on the line immediately before the struct/type declaration:

```c
// padlock: ignore[ReorderSuggestion]
struct NetworkPacket { uint8_t type; uint64_t timestamp; };
```

Multiple kinds can be suppressed in one directive, comma-separated:

```c
// padlock: ignore[PaddingWaste, ReorderSuggestion]
struct WireFormat { uint8_t version; uint64_t id; };
```

The directive works the same way in all supported languages:

*Rust:*
```rust
// padlock: ignore[FalseSharing]
struct Counters {
    reads:  AtomicU64,
    writes: AtomicU64,
}
```

*Go:*
```go
// padlock: ignore[LocalityIssue]
type DB struct {
    mu    sync.Mutex
    cache map[string][]byte
    name  string
}
```

*Zig:*
```zig
// padlock: ignore[ReorderSuggestion]
const Packet = struct {
    version: u8,
    id:      u64,
};
```

**Valid kind names** (case-sensitive):

| Kind | Suppresses |
|---|---|
| `PaddingWaste` | Padding waste finding |
| `ReorderSuggestion` | Field reorder suggestion |
| `FalseSharing` | False sharing finding |
| `LocalityIssue` | Hot/cold locality finding |

**Difference from `// padlock:ignore`**

`// padlock:ignore` (no brackets) removes the struct from analysis entirely — it does not appear in output or reports. `// padlock: ignore[Kind]` keeps the struct in the report but omits the named finding kind.

---

## Score

Each struct receives an overall score from 0 to 100:

| Score | Meaning |
|---|---|
| 100 | No findings at all |
| 80–99 | Only Low-severity findings |
| 50–79 | One or more Medium findings |
| 0–49 | One or more High findings |

The score is computed by the `scorer` analysis pass in `padlock-core`. High findings each deduct a larger penalty than Medium or Low findings. False sharing incurs the largest single deduction because it is a confirmed runtime hazard; padding waste deductions scale with the waste percentage.
