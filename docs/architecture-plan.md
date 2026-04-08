# padlock — struct memory layout analyzer

## Executive summary

padlock is a CLI tool that analyzes struct/class memory layouts across languages to find
padding waste, cache-line false sharing, and data locality problems. It auto-fixes field
ordering, flags concurrent access risks, and ranks findings by real impact.

Built in Rust. CI-ready. Multi-arch aware.

---

## 1. The critical design decision: hybrid extraction

### Why not pure source front-ends

Parsing source code to determine struct layout means reimplementing:
- Type resolution (typedefs, aliases, generics, templates)
- Macro expansion (`#define`, `cfg!`, build tags)
- Platform ABI rules (System V AMD64, AAPCS64, MSVC x64)
- Pragma/attribute handling (`#pragma pack`, `__attribute__((aligned))`, `#[repr]`)
- Generic/template monomorphization (a `Vec<u8>` and `Vec<u64>` have different layouts)

This is effectively rebuilding a compiler front-end. For C++ alone, it's years of work.

### The recommended approach: debug info first, source second

**Primary path — debug info (DWARF / PDB):**
The compiler already computed exact field offsets, sizes, alignments, and padding for the
actual target. DWARF debug info encodes all of this. Reading it gives ground truth with
zero ABI reimplementation. This works for any language that emits DWARF: C, C++, Rust,
Go, Zig, Swift, D, and more — you get multi-language support almost for free.

**Secondary path — source front-ends:**
Source-level parsing serves two purposes that debug info can't:
1. **Concurrency annotations**: detecting `Mutex<T>` wrappers, `// GUARDED_BY` comments,
   `atomic` fields, `sync.Mutex` proximity — things that hint at concurrent access patterns.
2. **Auto-fix generation**: to rewrite struct definitions, you need to parse and modify
   source code. Debug info tells you what's wrong; source parsing tells you where to fix it.

**How they combine:** Run DWARF analysis to find all layout problems and compute optimal
orderings. Then, for structs with findings, use the source front-end to (a) enrich with
concurrency metadata and (b) generate fix patches.

### Why this is better

| Concern | Pure source | Pure debug info | Hybrid |
|---------|------------|-----------------|--------|
| Layout accuracy | Must reimplement ABI | Ground truth | Ground truth |
| Multi-language | One front-end per lang | Free (anything with DWARF) | Free + targeted enrichment |
| Concurrency detection | Good (can read annotations) | None | Good |
| Auto-fix | Native | Cannot modify source | Yes, via source layer |
| Build requirement | None (can lint pre-build) | Needs compiled binary | Needs compiled binary |
| Complexity | Very high | Low-medium | Medium |

The one trade-off: the hybrid approach requires a compiled binary with debug info. For CI
this is fine — you already build. For pre-commit linting, you could add a lightweight
source-only mode that uses heuristic sizing (assumes LP64, standard alignment) as a fast
approximation. Flag it as "estimated" in output.

---

## 2. Crate structure

```
padlock/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── padlock-cli/              # Binary crate — CLI entry point
│   │   └── src/
│   │       ├── main.rs           # Argument parsing (clap), orchestration
│   │       ├── commands/         # Subcommands: analyze, fix, report, ci
│   │       └── output/           # Terminal formatting, colors, tables
│   │
│   ├── padlock-core/             # Library crate — IR + analysis engine
│   │   └── src/
│   │       ├── ir.rs             # StructLayout, Field, TypeInfo, AccessPattern
│   │       ├── arch.rs           # ArchConfig: sizes, alignments, cache lines
│   │       ├── analysis/
│   │       │   ├── padding.rs    # Padding waste detection
│   │       │   ├── false_sharing.rs  # Cache-line false sharing
│   │       │   ├── locality.rs   # Data locality / hot-cold splitting
│   │       │   ├── reorder.rs    # Optimal field ordering algorithm
│   │       │   └── scorer.rs     # Impact ranking engine
│   │       └── findings.rs       # Finding, Severity, Suggestion types
│   │
│   ├── padlock-dwarf/            # DWARF debug info extraction
│   │   └── src/
│   │       ├── reader.rs         # ELF/Mach-O/PE binary loading
│   │       ├── extractor.rs      # DIE traversal → StructLayout
│   │       └── type_resolver.rs  # DWARF type graph → TypeInfo
│   │
│   ├── padlock-source/           # Source front-end (enrichment + fix gen)
│   │   └── src/
│   │       ├── frontends/
│   │       │   ├── c_cpp.rs      # tree-sitter-c / tree-sitter-cpp
│   │       │   ├── rust.rs       # syn crate
│   │       │   └── go.rs         # tree-sitter-go
│   │       ├── concurrency.rs    # Detect concurrent access patterns
│   │       └── fixgen.rs         # Generate reordered source + patches
│   │
│   └── padlock-output/           # Serializable output formats
│       └── src/
│           ├── json.rs           # JSON findings for CI
│           ├── sarif.rs          # SARIF for IDE integration
│           ├── diff.rs           # Unified diff patches
│           └── summary.rs        # Human-readable summary stats
│
├── tests/
│   ├── fixtures/                 # Test binaries and source files
│   └── integration/              # End-to-end CLI tests
└── docs/
    └── architecture.md
```

### Key Rust dependencies

| Crate | Purpose |
|-------|---------|
| `clap` | CLI argument parsing with derive macros |
| `gimli` | DWARF debug info parsing (production-grade, used by Firefox) |
| `object` | ELF/Mach-O/PE binary format parsing |
| `pdb` | Windows PDB debug info |
| `tree-sitter` + grammars | C/C++/Go source parsing |
| `syn` + `quote` | Rust source parsing and code generation |
| `serde` / `serde_json` | Serialization for JSON/SARIF output |
| `owo-colors` / `yansi` | Terminal coloring |
| `comfy-table` | Terminal table rendering |
| `similar` | Diff generation for patches |
| `rayon` | Parallel analysis of large codebases |

---

## 3. Intermediate representation (IR)

The IR is the heart of the system. Every front-end produces it; every analysis pass consumes it.

```rust
/// Target architecture configuration
pub struct ArchConfig {
    pub name: &'static str,          // "x86_64", "aarch64", "arm32"
    pub pointer_size: usize,          // 8, 4
    pub cache_line_size: usize,       // 64, 128 (Apple Silicon)
    pub max_alignment: usize,         // 16 (SSE), 32 (AVX)
    pub endianness: Endianness,
    pub char_is_signed: bool,
}

/// A struct/class/record as extracted from debug info or source
pub struct StructLayout {
    pub name: String,
    pub qualified_name: String,        // e.g., "crate::module::MyStruct"
    pub source_file: Option<PathBuf>,
    pub source_line: Option<u32>,
    pub fields: Vec<Field>,
    pub total_size: usize,             // As reported by compiler
    pub alignment: usize,
    pub is_packed: bool,               // #[repr(packed)] / __attribute__((packed))
    pub repr: ReprKind,                // C, Rust, Transparent, etc.
    pub instance_count_hint: Option<u64>,  // From profiling data if available
    pub arch: ArchConfig,
}

/// A single field within a struct
pub struct Field {
    pub name: String,
    pub ty: TypeInfo,
    pub offset: usize,                 // Byte offset from struct start
    pub size: usize,
    pub alignment: usize,
    pub bit_field: Option<BitFieldInfo>,
    pub access: AccessPattern,
    pub source_line: Option<u32>,
}

/// Concurrency and access pattern metadata
pub enum AccessPattern {
    Unknown,
    ReadMostly,
    WriteMostly,
    ReadWrite,
    Concurrent {
        guard: Option<String>,         // "self.mu", "LOCK_A"
        is_atomic: bool,
    },
    Padding,                           // Compiler-inserted padding
}

/// Type information (recursive for nested types)
pub enum TypeInfo {
    Primitive { name: String, size: usize, alignment: usize },
    Pointer { pointee: Box<TypeInfo>, size: usize },
    Array { element: Box<TypeInfo>, count: usize },
    Struct { name: String, layout: Box<StructLayout> },
    Enum { name: String, size: usize, alignment: usize },
    Union { name: String, size: usize, alignment: usize },
    Opaque { name: String, size: usize, alignment: usize },
}
```

---

## 4. Analysis passes

### 4.1 Padding waste detection

Walk the field list in offset order. For each pair of adjacent fields, compute
`field[i+1].offset - (field[i].offset + field[i].size)`. Any positive value is padding.
Also check tail padding: `struct.total_size - (last_field.offset + last_field.size)`.

**Output**: bytes wasted per gap, total waste, waste as percentage of struct size.

**Severity heuristic**:
- >50% waste on a struct >64 bytes → High
- >25% waste on a struct >32 bytes → Medium
- <8 bytes waste total → Low (unless struct is in a hot array)

### 4.2 Optimal field reordering

Classic algorithm: sort fields by alignment descending, then by size descending within
each alignment group. This provably minimizes padding for `repr(C)` / C structs.

**Refinements**:
- Respect `#[repr(C)]` — if the struct must maintain C layout compatibility, only
  suggest reordering (don't auto-fix unless user confirms ABI break is acceptable).
- Group concurrently-accessed fields onto the same cache line when possible.
- Keep hot fields (high access frequency) in the first cache line.
- Preserve field groups that are semantically related (heuristic: same-prefix names,
  adjacent in source).

**Compute savings**: reorder fields virtually, recompute layout, report bytes saved.

### 4.3 Cache-line false sharing

Partition fields into cache-line groups (64-byte or 128-byte buckets based on arch).
Flag when fields with `AccessPattern::Concurrent` and different guards share a cache line.

**Heuristics for concurrent access** (from source front-end):
- Field is `AtomicU64`, `AtomicBool`, etc. → concurrent
- Field is inside a struct that's wrapped in `Arc<Mutex<T>>` → all fields concurrent
- Field has `// GUARDED_BY(mu)` comment → concurrent, guard = mu
- Adjacent `sync.Mutex` field in Go → next N fields likely guarded
- Field in a struct implementing `Send + Sync` in Rust → potentially concurrent

**Suggestion**: Insert explicit padding (`[u8; N]` / `char pad[N]`) between fields
accessed by different threads, or use `#[repr(align(64))]` on the struct.

### 4.4 Data locality analysis

Group fields by likely access pattern:
- **Hot path**: fields accessed on every operation (determined by name heuristics,
  profiling data, or user annotations)
- **Cold path**: fields accessed rarely (metadata, debug info, error paths)
- **Temporal locality**: fields typically accessed together should be adjacent

**Heuristic signals**:
- Fields named `len`, `size`, `count`, `ptr`, `data` → hot
- Fields named `created_at`, `debug_*`, `_reserved`, `metadata` → cold
- Fields of type `Option<T>` where T is large → likely cold (the None case is common)
- Fields accessed in the same function (requires source analysis or profile data)

**Suggestion**: Split into hot/cold structs, or reorder to put hot fields first so
they share a cache line.

### 4.5 Impact scoring

Each finding gets a composite score:

```
impact = base_severity
       × struct_size_multiplier      // larger structs waste more
       × instance_count_multiplier   // more instances = more total waste
       × array_bonus                 // structs in arrays multiply the problem
       × concurrency_bonus           // false sharing is very expensive at runtime
```

Findings are ranked by impact score. The top N are shown in terminal output; all are
included in JSON/SARIF.

---

## 5. CLI design

```
padlock — analyze struct memory layouts

USAGE:
    padlock <COMMAND> [OPTIONS]

COMMANDS:
    analyze     Analyze binary/source for layout problems
    fix         Generate auto-fix patches
    report      Generate a full report (JSON, SARIF, or text)
    diff        Show layout changes between two versions
    list        List all structs and their layouts

GLOBAL OPTIONS:
    --arch <ARCH>           Target architecture [default: native]
                            Values: x86_64, aarch64, arm32, riscv64, wasm32
    --cache-line <BYTES>    Cache line size override [default: auto from arch]
    --min-severity <SEV>    Minimum severity to report [default: low]
    --format <FMT>          Output format: text, json, sarif [default: text]
    --color <WHEN>          Color output: auto, always, never [default: auto]
    -q, --quiet             Only output findings (CI mode)
    -v, --verbose           Show detailed analysis
```

### Example usage

```bash
# Analyze a compiled binary
padlock analyze ./target/debug/myapp

# Analyze for a different architecture
padlock analyze ./myapp --arch aarch64 --cache-line 128

# Generate fix patches
padlock fix ./target/debug/myapp --source ./src/ -o fixes.patch

# CI mode — exit code 1 if any high-severity findings
padlock analyze ./target/debug/myapp --min-severity high --format json -q

# Compare layouts between two builds
padlock diff ./build-v1/app ./build-v2/app

# List all struct layouts (useful for exploration)
padlock list ./target/debug/myapp --filter "MyStruct*"
```

### Example terminal output

```
padlock v0.1.0 — struct memory layout analyzer

Analyzing: ./target/debug/myapp (x86_64, cache line: 64B)
Found 342 structs, 47 with findings

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  HIGH  ConnectionPool (src/pool.rs:24)
        Size: 192B (could be 128B) — 33% padding waste

        Current layout:
        ┌─offset─┬─size─┬─field──────────────┬─padding─┐
        │   0    │   1  │ is_active: bool     │  7B gap │
        │   8    │   8  │ max_conns: u64      │         │
        │  16    │   1  │ is_tls: bool        │  7B gap │
        │  24    │   8  │ timeout_ms: u64     │         │
        │  32    │  24  │ name: String         │         │
        │  56    │   1  │ debug_mode: bool    │  7B gap │
        │  64    │ 128  │ connections: Vec<..> │         │
        └────────┴──────┴─────────────────────┴─────────┘
        Total padding: 64B in 3 gaps

        ⚡ Suggested reorder saves 64B (fix available)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  HIGH  SharedCounter (src/metrics.rs:8)
        False sharing: `count` and `name` on same cache line
        Both accessed concurrently with different guards

        ⚡ Suggest: align `count` to cache line boundary

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Summary: 12 high, 18 medium, 17 low findings
         Total recoverable padding: 4.2 KB across 47 structs
         3 potential false sharing sites
```

---

## 6. CI integration

### GitHub Actions example

```yaml
- name: Build with debug info
  run: cargo build  # debug builds include DWARF by default

- name: Run padlock
  run: |
    padlock analyze ./target/debug/myapp \
      --min-severity medium \
      --format sarif \
      -o padlock-results.sarif

- name: Upload SARIF
  uses: github/codeql-action/upload-sarif@v4
  with:
    sarif_file: padlock-results.sarif
```

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | No findings at or above `--min-severity` |
| 1 | Findings found |
| 2 | Analysis error (bad binary, missing debug info) |

### Baseline / diff mode

```bash
# Record a baseline
padlock analyze ./app --format json -o .padlock-baseline.json

# In CI, compare against baseline (only new findings fail)
padlock analyze ./app --baseline .padlock-baseline.json --format json -q
```

---

## 7. Multi-arch awareness

The arch config drives all layout calculations. Predefined configs:

| Arch | Pointer | Cache line | Max align | Notes |
|------|---------|-----------|-----------|-------|
| x86_64 (SysV) | 8 | 64 | 16 | Default Linux/macOS |
| x86_64 (MSVC) | 8 | 64 | 16 | Different struct packing rules |
| aarch64 | 8 | 64/128 | 16 | Apple Silicon uses 128B cache lines |
| arm32 | 4 | 32/64 | 8 | Varies by core |
| riscv64 | 8 | 64 | 16 | |
| wasm32 | 4 | N/A | 8 | No cache lines, padding still matters |

When analyzing DWARF, the arch is automatically detected from the binary. The `--arch`
flag is for cross-compilation scenarios or source-only analysis.

**Cross-arch comparison**: `padlock diff --arch x86_64 --arch aarch64 ./app` shows
layout differences between architectures for the same structs.

---

## 8. Implementation roadmap

### Phase 1: Foundation (4-6 weeks)
- [ ] Workspace setup, CI, testing infrastructure
- [ ] `padlock-core`: IR types, arch configs
- [ ] `padlock-dwarf`: DWARF reading via gimli, extract struct layouts from ELF binaries
- [ ] `padlock-core/analysis/padding.rs`: padding detection
- [ ] `padlock-core/analysis/reorder.rs`: optimal reordering algorithm
- [ ] `padlock-cli`: basic `analyze` and `list` commands with terminal output
- [ ] JSON output format

**Milestone**: `padlock analyze ./binary` shows padding waste for all structs.

### Phase 2: Deep analysis (3-4 weeks)
- [ ] `padlock-core/analysis/false_sharing.rs`: cache-line analysis
- [ ] `padlock-core/analysis/locality.rs`: hot/cold field grouping
- [ ] `padlock-core/analysis/scorer.rs`: impact ranking
- [ ] SARIF output for IDE integration
- [ ] Mach-O support (macOS binaries)
- [ ] `--min-severity` filtering, `--baseline` diff mode

**Milestone**: full analysis pipeline with ranked findings and CI integration.

### Phase 3: Source enrichment + auto-fix (4-6 weeks)
- [ ] `padlock-source/frontends/rust.rs`: Rust source parsing via syn
- [ ] `padlock-source/frontends/c_cpp.rs`: C/C++ via tree-sitter
- [ ] `padlock-source/concurrency.rs`: concurrent access detection
- [ ] `padlock-source/fixgen.rs`: generate reordered source code
- [ ] `padlock-cli`: `fix` command that writes patches
- [ ] Diff output format

**Milestone**: `padlock fix ./binary --source ./src/ -o fix.patch` generates valid patches.

### Phase 4: Polish + ecosystem (3-4 weeks)
- [ ] `padlock-source/frontends/go.rs`: Go support via tree-sitter
- [ ] Windows PE + PDB support
- [ ] Cross-arch comparison (`padlock diff --arch`)
- [ ] Profile data ingestion (perf, callgrind) for access frequency hints
- [ ] `padlock-source`: LSP/editor plugin integration
- [ ] Documentation, website, crates.io publish

---

## 9. Key algorithms in detail

### Optimal field ordering (padding-minimal)

```
Input:  fields[] with (size, alignment) for each
Output: permutation that minimizes total struct size

Algorithm:
1. Sort fields by alignment DESC, then by size DESC
2. Place fields greedily:
   - Start at offset 0
   - For each field, round up offset to field.alignment
   - Place field, advance offset by field.size
3. Round final size up to struct alignment (max field alignment)

This is optimal for the general case. O(n log n).
```

### Cache-line partitioning for false sharing

```
Input:  fields[] with (offset, size, access_pattern)
        cache_line_size (e.g. 64)
Output: list of (cache_line_index, conflicting_field_pairs)

Algorithm:
1. For each field, compute cache_line = offset / cache_line_size
2. Group fields by cache_line
3. Within each group, find pairs where:
   a. Both have AccessPattern::Concurrent
   b. They have different guards (different locks)
   c. At least one is write-accessed
4. Emit findings for each conflicting pair
```

### Impact scoring formula

```
base_severity:     HIGH=100, MEDIUM=50, LOW=10
size_mult:         min(struct_size / 64, 4.0)       -- larger = worse
instance_mult:     log2(instance_count + 1)          -- if available
array_mult:        3.0 if struct appears in Vec/slice/array, 1.0 otherwise
concurrency_mult:  5.0 for false sharing, 1.0 otherwise

impact = base × size_mult × instance_mult × array_mult × concurrency_mult
```

---

## 10. Open questions and trade-offs

**Should source-only mode exist?**
A lightweight mode that parses source and uses heuristic platform sizes (assume LP64,
natural alignment) would enable pre-commit hooks without building. The trade-off is
accuracy — it can't handle `#pragma pack`, platform-specific types, or template
instantiations. Recommendation: yes, but always label output as "estimated" and
recommend full analysis on the built binary.

**How to handle Rust's default repr?**
Rust's default layout (`repr(Rust)`) is deliberately unspecified — the compiler can
reorder fields. This means padding analysis on Rust structs is only meaningful for
`repr(C)` or when looking at the actual compiled layout (DWARF path). For `repr(Rust)`,
padlock should still report the layout the compiler chose but note that it may change
between compiler versions. Focus suggestions on false sharing and locality, not padding.

**Profile-guided analysis?**
Ingesting `perf` or `callgrind` data to know actual field access frequencies would
make locality analysis much more accurate. This is high-value but adds pipeline
complexity. Recommendation: support it as an optional input in Phase 4, not a
requirement.

**What about unions and bit-fields?**
Unions don't have padding in the traditional sense. Bit-fields have complex,
platform-specific packing rules. Recommendation: report their presence and total size
contribution but skip reordering suggestions. Flag bit-field structs as "manual review
recommended."