# padlock for eBPF — BTF-Aware Struct Analysis

eBPF programs use C structs for BPF map values, perf event payloads, and ring buffer entries that transit the kernel at high frequency. A `struct event` written millions of times per second per CPU core has padding waste that directly increases memory bandwidth and map capacity pressure.

padlock reads the BTF (BPF Type Format) section embedded in compiled eBPF object files, giving compiler-accurate layout data — no guessing from source, no type-table approximations.

---

## The `padlock bpf` subcommand

```bash
padlock bpf my_prog.bpf.o
```

This is an alias for `padlock analyze` that prints a brief orientation note and then runs the full analysis. All flags work identically:

```bash
# JSON output for scripting
padlock bpf my_prog.bpf.o --json

# SARIF for CI
padlock bpf my_prog.bpf.o --sarif > padlock-bpf.sarif

# Fail CI on high-severity findings
padlock bpf my_prog.bpf.o --fail-on-severity high

# Filter to structs used as map values (by naming convention)
padlock bpf my_prog.bpf.o --filter 'event|value|entry'
```

You can also use `padlock analyze` directly — `padlock bpf` is just a convenience wrapper that documents the intent.

---

## Why BTF analysis is more accurate than source analysis

eBPF programs are compiled with `clang -target bpf`. The compiler resolves `__u64`, `__be32`, `pid_t`, and all other typedefs to their actual sizes for the BPF target, and embeds the complete type graph in the `.BTF` ELF section.

When padlock reads a `.bpf.o` file it reads this section directly — the same data that `bpftool btf dump` shows. There is no approximation: you get the exact field offsets, sizes, and alignment the kernel will see when your map is created.

```bash
# Confirm BTF is present before running padlock
llvm-objdump --section-headers my_prog.bpf.o | grep BTF
```

---

## Example: a perf event payload with padding waste

```c
// execsnoop.bpf.c
struct event {
    pid_t   pid;       // 4 bytes
    bool    is_exec;   // 1 byte
    // 3 bytes padding
    __u64   ts_ns;     // 8 bytes (8-byte aligned)
    char    comm[16];  // 16 bytes
};                     // total: 32 bytes — 3 wasted
```

```
$ padlock bpf execsnoop.bpf.o

padlock bpf: analysing BTF section — layouts reflect compiled types (compiler-accurate).
False-sharing findings on BPF map structs are directly actionable: pad to separate
frequently-updated map values onto distinct cache lines.

[✗] event  32B  fields=4  holes=1  score=62
    [MEDIUM] Padding waste: 3B (9%) — 3B after `is_exec` (offset 5)
    [MEDIUM] Reorder fields: 32B → 32B (saves 0B)
```

In this case reordering doesn't help because `char comm[16]` ends the struct at a natural boundary. The fix is to move `is_exec` after `comm`:

```c
struct event {
    pid_t   pid;       // offset 0,  4 bytes
    __u64   ts_ns;     // offset 8,  8 bytes  ← moved up (needs 8-byte align)
    char    comm[16];  // offset 16, 16 bytes
    bool    is_exec;   // offset 32,  1 byte
    // 3 bytes trailing padding (unavoidable — struct align is 8)
};
```

Or use `__attribute__((packed))` if the struct is only written and never read by field (e.g., raw perf event bytes).

---

## Example: false sharing in a per-CPU map value

Per-CPU maps give each CPU its own copy of a value, so cross-CPU false sharing is usually not an issue. But shared maps used by both kprobes and uprobes running on different CPUs can exhibit false sharing:

```c
struct counters {
    __u64   hits;      // updated on every syscall entry (hot)
    __u64   misses;    // updated on cache miss path (less hot)
    __u64   errors;    // updated on error path (cold)
    spinlock_t lock;   // 4 bytes — protects hits and misses
};
```

```
$ padlock bpf probe.bpf.o --filter counters

[✗] counters  36B  fields=4  holes=1  score=55
    [LOW]  Padding waste: 4B (11%) — 4B after `lock` (offset 32)
    [HIGH] False sharing: cache line 0: [hits, misses, errors, lock]
           (inferred from type names — add guard annotations or verify with profiling)
```

All four fields share a single 64-byte cache line. `hits` is updated on every syscall entry; `errors` is rarely updated. Under high concurrency, every write to `hits` invalidates the line for CPUs waiting to read `errors`.

Fix: separate hot and cold fields:

```c
struct counters {
    __u64   hits;      // offset 0  — hot, updated every syscall
    __u64   misses;    // offset 8  — warm
    __u8    _pad[48];  // offset 16 — push errors to its own cache line
    __u64   errors;    // offset 64 — cold
    spinlock_t lock;   // offset 72
};
```

---

## CI integration for eBPF projects

```yaml
# .github/workflows/bpf-layout.yml
name: BPF struct layout check

on: [push, pull_request]

jobs:
  padlock-bpf:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Build eBPF objects
        run: make -C bpf/

      - name: Install padlock
        run: cargo install padlock-cli

      - name: Analyse BPF struct layouts
        run: padlock bpf bpf/*.bpf.o --fail-on-severity high --sarif > padlock.sarif

      - name: Upload SARIF
        uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: padlock.sarif
```

---

## Using alongside bpftool

padlock and `bpftool` are complementary:

```bash
# bpftool: inspect the live type information in a running program
bpftool btf dump file my_prog.bpf.o format raw

# padlock: find layout waste and suggest fixes
padlock bpf my_prog.bpf.o

# padlock explain: visualise field-by-field layout with cache line markers
padlock explain my_prog.bpf.o --filter event
```

---

## Supported object formats

| Format | Detection | Notes |
|---|---|---|
| `.bpf.o` (ELF with `.BTF` section) | Automatic | Primary target |
| Compiled binary with BTF | Automatic | From kernel modules or CO-RE programs |
| C source (`.bpf.c`) | Source analysis | Approximate — use object file for accuracy |

padlock automatically detects the `.BTF` ELF section and switches to BTF-mode analysis. No flags required.
