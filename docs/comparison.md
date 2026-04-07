# padlock vs Other Tools

## Tool Landscape

Several tools address struct layout and memory performance, but they have different scopes, languages, and integration points. padlock occupies a specific niche: **static, source-level layout analysis across four languages with CI-ready output**.

---

## Comparison Table

| Tool | Languages | Scope | Source? | Binary? | CI output | False sharing | Auto-fix |
|---|---|---|---|---|---|---|---|
| **padlock** | C, C++, Rust, Go | Layout waste, false sharing, locality | ✓ | ✓ (DWARF/PDB) | JSON, SARIF | ✓ | ✓ (diff) |
| `pahole` | Any (DWARF) | Struct holes, reorder | — | ✓ | Text only | — | Text only (`--reorganize`) |
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

**A note on language support:** pahole reads DWARF debug info, so it technically works on any language that emits DWARF — that includes Rust, Go, C, C++, and others. However, it is designed and optimised around C/C++ and the Linux kernel. Its `--reorganize` output is a C struct declaration. It has no understanding of Rust's ownership model, Go's runtime type headers, or language-specific layout rules. Passing it a Rust binary gives you a raw DWARF view with C-style output, not a Rust-native analysis.

**Where the tools genuinely overlap:**

Both tools, given a compiled binary with DWARF:
- Show field offsets, sizes, and padding gaps
- Can suggest reordered layouts (`pahole --reorganize` / `padlock diff`)
- Work across any language that produced DWARF

So for the binary-analysis path on C/C++ programs, the capabilities are substantially similar.

**Where padlock differentiates:**

| | pahole | padlock |
|---|---|---|
| Source analysis (no compilation) | — | ✓ |
| Language-native output (Rust/Go syntax) | — | ✓ |
| False sharing detection | — | ✓ |
| Explicit guard annotation | — | ✓ (`#[lock_protected_by]`, `GUARDED_BY()`, `// padlock:guard=`) |
| In-place source rewriting | — | ✓ |
| SARIF / CI integration | — | ✓ |
| Impact scoring (0–100) | — | ✓ |
| Compile-time assertions | — | ✓ (`#[assert_no_padding]`) |
| Watch mode | — | ✓ |
| Cargo subcommand | — | ✓ (`cargo padlock`) |
| Exact compiler-verified layout | ✓ | Binary only |
| Linux kernel / driver use | ✓ | — |

**When to use pahole:** forensic investigation of compiled C/C++ or kernel binaries where you need exact, compiler-verified layout and are already in a DWARF-centric workflow.

**When to use padlock:** development-time feedback, multi-language codebases, CI layout gates, false-sharing detection, or any workflow where source analysis (no build required) or actionable output (diffs, patches, SARIF) matters.

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

**No** (or low priority), if you:
- Only write C/C++, work exclusively with compiled binaries, and are already satisfied with `pahole` + `-Wpadded`
- Have very few structs and performance is not a concern
- Need exact compiler-verified layout for kernel/driver work (use pahole + compiler for that path; padlock's binary analysis is supplementary)
