# Changelog

All notable changes to padlock are documented here.

## [0.72.0] — 2026-10-30

### Fixed
- **`--perf-types`: Go qualified type names and hex offsets not matched.** Go perf output emits fully-qualified type identifiers (`github.com/foo/bar/pkg.TypeName`) and hexadecimal field offsets (`+0x48`). The parser only handled short C-style names and decimal offsets, so no fields were ever marked hot when profiling Go binaries. Now indexes by both full and short (after last `.`) name, and parses `0x…` offsets.

## [0.71.0] — 2026-10-30

Found by checking mimalloc (C), typescript-go and microsoft/go against gcc's DWARF (`padlock verify`) and `go test` (`--go-exact`). mimalloc: 57 of 57 structs now match the compiler, up from 39 of 50.

### Fixed
- **C: a field typed with the typedef name of a tagged record was sized as a pointer.** For `typedef struct Tag { … } Name;` a member `Name x;` got 8 bytes instead of the record's size; only `typedef struct X { … } X;` worked. Struct sizes that contain such members were wrong without being reported as uncertain.
- **C: `typedef __attribute__((aligned(N))) struct Tag { … } Name;`** (GNU attribute before `struct`, as `mi_decl_align`-style macros produce) is read; tree-sitter turned the whole typedef into an error node and the alignment was lost.
- **C/C++: a multi-line `#define` with `/* … */` comments between its lines** (`MI_STAT_FIELDS()` in mimalloc) made tree-sitter drop the declarations after it, including enums whose constants size later arrays (`[?]`).
- **C/C++: `_Atomic(T)` defined as a macro** (`#define _Atomic(tp) std::atomic<tp>` for C++, `… tp` for MSVC) is expanded; the C++ grammar cannot parse `_Atomic(T)` and the field vanished. A C++ translation unit now defines `__cplusplus`, and a `.h` reached from a C unit is read as C even when it mentions `std::` in a C++ branch.
- **Go: generic arguments that are structs of the package** (`Box[Local]`, `Value T`) were sized as a pointer; a generic declared in one file is instantiated for the uses in every file of the package.
- **Go: `sync.Map` is 48 bytes with Go 1.24+** (HashTrieMap), 32 before; padlock asks the installed toolchain (`go env GOVERSION`), and the parse cache is keyed on it.
- **`padlock summary`: files with only zero-size structs were listed as the worst files with score 0.**
- **`--go-exact` skipped whole packages** when one struct was declared inside a function or named a package-qualified type argument; those types are now left out of the probe and the rest is checked.
- **`padlock verify` ignored `.padlock.toml` and had no `-I`/`-D`**, so C/C++ sources were compared without their headers. It takes `-I`/`--include-dir` and `-D`/`--define` and reads `include_dirs`/`defines` from the config.

### Fixed (continued)
- **Go: pointers, slices, maps, channels and funcs of package-qualified types** (`[]*ast.Node`, `map[tspath.Path]*Node`) were flagged as uncertain although their size does not depend on the element.

- **Go: types of imported packages are sized from their source.** A field typed `tspath.Path` or `ast.Kind` is read from the package it comes from — packages of the surrounding module(s) (`go.mod`, `go.work`), the standard library (`$GOROOT/src`, or its own tree when analyzing it) — instead of being guessed as one word and flagged. Aliases (`type Kind int16`), structs, interfaces and chains of them are followed, also through generated files too large to parse. Generic types of other packages are still flagged. The parse cache now tracks the sibling files and the imported packages' files, and the Go toolchain version.

- **C: `_Atomic(T)` / `_Atomic T` fields were not treated as atomics**, so C11 code (mimalloc) could never get a false-sharing finding; they are now, and a field's displayed type reads `_Atomic uint64_t`.
- **False sharing between locks alone is Low** (inferred from type names: two mutexes side by side only hurt when both are contended at once); it stays Medium when an atomic is involved and High when a guard is annotated. On microsoft/TypeScript 7 of 18 inferred findings move to Low, on microsoft/go 6 of 53; mimalloc goes from 0 to 9 (all involving atomics).

- **Go: standard-library sizes are used only for the standard package.** `atomic.Bool` is 4 bytes in `sync/atomic` but 1 in `internal/runtime/atomic`; inside the runtime the hardcoded 4 produced silently wrong sizes (`g`, `mOS`, `gcCPULimiterState`).
- **`padlock verify` skips blank Go fields (`_`)**: several share the name and were compared with the first one's offset.

### Measured
- Go standard library, 19 packages (708 types, Go 1.27, linux/amd64, `go test -c` DWARF as reference): 95% exact, 5% flagged, 0 silently wrong; the previous build gave 81% / 18% / 5 silent.
- microsoft/TypeScript (the Go port; 1201 non-test structs compared with `go test` via `--go-exact`): 1125 sizes match without `--go-exact`, up from 825. 1090 structs are not flagged uncertain (up from 674) and every one that `go test` can probe matches; 111 remain flagged (generics of other packages such as `collections.Set[T]`).

## [0.70.0] — 2026-10-25

### Fixed
- **PE/COFF binaries with GNU-compressed debug sections** (`.zdebug_*`, as mingw produces) are read; they previously yielded no types.

## [0.69.0] — 2026-10-25

### Fixed
- **C++ layouts with `packed` and inheritance match g++/clang.** Empty classes with an alignment attribute, tail-padding reuse by derived classes, base-class alignment (including `alignas`/`aligned` on the base), GCC's rule that `packed` is ignored for a non-POD member, bit-fields that start inside a base's tail padding, `#pragma pack` caps and union member alignment are modelled as the compilers do. Differential fuzzing against g++: 0 mismatches in thousands of seeds, up from rare mismatches.
- **`aligned(N)` on a field of a `packed` struct under `#pragma pack(M)`** is capped at M, as GCC does (padlock kept the full value; found by C fuzzing).
- **C headers using `#include <pshpack*.h>`/`<poppack.h>`** (Windows SDK, mingw) get the packing they imply; conditionals around them are resolved as the compiler would.
- **Zig**: type aliases, `void` union fields, explicit tag types and tagged-union layout (tag after the payload when the payload is at least as aligned) follow the compiler; uncertain fields are reported as such.

### Known limits
- A header whose packing comes from the file that includes it (`mciapi.h` via `mmsystem.h`) is laid out unpacked when analyzed on its own.

## [0.68.0] — 2026-10-25

### Added
- **`padlock-lsp` resolves types from outside the open file.** The buffer is laid out against the rest of its Go package, its Rust crate or the headers a C/C++ file includes (found next to the file, in its parent directories and, on Linux, the system include paths — an editor has no `-I` flags), with the unsaved text taking precedence over the saved file. A field typed with a struct from a sibling file is sized instead of guessed. Without context — a new unsaved file, a Go directory with more than 400 files, nothing found — the buffer is analyzed alone, as before. Opening a file from `net/http` takes 0.15 s, `uv.h` 0.19 s.
- `padlock_source::parse_source_text(path, text, lang, arch)`: the library entry point for this.

## [0.67.0] — 2026-10-25

### Fixed
- **`padlock fix` detached comments from their fields.** A comment after a field on the same line (`char state; // connection state`) was taken for the *next* field's, so after reordering it described the wrong one; in Rust a comment after the last field was deleted outright and another ended up glued to the struct's opening brace; in Go the first field landed on the `struct {` line and standalone comment lines above a field were dropped. Trailing comments (`// …` and one-line `/* … */`) now travel with their field in C, C++, Rust and Zig, Go keeps the comment lines above a field with it, and a comment after the last field is kept.
- A last field written without its terminator (Rust and Zig allow that) gets a comma when it moves, instead of producing invalid code.
- C/C++ field names were misread when a field carried a trailing comment, which made `fix` fall back to a regenerated struct that dropped every comment.

## [0.66.0] — 2026-10-25

### Fixed
- **Deliberate cache-line padding was reported as waste.** `#[repr(align(64))]`, `alignas(64)` and `__attribute__((aligned(64)))` on a struct or a field — the standard fix *for* false sharing — produced "Padding waste: 56B (88%)" (High in C++). Only the padding the fields would need at their natural alignment is reported now; the part an explicit alignment request adds is not.

## [0.65.0] — 2026-10-25

### Fixed
- **False sharing between two atomics was never reported.** Two independently updated atomics on one cache line (`AtomicU64 produced, consumed`), the textbook case, passed as clean in every language because pairs of atomics were excluded. They are now a conflict like two locks or a lock and an atomic; fields under the same guard still are not.
- **False-sharing findings list only the conflicting fields** (the locks and atomics on the line), not every field that happens to share it.

### Changed
- **False-sharing severity follows confidence**: High when a conflicting field carries a guard annotation, Medium when the guards are inferred from type names (it was always High). The score deduction for an inferred finding is 15 instead of 30.

## [0.64.0] — 2026-10-25

### Fixed
- **C/C++ headers are preprocessed in order.** A header included part-way through a file now sees the declarations above its `#include` (libuv's `uv.h` defines `struct uv__queue` before including `uv/unix.h`, which uses it), and the macros it defines apply only to the lines after it; `#include`s in branches the target does not take are no longer followed. Include guards (`#ifndef X_H` / `#define X_H`) count as decided.
- **Results are the same on every run.** A header shared by several files is laid out in the context of the first file, in sorted order, that includes it, instead of whichever thread got there first; output order no longer depends on hash-map iteration.
- Without `-I`, a quoted `#include` that is not next to the including file is also looked for up to three directories higher (a project's include root), and an `<…>` one after the system paths.
- Function-pointer typedefs (`typedef void (*cb_t)(…)`) and array typedefs (`typedef long jmp_buf[8]`) are sized.
- In a directory scan, Go files not built for the target (`_windows.go` on Linux, `//go:build` tags that are off) are skipped, as `go build` would; naming a file explicitly still analyzes it.
- `padlock init --force` overwrites an existing `.padlock.toml` (it was documented but missing).
- VS Code extension: the hover says when a struct's size is uncertain (fields whose type padlock could not see), instead of showing it as exact.

### Documentation
- README accuracy table measured with default options and extended with libuv; the license badge links to the license section; the GitHub Action and `padlock-macros` examples reference released versions (`v0.12.0`, `0.20`), noting that `assert_align`/`assert_offsets` need 0.47 or later. `padlock-source`'s README and CLAUDE.md updated for the current layout model.

## [0.63.0] — 2026-10-25

### Fixed
- **Go empty structs (`type noCopy struct{}`) were sized as one word**, so `sync.WaitGroup` and every struct embedding such a marker came out too large without a warning. They are zero-sized now.
- **Go files built only under a `GOEXPERIMENT` were treated wrongly**: build constraints now use the toolchain's own tags (`go list -f '{{context.ToolTags}}'`, e.g. `goexperiment.exectracer2`, `amd64.v1`), falling back to Go 1.22–1.23 defaults without a toolchain.
- An array length using `unsafe.Sizeof(localStruct{})` took a one-word placeholder as the size; it is now flagged uncertain.
- **Rust types from other modules of the crate were guessed silently.** Rust files are laid out together with their crate (every `.rs` under `src/`, cached per crate); a name declared in more than one module stays unknown and is flagged. A type name padlock knows nowhere is flagged uncertain instead of taken as one word.
- `Option` of a fieldless enum, of a struct with a niche-bearing field, and of a tuple with one (`Option<(String, usize)>`) is sized as rustc does (no extra tag).
- **`--rust-exact` silently did nothing on a second run** (cargo considered the crate fresh, so rustc printed no sizes); the crate is now always recompiled for the probe, a run that prints nothing is reported, and binary-only packages with several binaries are probed per binary.
- `__attribute__ ((__aligned__ (16)))` with spaces (glibc's spelling) on a C field is honoured; RISC-V Linux predefines `__riscv_float_abi_double`.

### Documentation
- README: accuracy measured on real code (Go standard library on amd64/arm64/386, glibc on x86_64/aarch64/riscv64, Python and OpenSSL headers, Rust crates), replacing an earlier Go figure that had been measured incorrectly (`--go-exact` cannot probe some standard-library packages, which were then compared with themselves).

## [0.62.0] — 2026-10-25

### Fixed
- **C/C++ types from included headers were guessed.** Each file was laid out on its own, so a typedef, constant or record from another header (`struct timespec`'s `__time_t`, `sem_t`'s `__SIZEOF_SEM_T`) got a pointer-sized placeholder. A C/C++ file is now laid out with what its `#include`s declare, processed in preprocessing order: headers are found next to the including file, in `-I DIR` / `include_dirs` and, for Linux targets, the system include paths, and each header is processed once per run in the context of the translation unit that first reaches it (so umbrella headers such as `Python.h` work). Against gcc's DWARF for glibc headers: exact 89 → 161 of 173 types, silently wrong 12 → 0; Python and OpenSSL headers: exact 28 → 144 of 152, silently wrong 16 → 0.
- **Macros are expanded.** Object-like and function-like macros (with `#` and `##`) are expanded in declarations, in order (`#undef` respected), with the target's predefined macros (`__x86_64__`, `__SIZEOF_LONG__`, `__UINT8_TYPE__`, …); `#if` groups whose conditions involve only known or reserved macros are resolved as the compiler would, while those on project macros are still left to `-D` and flagged when undecided.
- `typedef struct X Y;` (no body) made `Y` an unknown type even in the same file; it now names the record `X`.
- `typedef float v4 __attribute__((__vector_size__(16)))` vectors are sized (16 bytes, aligned to their size, 8 on Cortex-M).
- `sizeof(struct X)` in array lengths now uses the record's size; a `sizeof` of an unknown type no longer guesses one word.
- Typedef chains (`uint8_t` → `__uint8_t` → `unsigned char`) are followed; header typedefs never override types padlock sizes natively.
- A member the parser cannot make sense of (an unknown member macro such as `PyObject_HEAD`) is flagged uncertain instead of silently dropped.

### Added
- `-I DIR` / `--include-dir DIR` and `include_dirs = [...]` in `.padlock.toml`.
- The parse cache records each C/C++ file's included headers and invalidates the file when one changes.

## [0.61.0] — 2026-10-25

### Fixed
- **Go types from another file of the same package were sized as one word, silently.** A Go package spans every `.go` file of its directory, but each file was laid out on its own, so a field of a struct, alias or constant-length array declared in a sibling file (`image.RGBA.Rect`, `tar.Reader.blk`) got 8 bytes and no warning. Go files are now laid out together with their package (cached per directory, built in parallel); files excluded for the target by `_GOOS`/`_GOARCH` name suffixes or `//go:build` lines are left out, as `go build` would. (A before/after figure first given here was mismeasured: `--go-exact` cannot probe several standard-library packages, which were then compared with themselves. See 0.63.0 for accuracy measured against the compiler's debug info.)
- **An uncertain size now propagates to the structs that contain it** (all languages): a struct holding a struct with an unresolvable field is itself flagged uncertain instead of looking exact.
- A Go field whose type is a bare name declared nowhere in the package is flagged uncertain instead of silently sized as one word.
- The parse cache invalidates a Go file when any `.go` file in its directory changes.

## [0.60.0] — 2026-10-25

### Changed
- Dependency updates: `comfy-table` 7.2.2 → 8.0.1, `syn` 3.0.6 → 3.0.9, `serde_json` 1.0.151 → 1.0.154, `anyhow` 1.0.102 → 1.0.104, `glob` 0.3.3 → 0.3.4; VS Code extension `@types/node` 26.6.4 → 26.6.5.
- CI/actions: `actions/cache` 4 → 6 (composite action), `actions/setup-node` 4 → 7 (release workflow), `dependabot/fetch-metadata` 2 → 3, example workflow pins `gidotencate/padlock@v0.12.0`.

## [0.59.0] — 2026-10-25

### Fixed
- **`--go-exact` silently did nothing on Windows**: the package directory was passed to `go test -overlay` in verbatim form (`\\?\D:\...`), which `go` does not match against its own path, so the probe never ran and the source-level layouts were kept without a note. The prefix is now dropped, and a probe that prints nothing is reported (`padlock: note: --go-exact skipped ...`) instead of being accepted.
- `detect_arch_from_host_returns_valid_config` now accepts the `x86_64-msvc` host config added in 0.43.0.

## [0.58.0] — 2026-10-25

### Fixed
- **`padlock-lsp` analysis could race on its parse settings**: `analyze_text` sets the preprocessor defines, declared-order flag and configured instantiations process-wide, so two concurrent calls (seen in parallel tests) could parse with each other's settings. The settings are now held under a lock for the whole parse.

### Changed
- The integration tests that build with the host `gcc`/`g++` (DWARF input variants, `verify`, `--compile-commands`) now run on Linux only: they assume ELF output and an 8-byte `long`, which MinGW (LLP64, `.exe` output) and macOS (`gcc` is clang, DWARF left in the object files) do not give. `define_flag_selects_ifdef_branch`, `template_instantiated_by_another_file_is_analyzed` and `perf_data_types_mark_measured_hot_fields` pin `--target x86_64`, since on a Windows host `long` is correctly 4 bytes.

### Documentation
- README accuracy tables, Rust `repr` table, bit-field notes, "skips" table, known limitations and architecture table brought up to date with 0.25–0.57 (compiler-order layout, per-instantiation generics, Itanium inheritance, `repr(align)`, enum niches, MSVC and 32-bit x86 targets, new macros). CLAUDE.md generics and bit-field notes likewise.

## [0.57.0] — 2026-10-23

### Fixed
- **`padlock-lsp` ignored the new parse settings in `.padlock.toml`** (`defines`, `declared_order`, `[instantiate]`), so the editor could show a different layout than the CLI for the same file.

## [0.56.0] — 2026-10-23

### Fixed
- **Rust enums with a niche** (`enum E { X(Box<u8>), Y }`, `enum F { X(bool), Y, Z }`, `X(u32, bool)`) were given a separate tag byte; rustc stores the tag in the payload's spare bit patterns, so they are the payload's size (8 and 1 bytes, not 16 and 2).
- **Rust explicit discriminants** (`X = 1000`, `X = -1`) now widen the tag to the integer that holds them (2 bytes, not 1).
- **Go inline `struct { … }` types containing a local struct** (also inside arrays: `[2]struct{ i In }`) were sized with a pointer-sized placeholder for it; they are now synthetic layouts the nested-struct resolver sizes, like C's anonymous aggregates.

## [0.55.0] — 2026-10-23

### Changed
- `padlock init` documents the new `.padlock.toml` keys (`defines`, `declared_order`, `[instantiate]`, all `arch.override` values).

## [0.54.0] — 2026-10-23

### Changed
- README: new "How exact is it?" section (conformance and fuzzing, what is reported as uncertain, the exact modes, `-fno-eliminate-unused-debug-types` for binary analysis), `verify` and `ffi` commands, and the new `.padlock.toml` keys (`defines`, `declared_order`, `[instantiate]`, more `arch.override` values).

## [0.53.0] — 2026-10-23

### Fixed
- **`padlock verify` matched Go types across packages by short name** (a `regexp/syntax.Error` checked against `regexp.Error`, a type from an `_test.go` example against an unrelated one). Go types now match only the binary type of their own package (`pkg.Name` / `…/pkg.Name`, or `main.Name`), and test-only types are skipped. Checked against the Go standard library (`net/http`, `encoding/json`, `bufio`, `regexp`).
- `reflect.Value` is a known Go type (3 words).

## [0.52.0] — 2026-10-23

Found by checking padlock against the compiler on real code (glibc's own headers with `padlock verify`): analyzing a header on its own, without the headers it includes, silently produced wrong layouts that looked exact.

### Fixed
- **C/C++ fields of a type padlock cannot size** (a typedef from a header that was not analyzed, such as `in_port_t` or `__uid_t`) were silently given a pointer-sized placeholder and presented as exact. They are now opaque types: resolved if another analyzed struct defines them, otherwise listed in `uncertain_fields`.
- **Members written as macro invocations** (`__SOCKADDR_COMMON (sin_);`, `DECLARE_BITMAP(bits, 64);`) were silently dropped. The struct now lists ``macro member `…` (not expanded)`` in `uncertain_fields` (use `--compile-commands` or binary analysis for the expanded layout).

## [0.51.0] — 2026-10-23

### Added
- **`--perf-types <file>`** (`analyze`, `summary`, `check`, `list`): measured hot fields from Linux perf's data-type profiling (`perf mem record …` then `perf report --stdio -s type,typeoff`, perf ≥ 6.8). Fields that receive memory-access samples are treated as hot, so locality (hot/cold interleaving) and false-sharing analysis use what the program actually touches instead of guessing from type names.

## [0.50.0] — 2026-10-23

### Added
- **`padlock ffi <paths>…`**: checks every Rust `#[repr(C)]` struct against the C/C++ struct of the same name among the analyzed files — size, alignment, field count, and each field's offset and size by position — and exits 1 on any mismatch (`--json` available). Catches FFI drift such as a `u64` mirroring a C `unsigned int`.

## [0.49.0] — 2026-10-23

### Changed
- Rust `Mutex<T>` / `RwLock<T>` / `OnceLock<T>` / `Condvar` / `Once` / `Barrier` are sized with std's futex layout, which is the one Linux, Android and the BSDs use. On Apple and Windows (MSVC) targets std uses other primitives, so those fields are now listed in `uncertain_fields` instead of being presented as exact (use `--rust-exact` there).

## [0.48.0] — 2026-10-23

New: `tests/conformance/fuzz.py`, differential fuzzing of C/C++ layouts against gcc/g++ (random scalars, constant-expression arrays, bit-fields incl. `T : 0`, nested/anonymous aggregates, unions, enums, `packed`/`aligned`/`#pragma pack`, C++ inheritance). Before the fixes below: 483 mismatching fields in the first 30 C rounds. Now: **0 mismatches in 260 C rounds (~6,500 structs)**; C++ is down to a handful of mismatches per 60 rounds, all combining `packed` with inheritance.

### Fixed
- **Anonymous member aggregates (`struct { … } m;`, anonymous unions) were sized before their own members' types were known**, so one containing a named struct got a pointer-sized placeholder for it. They are now synthetic layouts the nested-struct resolver sizes like any type (shown as `struct { … }` / `union { … }`); records laid out earlier in a file are also known while parsing, so bit-field runs after them start at the right offset.
- **`#pragma pack` was ignored for unions and for anonymous aggregates defined inside the packed region.**
- **Explicit member alignment**: `aligned(N)` / `alignas(N)` survives the `packed` attribute (but not `#pragma pack`), can never lower a type's natural alignment outside a packed struct (`aligned(2)` on a `long double` is ignored), survives nested-struct resolution, applies inside unions, and a type-level `aligned(N)` wins over `#pragma pack`; a packed struct with an aligned member is padded to that alignment.
- **Bit-fields**: under `packed` or `#pragma pack` they may straddle unit boundaries; `T : 0` reserves the bytes up to the next `T` boundary even when packing or a smaller next type would pull the run back, including at the end of the struct and before an ordinary member (`struct { char a; long long : 0; char b; }` puts `b` at 8); a run is aligned by its *first* member's type; bit-field types no longer raise the alignment of a `packed` struct, and `#pragma pack` caps it.
- **C++ `packed`**: base subobjects and the vptr are not packed (but `#pragma pack` caps them); a member of non-POD class type keeps its alignment (GCC ignores `packed` for it) unless that type is itself packed; non-POD-ness propagates through members; a class's non-virtual alignment never exceeds its own (pragma-capped) alignment.
- `union U { … } __attribute__((aligned(N)))` kept its alignment only until a nested type was resolved.

### Changed
- `StructLayout` gained `explicit_align` (field names with an explicit alignment; omitted from JSON when empty).

## [0.47.0] — 2026-10-23

### Added
- **`#[padlock::assert_offsets(field = N, …)]`** — compile-time error if a listed field is not at the given offset (`core::mem::offset_of!`, Rust 1.77+); unknown field names are a compile error too. Pins FFI/on-disk layouts or a `padlock fix` reorder against drift.
- **`#[padlock::assert_align(N)]`** — compile-time error if `align_of` is not exactly `N`.

## [0.46.0] — 2026-10-23

### Added
- **`--rust-exact`** (`analyze`, `summary`, `check`, `list`): exact Rust layouts from rustc. Each Cargo package owning an analyzed `.rs` file is built once (into `target/padlock-rust-exact`, so the normal build cache is untouched) with `-Zprint-type-sizes`, enabled on stable through `RUSTC_BOOTSTRAP=1`; the field order, sizes and alignment rustc chose replace padlock's estimates — exact for `repr(Rust)` reordering, niche placement and std types. Refined structs are no longer flagged as estimates. Only types rustc generated code for are refined; enums keep padlock's model.

## [0.45.0] — 2026-10-23

### Added
- **`--go-exact`** (`analyze`, `summary`, `check`, `list`): exact Go layouts from the Go toolchain. A probe test is added to each package *virtually* (`go test -overlay`, `-vet=off`; nothing is written to the source tree) and prints `reflect`'s size, alignment and per-field offset/size for every struct padlock found — exact even for fields whose types come from other packages (`url.URL`, `driver.Connector`, third-party types), which source analysis can only flag as uncertain. Packages whose probe cannot build keep their source-level layouts, with a note.

## [0.44.0] — 2026-10-23

### Added
- **`--compile-commands compile_commands.json`** (`analyze`, `summary`, `check`, `list`): exact C/C++ layouts. Every translation unit is recompiled with its own flags, defines and include paths plus complete debug info (`-g -fno-eliminate-unused-debug-types`, and `-femit-class-debug-always` / `-fstandalone-debug`), and the layouts the compiler produced are read from the objects. This sees through macros, `#if` configuration, templates (every instantiation actually used) and platform ABI. Types declared under the project's source directory are reported; their source-level estimates are replaced.
- Binary analysis now reports **source file paths** for DWARF types (resolved through the line-program file table), and decl lines in any data form.

### Fixed
- `summary` applied `.padlock.toml` parse settings (`defines`, `declared_order`, `[instantiate]`) only after parsing.

## [0.43.0] — 2026-10-23

### Added
- **Windows x64 / MSVC ABI** (`--target x86_64-msvc`, `x86_64-pc-windows-msvc`, `msvc`, `windows`; the default for sources on a Windows MSVC host): `long` is 4 bytes (LLP64), `wchar_t` 2, `long double` 8, bit-fields of different declared types never share a storage unit (each unit is laid out like a field of its type), and C++ derived classes never reuse a base's tail padding. `x86_64-pc-windows-gnu` keeps the SysV layout.
- Conformance `--cross` checks the MSVC ABI against clang's own record-layout dump (`-fdump-record-layouts`, `-triple x86_64-pc-windows-msvc`): 47/47.

## [0.42.0] — 2026-10-23

The conformance harness gained `--cross`: C layouts for aarch64, riscv64, x86 (i386), Cortex-M4, Cortex-M0 and AVR (compiled with `zig cc`; truth read from symbol sizes), and Go/Zig on 32-bit x86. Before the fixes: x86 C 40/46, Cortex-M0 C and AVR C 0 (crash), x86 Go 26/36, x86 Zig 40/53. Now all match.

### Fixed
- **`--target` / `arch.override` / `--word-size` did not change how source files were laid out** — layouts were parsed for the *host* and only relabelled afterwards, so `--target cortex_m` still reported 8-byte pointers and `long`s. Sources are now parsed for the requested target (`analyze`, `summary`, `check`, `explain`), and the parse cache is keyed by it.
- **`padlock explain` panicked (division by zero)** for targets without a cache (`cortex_m`, `avr`).
- **C/C++ data model per target**: `int` is 2 bytes and `long` 4 on AVR, `long` is 4 on every 32-bit target, `long double` is 12 bytes/4-aligned on i386, 8 on 32-bit ARM and Apple AArch64, `double` is 4 bytes on AVR, `wchar_t` 2 on AVR, and natural alignment is capped at the target's maximum (`double`/`long long` are 4-aligned in i386 structs; everything is 1-aligned on AVR).
- **Go on 32-bit targets**: `int64`/`uint64`/`float64`/`complex128` are word-aligned (4 on 386/arm); `time.Time`, `sync.Map`, `sync.Cond`, `time.Month` sizes follow the word size.
- **Zig**: scalars up to 8 bytes follow the target's maximum alignment (`u64` 4-aligned on x86); `f80` is 16 bytes on x86-64 (was 10) and 12 on x86; `c_longdouble` is 12 on x86.

### Added
- `padlock explain --target <arch>`.

## [0.41.0] — 2026-10-23

### Fixed
- **C++ types with the same name in different namespaces or classes collided**: the nested-struct resolver keyed on the bare name, so a field of `nb::Same` could be sized as `na::Same`. C++ layouts are now named with their enclosing namespaces and classes (`net::Conn`, `Outer::Inner`), and field and base-class types are resolved the way C++ name lookup does (innermost scope first). Anonymous namespaces add no qualifier.
- **`typedef struct { … } T;` could rename the wrong anonymous struct** (the first anonymous layout in the file, e.g. an unrelated nested member type) instead of the one it declares.

### Changed
- **BREAKING (output)**: namespaced and nested C++ types are reported with their qualified name (`Outer::Inner`, `ns::Conn`).

## [0.40.0] — 2026-10-23

### Added
- **`padlock verify --binary <BIN> <sources>…`**: checks padlock's source-level layouts against the compiler's own, read from the debug info of a binary built from those sources. Reports every struct whose size or field offsets differ (`size-mismatch`, `offset-mismatch`; compiler-ordered Rust/Zig structs whose sizes agree but offsets differ are reported as `offsets-differ-estimate`, not failures) and exits 1 on any mismatch — a CI guard that source analysis can be trusted for a code base. Matches names across module/namespace qualifiers (`main.Conn`, `ns::Conn`) and template spacing; `--json` and `--filter` supported. On the conformance probes: C 51/51, C++ 57/57, Go 37/37.

### Fixed
- **Multi-dimensional arrays read from DWARF were sized by their first dimension only** (`short m[2][3]` came out as 4 bytes instead of 12), so padding after them was mis-reported. All subranges are multiplied, `DW_AT_lower_bound` is honoured, and an array's own `DW_AT_byte_size` (Go emits one) wins.

## [0.39.0] — 2026-10-23

The conformance harness now also checks **binary analysis**: each probe is built with debug info and padlock's DWARF reader is compared against the compiler (`dwarf-<lang>` rows, on by default). Before the fixes below: Go 0/36, Rust 0/60, C++ 39/48, Zig 30/53. Now: Go 36/36, C 50/50, C++ 45/48, Rust 59/60, Zig 53/53, C23 3/3 (the four remaining rows are template names spelled with their default arguments in DWARF; sizes agree).

### Fixed
- **Go binaries: every struct had zero fields.** Go emits `DW_AT_data_member_location` as `DW_FORM_data1` and member types as `DW_FORM_ref_addr` (references into another compile unit); padlock only accepted `udata`/`sdata` locations and unit-local references. Both are handled now (also Zig's cross-unit references).
- **DWARF 4/5 type units (`-fdebug-types-section`) produced no structs**, and `DW_FORM_ref_sig8` references were not followed. Type units are now read, and typedef names attached to type-unit structs are applied.
- **Split DWARF (`-gsplit-dwarf`) produced no structs.** The `.dwo` files named by the skeleton units are now located (via `DW_AT_comp_dir`, the binary's directory, or as given) and read.
- **Unions in binaries were not reported**, and a union member type was given alignment = size (`union { char c[13]; long l; }` aligned to 16 instead of 8), mis-placing every field after it.
- **C++ pointer-to-member fields were sized 0 bytes** from DWARF; they are one word (data member) or two (member function).
- **Zig packed structs** (described as a structure over a backing integer, with bit-offset members) were not reported.

### Added
- `tests/conformance/run.py --backend source|dwarf|both`.

## [0.38.0] — 2026-10-23

### Fixed
- **C23 `_BitInt(N)` fields were silently dropped** — tree-sitter-c/cpp do not know the type and error-recovered the declaration away. `_BitInt(N)` / `unsigned _BitInt(N)` / `signed _BitInt(N)` are now parsed and sized per the x86-64/AArch64 psABIs (1/2/4/8 bytes; wider ones in 8-byte chunks on x86-64, 16-byte chunks on AArch64).

### Added
- Conformance language `c23`, compiled with `zig cc -std=c23` (gcc < 14 has no `_BitInt`).

## [0.37.0] — 2026-10-23

### Changed
- CI lints test code too (`cargo clippy --workspace --all-targets -- -D warnings`); fixed the two lint failures that had accumulated in test code (`padlock-dwarf` BTF test, an unused `padlock-cli` test helper).

## [0.36.0] — 2026-10-23

### Fixed
- **C++ in `.h` headers was parsed with the C grammar**, so classes, templates, namespaced types, access specifiers and `std::` members in `.h` files were lost or mis-sized — common in C++ code bases, which mostly use `.h`. A `.h` file whose contents are C++ (`class`, `template<`, `namespace`, `public:`, `std::`, `virtual`) is now parsed as C++ by `analyze`, `fix`, `diff`, cross-file template discovery and `padlock-lsp`.
- More C++ extensions are recognised: `.hh`, `.hxx`, `.h++`, `.c++`, `.ipp`, `.tpp`, `.inl`, `.cppm`, `.ixx`.

### Added
- `padlock_source::detect_language_from_source(path, text)`.

## [0.35.0] — 2026-10-23

Generic types are analyzed **per concrete instantiation** instead of being skipped. Conformance: C++ 48/48, Rust 60/60, Go 36/36, Zig 53/53 (templates/generics included).

### Added
- **C++ class templates** are laid out once per instantiation used in the analyzed code: field types (`Buf<char, 8> b;`), `using B = Buf<int>;`, `template struct Buf<long>;` (explicit instantiation), nested arguments, and uses in **other files** of the same run (the defining file is re-parsed with those arguments). Type, non-type (`int N`) and defaulted parameters are bound (`class A = Alloc<T>` substitutes earlier arguments). Explicit specializations (`template<> struct Node<int> {…}`) are analyzed as concrete types and replace the primary template for those arguments; when a partial specialization exists, instantiations of the primary are marked uncertain. Variadic and template-template parameters still skip the template.
- **Rust generic structs** (`struct Pair<A, B>`, const generics, defaults), **Go generic structs** (`type Pair[K comparable, V any] struct`) and **Zig type-returning functions** (`fn List(comptime T: type) type { return struct {…}; }`, comptime value parameters too) are instantiated the same way from their concrete uses (`Pair<u8, u64>`, `Pair[int, string]`, `List(u8)`, `const L = List(u8);`).
- `[instantiate]` in `.padlock.toml` (also `[cpp.instantiate]`) names instantiations to analyze even when no code uses them: `Buf = ["char, 8", "int"]`. `// padlock:instantiate(...)` comments keep working.
- Fields whose type is an instantiation resolve to its layout (`Name<args>` / `Name[args]` / `Name(args)`, canonical spacing).
- A generic type with no instantiation anywhere is still skipped, with a note saying how to supply one.

### Fixed
- **C++ `using Name = Type;` aliases were ignored** (only C `typedef`s were resolved), so `using u32 = unsigned int;` fields were pointer-sized.
- Template parameters bound to a struct name (`Tag = Empty`) are shown and resolved as that struct.

## [0.34.0] — 2026-10-23

Conformance now also compares the **size** of `repr(Rust)` and plain Zig structs (previously not probed at all): Rust 55/55, Zig 50/50.

### Changed
- **BREAKING (output)**: Rust `repr(Rust)` structs and plain (non-`extern`/`packed`) Zig structs are laid out in the order the compiler actually uses — largest alignment first, ties in declaration order — instead of source order. Their reported size now matches the compiled program (`struct S { a: u8, b: u64, c: u8 }` is 16 bytes, as rustc makes it, not 24), so a `repr(C)` struct that embeds one is sized correctly too, and padding the compiler already removes is no longer reported as waste or offered as a reorder. Findings on these structs remain downgraded one severity level, since the order is not guaranteed (rustc may move a large-niche field to the front, so individual offsets are an estimate).
- `--declared-order` (or `declared_order = true` in `.padlock.toml`) restores the previous source-order view, e.g. to see the waste the declared order would have. It is part of the parse-cache fingerprint.

## [0.33.0] — 2026-10-23

Conformance: Rust 45/46 → 46/46.

### Fixed
- **A local struct inside a Rust tuple was pointer-sized** (`(In, u8)` where `In` is a local `repr(C)` struct). Local non-generic structs are now sized directly by the Rust frontend wherever they appear (tuples, arrays, fields), using the same layout padlock reports for them; the nested-struct resolver still cross-checks plain fields.

## [0.32.0] — 2026-10-23

### Fixed
- **`--stdlib libc++` / `msvc` was silently ignored for most files.** The setting was stored thread-locally on the main thread, but parsing runs on rayon worker threads, which kept the libstdc++ default — so `std::string`, `std::mutex` etc. were sized for the wrong standard library. It is now process-global.
- The parse-cache fingerprint includes the stdlib variant, so switching `--stdlib` re-parses instead of serving cached layouts.

## [0.31.0] — 2026-10-23

Conformance (`g++ -std=c++17`): C++ 27/28 → 42/42.

### Fixed
- **C++ inheritance is now laid out by the Itanium C++ ABI**, after base sizes are known, instead of a fixed "vptr, then bases, then members" guess:
  - **Tail-padding reuse**: members of a derived class start inside a non-POD base's tail padding (`class B { int a; char b; public: … }; struct D : B { char c; }` is 8 bytes, not 12). A base is non-POD when it has a user-declared constructor/destructor/copy assignment (not `= default`/`= delete`), a private/protected data member, a default member initializer, a base class, or a vtable.
  - **Empty base optimisation** and **`[[no_unique_address]]`**: empty bases and empty members marked `[[no_unique_address]]` take no space. Previously an empty base was an 8-byte placeholder (`struct D : Empty { int x; }` was 16 bytes, not 4).
  - **Virtual bases** (`struct B : virtual A`): the class gets a vtable pointer and each virtual base is placed once, after the non-virtual part of the most-derived class (diamonds included). Previously virtual bases were dropped.
  - **Primary base**: the first non-virtual *dynamic* base goes to offset 0 and shares its vptr, even when it is not declared first.
  - Empty classes are 1 byte in C++ (and 0 in GNU C) and are now emitted as layouts, so fields and bases of empty type resolve.
- The conformance harness can compare the offset of a named probe field instead of the last row.

### Known limitations
- `-std=c++20` treats a class with a defaulted constructor as non-POD for layout; padlock uses the C++17 rule.

## [0.30.0] — 2026-10-23

### Fixed
- **Struct-level alignment and packing were lost whenever a struct contained another struct.** After nested types are sized, the resolver re-lays-out the parent from its fields alone, dropping `alignas(N)` / `__attribute__((aligned(N)))` / `repr(align(N))`, bit-field-imposed alignment, and `#pragma pack(N)` / `repr(packed(N))`. `struct alignas(64) A { In i; char c; }` came out as 12 bytes instead of 64, and a `#pragma pack(2)` struct as 24 instead of 18. `StructLayout` now records `forced_align` and `pack_limit`, and re-layout honours them.
- **Zig packed structs nested in packed structs** were counted as pointer-sized bytes; they now contribute their exact bit width (enums too), so `packed struct { x: In, y: u8, z: In }` with a 5-bit `In` is a `u18` (4 bytes).

### Changed
- `StructLayout` gained two optional fields (`forced_align`, `pack_limit`, omitted from JSON when unset).

## [0.29.0] — 2026-10-23

### Fixed
- **C/C++ fields inside `#if` / `#ifdef` / `#else` blocks were silently dropped** — the struct-body walker only looked at direct `field_declaration` children, so a struct with configuration-dependent members was reported smaller than any real build. Conditionals are now resolved the way the preprocessor would: `#ifdef`/`#ifndef`/`#elifdef`, `#if`/`#elif` with `defined(X)`, arithmetic, comparisons and `&&`/`||` over `#define`d constants (undefined identifiers are 0, as in C). Only the compiled-in branch contributes fields. A macro counts as defined only after its `#define`, so header guards work.
- **Top-level code under `#if 0` (or the losing side of `#ifdef … #else`) was analyzed**, producing dead or duplicate struct definitions.

### Added
- `-D/--define NAME[=VALUE]` (repeatable) and `defines = [...]` in `.padlock.toml` select `#if` branches like a compiler's `-D`. When a branch choice depends on a macro neither the file nor the user defines, the struct lists `fields under `#ifdef X`` in `uncertain_fields`.
- The integer constant-expression evaluator understands `== != < > <= >= && || !`.
- The parse cache fingerprint includes the active defines, so changing `-D` re-parses.

## [0.28.0] — 2026-10-23

Conformance harness against `zig run` (`@sizeOf` / `@offsetOf`, zig 0.15): Zig 12/18 → 43/43. Overall: Go 33/33, C 48/48, C++ 24/25, Rust 43/44, Zig 43/43.

### Fixed
- **Zig array lengths were only understood as integer literals** (`[N * 2 + 1]u8`, `[N]u8` with a `const N`, `[@sizeOf(u64)]u8` fell back to pointer size). Lengths are now evaluated (consts, arithmetic, `@sizeOf`); an unevaluable length keeps the field and lists it in `uncertain_fields`. Arrays of local structs are scaled by the nested-struct resolver.
- **Enum-typed fields were sized as pointers.** Local `enum(u32)` / `enum` types now use their backing integer (default: the smallest `uN` holding every variant; a single-variant enum is zero-sized).
- **`align(N)` on a field was ignored.**
- **Packed struct size**: a packed struct is stored as one integer of its total bit count, so `packed struct { a: u8, b: u32 }` is a `u40` occupying **8** bytes, not 5 (alignment 8 too).
- **Arbitrary-width integers** (`u24`, `u48`, `u65`, `u129`) now use their ABI size/alignment (power-of-two byte size up to 16, then multiples of 16): `u24` is 4 bytes, not 3.
- **Error unions** were two words regardless of payload: `anyerror!u8` is 4 bytes, `anyerror!u32` 8, `anyerror!void` 2. `?[]T` carries no extra tag.

## [0.27.0] — 2026-10-23

Conformance harness against `rustc` (`repr(C)` structs, `size_of`/`offset_of!`): Rust 22/33 → 43/44 (the remaining case is a tuple holding a local struct).

### Fixed
- **Rust array lengths were only understood as integer literals**; `[u8; N]` with a `const N`, or `[u8; N * 2 + 1]`, silently became a **zero-length** array. Lengths are now evaluated: `const`s (including ones defined in terms of other consts), arithmetic, `as` casts, and `size_of::<T>()`. An unevaluable length is sized as one element and listed in `uncertain_fields`. Local `type Alias = T;` is resolved, and arrays of local structs (including nested arrays) are scaled by the nested-struct resolver.
- **Tuples** (`(u8, u32, u8)`) were pointer-sized; they are now laid out like rustc does (largest alignment first).
- **Fat pointers to `str` / `[T]`** (`&str`, `&[T]`, `Box<str>`, `Box<[T]>`, `Arc<str>`, `*const [T]`) were one word instead of two; `Option<…>` of them follows.
- **`Option<T>` of types with a niche** (`Vec`, `String`, `PathBuf`, `bool`, `char`, `Duration`, `Box<str>`, …) was inflated by a tag byte.
- **`Mutex<T>`, `RwLock<T>`, `OnceLock<T>`, `Condvar`, `Once`, `Range<T>`, `RangeInclusive<T>`, `mpsc::Sender/Receiver`, `CString`** were pointer-sized regardless of `T` (std futex implementation on Linux; macOS/Windows differ).
- **`#[repr(packed(N))]`** was treated as `packed(1)`; it now caps alignment at `N` and keeps trailing padding.
- **Fieldless enum discriminant width** ignored `#[repr(u16)]` / `#[repr(u32)]` / `#[repr(C)]` (always 1 byte), so a `repr(C)` enum field made its parent struct too small; a single-variant fieldless enum is zero-sized.

## [0.26.0] — 2026-10-23

Conformance harness (`tests/conformance/run.py`) against `go run` + `unsafe.Sizeof/Offsetof`: Go 9/26 → 33/33.

### Fixed
- **Go array fields were sized as 8 bytes** (`_ [48]byte` took 8, so every field after it was misplaced). The classic cache-line padding idiom therefore produced a wrong layout. Array lengths are now evaluated: literals, `const` (including `iota` groups and implicit repetition), arithmetic, and `unsafe.Sizeof(T{})` — so `_ [64 - unsafe.Sizeof(atomic.Int64{}) - unsafe.Sizeof(atomic.Uint64{})]byte` works. Arrays of local structs are scaled by the nested-struct resolver; an unevaluable length keeps the field and lists it in `uncertain_fields`.
- **Named non-struct types were sized as pointers**: `type Alias int16`, `type Vec [4]int32`, `type X = Y` now resolve to their underlying type.
- **Fields silently dropped**: inline `struct{…}` members, `func(...)` fields (the struct collapsed to 2 bytes), `atomic.Pointer[T]`, and parenthesised types. An embedded `*Base` was treated as the full `Base` instead of a pointer.
- **Grouped declarations** (`type ( A struct{…}; B struct{…} )`) analysed only the first struct.
- **Wrong stdlib sizes/alignments**: `complex64` align 4 (was 8), `sync.RWMutex` align 4 (was 8), `sync.WaitGroup` 16B (was 12), plus `sync.Pool`, `sync.Cond`, `atomic.Pointer`, `time.Duration`. Common stdlib interfaces (`io.Reader`, `context.Context`, `database/sql/driver.Connector`, …) are 16B and no longer reported as uncertain.
- A struct ending in a zero-size field gets Go's trailing padding byte.

### Changed
- The integer constant-expression evaluator moved from the C/C++ frontend into a shared `frontends/consteval` module (now also understands `unsafe.Sizeof`, `@sizeOf`, `size_of::<T>()`, qualified names, `_` digit separators).

## [0.25.0] — 2026-10-23

Found by the new layout conformance harness (`tests/conformance/run.py`), which compiles probe programs with the real compiler and diffs `sizeof`/`offsetof` against padlock's source-level layout. C went from 19/43 to 48/48 matching `gcc`, C++ from 12/25 to 24/25.

### Fixed
- **C/C++ silently dropped whole classes of fields**, so the reported struct was smaller and tidier than reality. Dropped before: fixed-size and flexible arrays (`char b[48]`, `char data[]`), enum-typed fields (`enum E e`, `enum class E : uint8_t`, and a bare `E` naming an enum declared elsewhere), function-pointer fields (including arrays of them), C++ references (`T&`, `T&&`), and `_Atomic(long)`. A `char a; char b[48]; long c;` struct was reported as 16 bytes instead of 64. All are now laid out; pointer-to-member-function is two words.
- **Array lengths are evaluated**, not guessed: integer literals, `#define`d and `enum` constants, `const`/`constexpr` integers, `sizeof(type)`, and arithmetic over them (`char buf[N * 2 + 1]`). A length that cannot be evaluated keeps the field (sized as one element) and lists it in `uncertain_fields`. Arrays of named structs (`struct In b[2]`) are scaled by the nested-struct resolver.
- **Bit-fields followed by smaller fields were over-sized.** The old model gave each bit-field run a whole storage unit, so `unsigned a:3, b:5; char c;` came out at 8 bytes with `c` at offset 4; `gcc` gives 4 and 1. Bit-fields are now packed bit by bit across declared types, never straddle their type's unit boundary, honour `T : 0`, and raise the struct's alignment to the largest named bit-field type (verified against `gcc`).
- **`int a, b;` declared only the last name**, `static` data members were counted as fields, and a named anonymous-struct member (`struct { … } n;`) was flattened into its parent instead of being laid out as one member.
- **Bare `unsigned` / `signed` (and `long unsigned`) were sized as pointers (8B)** instead of 4B, and `double _Complex` lost its name and was sized as a single `double`.
- **Struct-level `__attribute__((aligned(N)))` written after the closing brace** was ignored.
- **C++ `struct D : Base`** (not just `class`) now includes its base-class subobjects.
- **The on-disk parse cache (`.padlock-cache/`) was keyed only on path + mtime, so upgrading padlock kept serving layouts parsed by the old version** until each file was edited — any parser fix was invisible on a project that had been analyzed before. The cache now records the padlock version that wrote it and is discarded on mismatch.

### Added
- `tests/conformance/` — layout conformance harness for C, C++, Rust, Go and Zig (`python3 tests/conformance/run.py`, needs the relevant toolchains; missing ones are skipped). Known gaps are tracked in `xfail.txt`; a gap that gets fixed must be removed from it.

### Known limitations
- C++: Itanium-ABI reuse of a non-POD base's tail padding is not modelled (`struct D : B { char c; }` where `B` has trailing padding).

## [0.24.0] — 2026-10-21

### Fixed
- **Every Rust struct with 2+ `Arc<T>` fields on the same cache line got a High-severity "FalseSharing" finding and a 30-point score hit, regardless of what's inside the `Arc`.** The Rust frontend records only the outer generic name (`"Arc"`) for `Arc<T>`/`Rc<T>` fields, so the concurrency heuristic's `Arc` substring match couldn't tell `Arc<Mutex<T>>` (a real lock) apart from `Arc<String>`/`Arc<Config>`/`Arc<dyn Trait>` (plain shared ownership, no synchronization at all — one of the most common patterns in real Rust code). `struct Config { name: Arc<String>, logger: Arc<Logger> }` scored 70/100 with a High finding for no actual reason. Fixed by recording the inner type's leaf name too (`Arc<Mutex>`, `Arc<String>`) and matching the lock/atomic substrings against *that*, not the bare `Arc` wrapper. `Arc<Mutex<T>>`/`Arc<AtomicUsize>` etc. are still correctly flagged — verified both directions against the real binary.

### Changed
- **BREAKING (output)**: scores and findings change for any Rust struct holding 2+ `Arc<T>`/`Rc<T>` fields that don't wrap an actual lock/atomic type — such structs now correctly score higher with no spurious `FalseSharing` finding.

## [0.23.0] — 2026-10-21

### Fixed
- **Per-struct caveat note said "repr(Rust)" for a plain Zig struct.** `is_repr_rust` is shared between Rust `repr(Rust)` structs and plain (non-`extern`/`packed`) Zig structs — both have compiler-defined, not fixed, field order — but the rendered note unconditionally used Rust's own term, which means nothing in Zig and actively misleads a Zig-only user about which language the caveat even applies to. Now picks the wording from the struct's source file extension.

### Changed
- **README's top-level language-support table flattened five very different accuracy guarantees into uniform checkmarks.** C and Go are genuinely ABI-fixed from source alone (no compiler invoked, and none needed); `extern`/`packed` Zig and `repr(C)`/`packed`/`transparent` Rust are too. But plain Zig structs and Rust's default `repr(Rust)` have no layout guarantee at all — the compiler may reorder freely — and C++ has a hard structural ceiling (templates can't be sized from source without monomorphisation) regardless of how carefully everything else is modeled. The table and a `padlock fix` ABI-safety feature-row description previously said "C, Go, Zig" as if uniformly fixed-layout, which isn't true for a plain Zig struct. Rewritten to show the real accuracy tiers per construct, not per language.

Both found during an audit of whether padlock communicates its own confidence level clearly for each language when only source files (no compiled binary) are analyzed.

## [0.22.0] — 2026-10-21

### Fixed
- **`padlock watch --json` injected a terminal screen-clear escape sequence into stdout on every re-analysis, even when piped.** `clear_terminal()` was called unconditionally before each refresh — if `--json` output were piped into a consumer (or human-mode output redirected to a file/log), the raw `\x1b[2J\x1b[H` bytes would land directly in the stream, corrupting it. Found via a deliberate audit against the Unix philosophy ("the output of every program should become the input to another, as yet unknown, program"). Now gated on `!json && stdout.is_terminal()`.

## [0.21.0] — 2026-10-21

### Fixed
- **Scorer silently docked 10 points from ordinary, non-concurrent structs with no finding shown to explain why.** The "potential false sharing" tier called `false_sharing::find_sharing_conflicts`, which flags *any* two fields sharing a cache line regardless of access pattern — true of nearly every struct under 64 bytes, concurrency or not. A perfectly packed, zero-waste `struct { long a; long b; }` scored 90, not 100, with zero findings listed. Replaced with a new `has_potential_sharing_risk` that requires at least one field in the shared cache line to actually be `Concurrent`. Also added the `reorder_deduction` field that was missing from `ScoreBreakdown` (currently unused outside tests, but was silently inconsistent with `total`).

### Changed
- **BREAKING (output)**: struct scores change for any non-concurrent struct that previously lost 10 points to the bug above — scores for such structs are now higher (correctly) and match what the displayed findings justify.

Found via a deliberate audit of the scorer's internal consistency, verified against the real binary before and after the fix.

## [0.20.0] — 2026-10-10

### Changed
- **Audited the BTF binary parser (`padlock-dwarf/src/btf.rs`) for the same unchecked-offset panic class fixed in the C/C++ frontend this cycle.** Found it already correctly bounds-checked at every slice site (header length check, the per-iteration loop guard, explicit `ARRAY`/`STRUCT`/`UNION` member-count guards, and `read_btf_str`'s own offset check). Added 4 permanent adversarial regression tests — truncated header, overflowing `type_off`/`type_len`, a lying `vlen` with no member bytes present, and a name offset past the string table — all confirmed to error gracefully rather than panic. No behavior change; this closes out the "are there other situations where padlock could break" follow-up from the C++ panic fix.

## [0.19.0] — 2026-10-10

### Fixed
- **C/C++ analysis panicked on non-ASCII characters in comments**: `parse_class_specifier`/`parse_struct_or_union_specifier`'s `__declspec(align(N))` fallback scans a fixed 256-byte look-back window before the struct's start byte (`start_byte.saturating_sub(256)`), raw arithmetic with no awareness of UTF-8 char boundaries. A multi-byte character anywhere in that window (an em dash, a box-drawing separator like `─`, an arrow `→` — exactly the kind of "pretty" divider comment real codebases use) could land the window's start inside it, panicking with `byte index N is not a char boundary`. Since the VS Code extension and LSP both shell out to (or embed) this frontend, this surfaced as "analysis failed"/"doesn't analyze" for an otherwise perfectly ordinary file, with no indication the cause was a stray `─` in a comment three structs up. Fixed with a `floor_to_char_boundary` helper that nudges the window start forward to a valid boundary; both occurrences of the pattern fixed.

## [0.18.0] — 2026-10-10

### Fixed
- **VS Code extension: on-save analysis failures were silently swallowed**: `analyzeFile()`'s error handler only surfaced a message for `ENOENT` (executable not found); any other failure — most importantly a CLI flag the resolved `padlock` binary doesn't recognise yet (e.g. a version-skewed install predating `--no-cache`, added in `0.14.0`) — just reverted the status bar with zero indication analysis had stopped working at all. Root-caused live: the globally `cargo install`-ed `padlock` on this machine's PATH was a stale `0.12.0` build, so every on-save call since `--no-cache` was wired in has been failing with `error: unexpected argument '--no-cache' found`, invisibly. Now shows a warning with the actual error for any non-`ENOENT` failure.

## [0.17.0] — 2026-10-10

### Fixed
- **`padlock diff --against <git-ref>` was broken on Windows**: `git_show()` canonicalized the analyzed path and stripped the repo root off the front to get a git-relative pathspec, but `Path::canonicalize()` on Windows always returns a `\\?\`-verbatim path, which never matches the plain path `git rev-parse --show-toplevel` prints — `strip_prefix` failed on every single invocation, not just the already-fixed "run from outside the repo" case. Now strips the `\\?\` prefix before comparing.

## [0.16.0] — 2026-10-10

### Fixed
- **`padlock diff --against <git-ref>` resolved the wrong git repository**: `git_repo_root()` ran `git rev-parse --show-toplevel` using the CLI process's own working directory rather than the analyzed path's directory. Invoking it from outside the target file's repo (the common case for an editor integration) silently resolved a different repo — or none — so `git show` failed for every file and every struct was misreported as newly added instead of showing its real size/score change. Now anchored on the first analyzed path.
- **VS Code extension: CodeLens showed a `command '' not found` error on click** for structs with nothing to reorder (clean structs, or findings with no applicable fix): the lens was given an empty-string command instead of either a real command or none. Now reuses the `padlock.analyzeFile` re-analyze command for that case — a `CodeLens` with no command at all would render no text, since this provider doesn't implement `resolveCodeLens`.
- **VS Code extension: on-save analysis wrote a useless `.padlock-cache/`** in every saved file's directory — the `--no-cache` flag (added in `0.14.0`) existed but was never actually passed by the extension's single-file analysis call. Now wired in; the workspace-wide scan still uses the cache, since that one does benefit from it.

## [0.15.0] — 2026-10-10

### Fixed
- **VS Code extension: stale status bar tooltip**: the "not yet analyzed" branch reset the status bar's visible text but not its tooltip, so hovering over it could show a real score/finding count left over from a previous update while the text itself already read the neutral `padlock` state — misleadingly implying the file had been analyzed when it hadn't.

## [0.14.0] — 2026-10-10

### Added
- **`--no-cache`** flag on `analyze`/`summary`/`list`/`check`/`explain`: skips reading from and writing to `.padlock-cache/` entirely. Intended for single-file invocations (e.g. an editor's on-save analysis) where the cache can never be reused and only adds disk I/O and a stray cache directory.
- **C++ `// padlock:instantiate(Args...)` template instantiation**: name concrete type arguments directly above a `template<...>` declaration to get a real, sized layout for that instantiation instead of a skip note — `// padlock:instantiate(float)` above `template<typename T> struct Vector3 { T x, y, z; }` produces an analyzed `Vector3<float>`. One line per desired instantiation; multi-parameter templates list arguments positionally. Verified against real `g++ -g` DWARF output for ordinary aggregates.
- **`padlock diff --against <git-ref>`**: compares struct layout between a git ref and the working tree (size, padding, score — not the existing declared-vs-optimal-order diff), so a size/alignment/field change on a `#[repr(C)]`/`extern` struct shows up directly in a pre-merge check instead of hiding in a plain source diff.
- **VS Code extension: CodeLens and line highlighting**: each struct's declaration line gets a clickable `padlock: score N · MB wasted` CodeLens (click to apply the reorder fix directly) and a severity-tinted background (red for High, amber for Medium), so problem structs are visible while scrolling without opening the Problems panel.

### Fixed
- **`padlock-cli`/`padlock-lsp` crates.io packages shipped the wrong README**: both inherited `readme.workspace = true`, which resolves to the root `README.md` — two directories outside the crate's own package root, so `cargo package` silently falls back to whatever stray `README.md` happens to sit in the crate directory instead (a year-old stub for `padlock-cli`; nothing at all for `padlock-lsp`). Both now point `readme` at an in-package file; `padlock-lsp` got one. This fix lands in `0.14.0` — the `0.13.0` package already published to crates.io still has the stale README; crates.io versions are immutable, so it can't be corrected in place.
- **`collect_layouts` silently dropped duplicate-line structs**: the final dedup pass keyed on `(source_file, source_line)` only, so two distinct C++ template instantiations declared on the same line (same template, different `padlock:instantiate` args) collapsed into one. Dedup key now includes the struct name.

## [0.13.0] — 2026-10-10

### Added
- **`padlock-lsp` document symbols** (`textDocument/documentSymbol`): one outline entry per struct (`name` + `N bytes · score X` as `detail`).
- **`padlock-lsp` editor-pushed config**: `initializationOptions` and `workspace/didChangeConfiguration` can now set `minSeverity`/`arch` directly from the editor, layered on top of `.padlock.toml` — a settings change re-analyzes every open document immediately, with no need to touch a repo file. Diagnostics also tag Low-severity findings with `DiagnosticTag::UNNECESSARY` so supporting clients render them faded.
- **`padlock-lsp` `.padlock.toml` honoring**: `ignore`, `min_severity`, and `arch` overrides from the config file are now applied on every analysis pass, matching the CLI exactly.
- CI: `cargo-nextest` for process-isolated, cross-binary parallel test runs, and a `cargo-semver-checks` job that fails a PR on breaking public API changes against the last tag.

### Fixed
- **`padlock --version` showing `unknown`** for a `cargo install padlock-cli` from crates.io: `build.rs` only tried `git rev-parse`, which fails outside a git checkout (crates.io strips `.git` from published packages). Now falls back to the commit SHA recorded in `.cargo_vcs_info.json`.
- Release names and notes are now populated from the matching `## [X.Y.Z]` section of `CHANGELOG.md` instead of being left blank.

### Changed
- **`padlock-core::config`**: moved from `padlock-cli` so `padlock-lsp` can depend on it without depending on the CLI crate. No behavior change.
- Release profile: added `panic = "abort"` and `codegen-units = 1` for smaller, faster shipped binaries.
- Added a `commit-msg` git hook (`.githooks/`) rejecting AI attribution trailers in commit messages.

## [0.12.0] — 2026-10-10

### Added
- **`padlock-lsp` code actions**: quick-fix reorder support — "Reorder `Name` fields" for the struct at the requested range, and "fix all reorder suggestions in file" when more than one struct needs it. Reuses `padlock_source::fixgen::apply_fixes_*` (the same functions the CLI's `fix` command uses) on the cached live buffer text; no subprocess, no disk write. Returns a whole-document `WorkspaceEdit`, matching the VS Code extension's own whole-document-replace approach.

### Changed
- **`padlock-lsp` per-document cache** now holds the live buffer text and detected language alongside the analysis results (`DocState { text, lang, structs }`), not just the scored `StructReport`s — needed so code actions can produce a fix without re-requesting the document from the client.

### Dependencies
- `gimli` 0.29 → 0.34. `attr_value()`/`attr()` dropped their `Result` wrapper (now plain `Option`); `next_dfs()`/`next_sibling()` dropped the depth-delta from their return type. Both are mechanical signature changes with no logic impact — verified against the existing 17 extractor unit tests (unchanged) plus real compiled binaries (padded struct, bitfield struct, C++ single inheritance) producing identical results to before the bump. Benefits: overflow checks on LEB128/indexed-offset decoding (padlock's own bitfield-offset logic decodes exactly these forms) and faster LEB128/debug-line parsing.

## [0.11.0] — 2026-10-10

### Added
- **`padlock-lsp`**: a new standalone Language Server Protocol binary for editors without a bespoke padlock integration (Neovim, Helix, Zed, JetBrains, Sublime). Calls `parse_source_str` directly, in-process, on the live buffer — no subprocess, no disk write. Diagnostics (`didOpen`/`didChange`/`didSave`) and hover for the same five source languages the CLI supports. `cargo install padlock-lsp`.
- **`Report::aggregate_score` / `aggregate_grade`**: the weighted-by-size aggregate score and A–F letter grade (same formula as `padlock summary`) are now computed once in `padlock-core` and included in every `--json` response, rather than being recomputed independently by consumers.

### Fixed
- **`action.yml` shell injection and stderr-corrupted JSON**: every `${{ inputs.* }}` value was interpolated directly into `run:` bash blocks — unquoted and unsandboxed, so a `path`/`target`/`padlock-version` input containing shell metacharacters executed as code. Moved to `env:` vars. Also `PATHS=(${{ inputs.path }})` word-split on spaces despite the newline-separated input contract; switched to `readarray` on newlines only. Also dropped `2>&1` on the JSON-collection step — padlock's skip-notes go to stderr by design, and merging them corrupted the JSON silently instead of failing loudly.
- **VS Code: `vsce package` was broken outright** — dependabot bumped `@types/vscode` past `engines.vscode` without updating the floor, and `vsce` refuses to package when that happens. Downgraded the devDependency back in range; no API newer than `engines.vscode` was actually in use.
- **VS Code: multi-root workspace used the wrong `.padlock.toml`** — `runCommand`'s `cwd` always used `workspaceFolders[0]`, not the folder actually containing the analyzed file. Added `resolveCwd()`.
- **VS Code: `fixStruct`/`fixStructPreview` commands undeclared** — reachable via code actions but missing from `contributes.commands`, so they couldn't be rebound or found in the keybindings UI.
- **VS Code: hand-duplicated score formula** — the status bar reimplemented the weighted-score/grade formula in TypeScript independently of `padlock-core`'s. Now reads `aggregate_score`/`aggregate_grade` straight from JSON for any single-file-scoped response (every on-save analysis); keeps an explicitly-labeled local fallback only for files whose score has only ever arrived via a multi-root `analyzeWorkspace` scan.
- **Homebrew formula missing binaries**: `install do` only ran `bin.install "padlock"` — `cargo-padlock` was never installed via brew either, and the new `padlock-lsp` wouldn't have been. All three now installed.
- **`docs/extending.md` repository-map tree** was missing the new `padlock-lsp` entry.
- **`padlock-lsp` process hang on `exit`**: `main()` called `io_threads.join()` while `connection` (and its live sender) was still in scope, so the stdout writer thread never saw every sender drop and the process never exited. Fixed by dropping `connection` first.

### Changed
- CI now builds and attaches the VS Code extension `.vsix` to each GitHub release (`build-vscode-extension` job). This does **not** publish to the Marketplace — PAT-based `vsce publish` auth is currently broken on the Marketplace's side; publish the attached `.vsix` manually via the web UI until that's fixed upstream.
- Release artifacts (all platform tarballs/zips) now include `padlock-lsp` alongside `padlock`/`cargo-padlock`.

### Dependencies
- `tree-sitter` (core runtime) 0.25 → 0.27 — 0.25.3 fixed an infinite loop during parser error recovery, relevant now that `padlock-lsp` re-parses malformed mid-edit source on every keystroke. Grammar crates (`tree-sitter-{c,cpp,go,zig}`) unchanged.
- `syn` 3.0.3 → 3.0.6 (trivial patch; no API change).

## [0.10.10] — 2026-08-01

### Added
- **i686/x86_32 architecture**: `x86`, `i386`, `i586`, `i686` short names and `i686-*` / `i386-*` / `i586-*` target triples now resolve to an `X86_32` arch config with 4-byte pointer, 64-byte cache line, and `max_align = 4` (Linux SysV ABI — `double` and `long long` are 4-byte aligned in structs).
- **C++ `struct` keyword virtual dispatch pointer**: C++ structs with `virtual` methods now get the same hidden `__vptr` field injected at offset 0 as classes. Previously, vptr injection only applied to `class`-keyword types; structs with `virtual ~Foo()` or `virtual void bar()` were silently undersized by 8 bytes.
- **MSVC `__declspec(align(N))` support**: `__declspec(align(N))` on a struct type or on individual fields is now parsed and applied as an alignment override, matching the existing support for GCC/Clang `__attribute__((aligned(N)))` and C++11 `alignas(N)`. Handles the case where `ms_declspec_modifier` appears as a sibling of the struct node in the AST (not a child).
- **`std::variant<T1, T2, ...>` sizing**: parametric sizing via a new bracket-aware template argument splitter (`split_template_args`). Storage = `max(sizeof(T...))` padded to `max(alignof(T...))`, plus a 4B discriminant, total padded to effective alignment. Also handles deeply-nested args like `std::variant<std::map<int,int>, double>`.
- **`std::pair<T1, T2>` sizing**: sequential struct layout of two fields with proper alignment.
- **`std::tuple<T1, T2, ...>` sizing**: approximate sequential layout (same total size as struct-of-fields).
- **`std::array<T, N>` sizing**: `N × sizeof(T)` with `alignof(T)` — now computed from template args instead of falling through to pointer-size.
- **Zig `@Vector(N, T)` sizing**: SIMD vector types are now sized as `N × sizeof(T)` with SIMD-aligned alignment (`size.next_power_of_two().min(64)`). Tree-sitter `builtin_function` nodes are now recognized in both `parse_container_field` and `type_node_size_align`.
- **`LocalityIssue` split suggestion**: all output formatters (human summary, SARIF, Markdown report, `explain`) now include a concrete split suggestion when a locality issue is detected. Example: `Split: struct FooHot { a, b } / struct FooCold { c, d }`.
- **More `std::` type sizes**: `std::thread` (8 words), `std::jthread` (9 words), `std::complex<T>` (2 × sizeof(T)), `std::bitset<N>` (ceil(N/64) × 8 bytes), `std::chrono::duration<Rep, Period>` and `std::chrono::time_point<Clock, Duration>` (sized by Rep/Duration's underlying type), `std::initializer_list<T>` (2 words: pointer + size), `std::filesystem::path` (32B on 64-bit).
- **Go `sync.Map`, `strings.Builder`, `bytes.Buffer` sizing**: all three previously fell to `uncertain_fields` as unrecognised qualified names. Added to the known-type table with correct 64-bit sizes (`sync.Map` = 32B, `strings.Builder` = 32B, `bytes.Buffer` = 40B).

### Fixed
- **C++ fix generator method preservation**: method declarations inside `class` or `struct` bodies (e.g. `void foo();`, `virtual ~Foo();`) were silently dropped when the reorder fix was applied. The C chunk extractor now tags each chunk as either `Field` (reorderable) or `Passthrough` (method or other non-field text). Passthrough chunks are emitted in their original relative positions around the reordered fields, so class bodies are structurally preserved after a fix.

## [0.10.9] — 2026-08-01

### Security
- **`js-yaml`** — forced to `>=4.3.0` via npm override; YAML merge-key chains could cause quadratic CPU consumption (GHSA-mh99-v99m-4gvg equivalent, transitive via `@vscode/vsce` → `@secretlint`).
- **`fast-uri`** — forced to `>=3.1.4` via npm override; host confusion via literal backslash authority delimiter and failed IDN canonicalization (transitive via `@vscode/vsce` → `ajv`).
- **`brace-expansion`** — forced to `>=5.0.8` via npm override; DoS via unbounded expansion length (transitive via `@vscode/vsce` → `minimatch`).
- **`linkify-it`** — bumped to `5.0.2` via dependabot PR #42; quadratic-complexity DoS via `mailto:` validator scan-loop (transitive via `@vscode/vsce` → `markdown-it`).

## [0.10.8] — 2026-07-31

### Fixed
- **DWARF5 bitfields (`DW_AT_data_bit_offset`)**: modern GCC (12+) emits bitfield members using `DW_AT_data_bit_offset` (an absolute bit offset from the struct start, DWARF5) instead of the older `DW_AT_data_member_location` (byte offset, DWARF4). The extractor only handled the DWARF4 form, so DWARF5 bitfield members were silently skipped — their occupied bytes appeared as spurious padding gaps. Fixed by falling through to `DW_AT_data_bit_offset` when `DW_AT_data_member_location` is absent, handling `DW_FORM_data1`/`data2`/`data4`/`data8`/`udata`/`sdata` encodings, and deriving the storage-unit size from the bit span (tracking `max_bit_exclusive` per group) when `DW_AT_byte_size` is absent — also typical for DWARF5 groups. Also handles `DW_AT_bit_size` encoded as `DW_FORM_implicit_const` (decoded by gimli as `Sdata` rather than `Udata`). Eliminates false gap reports on structs like SQLite's `VdbeCursor`.
- **`DW_TAG_class_type` for C++ classes**: C++ `class` declarations use `DW_TAG_class_type` instead of `DW_TAG_structure_type`. Previously the DWARF extractor (both `extract_from_unit` and `type_resolver`) only processed `DW_TAG_structure_type`, so binary analysis of C++ binaries silently omitted all classes declared with the `class` keyword. Added `DW_TAG_class_type` alongside `DW_TAG_structure_type` in both places.
- **`DW_AT_alignment` on struct and field DIEs**: explicit `alignas(N)` / `__attribute__((aligned(N)))` on a struct type declaration produces `DW_AT_alignment` on the struct DIE; on a field declaration it produces `DW_AT_alignment` on the member DIE. Neither was read — alignment was always inferred from the maximum field type alignment, which can understate the actual alignment and affect false-sharing analysis. `extract_struct` now reads the struct-level `DW_AT_alignment`; `extract_field` now reads the member-level `DW_AT_alignment` and takes the maximum with the type alignment.
- **Zig regular struct `is_repr_rust` flag**: regular Zig structs (not `extern` or `packed`) have implementation-defined field order — the compiler may reorder fields just like Rust's `repr(Rust)`. Previously `is_repr_rust` was hardcoded to `false` for all Zig struct variants, so padding/reorder findings were reported at full severity even on layouts the compiler had already optimised. `extern struct` and `packed struct` continue to receive full-severity findings (their layouts are fixed and ABI-stable); regular structs now have findings downgraded one level, matching the existing behaviour for plain Rust structs.
- **Rust structs/enums with only lifetime generic parameters**: structs like `struct Cache<'a> { ptr: &'a u64, len: usize }` were skipped alongside genuinely-unanalyzable type-generic structs (`struct Foo<T>`). Lifetime parameters (`'a`) do not affect struct layout at all, so the check now distinguishes `GenericParam::Lifetime` from `GenericParam::Type` / `GenericParam::Const`. Structs and enums with only lifetime params are now analyzed normally; those with type or const params continue to be skipped.
- **Go `sync.WaitGroup` and `time.Time` sizing**: both types went to `uncertain_fields` as unrecognised qualified names. `sync.WaitGroup` (`state atomic.Uint64` + `sema uint32` = 12B, align 8) and `time.Time` (`wall uint64` + `ext int64` + `loc *Location` = 24B, align 8) are now hardcoded in the known-type table alongside the existing `sync.Mutex`/`sync.RWMutex`/`atomic.*` entries. Both are stable under Go's compatibility guarantee.
- **Rust collection type sizing corrections**: `VecDeque<T>` was sized as 24B (3 words) but is actually 32B (4 words: `head + len + ptr + cap`); `HashMap<K,V>` and `HashSet<V>` were 24B but are 48B on 64-bit (hashbrown `RawTableInner` = 4 words + `RandomState` = 2 words). `BTreeMap`/`BTreeSet` were already correct at 24B.
- **`RefCell<T>` source sizing**: `RefCell<T>` was sized as pointer_size (8B) regardless of `T`. It is now modelled as `isize` borrow counter (pointer_size at offset 0) + `UnsafeCell<T>` (T at `next_multiple_of(pointer_size, T_align)`), matching measured sizes: `RefCell<()>`=8B, `RefCell<u8>`=16B, `RefCell<u64>`=16B.
- **Rust unit type `()` field sizing**: `()` as a field type fell through to the pointer-size catch-all and was sized as 8B instead of 0B. Added an explicit `Type::Tuple` branch for empty tuples. This was a latent bug that materialized via `RefCell<()>` and `Option<()>`.
- **DWARF C++ inheritance (`DW_TAG_inheritance`)**: C++ class inheritance emits a `DW_TAG_inheritance` child DIE in DWARF; previously the extractor only processed `DW_TAG_member` children, so base-class bytes were invisible. A derived class's inherited fields appeared as phantom padding, and the reorder pass produced nonsensical suggestions like "24B → 4B" on a `struct Derived : Base`. Base-class subobjects are now extracted as synthetic `[BaseName]` opaque fields at their correct byte offset. Virtual base classes (`DW_AT_virtuality != 0`) are skipped as their offsets depend on vbtable thunks that cannot be statically modelled.
- **PDB C++ inheritance (`LF_BCLASS`)**: same gap in the PDB reader — `TypeData::BaseClass` entries in a FieldList were silently ignored alongside static members. Non-virtual base classes are now extracted as synthetic `[BaseName]` opaque fields at their `offset` byte. `TypeData::VirtualBaseClass` (`LF_VBCLASS`/`LF_IVBCLASS`) entries are still skipped for the same vbtable reason.

## [0.10.7] — 2026-07-30

### Fixed
- **DWARF `DW_TAG_atomic_type` (C11 `_Atomic T`)**: the qualifier wrapper tag was not in padlock's passthrough arm, so it fell to the catch-all which read `DW_AT_byte_size` on the wrapper itself — absent on qualifier tags — and sized the field as 0 bytes. The gap after it then appeared inflated by the real field size. Added to the `const`/`volatile`/`restrict` passthrough arm; the inner type is resolved correctly now.
- **DWARF `DW_TAG_rvalue_reference_type` (C++ rvalue references)**: rvalue reference fields (`T&&`) fell to the `_` catch-all and were sized as 0 bytes. Added to the pointer arm alongside `DW_TAG_reference_type`; sized as `pointer_size` on all architectures.
- **C/C++ source `_Atomic(T)` function-form**: tree-sitter represents the `_Atomic(T)` specifier form as an `atomic_type` node (distinct from the `type_qualifier` keyword form). The parser did not handle this node, so the field type was lost and the field was dropped entirely. Added an `atomic_type` handler that extracts the wrapped base type.
- **C/C++ source `__attribute__((aligned(N)))` on fields**: tree-sitter wraps a field identifier that carries `__attribute__((...))` in an `attributed_declarator` node. The old parser only checked direct children of `field_declaration`, so the field was silently dropped when an attribute was present. Added an `attributed_declarator` handler that extracts both the identifier and the attribute text; added `extract_aligned_from_c_field_text` to parse `aligned(N)` / `__aligned__(N)` from attribute text.
- **C/C++ source anonymous nested union sizing**: anonymous `union { … }` members inside structs were previously flattened sequentially into the parent (wrong offsets, wrong total size). They are now treated as a single synthetic `__anon_union<size,align>` field sized as `max(variant sizes)` rounded to alignment — correct ABI behaviour.
- **Go source `sync.Mutex`, `sync.RWMutex`, `sync.Once`, `atomic.*` sizing**: qualified external types that are concrete structs with well-known sizes were treated as unknown opaque types (sized as one pointer) and added to `uncertain_fields`. Added a lookup table for common `sync` and `atomic` package types with their correct sizes; these are no longer added to `uncertain_fields`.

## [0.10.6] — 2026-07-20

### Fixed
- **Zig frontend `question_mark` lint**: the `else if let Some(un) = union_node { ... } else { return None }` pattern in the type-node dispatcher has been rewritten as idiomatic `let un = union_node?`, satisfying `clippy::question_mark`.

### Security
- **`fast-uri`** 3.1.0 → 3.1.2: addresses GHSA-v39h-62p7-jpjc and GHSA-q3j6-qgpj-74h6 (VSCode extension transitive dependency).
- **`qs`** → 6.15.2 and **`tmp`** → 0.2.7: security patches for VSCode extension dependencies.
- **`markdown-it`**: bumped to patched version in VSCode extension.

### Maintenance
- **`dependabot.yml` added**: automatic weekly dependency updates for Cargo, npm (VSCode extension), and GitHub Actions, with `rebase-strategy: auto` to keep PR branches rebased on main.
- **Explicit CodeQL workflow**: `.github/workflows/codeql.yml` added with `security-events: write` permission, replacing GitHub Default Setup which lacked that permission on PR heads from forks.
- **`padlock-example.yml` updated**: action reference bumped from `gidotencate/padlock@v0.9.3` to `gidotencate/padlock@v0.10.2`.

## [0.10.5] — 2026-04-30

### Fixed
- **Go embedded pointer field names**: fields declared as `*T` now use `T` as the field name (per Go spec — embedded pointer promotion strips the `*`). Previously the asterisk was included, producing incorrect field names.
- **`resolve_nested_structs` infinite loop**: iteration is now capped at `len * 2` to prevent an infinite loop when tree-sitter error-recovery produces circular field references (e.g. mutually-recursive pointer types where the `*` is stripped from the error tree).
- **Child-iteration early-exit in Go, C/C++, Zig frontends**: `node.child(i)?` inside `for` loops silently aborted the enclosing function on any `None` child. Replaced with `else { continue }` so a missing node skips that child and continues parsing.

### Performance
- **`padlock:ignore` scan**: source is now scanned once for all ignored names (O(source_len)) rather than once per struct (O(N × source_len)). HashSet lookup replaces per-struct linear search.
- **`should_skip_source_file` filter**: parallelised via rayon — ~15× speedup on cold-cache runs over large source trees.
- **Parse cache entry cap**: `ParseCache::flush` now drops entries beyond 100k to bound disk and RAM usage on very large projects.

### Dependencies
- `tree-sitter` → 0.25 (fixes `LanguageError { version: 15 }` crash with ABI-15 grammars)
- `tree-sitter-c` → 0.24
- `tree-sitter-go` → 0.25

## [0.10.4] — 2026-04-29

### Performance
- **Parallel directory walk**: `walk_source_files` now fans out across subdirectories concurrently via rayon, with a final `sort_unstable` for deterministic order.
- **Thread-local tree-sitter parsers**: Go, Zig, C, and C++ frontends hold one `Parser` per rayon worker thread instead of constructing one per file.
- **Parallel analysis passes**: `Report::from_layouts` runs per-struct analysis concurrently via `par_iter()`.
- **Cache rooted at analyzed directory**: `.padlock-cache/layouts.json` is now placed relative to the first analyzed path, not the working directory.
- **Cache eviction of deleted files**: stale entries are pruned from the cache on flush.
- **Cache streaming write**: uses `serde_json::to_writer(BufWriter::new(...))` instead of building the full JSON in RAM.

## [0.10.3] — 2026-04-28

### Added
- **Machine-generated and oversized files are skipped by default**: padlock now detects and silently skips source files that are not worth analyzing — either because they declare themselves as machine-generated (Go `// Code generated`, Rust `// @generated` / `//! @generated`, C/C++ `// Generated by` / `/* Generated by`, protobuf `.pb.h`/`.pb.cc`/`.pb.c`/`.pb.cpp`) or because they exceed 500 KB (data tables such as ECC curve constants, Go SSA rewrite tables, stdlib manifests — files that contain no hand-tunable struct definitions). Use `--include-generated` to opt back in. Resolves multi-minute hangs on codebases such as the Go source tree.
- **Parallel directory walk**: `walk_source_files` now uses rayon to fan out across subdirectories simultaneously, with a final `sort_unstable` to preserve deterministic file order. Significant speedup on large source trees.
- **Thread-local tree-sitter parsers**: Go, Zig, C, and C++ frontends now hold one `Parser` instance per rayon worker thread (via `thread_local!`) instead of constructing and initializing a new parser for every file. Eliminates per-file grammar-load overhead at scale.
- **Parallel analysis passes**: `Report::from_layouts` now runs per-struct analysis concurrently via `par_iter()`. Each struct's padding, reorder, locality, and false-sharing passes are independent and scale with available cores.
- **Cache rooted at analyzed directory**: the on-disk parse cache (`.padlock-cache/layouts.json`) is now placed relative to the first analyzed path rather than the current working directory. Running `padlock` from different directories on the same codebase shares one cache instead of creating duplicates.
- **Cache eviction of deleted files**: `ParseCache::flush` now prunes entries whose source file no longer exists before writing. Prevents unbounded cache growth when files are deleted or moved.
- **Cache uses streaming JSON writer**: `serde_json::to_writer(BufWriter::new(...))` replaces `to_string()` + `fs::write`. Avoids building the full JSON in RAM for large caches.

### Fixed
- **Go and Zig frontends still printed per-struct `eprintln!` skip notes**: the two-tier skipped-type output introduced in v0.10.1 removed these from the Rust and C/C++ frontends but missed Go and Zig. Both frontends already called `crate::record_skipped` immediately after; the stale `eprintln!` above it was simply redundant and has been removed.

## [0.10.2] — 2026-04-28

### Fixed
- **O(n²) merge stall on large codebases**: after parallel parsing, hit/miss results were reassembled into walk order using `Vec::find()` — O(n) per file, O(n²) overall. On a 64 K-file codebase (e.g. the Linux kernel source tree) this produced ~4 billion comparisons in the merge step alone, stalling `padlock` after parsing finished. Both lookups are now O(1) via `HashMap`.

## [0.10.1] — 2026-04-28

### Added
- **Source coverage ratio in output headers**: when any types are skipped (generics/templates), `analyze` output now appends `[N of M types, X% source coverage]` to the header line, with a "consider binary analysis" hint when coverage drops below 70%. `padlock summary` appends `· X% coverage` to the score header.
- **Live progress indicator during large scans**: directory scans with ≥ 20 cache-miss files show a live stderr counter `padlock: scanning N / M files…` (overwritten with `\r`). Only activates when stderr is a terminal; automatically suppressed for `--json`, `--sarif`, and piped output. Counter starts at cache-hit count so N/M reflects total files, not just misses.
- **`--show-skipped` flag** (`padlock analyze`): expands the skipped-types section from a count + breakdown to the full per-entry list. By default, at most 10 entries are shown with an "… and N more" trailer.
- **Skipped types now shown in `padlock summary` and `padlock check`**: `summary` appends a count+breakdown note; `check` was previously discarding skipped data and now attaches it to the report.

### Changed
- **Skipped-type output is now two-tier**: per-struct `eprintln!` calls during parallel parsing are removed (they fired unordered from rayon threads and duplicated `record_skipped` data). Default output shows one summary line with count and category breakdown (`1 200 C++ template, 47 Rust generic`). Use `--show-skipped` for the full list or `--json` for the complete `skipped` array.

## [0.10.0] — 2026-04-21

### Added
- **PDB enum extraction**: `TypeData::Enumeration` records in Windows PDB files are now extracted as layouts with a single `__discriminant` field, matching the DWARF and source-analysis representation.
- **DWARF/PDB `uncertain_fields` surfaced**: bitfield groups in both DWARF and PDB where the storage-unit size cannot be determined are now added to `StructLayout::uncertain_fields` and shown in output. Previously they were silently dropped.
- **Cache persists skipped types**: the on-disk parse cache (`.padlock-cache/layouts.json`) now stores `SkippedStruct` entries alongside layouts. Repeat runs on unchanged files correctly restore both layouts and skip notes without re-parsing.
- **Zig comptime-generic function detection verified**: `fn Foo(comptime T: type) type` functions are correctly detected and recorded as skipped via AST inspection of the `function_declaration` node. Added a test that confirms end-to-end detection.

### Fixed
- **PDB array dimensions**: `TypeData::Array::dimensions` holds cumulative byte lengths (not element counts). Array field sizes now use `dimensions.last()` as the total byte size, avoiding double-multiplication.
- **PDB dead `is_union` code removed**: the Class/Struct/Interface branch no longer computes and discards an `is_union` variable; `is_union: false` is set directly.
- **`SkippedStruct` derives `Deserialize`**: required for cache round-trip; was `Serialize`-only.

### Changed
- Documentation comprehensively updated: `docs/architecture.md`, `docs/extending.md`, `docs/findings.md`, main `README.md`, `CLAUDE.md`, and all six crate `README.md` files. Key corrections: DWARF/PDB bitfields are now described as grouped (not skipped); `pdb.rs` → `pdb_reader.rs`; Zig added to all language lists; PDB "not yet supported" note removed; cache stores skipped items; `padlock-output` module table now lists all formatters including `explain`, `markdown`, and `project_summary`.

## [0.9.9] — 2026-04-21

### Added
- **`padlock --version` includes git SHA**: `padlock --version` now outputs `padlock 0.9.9 (abc1234)` for traceability in bug reports. Falls back to `unknown` in vendored source tarballs where git is unavailable.
- **Zig integration test fixture** (`tests/fixtures/padded.zig`): covers the Zig source path in integration test runs.

### Fixed
- **`padlock fix --dry-run` now exits 1 when reorderings are pending** (previously always 0). This matches the `git diff --exit-code` / `cargo fmt --check` convention and makes `--dry-run` usable as a CI gate. Exits 0 only when every struct is already optimally ordered.
- **`cargo padlock` now respects per-struct severity overrides** from `.padlock.toml` (e.g. `[structs.MyStruct] min_severity = "low"`). Previously, per-struct `min_severity` overrides in the `[structs]` table were silently ignored by the `cargo padlock` binary; only the global `min_severity` was applied.
- **`padlock fix` blank line after `{`** in C, Go, and Zig source-aware rewrite output (Rust was already fixed in 0.9.8). The rewritten struct body no longer starts with a blank line.
- **`padlock explain` "no issues" message**: terminal output for structs with no padding waste now reads "no layout issues — struct is already optimally laid out" instead of the bare "no padding waste".

### Changed
- `--dry-run` help text updated to document the exit-code behaviour.

## [0.9.8] — 2026-04-21

### Added
- **`padlock fix --backup` flag**: opt-in `.bak` copy of the original file before rewriting. The backup is no longer created automatically — use `--dry-run` to preview without touching any files, or `--backup` if you want a safety copy alongside your rewrite.
- **VS Code: per-struct fix preview**: the lightbulb (⚡) CodeAction menu now offers "Preview reorder of `StructName`" alongside the direct-apply action. Opens the VS Code diff editor scoped to just that struct's changes before applying.
- **VS Code: editor context menu**: Analyze, Preview fix, and Apply fix are now available from the right-click context menu when editing a supported file.
- **VS Code: command palette `when` guards**: fix and analyze commands are hidden from the command palette when the active file is not a supported language.

### Changed
- **VS Code: fixes applied via `WorkspaceEdit`**: all fix paths — `padlock.fixFile`, `padlock.fixStruct`, and the Apply button in the preview flow — now apply changes through the VS Code `WorkspaceEdit` API rather than writing directly to disk. Changes land on the editor undo stack; Ctrl+Z reverts them. No `.bak` file is created.
- **VS Code: fix reads live editor buffer**: the fix and preview flows read the document's current in-memory content, so unsaved edits are included rather than the last-saved version on disk.
- **VS Code: command titles** updated: "Apply fix (reorder fields)" → "Apply fix (reorder all structs)", "Fix all (preview)" → "Preview fix (reorder all structs)".

## [0.9.7] — 2026-04-17

### Added
- **`--stdlib libstdc++|libc++|msvc` flag** (`analyze`, `summary`, `check`): selects the C++ standard library variant for type sizing. `libstdc++` (default, GCC/Linux), `libc++` (Clang/macOS/Android), and `msvc` (Windows MSVC) each have different sizes for `std::string`, `std::mutex`, `std::shared_ptr`, and other stdlib types. Use `--stdlib libc++` when analyzing macOS or iOS projects.
- **MSVC `#pragma pack` support**: `#pragma pack(N)`, `#pragma pack(push, N)`, `#pragma pack(pop)`, and `#pragma pack()` are now fully tracked during C/C++ source analysis. A pack stack mirrors the compiler's state across nesting levels; each struct's pack value is applied as a field-alignment cap, matching MSVC and GCC/Clang behaviour. Complements the existing `__attribute__((packed))` support.
- **Custom sync type names in `.padlock.toml`**: new `custom_sync_types` list in the `[padlock]` section lets you name project-specific lock wrapper types (e.g. `MyMutex`, `ProtectedData`) that should be treated as concurrent fields for false-sharing detection. Fields whose type name contains any listed string are classified as `Concurrent`.
- **Rust `Option<T>` niche optimization**: `Option<NonZeroU8/I8>` through `Option<NonZeroUsize/Isize>`, `Option<&T>`, `Option<&mut T>`, `Option<Box<T>>`, `Option<NonNull<T>>`, `Option<Arc<T>>`, and `Option<Rc<T>>` are now sized as their inner type (no extra discriminant byte). Previously these were over-approximated, causing false padding findings on structs using niche-optimized options.
- **Zig bit-level packed struct layout**: fields in `packed struct` declarations are now laid out bit-accurately. Arbitrary-width integer fields (`u3`, `u11`, etc.) consume exactly N bits; the struct's total size is `ceil(total_bits / 8)`. Previously all fields used `ceil(N/8)` bytes regardless of packing.
- **Inter-struct embedding hints**: when a struct with padding waste is embedded as a field in other analyzed structs, a `note: embedded in [Foo, Bar] — fixing layout would reduce size of each` line is appended to its output. Helps prioritize fixes whose impact propagates through a type hierarchy.
- **Skipped-struct diagnostics**: when a generic Rust struct (`struct Foo<T>`) or C++ template (`template<typename T> struct Foo`) is skipped during source analysis, a short `note:` is printed to stderr with the struct name. Makes silent skips visible so engineers know why a specific type is absent from the report.
- **Deduplication by (file, line)**: when multiple glob patterns or directory entries resolve to the same struct (same source file + line number), duplicates are silently removed. Prevents double-counting in multi-glob configurations or when headers are reached via multiple include paths.

### Changed
- C++ stdlib table: `std::string` note updated to reflect that `--stdlib libc++` or `--stdlib msvc` now selects the correct sizes at the CLI level, so binary analysis is only needed for unusual configurations.
- Rust stdlib table: `Option<NonZeroXxx>`, `Option<&T>`, `Option<Box<T>>` now listed as niche-optimized (sized as inner type), not as an approximate.
- **Limitations** section: removed `#pragma pack(N)` and Zig bit-packed struct entries (now implemented). Updated Rust niche entry to reflect that common niche types are now modeled; only unusual/custom niche types require binary analysis.

## [0.9.6] — 2026-04-17

### Added
- **Transparent newtype sizing** (Rust): `Cell<T>`, `MaybeUninit<T>`, `UnsafeCell<T>`, `Wrapping<T>`, `Saturating<T>`, and `ManuallyDrop<T>` now recurse into the inner type argument and report the correct size. `Cell<u8>` is 1 byte, not pointer-size.
- **C/C++ typedef alias resolution**: phase-0 pre-scan builds a scalar alias map (`typedef uint32_t UserId`). Fields typed via scalar aliases are now sized correctly.
- **C++ template skipping**: `template<typename T> struct/class/union` declarations are now skipped instead of producing wrong pointer-size placeholder layouts. Non-template types in the same TU are unaffected.
- **Go local interface sizing**: locally-declared named interfaces (`type Reader interface { ... }`) are sized as 2-word fat pointers. Cross-package types (`io.Reader`) remain 2-word approximations and are flagged as `uncertain_fields`.
- **`uncertain_fields` on `StructLayout`/`StructReport`**: new field records names whose size could not be determined from source alone; shown as a per-struct note in terminal output and as a JSON array (omitted when empty).
- **Raw BTF file support**: `padlock analyze /sys/kernel/btf/vmlinux` and `.btf` files now work without an ELF wrapper.
- **Locality: cache-line mixing detection**: `has_locality_issue` now also flags structs where hot and cold fields share a cache line even without interleaving, provided the struct spans more than one cache line. Architectures without a cache (`cache_line_size = 0`) skip this check.
- **`PaddingWaste` severity: absolute waste thresholds**: alongside percentage gates (≥30% → High, ≥10% → Medium), absolute gates now apply (≥32B → High, ≥8B → Medium) so large structs with modest-percentage waste are not under-reported.

### Fixed
- Locality output: `"interleaved with"` changed to `"mixed with … on same cache line(s)"` — accurate for both interleaving and cache-line sharing.
- `ref_dyn_trait_is_fat_pointer` test now asserts the 16-byte fat pointer size (the old test used a lifetime-parameterized struct which is skipped by the parser, so it asserted nothing).

### Docs
- All crate READMEs, `docs/architecture.md`, and `docs/findings.md` updated to include Zig in language lists and reflect typedef alias resolution, template skipping, transparent newtypes, and Go interface sizing.
- Main README: corrected Rust enum description (data enums are modeled via `__payload` + `__discriminant`; only niched layouts are not); added Go 1.18+ generics limitation; moved C++ templates from limitations to the skipped table.
- `docs/publishing.md`: updated description string and fix language list to include Zig.
- CLAUDE.md: added all 11 subcommands with descriptions, full `padlock-output` module breakdown, and accuracy notes for all new features.

## [0.9.5] — 2026-04-14

### Added
- **Cortex-M and AVR architecture support**: three new built-in `ArchConfig` constants — `CORTEX_M` (M0/M0+/M3/M23, no cache, 4-byte pointers), `CORTEX_M4` (M4/M7/M33, 32-byte cache lines, 4-byte pointers), and `AVR` (ATmega, no cache, 2-byte pointers, 1-byte max alignment). Short names (`cortex_m`, `cortex_m4`, `avr`) and Rust target triples (`thumbv6m-none-eabi`, `thumbv7em-none-eabihf`, `avr-unknown-gnu-atmega328p`, …) resolve to these configs via `arch_by_name` / `arch_by_triple`. When `cache_line_size = 0`, false-sharing and locality analysis is suppressed entirely; padding waste and reorder findings are unaffected.
- **`padlock bpf` subcommand**: thin alias for `padlock analyze` targeted at eBPF programs. Runs BTF-backed analysis on kernel object files and prints an orientation note (suppressed under `--json`/`--sarif`) explaining that BTF layouts reflect compiled types and are ABI-stable. All `analyze` flags are supported.
- **Homebrew formula** (`Formula/padlock.rb`): installs the `padlock` binary via `cargo install` with `depends_on "rust" => :build`. Includes a smoke test that checks `--version` and analyses a minimal padded C struct.
- **Game-dev / ECS documentation** (`docs/game-dev-ecs.md`): DOD-style Transform and RigidBody component examples, CI integration for game build pipelines, and compile-time size assertions with `padlock::assert_size`.
- **eBPF / BTF documentation** (`docs/ebpf-btf.md`): `padlock bpf` usage guide, BTF accuracy explanation, perf event payload examples, false sharing in shared BPF maps, and a CI workflow snippet.
- **Robotics / ROS 2 documentation** (`docs/robotics-ros2.md`): Cortex-M target architecture table, STM32F4 IMU example with 19% waste reduction, ROS 2 generated message layout analysis, CDR serialization padding impact, and CI integration.
- **Extension guide** (`docs/extending.md`): step-by-step instructions for adding analysis passes, language frontends, output formats, and architecture configs; full repository map; testing conventions.

## [0.9.4] — 2026-04-14

### Added
- **Evidence labels in all output surfaces**: `FalseSharing` and `LocalityIssue` findings now carry an `is_inferred` flag that is `true` when the finding was derived from type-name heuristics (`Mutex<T>`, `sync.Mutex`, `AtomicU64`, etc.) rather than explicit guard annotations. Inferred findings are labeled `(inferred from type names — add guard annotations or verify with profiling)` in terminal, Markdown, SARIF, and VS Code diagnostics. Explicitly annotated findings show no label — they are confirmed. The distinction is present in JSON output as `"is_inferred": true/false` on each finding.
- **`is_annotated` on `AccessPattern::Concurrent`**: the IR now records whether a `Concurrent` access pattern was set by an explicit source annotation (`GUARDED_BY`, `#[lock_protected_by]`, `// padlock:guard=`) or by the type-name heuristic pass. Explicit annotations in all frontends (Rust, C/C++, Go) set `is_annotated: true`; the heuristic pass sets `is_annotated: false`. Used to derive `is_inferred` on findings.
- **`repr(Rust)` severity downgrade**: findings on `repr(Rust)` structs (no `#[repr(C/packed/transparent)]`) are downgraded one severity level (High→Medium, Medium→Low) since the compiler may already have applied the optimisation at compile time. `ReorderSuggestion` is capped at Medium for these structs. Severity is still emitted at full strength for `repr(C)` and `repr(packed)` structs where the layout is fixed.
- **`--hide-repr-rust` flag** (`analyze`, `summary`, `check`): excludes all `repr(Rust)` structs from output entirely, focusing CI gating and terminal output on types with a fixed binary layout (C, `repr(C)`, Go, Zig) where findings are fully accurate and directly actionable.
- **`--target <TRIPLE>` flag** (`analyze`, `summary`, `check`): sets the target architecture for source analysis using a Rust target triple or short name. Common values: `aarch64-apple-darwin` (Apple Silicon, 128-byte cache lines), `aarch64-unknown-linux-gnu`, `wasm32-unknown-unknown`. Overrides `arch.override` in `.padlock.toml`.
- **`arch_by_triple()` in `padlock-core`**: new function that maps Rust target triples to `ArchConfig` by prefix matching. `arch_by_name()` now falls through to `arch_by_triple()` for strings not matching a short name.
- **`exclude_paths` in `.padlock.toml`**: glob patterns matched against `source_file` in each layout. Matching layouts are excluded from all analysis output. Useful for skipping generated files (`proto/**`), vendored code (`vendor/**`), or third-party directories. Backslashes normalised before matching (Windows compatibility).
- **ABI safety warning in `padlock fix`**: before rewriting any struct with a fixed binary layout (C, `repr(C)`, Go, Zig), `padlock fix` emits a warning to stderr listing the affected struct names and advising review of FFI boundaries and serialized data. `repr(Rust)` structs do not trigger this warning.
- **Guard name normalization in false-sharing detection**: guard identifiers are now normalized before comparison — `self.mu`, `this->mu`, `&mu`, `*mu` all compare equal to `mu`. Prevents false positives from annotation style differences across frontends.
- **Per-gap detail in `PaddingWaste` output**: terminal, Markdown, and SARIF output now shows `"NB after 'field' (offset O)"` for each gap (up to 3, then "and N more") instead of the previous `"across N gap(s)"`. Makes it immediately clear where each padding byte is located.
- **Before/after sizes in `ReorderSuggestion` output**: terminal, Markdown, and SARIF output now shows `"XB → YB (saves ZB)"` instead of `"save ZB → YB"`. Both sizes visible at a glance without mental arithmetic.
- **VS Code extension `is_inferred` support**: `PadlockFinding` interface gains `is_inferred?: boolean`; hover popups and Problems panel diagnostic messages for `FalseSharing` and `LocalityIssue` append the inferred label when set.

### Fixed
- **`.pre-commit-hooks.yaml` `--min-severity` typo**: `padlock-strict` hook was passing `--min-severity high` which is not a valid flag. Corrected to `--fail-on-severity high`. Also added `go` and `zig` to `types_or` in both hooks.

### Changed
- **GitHub Action**: exposed `target` and `hide-repr-rust` inputs, wired through to all `padlock analyze` invocations. Updated example workflow version pin from `v0.5.4` to `v0.9.3`.
- **Documentation**: README, `docs/findings.md`, `docs/real-world-examples.md`, and `editors/vscode/README.md` updated to reflect all new features, new output format strings, evidence labeling, `.padlock.toml` configuration section, and ABI warning behavior.

## [0.9.3] — 2026-04-12

### Added
- **`padlock explain` CL column**: the layout table now includes a `CL` column (zero-indexed
  cache-line number) for every field and padding row, making it immediately clear which cache
  line each piece of data occupies. Works alongside the existing cache-line separator rows that
  mark boundaries between lines.

## [0.9.2] — 2026-04-12

### Added
- **Go fix test coverage**: `generate_go_fix`, `generate_go_fix_from_source`, and `apply_fixes_go`
  are now covered by integration tests that verify field reordering, verbatim line preservation,
  and in-place file rewriting for Go structs. (The implementation was already complete.)

## [0.9.1] — 2026-04-12

### Changed
- **`padlock check` drift summary**: the terminal and JSON output of `padlock check` now always shows a `N new / M resolved / K unchanged` drift summary line. `resolved` counts both structs that improved significantly since the baseline and baseline structs that no longer appear in the current analysis (deleted/refactored). This gives CI logs a clear picture of baseline progress at a glance.

## [0.9.0] — 2026-04-12

### Added
- **`.padlock.toml` filter defaults**: the config file now supports per-project filter defaults under `[padlock]`. CLI flags always take precedence; config values fill in fields not specified on the command line. New options: `filter` (regex include), `exclude` (regex exclude), `min_size` (minimum struct size in bytes), `min_holes` (minimum padding holes), `sort_by` (`score`|`size`|`waste`|`name`), `fail_on_severity` (`high`|`medium`|`low`). The `padlock init` template documents all new options.

## [0.8.9] — 2026-04-12

### Added
- **Field-level source locations**: all source frontends (C/C++, Go, Zig, Rust) now populate `source_line` on each `Field` in the IR. The line number is 1-based, matching the field declaration in the source file. This enables future tooling (IDE integration, diagnostic output) to pinpoint individual fields. DWARF-parsed layouts are unaffected (line info already comes from debug info).

## [0.8.8] — 2026-04-12

### Added
- **Bit-field skip warning**: when the C/C++ source frontend skips a struct because it contains bit-field members, it now prints a diagnostic note to stderr explaining why the struct is absent from output and advising use of binary analysis for accurate results. Previously the skip was silent.

## [0.8.7] — 2026-04-12

### Added
- **`padlock init`**: new subcommand that generates a `.padlock.toml` template in the current directory with all options commented out and documented. Errors if a `.padlock.toml` already exists.

## [0.8.6] — 2026-04-12

### Added
- **Per-finding suppression**: place `// padlock: ignore[Kind1, Kind2]` on the line immediately before a struct/type declaration to suppress specific finding types while keeping the struct in the report. Supports all languages (C/C++/Rust/Go/Zig). Valid kinds: `PaddingWaste`, `ReorderSuggestion`, `FalseSharing`, `LocalityIssue`. Multiple kinds can be comma-separated in a single directive.
  - Distinct from `// padlock:ignore` (which removes the struct from analysis entirely).
  - `StructLayout` carries a `suppressed_findings: Vec<String>` field (serde `default` + `skip_serializing_if` for backward compatibility).
  - Documented in `docs/findings.md` with per-language examples.

## [0.8.5] — 2026-04-12

### Changed
- **False sharing heuristic tightening**: `has_false_sharing` no longer flags two purely-atomic fields sharing a cache line as false sharing. The type-name heuristic assigns each field's own name as its guard, so two `AtomicU64` fields on the same line would previously always trigger this check — but that pattern is cache-line bouncing (a locality issue), not classical lock-based false sharing. A conflict is now only reported when **at least one** of the two fields has `is_atomic: false` (i.e. a mutex/lock type, or explicitly annotated via `GUARDED_BY` / `#[lock_protected_by]`). Explicit annotations always set `is_atomic: false`, so annotated conflicts are always reported as before.
- Tests added to verify: two pure atomics → no false sharing; atomic + mutex → false sharing; mixed two atomics + one mutex → false sharing.

## [0.8.4] — 2026-04-12

### Added
- **C++ stdlib type sizes**: the C/C++ frontend now resolves sizes for common standard library types instead of falling back to pointer-size:
  - `std::string` / `std::basic_string<char>` → 32B (libstdc++ layout)
  - `std::string_view` / `std::basic_string_view<char>` → 2×pointer (ptr + length)
  - `std::vector<T>` (any `T`) → 3×pointer (ptr + size + capacity)
  - `std::deque<T>` → 80B; `std::list<T>` → 3×pointer
  - `std::map<K,V>` / `std::set<T>` / `std::multimap` / `std::multiset` → 48B; unordered variants → 56B
  - `std::unique_ptr<T>` → pointer; `std::shared_ptr<T>` / `std::weak_ptr<T>` → 2×pointer
  - `std::optional<T>` → recursively resolved: `ceil(sizeof(T)+1)` rounded up to `alignof(T)`
  - `std::function<Sig>` → 32B; `std::any` → 32B; `std::error_code` → 16B
  - `std::span<T>` → 2×pointer; `std::atomic_flag` → 4B
- **`repr(Rust)` caveat**: Rust structs and enums without `#[repr(C)]`, `#[repr(packed)]`, or `#[repr(transparent)]` now carry an `is_repr_rust` flag. Terminal `analyze` output appends a note that the compiler may have already reordered fields; the `explain` layout table shows `[repr(Rust) — compiler may reorder]` in the header. The `is_repr_rust` flag is also present in JSON/SARIF output (`#[serde(default)]` for backward compatibility).
- Tests for all new C++ stdlib types and for `repr(Rust)` detection on structs and enums.

## [0.8.3] — 2026-04-12

### Added
- **Extended type coverage** — source frontends now recognise significantly more types without falling back to pointer-size:
  - **C/C++**: Linux kernel integer types (`u8`/`u16`/`u32`/`u64`, `s8`–`s64`, `__u8`–`__u64`, `__s8`–`__s64`); endian-annotated kernel types (`__be16`, `__le32`, `__be64`, …); C99 fast/least family (`uint_fast32_t` is correctly pointer-sized on 64-bit); `intmax_t`/`uintmax_t`; GCC/Clang `__int128`/`__uint128`; Windows SDK types (`BYTE`, `WORD`, `DWORD`, `QWORD`, `BOOL`, `HANDLE`, `LPVOID`, `UINT8`–`UINT64`, `INT8`–`INT64`, pointer aliases); half-precision floats (`_Float16`, `__fp16`, `__bf16`, `_Float128`); character types (`wchar_t` 4B on POSIX, `char8_t`, `char16_t`, `char32_t`); multi-word spellings (`long int`, `long long int`).
  - **Zig**: C interop types (`c_char`, `c_short`, `c_int`, `c_uint`, `c_long`, `c_ulong`, `c_longlong`, `c_ulonglong`, `c_float`, `c_double`, `c_longdouble`); arbitrary-width integers (`u1`–`u65535`, `i1`–`i65535`) — size is `ceil(N/8)` bytes aligned to next power-of-two (capped at 8).
  - **Rust**: All `NonZeroXxx` types (`NonZeroU8`–`NonZeroU128`, `NonZeroUsize`, signed variants) — now correctly sized as their underlying integer; `f16` (2B) and `f128` (16B).
- Tests for all new type additions in `c_cpp.rs`, `zig.rs`, and `rust.rs`.
- README "What source analysis gets right" table expanded to reflect the new coverage.

## [0.8.2] — 2026-04-12

### Added (VS Code extension)
- **Status bar health score**: the status bar now shows a live layout score (`$(lock) 67 D`) and letter grade (A–F) for the active file, weighted by struct size. Yellow warning background and `$(warning) N` count appear when High-severity findings are present. Spinner shown while analysis runs.
- **Hover popup**: hovering over a struct definition line shows a markdown popup with score bar (`██████░░░░`), wasted bytes, and each finding with severity icon (🔴/🟡/🔵).
- **Quick-fix lightbulb (CodeAction)**: `ReorderSuggestion` diagnostics now show a lightbulb (⚡) with two actions: reorder the specific struct in-place, or open the diff editor preview for the whole file.
- **Fix all with diff preview** (`padlock: Fix all (preview)`): copies the file to a temp location, runs `padlock fix`, and opens the VS Code diff editor so you can review every reordering before applying. Clicking Apply writes the changes with a `.bak` backup.
- Zig added to extension keywords, activation events, and README language table.
- README updated with full documentation for all finding types, concurrent-field annotation syntax (Rust/C++/Go), and how the extension fits with the CLI and CI.

## [0.8.1] — 2026-04-11

### Fixed
- `cargo clippy --workspace -- -D warnings` now passes cleanly. Fixes:
  - `explain.rs`: `map_or(true, …)` → `is_none_or(…)`
  - `rust.rs`: two nested `if`/`if-let` blocks collapsed to let-chain form
  - `c_cpp.rs`: `parse_anonymous_nested` return type aliased to `RawField` (was `type_complexity`); `#[allow(clippy::only_used_in_recursion)]` on the `arch` parameter that threads through the recursive call
  - `cache.rs`: nested `if let Some(dir)` / `if err` collapsed to let-chain
  - `commands/analyze.rs`: 8-argument `run` refactored — output and arch options moved into `AnalyzeOpts` struct, bringing the function back under the 7-argument limit

## [0.7.2] — 2026-04-11

### Fixed
- `cargo fmt`: long-line reformatting in `go.rs` and `rust.rs`

## [0.8.0] — 2026-04-11

### Added
- **`padlock summary`**: project health overview that fits in one terminal screen. Shows aggregate weighted score with letter grade (A–F), a severity bar chart (High / Medium / Low / Clean), the N worst files grouped by source file (with per-file score, High-finding count, and total wasted bytes), and the N worst structs (by score then wasted bytes). Ends with a next-step hint: `Run 'padlock analyze <worst-file>' for full detail.` Supports `--top N` (default 5), `--cache-line-size`, `--word-size`, and the same `--filter` / `--exclude` flags as `analyze`.
- **`--fail-on-severity high|medium|low`** flag on `padlock analyze`: exits non-zero when any finding meets or exceeds the specified severity. `medium` triggers on Medium and High; `low` triggers on any finding. Composable with `--fail-below` score threshold.
- **`repr(align(N))` in Rust source**: the Rust frontend now parses `#[repr(align(N))]` on struct definitions. The effective alignment is raised to `N` (when `N > natural_align`); trailing padding is adjusted so the total size is a multiple of the new alignment. Exposed as a new `repr_align` helper in `padlock-source`.
- **Rust tuple structs**: the Rust frontend now recognizes and parses tuple struct declarations (`struct Foo(u64, u8)`). Fields are named `_0`, `_1`, etc. Source-aware fix generation (`padlock fix`) reorders tuple fields inside the `(...)` body verbatim, preserving visibility modifiers and attribute annotations. IR-level fallback emits `struct Name(T0, T1, …);`.
- **C/C++ anonymous nested structs/unions**: anonymous `struct`/`union` members inside outer `struct`/`union` declarations are now flattened into the parent layout, matching how the C/C++ compiler treats them. Named nested structs (used as field types) are still treated as opaque fields.
- **Cache-line boundary markers in `explain`**: when a struct's fields span more than one cache line, `padlock explain` inserts a visual separator row (`╞── cache line N (offset O) ════╡`) at each boundary. Single-cache-line structs show no separator.
- **Parallel file parsing**: directory walks now parse source files in parallel using `rayon`, significantly reducing wall-clock time on large codebases.
- **On-disk parse cache**: unchanged source files are served from `.padlock-cache/layouts.json` (keyed by path + mtime) and skipped on repeat runs. The cache is silently invalidated on mtime change and ignored if corrupt. Binary and DWARF paths bypass the cache.

## [0.7.1] — 2026-04-11

### Fixed
- `cargo fmt`: reformatted `fixgen.rs` (compact `match` arms expanded to block style), `zig.rs` (long iterator chains split across lines, blank line removed before `#[test]`), and `c_cpp.rs` to satisfy `rustfmt` in CI

## [0.7.0] — 2026-04-11

### Added
- **Rust enum analysis**: unit-only enums emit a single `__discriminant` field sized to the variant count (1, 2, or 4 bytes). Enums with one or more data variants additionally emit a `__payload` field (sized to the largest variant payload), matching Rust's conservative union-then-discriminant layout. Generic enums and empty enums are skipped.
- **C++ `alignas(N)`**: field-level and struct-level `alignas` specifiers are now extracted from source. Field-level `alignas` overrides the natural alignment when `N > natural_align`; struct-level `alignas` raises the struct's minimum alignment and adjusts trailing padding accordingly. Handles both `alignas_qualifier` (tree-sitter-cpp node) and the `type_qualifier → alignas_qualifier` wrapping pattern used for field declarations.
- **Go embedded structs**: anonymous (embedded) struct fields such as `sync.Mutex` or a bare `Base` type reference are now detected and emitted as IR fields. The simple (unqualified) type name is used as the field name. When the embedded type is defined in the same file, nested-struct resolution fills in the correct size.
- **Zig `union` and tagged `union`**: the Zig frontend now parses `union_declaration` nodes. All fields are placed at offset 0 (`is_union = true`). Tagged unions (those with an `enum` keyword child in the tree-sitter AST) receive a synthetic `__tag` field appended after the payload. Empty `union {}` declarations are filtered correctly.
- **Source-preserving fix generation**: `padlock fix` now extracts field declarations verbatim from the original source and reorders them as chunks, preserving `pub`, `pub(crate)`, `#[serde(...)]`, `/// doc-comments`, inline comments (including `GUARDED_BY(mu)`), and field tags. IR-based generation is retained as a fallback when chunking fails or a field cannot be matched.

## [0.6.2] — 2026-04-10

### Fixed
- `cargo clippy`: mixed-case hex literal (`0xeB9F` → `0xEB9F`), unused `RawBtfType` fields, two `collapsible_if` patterns in `zig.rs`
- Release workflow: added a `create-release` job so the GitHub release is created once before the matrix build jobs run, eliminating the `already_exists` race condition when multiple platform builds finish simultaneously

### Changed
- VS Code extension version aligned to `0.6.2` to match the Rust crates

## [0.6.1] — 2026-04-10

### Changed
- VS Code extension version aligned to `0.6.1` to match the Rust crates

## [0.6.0] — 2026-04-10

### Added
- **Zig source frontend**: struct layouts extracted from `.zig` files via `tree-sitter-zig`. Handles regular, `packed`, and `extern` structs; resolves built-in types (`u8`–`u128`, `i8`–`i128`, `f16`–`f128`, `usize`, `isize`, `bool`, `void`), pointers, optionals (`?T`), slices (`[]T`), arrays (`[N]T`), and error unions. Concurrency heuristics detect `std.Thread.Mutex`, `std.Thread.RwLock`, `std.atomic.Value`, and `Atomic` wrappers. Fix generation not yet supported for Zig.
- **BTF binary frontend**: pure-Rust parser for the BPF Type Format (`.BTF` ELF section) present in Linux eBPF object files. Handles all stable BTF kinds (`INT`, `PTR`, `ARRAY`, `STRUCT`, `UNION`, `ENUM`, `TYPEDEF`, `VOLATILE`, `CONST`, `RESTRICT`, `FLOAT`, `ENUM64`) and gracefully skips modern kinds (`FUNC`, `FUNC_PROTO`, `VAR`, `DATASEC`, `DECL_TAG`, `TYPE_TAG`, `FWD`) without aborting. Bitfield members are represented as synthetic storage-unit fields (`flags__bits: u32`) so their byte-level footprint is preserved for padding analysis — consecutive bitfields sharing the same storage unit produce a single synthetic field. Packed structs detected automatically from size vs. natural-aligned size. Automatically preferred over DWARF when a `.BTF` section is present (`padlock analyze my.bpf.o`).
- **`--cache-line-size <N>`**: override the assumed cache-line size (bytes) for the analysis, independent of the target architecture. Useful for analysing structs for non-standard hardware or comparing AMD (64-byte) vs Apple Silicon (128-byte) behaviour.
- **`--word-size <N>`**: override pointer/word size (bytes) for cross-architecture analysis (e.g. `--word-size 4` for 32-bit targets).
- **`--markdown` output format**: `padlock analyze --markdown` emits a GitHub-Flavored Markdown report with score emoji, severity badges (🔴/🟡/🔵), and per-struct GFM tables.
- **GitHub Action `output-format: markdown`**: when set, the markdown report is appended to `$GITHUB_STEP_SUMMARY` so findings appear on the workflow summary page without leaving the Actions UI.

### Changed
- `tree-sitter` upgraded from `0.22` to `0.23`; grammar crates updated to matching versions (`tree-sitter-c 0.23`, `tree-sitter-cpp 0.23`, `tree-sitter-go 0.23`). Language initialisation now uses the `LANGUAGE.into()` API instead of the deprecated `language()` function.

## [0.5.5] — 2026-04-09

### Changed
- GitHub Action renamed to `padlock-action`; VS Code extension icon and license added

## [0.5.4] — 2026-04-09

### Changed
- Added CHANGELOG, one-line pitch, pre-commit docs, and VS Code extension

## [0.5.3] — 2026-04-09

### Fixed
- Clippy `collapsible_if` errors introduced by the Rust 2024 edition — 19 nested `if` blocks collapsed into combined `if`/`if-let` chains across `extractor.rs`, `fixgen.rs`, `c_cpp.rs`, `go.rs`, `rust.rs`, `diff.rs`, and `fix.rs`
- `rust-version` bumped from `1.85` to `1.88` (let chains require Rust 1.88+)

## [0.5.2] — 2026-04-09

### Fixed
- GitHub Action (`action.yml`): version resolution curl no longer uses `-f`, so a 404 from the Releases API (no releases yet) falls back to `cargo install` instead of killing the script
- `padlock-example.yml`: trigger changed from `push`/`pull_request` to `workflow_dispatch` — it is a copy-paste template for users, not a live CI job for the padlock repo itself; eliminates the race with `release.yml`

### Changed
- Edition upgraded from `2021` to `2024` (zero code changes required; `cargo fix` confirmed clean migration)
- `rust-version = "1.85"` added to workspace to document the MSRV
- `toml` dependency bumped from `0.8` to `1` (same API, better error messages)
- `object` dependency bumped from `0.36` to `0.39` (same API, better Mach-O/PE support)

## [0.5.1] — 2026-04-09

### Fixed
- `detect_arch_on_real_object` integration test: added `"aarch64-apple"` to the list of valid arch names — Apple Silicon CI runners expose this name and the test was panicking
- `padlock-example.yml`: fixed `path: src/` → `path: crates/` (the padlock repo has no `src/` directory; source lives in `crates/`)

## [0.5.0] — 2026-04-09

### Added
- **Impact quantification**: `explain` subcommand now shows KB/MB overhead at 1K and 1M instance scales when reordering would save bytes; shows cache-line span reduction when applicable. `analyze`/`report` summary appends a compact `(~N MB/1M instances)` hint on High-severity `ReorderSuggestion` findings
- **DWARF integration test suite** (`crates/padlock-dwarf/tests/extractor_tests.rs`): 8 tests that compile C snippets at test time and verify extraction correctness (field names/offsets, typedef names, pointer sizes, arch detection, forward-declared struct skipping, analysis pipeline smoke test, bitfield skipping)
- **Architecture detection unit tests** in `reader.rs`: 6 tests using synthetic minimal ELF and Mach-O headers
- **GitHub composite action** (`action.yml`): pre-built binary download from GitHub Releases, SARIF upload, severity-gated exit, per-severity output counts (`high-count`, `medium-count`, `low-count`, `findings-count`)

### Fixed
- **DWARF**: bit-field members (`DW_AT_bit_size`) are now silently skipped instead of producing wrong byte-level IR; remaining non-bitfield fields still extracted
- **C/C++**: `__attribute__((packed))` on struct/class nodes is now detected from source text; packed layout simulation inserts no inter-field alignment padding and sets `is_packed = true`
- **Go**: `interface{}` and `any` fields are now correctly sized as 2 words (16 bytes on 64-bit), matching the Go runtime fat-pointer representation; `interface_type` AST nodes added to the tree-sitter field collector

### Changed
- Rust frontend: generic struct definitions (`struct Foo<T>`) are skipped (layout is unknowable without concrete type arguments)
- Rust stdlib type table expanded to ~55 entries: `Vec`/`String` (3×pointer), `Box`/`Arc`/`Rc` (1×pointer), all `AtomicXxx` (exact sizes), `PhantomData` (0), `Duration` (16 bytes), channels, smart pointers

## [0.4.0] — 2026-03-15

### Added
- `explain` subcommand: visual ASCII table of field layout with offset/size/align columns and inline padding gap rows
- `check` subcommand: baseline/ratchet mode — save a baseline JSON and fail only when new findings appear (CI regression detection)
- Per-struct suppression via `# padlock: ignore` / `// padlock: ignore` comments
- Rust `repr` documentation: explains the accuracy trade-offs between `repr(Rust)`, `repr(C)`, `repr(packed)`, and `repr(transparent)`
- `release.yml` workflow: builds pre-compiled binaries for Linux x64/ARM64, macOS x64/ARM64, and Windows x64 on tag push
- Repository badges (crates.io version, CI status, license)

## [0.3.0] — 2026-02-20

### Added
- Struct output grouped under `── filename ──` separator headers when analyzing multiple files
- `source_line` populated from AST node positions across all frontends
- Per-struct line numbers shown inline in grouped output

## [0.2.0] — 2026-02-01

### Added
- Multi-path support: `padlock analyze src/ lib/ include/` accepts any mix of files and directories
- `--filter` / `--exclude` flags: regex-based struct name filtering
- `--sort` flag: sort output by `score`, `size`, `waste`, or `name`
- `--min-size` and `--min-holes` thresholds to suppress noise
- `--version` flag

## [0.1.0] — 2026-01-15

### Added
- Initial implementation: padding waste detection, reorder suggestions, false sharing detection, locality issue detection
- Source frontends: C/C++ (tree-sitter), Rust (syn), Go (tree-sitter)
- Binary frontend: DWARF via gimli, PDB via pdb crate
- Output formats: human (coloured), JSON, SARIF 2.1.0, diff
- `fix` subcommand: in-place field reordering with `.bak` backup
- `watch` subcommand: re-runs analysis on file change
- `cargo padlock` subcommand
- Proc macros: `#[padlock::assert_no_padding]`, `#[padlock::assert_size(N)]`
- Architecture support: x86_64, aarch64, aarch64-apple (128-byte cache line), wasm32, riscv64
- `.padlock.toml` configuration file support
- Guard annotation support: Rust (`#[lock_protected_by]`), C/C++ (`GUARDED_BY()`), Go (`// padlock:guard=`)
