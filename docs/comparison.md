# padlock vs Other Tools

## Tool Landscape

Several tools address struct layout and memory performance, but they have different scopes, languages, and integration points. padlock occupies a specific niche: **static, source-level layout analysis across four languages with CI-ready output**.

---

## Comparison Table

| Tool | Languages | Scope | Source? | Binary? | CI output | False sharing | Auto-fix |
|---|---|---|---|---|---|---|---|
| **padlock** | C, C++, Rust, Go | Layout waste, false sharing, locality | ✓ | ✓ (DWARF/PDB) | JSON, SARIF | ✓ | ✓ (diff) |
| `pahole` | C, C++ | Struct holes, DWARF only | — | ✓ | Text only | — | — |
| `offsetof` / `sizeof` | Any | Size inspection | — | — | — | — | — |
| Clang `-Wpadded` | C, C++ | Padding warning | ✓ | — | Compiler diag | — | — |
| `cargo-geiger` | Rust | Unsafe usage | ✓ | — | — | — | — |
| Vtune / Perf | Any | Runtime cache misses | — | Runtime | — | Runtime | — |
| Cachegrind | Any | Cache simulation | — | Runtime | — | Heuristic | — |
| `heaptrack` | C++ | Heap profiling | — | Runtime | — | — | — |
| `clang-tidy` | C, C++ | Code quality | ✓ | — | SARIF | — | ✓ |

---

## padlock vs `pahole`

[`pahole`](https://git.kernel.org/pub/scm/devel/pahole/pahole.git) ("poke-a-hole") is the closest comparable tool. It reads DWARF from compiled binaries and shows struct layouts with hole annotations.

**pahole strengths:**
- Battle-tested on the Linux kernel
- Reads exact compiler output (no approximation)
- Generates optimized struct declarations

**padlock strengths:**
- Works on **source files without compilation** — useful in editors, pre-commit hooks, and fast CI checks
- Supports **Rust and Go** (pahole is C/C++ only)
- Detects **false sharing** from concurrency annotations
- Emits **SARIF** for GitHub code-scanning annotations
- Generates **unified diffs** and applies **in-place reordering**
- Scores structs 0–100 for at-a-glance prioritisation

**When to use pahole:** You need exact compiler-verified layout for C/C++ binaries, or you're working on kernel/driver code where accuracy is critical.  
**When to use padlock:** You want fast feedback during development, across multiple languages, with SARIF output for CI.

---

## padlock vs Clang `-Wpadded`

Clang's `-Wpadded` warns about padding when compiling C/C++. It is accurate (compiler-driven) but limited:
- Requires compilation (slow in CI, not useful without a compiler)
- Only warns — no suggested fix, no diff, no scoring
- No false-sharing detection
- No Rust/Go support

padlock is not a replacement but a complement: use `-Wpadded` for C/C++ when you want compiler-verified accuracy; use padlock for multi-language source scanning and actionable output.

---

## padlock vs Runtime Profilers (VTune, Perf, Cachegrind)

Runtime profilers show the *actual* cache miss rate under a real workload. They are authoritative but require a running program and a representative workload.

padlock is a *static* tool: it finds issues before runtime, across all structs in a codebase (not just those exercised by your test suite). The combination is:
1. Use padlock to fix obvious issues statically
2. Use a profiler on the hot path to find workload-specific issues

---

## Where padlock Fits in the Development Cycle

```
Write code  →  padlock (pre-commit, editor)   — catch layout issues immediately
            →  CI (SARIF / JSON)              — enforce layout quality in PRs
            →  Compilation (-Wpadded)         — catch anything padlock missed
            →  Testing (unit/integration)     — ensure correctness
            →  Profiling (VTune/Perf)         — measure real-world cache impact
            →  Deploy
```

padlock is most valuable at the **earliest stages** (editor/pre-commit) and in **CI gates** because:
- No compilation required — fast feedback
- SARIF output integrates with GitHub/GitLab code scanning
- Multi-language — one tool for polyglot codebases
- Actionable — diffs and in-place fixes, not just warnings

---

## Do You Need padlock?

**Yes**, if you:
- Have structs that are accessed in hot loops or allocated in large numbers
- Work across C, C++, Rust, and Go in the same codebase
- Want layout quality enforced in CI without running the full compiler
- Have concurrent data structures and want automatic false-sharing detection

**No**, if you:
- Only write C/C++ and are already using `pahole` + `-Wpadded` extensively
- Have very few structs and performance is not a concern
- Need exact compiler-verified layout (use pahole + compiler instead)
