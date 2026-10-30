# padlock-source

Source analysis backend for [padlock](https://github.com/gidotencate/padlock) — a struct memory layout analyzer for C, C++, Rust, Go, and Zig.

This crate parses source files without invoking a compiler, using tree-sitter (C/C++/Go/Zig) and syn (Rust) to extract struct definitions and simulate their memory layout.

## Supported languages

| Language | Parser | Notes |
|---|---|---|
| C | tree-sitter-c | structs, unions, typedefs, bitfields (grouped as `[a:3|b:5]` storage-unit fields); laid out with the file's `#include`s, macros expanded, `#if` branches resolved for the target |
| C++ | tree-sitter-cpp | classes, Itanium inheritance (tail padding, empty bases, virtual bases), vtable pointer, namespaces, `typedef` alias resolution; stdlib types (`std::string`, `std::vector`, `std::optional`, `std::shared_ptr`, …); templates analyzed per concrete instantiation |
| Rust | syn | `repr(C)`, `repr(packed)`, `repr(align(N))`, all primitive types, `NonZeroXxx`, `f16`/`f128`, transparent newtypes, enum niches; `repr(Rust)` laid out in rustc's order; generics per instantiation; laid out with the whole crate |
| Go | tree-sitter-go | built-in types, `sync.Mutex`, slices, maps, interfaces (2-word fat pointers), generics per instantiation; laid out with the whole package (build constraints applied); types of imported packages of the module and the standard library read from their source (generics of other packages and third-party packages flagged as uncertain) |
| Zig | tree-sitter | structs, unions (bare + tagged), packed structs; C interop types, arbitrary-width integers |

## Concurrency annotation

Guard annotations are read from source to enable accurate false-sharing detection:

- **Rust**: `#[lock_protected_by = "mu"]`, `#[guarded_by("mu")]`
- **C/C++**: `GUARDED_BY(mu)`, `__attribute__((guarded_by(mu)))`
- **Go**: `// padlock:guard=mu`, `// guarded_by: mu`, `// +checklocksprotects:mu`

Fields without explicit annotations are inferred from their type names (`Mutex`, `std::atomic`, `sync.Mutex`, …).

## Skipped types

Generic and template types with no concrete instantiation in the analyzed code are recorded rather than silently dropped. `parse_source` returns a `ParseOutput { layouts, skipped }` where `skipped: Vec<SkippedStruct>` lists every type that was encountered but not analyzed, along with the reason. These appear in JSON output and as SARIF `notifications` so CI pipelines can see what was skipped.

Skipped type categories:
- **Rust**: generic structs and enums (`struct Foo<T>`, `enum Result<T, E>`)
- **C++**: template structs/classes/unions (`template<typename T> struct Vec`)
- **Go**: generic structs (`type Pair[T any] struct`)
- **Zig**: comptime-generic functions (`fn ArrayList(comptime T: type) type`)

## Uncertain fields

Fields whose type cannot be accurately sized from source alone are added to `StructLayout::uncertain_fields` (a `Vec<String>` of field names). This happens for:
- Go: third-party packages and generic types of other packages are not resolved (flagged as uncertain)
- Zig: comptime-only field types (`type`, `anytype`, `comptime_int`, `comptime_float`)
- Post-parse: fields of `Opaque` type whose type name does not match any struct in the same file

Uncertain fields are surfaced in terminal output (as a per-struct note), JSON, and SARIF.

## Source locations

`parse_source(path, arch)` populates `StructLayout.source_file` and `StructLayout.source_line` for every returned layout, enabling line-level navigation in CLI output and SARIF reports.

## Usage

`padlock-source` is an internal library crate. To analyze source files, use the CLI:

```bash
padlock analyze src/
padlock analyze myfile.rs myfile.cpp
```

## Part of padlock

- [`padlock-cli`](https://crates.io/crates/padlock-cli) — CLI (`padlock` + `cargo-padlock` binaries)
- [`padlock-core`](https://crates.io/crates/padlock-core) — IR, analysis passes, findings
- [`padlock-source`](https://crates.io/crates/padlock-source) — Source analysis (C/C++/Rust/Go) *(this crate)*
- [`padlock-dwarf`](https://crates.io/crates/padlock-dwarf) — Binary analysis (DWARF/PDB)
- [`padlock-output`](https://crates.io/crates/padlock-output) — Output formatters (terminal/JSON/SARIF/diff)
- [`padlock-macros`](https://crates.io/crates/padlock-macros) — Compile-time layout assertions
