# Real-World Findings

padlock run against popular open-source projects — all findings reflect declared source layout, not compiler-optimised output.

---

## Summary

| Project | Language | Version | Structs | Wasted | Score | Notable |
|---|---|---|---|---|---|---|
| [tokio](https://tokio.rs) | Rust | 1.51.1 | 367 | 485B | 91/100 A¹ | `TraceStatus` — score 48, false sharing |
| [Redis](https://redis.io) | C | 7.0.15 | 282 | 892B | — | `multiState` — 20% waste, saves 8B |
| [Go net + database](https://pkg.go.dev) | Go | stdlib 1.22 | 607 | 1 236B | 86/100 B | `sql.DB` — false sharing, score 53 |
| Linux kernel `net/` | C | 6.x | 2 066 | 5 093B | 84/100 B | `virtio_vsock` — score 45, all 4 finding types |

¹ repr(Rust) structs are severity-downgraded (compiler may already reorder). Use `--hide-repr-rust` to focus on ABI-stable findings only. The per-struct average score is 91; the weighted project score is higher due to the majority of clean small structs.

The Go stdlib score is B (86/100) across 607 structs. 71% are clean; 12% have High findings — almost all from false sharing between atomic and mutex-protected fields rather than padding waste.

The Linux `net/` subsystem score is B (84/100) across 2 066 structs. 57% are clean; 12% have High findings — primarily false sharing in driver and protocol structs that have grown organically over many kernel versions.

---

## Rust — tokio 1.51.1

```
$ padlock analyze ~/.cargo/registry/src/.../tokio-1.51.1/src --sort-by waste --min-size 16
Analyzed 373 files, 197 structs — 480 bytes wasted across all structs
```

### `ReadyEvent` — 58% padding waste

```rust
// tokio/src/runtime/io/driver.rs:74  (as written)
pub(crate) struct ReadyEvent {
    pub(super) tick:        u8,     // offset  0, 1 byte
    //                              // 7 bytes padding
    pub(crate) ready:       Ready,  // offset  8, 8 bytes
    pub(super) is_shutdown: bool,   // offset 16, 1 byte
    //                              // 7 bytes padding (trailing)
}                                   // total: 24 bytes, 14 wasted (58%)
```

```
$ padlock explain tokio-1.51.1/src/runtime/io/driver.rs --filter ReadyEvent

ReadyEvent  24 bytes  align=8  fields=3  [repr(Rust) — compiler may reorder]
┌────────┬──────┬───────┬────┬──────────────────┐
│ offset │ size │ align │ CL │ field            │
├────────┼──────┼───────┼────┼──────────────────┤
│      0 │    1 │     1 │  0 │ tick: u8         │
│      1 │    7 │     — │  0 │ <padding>        │
│      8 │    8 │     8 │  0 │ ready: Ready     │
│     16 │    1 │     1 │  0 │ is_shutdown: bool│
│     17 │    7 │     — │  0 │ <padding>(trail.)│
└────────┴──────┴───────┴────┴──────────────────┘
14 bytes wasted (58%) — reorder: ready, is_shutdown, tick → 16 bytes
  ~8 KB extra per 1K instances · ~8 MB per 1M instances · ~125K extra cache lines/1M
```

The fix is straightforward — sort fields by descending alignment:

```
[HIGH] Reorder fields: 24B → 16B (saves 8B): ready, is_shutdown, tick
```

> **`repr(Rust)` note:** `ReadyEvent` has no `#[repr(C)]` annotation, so the Rust compiler is free to reorder fields itself. padlock flags the declared order as a potential source-level issue — if the struct ever gains `repr(C)` for FFI, or if the compiler happens not to reorder it, the waste becomes real. Use binary analysis (`padlock analyze target/debug/tokio-...`) for the compiler-accurate layout.

### `Builder` (runtime builder) — 30 bytes wasted, 24 bytes recoverable

The runtime `Builder` struct (200 bytes, 27 fields) has 30 bytes of padding across 5 gaps, recoverable to 176 bytes by reordering — freeing 24 bytes that every `Runtime::new()` call allocates on the stack:

```
[HIGH] Reorder fields: 200B → 176B (saves 24B)  (~24 MB/1M instances)
[MEDIUM] Padding waste: 30B (15%) — 4B after `worker_threads` (offset 12), and 4 more gaps
note: repr(Rust) — compiler may reorder fields; use binary analysis for actual layout
```

### `WorkerMetrics` — false sharing with `repr(align(128))`

Tokio uses `#[repr(align(128))]` on `WorkerMetrics` to prevent false sharing across scheduler threads — each instance lives on its own 128-byte cache line. padlock identifies false sharing candidates at the source level; the `repr(align(128))` is the recommended fix pattern.

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

```
[MEDIUM] Padding waste: 24B (19%) — 24B after `thread_id` (offset 32)
[HIGH]   False sharing: cache line 0: [busy_duration_total, queue_depth, thread_id]  (inferred from type names — add guard annotations or verify with profiling)
```

The struct has padding waste *within* the 128-byte block, but the inter-instance false sharing is correctly eliminated by the forced alignment.

---

## C — Redis 7.0.15

```
$ padlock analyze redis-7.0.15/src/ --sort-by waste --min-size 16
Analyzed 166 files, 282 structs — 892 bytes wasted across all structs
```

### `multiState` — 20% waste in the MULTI/EXEC transaction struct

`multiState` tracks the queued commands inside a Redis `MULTI`/`EXEC` block. Every client with an open transaction holds one:

```c
// redis-7.0.15/src/server.h:956  (as written)
typedef struct multiState {
    multiCmd *commands;       // offset  0, 8 bytes
    int       count;          // offset  8, 4 bytes
    int       cmd_flags;      // offset 12, 4 bytes
    int       cmd_inv_flags;  // offset 16, 4 bytes
    /* 4 bytes padding */
    size_t    argv_len_sums;  // offset 24, 8 bytes (pointer-sized)
    int       alloc_count;    // offset 32, 4 bytes
    /* 4 bytes padding (trailing) */
} multiState;                 // total: 40 bytes, 8 wasted (20%)
```

```
$ padlock explain redis-7.0.15/src/server.h --filter '^multiState$'

multiState  40 bytes  align=8  fields=6
┌────────┬──────┬───────┬────┬──────────────────────┐
│ offset │ size │ align │ CL │ field                │
├────────┼──────┼───────┼────┼──────────────────────┤
│      0 │    8 │     8 │  0 │ commands: multiCmd * │
│      8 │    4 │     4 │  0 │ count: int           │
│     12 │    4 │     4 │  0 │ cmd_flags: int       │
│     16 │    4 │     4 │  0 │ cmd_inv_flags: int   │
│     20 │    4 │     — │  0 │ <padding>            │
│     24 │    8 │     8 │  0 │ argv_len_sums: size_t│
│     32 │    4 │     4 │  0 │ alloc_count: int     │
│     36 │    4 │     — │  0 │ <padding> (trailing) │
└────────┴──────┴───────┴────┴──────────────────────┘
8 bytes wasted (20%) — reorder: argv_len_sums, commands, alloc_count, cmd_flags, cmd_inv_flags, count → 32 bytes
  ~8 KB extra per 1K instances · ~8 MB per 1M instances
```

Moving `argv_len_sums` (a `size_t`, pointer-aligned) to follow the pointer field eliminates both padding gaps:

```
[HIGH] Reorder fields: 40B → 32B (saves 8B): argv_len_sums, commands, alloc_count, cmd_flags, cmd_inv_flags, count
```

The fix in C is a one-line comment in the struct declaration — no code changes needed:

```c
typedef struct multiState {
    multiCmd *commands;       // offset  0, 8 bytes
    size_t    argv_len_sums;  // offset  8, 8 bytes  ← moved up
    int       count;          // offset 16, 4 bytes
    int       cmd_flags;      // offset 20, 4 bytes
    int       cmd_inv_flags;  // offset 24, 4 bytes
    int       alloc_count;    // offset 28, 4 bytes
} multiState;                 // total: 32 bytes, 0 wasted
```

### `redisCommandArg` — 15% waste, spans two cache lines

Command metadata structs are allocated once at startup and cached for every command lookup:

```
[HIGH] Reorder fields: 80B → 72B (saves 8B)  (~8 MB/1M instances)
  Spans 2 cache lines; optimal spans 1
```

The current layout crosses a 64-byte cache line boundary at offset 64. After reordering, the entire struct fits within a single cache line — every command-parsing lookup that touches this struct currently pulls two cache lines from memory.

### `redisServer` — the kitchen sink

The main `redisServer` struct (2 736 bytes, 397 fields) has 128 bytes of padding across 32 gaps and 42 false-sharing conflicts. It is intentionally a global singleton — the padding and false sharing primarily affect startup performance and the scheduler thread. padlock highlights it for completeness rather than as a target for optimization:

```
[LOW]  Padding waste: 128B (5%) — 4B after `activerehashing` (offset 124), and 31 more gaps
[HIGH] Reorder fields: 2736B → 2608B (saves 128B)
[HIGH] False sharing: cache line 0: [hz, dynamic_hz, aof_use_rdb_preamble, ...], and 41 more conflicts  (inferred from type names — add guard annotations or verify with profiling)
```

---

## Go — standard library 1.22

```
$ padlock summary /usr/lib/go-1.22/src/net/ /usr/lib/go-1.22/src/database/ /usr/lib/go-1.22/src/sync/
Score   86 / 100   B    607 structs · 162 files · 1 236B wasted
🔴 High   71 (12%)  🟡 Medium  75 (12%)  🔵 Low  30 (5%)  ✅ Clean  431 (71%)
```

Go's layout is deterministic — unlike `repr(Rust)` Rust, the compiler does not reorder struct fields. padlock's analysis of Go source is authoritative without caveats.

### `database/sql.DB` — false sharing between atomics and mutex-protected fields

Every Go program using a SQL database holds a `*sql.DB`. The struct intermingles atomic counters (accessed lock-free from any goroutine) with mutex-protected connection pool fields on the same cache line:

```go
// database/sql/sql.go:488  (as written)
type DB struct {
    waitDuration atomic.Int64   // offset   0 — hot, lock-free ← cache line 0
    connector    driver.Connector // offset  8
    numClosed    atomic.Uint64  // offset  16 — hot, lock-free ← same cache line!
    mu           sync.Mutex     // offset  24 — protects everything below
    freeConn     []*driverConn  // offset  32
    connRequests map[uint64]...  // offset  56
    // ... 15 more fields
}
```

```
$ padlock analyze /usr/lib/go-1.22/src/database/sql/sql.go --filter '^DB$'

[✗] DB  184B  fields=21  score=53
    [LOW]    Padding waste: 7B (4%) — 7B after `numClosed` (offset 16)
    [HIGH]   False sharing: cache line 0: [waitDuration, numClosed, mu]  (inferred from type names — add guard annotations or verify with profiling)
    [MEDIUM] Locality: hot [waitDuration, numClosed, mu] interleaved with cold [connector, freeConn, ...]
```

`waitDuration` and `numClosed` are atomic counters updated on every query — any goroutine that modifies them invalidates the cache line that also holds `connector` and the start of `mu`, which is locked on every connection acquire. Under concurrent load this generates unnecessary cache-coherence traffic across threads.

The fix is the standard Go pattern — separate the hot atomics onto their own cache line with a padding field or `atomic.Pad`:

```go
type DB struct {
    // Hot atomics — keep on their own cache line
    waitDuration atomic.Int64
    numClosed    atomic.Uint64
    _pad         [48]byte       // or: use golang.org/x/sys/cpu.CacheLinePad

    // Mutex-protected connection pool
    mu           sync.Mutex
    connector    driver.Connector
    freeConn     []*driverConn
    // ...
}
```

### `net/http.Transport` — false sharing across 4 cache lines

`http.Transport` is the client-side connection pool that every `http.Client` uses. It shows the most complex false-sharing pattern in the standard library: 4 cache-line conflicts, 16 bytes of padding waste, and a 240-byte struct that padlock can shrink to 224 bytes:

```
[✗] Transport  240B  fields=31  score=51
    [LOW]    Padding waste: 19B (8%) — 7B after `closeIdle` (offset 8), and 2 more gaps
    [HIGH]   Reorder fields: 240B → 224B (saves 16B)  (~16 MB/1M instances)
    [HIGH]   False sharing: cache line 0: [idleMu, reqMu]; cache line 1: [altMu, connsPerHostMu]  (inferred from type names — add guard annotations or verify with profiling)
    [MEDIUM] Locality: hot [idleMu, reqMu, altMu, connsPerHostMu] interleaved with cold [closeIdle, idleConn, ...]
```

The layout (abridged):

```
 cache line 0:  idleMu(8), closeIdle(1), [7 pad], idleConn(8), idleConnWait(8), idleLRU(16), reqMu(8), reqCanceler(8)
 cache line 1:  altMu(8), altProto(8), connsPerHostMu(8), connsPerHost(8), ..., DisableKeepAlives(1), [6 pad]
 cache line 2:  MaxIdleConns(8), MaxIdleConnsPerHost(8), ..., ProxyConnectHeader(8)
 cache line 3:  MaxResponseHeaderBytes(8), ..., ForceAttemptHTTP2(1), [6 pad]
```

`idleMu`, `reqMu`, `altMu`, and `connsPerHostMu` are all mutexes that are locked on every connection request — but they live across two different cache lines (0 and 1), so locking any one of them can invalidate the line that holds another.

---

## C++ — what padlock adds over C

C++ analysis extends C struct analysis with three additional capabilities:

**vtable pointer injection**: any class with `virtual` methods gets an implicit `vptr` (pointer-sized) inserted at offset 0. Without this, all size estimates for derived classes would be wrong.

**Inheritance base slots**: single and multiple inheritance inserts base-class sub-objects before derived fields. padlock models each base as an opaque `__base_N` field sized to the base class.

**Standard library type sizes** (new in v0.8.4): the C++ frontend now resolves sizes for `std::string` (32B on libstdc++), `std::vector<T>` (24B), `std::shared_ptr<T>` (16B), `std::optional<T>` (recursively computed), and 15 other stdlib types. Previously these fell back to pointer-size (8B), understating struct sizes and missing padding opportunities.

A struct like this:

```cpp
struct Session {
    bool            active;       // 1 byte
    std::string     name;         // 32 bytes (libstdc++) — was misreported as 8B
    int             port;         // 4 bytes
    std::vector<int> commands;    // 24 bytes — was misreported as 8B
};
```

Now correctly reports 72 bytes with 7 bytes of padding, rather than the former 24-byte underestimate. For exact sizes on a specific compiler/platform, binary (DWARF) analysis remains authoritative.


---

## C — Linux kernel `net/` subsystem (6.x)

```
$ padlock summary ~/linux/net
Score   84 / 100   B    2066 structs · 676 files · 5093B wasted

  🔴 High     ██░░░░░░░░░░░░░░░░░░   243  (12%)
  🟡 Medium   ████░░░░░░░░░░░░░░░░   471  (23%)
  🔵 Low      █░░░░░░░░░░░░░░░░░░░   176  (9%)
  ✅ Clean    ███████████░░░░░░░░░  1176  (57%)
```

### `virtio_vsock` — score 45, all four finding types

The vsock virtio driver struct accumulates all four padlock finding types in one place — a textbook example of layout debt in a struct that has grown over many kernel versions:

```
$ padlock analyze net/vmw_vsock/virtio_transport.c

[✗] virtio_vsock  136B  fields=19  holes=4  score=45
    [MEDIUM] Padding waste: 24B (18%) across 4 gaps
    [HIGH]   Reorder fields: 136B → 112B (saves 24B)  (~24 MB/1M instances)
    [HIGH]   False sharing: 2 cache-line conflict(s)
    [MEDIUM] Locality: hot [tx_lock, queued_replies, rx_lock, event_lock]
             interleaved with cold [vdev, vqs, tx_work, rx_work, ...]
```

The false sharing findings are `(inferred)` — padlock recognised `tx_lock`, `rx_lock`, and `event_lock` as mutex-type fields from their names. Adding `GUARDED_BY()` annotations would promote these to confirmed findings.

### `rcu_node` — score 52, 5 false-sharing conflicts

The RCU node struct in `kernel/rcu/tree.h` is one of the most concurrency-sensitive structs in the kernel. It has 39 fields, 5 false-sharing cache-line conflicts, and 16 bytes of padding:

```
$ padlock analyze kernel/rcu/tree.h --filter rcu_node

[✗] rcu_node  296B  fields=39  holes=3  score=52
    [LOW]  Padding waste: 16B (5%) across 3 gaps
    [HIGH] Reorder fields: 296B → 280B (saves 16B)
    [HIGH] False sharing: 5 cache-line conflict(s)
    [MEDIUM] Locality: hot [__private, boost_mtx, kthread_mutex, exp_poll_lock]
             interleaved with cold [gp_seq, qsmask, completedqs, ...]
```

The 5 false-sharing conflicts reflect the struct's role as the central node in RCU's scalable tree — multiple independent locks (`boost_mtx`, `kthread_mutex`, `exp_poll_lock`) protect different aspects of the same node and currently share cache lines. These are confirmed architectural trade-offs, not accidental bugs, but padlock surfaces them for review.
