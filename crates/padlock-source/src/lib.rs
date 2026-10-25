// padlock-source/src/lib.rs

mod c_includes;
mod c_preproc;
pub mod concurrency;
pub mod fixgen;
pub mod frontends;
mod go_package;
mod rust_crate;
pub use c_includes::{include_dirs, set_include_dirs};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use padlock_core::arch::ArchConfig;
use padlock_core::findings::SkippedStruct;
use padlock_core::ir::{StructLayout, TypeInfo};

/// C++ standard library implementation variant.
///
/// Affects hardcoded sizes of types like `std::string`, `std::mutex`, etc.
/// The default is `LibStdCpp` (GCC / Linux / glibc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CppStdlib {
    /// GCC libstdc++ (Linux/glibc default). `std::string` = 32B.
    #[default]
    LibStdCpp,
    /// LLVM libc++ (Clang/macOS/Android). `std::string` = 24B.
    LibCpp,
    /// Microsoft MSVC STL (Windows). `std::string` = 32B (SSO = 16 chars).
    Msvc,
}

/// Bumped whenever a process-global parse setting changes, so caches of parsed
/// results (the Go package cache) know to rebuild.
static SETTINGS_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn bump_settings_generation() {
    SETTINGS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn settings_generation() -> u64 {
    SETTINGS_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// `false` for a Go file `go build` would leave out for the target (another
/// OS or architecture, a build tag that is off); `true` for everything else.
pub fn built_for_target(path: &Path, arch: &'static ArchConfig) -> bool {
    path.extension().is_none_or(|e| e != "go") || go_package::built_for_target(path, arch)
}

/// Parse the Rust crate containing `path` ahead of its files (see
/// [`prepare_go_packages`]).
pub fn prepare_rust_crate(path: &Path, arch: &'static ArchConfig) {
    rust_crate::prepare(path, arch);
}

/// The root of the crate whose `src/` holds `path` (for grouping files).
pub fn rust_crate_root(path: &Path) -> Option<PathBuf> {
    rust_crate::crate_root(path)
}

/// Newest mtime among the source files of the crate rooted at `root`; a Rust
/// file's parse depends on all of them.
pub fn rust_crate_newest_mtime(root: &Path) -> Option<std::time::SystemTime> {
    rust_crate::newest_mtime(root)
}

/// Parse every Go package in `dir` ahead of its files, so callers that parse
/// many files in parallel find each package ready instead of waiting on one
/// another. Optional: `parse_source` builds a package on first use anyway.
pub fn prepare_go_packages(dir: &Path, arch: &'static ArchConfig) {
    go_package::prepare(dir, arch);
}

/// Set the C++ stdlib variant used for type-size lookups during source analysis.
///
/// Process-global: it takes effect for all subsequent calls to `parse_source` /
/// `parse_source_str` on every thread (including rayon workers). The default is
/// `CppStdlib::LibStdCpp`. Call this from the CLI before invoking analysis.
pub fn set_cpp_stdlib(stdlib: CppStdlib) {
    bump_settings_generation();
    frontends::c_cpp::set_stdlib(stdlib);
}

/// The C++ stdlib variant currently in effect.
pub fn cpp_stdlib() -> CppStdlib {
    frontends::c_cpp::active_stdlib()
}

// ── compiler-chosen field order ───────────────────────────────────────────────

static DECLARED_ORDER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Lay out Rust `repr(Rust)` structs and plain (non-`extern`/`packed`) Zig
/// structs in source declaration order instead of the order the compiler picks.
/// Off by default. Process-global.
pub fn set_declared_order(on: bool) {
    bump_settings_generation();
    DECLARED_ORDER.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether `set_declared_order(true)` is in effect.
pub fn declared_order() -> bool {
    DECLARED_ORDER.load(std::sync::atomic::Ordering::Relaxed)
}

/// Put the fields of a struct whose order the compiler chooses (`is_repr_rust`:
/// Rust `repr(Rust)`, plain Zig) in the order current rustc and zig use —
/// largest alignment first, ties in declaration order — and lay it out again.
/// Sizes match the compilers; individual offsets are an estimate (rustc may
/// move a field with a large niche to the front).
pub(crate) fn apply_compiler_order(layout: &mut StructLayout) {
    if layout.is_repr_rust && !layout.is_packed && !layout.is_union && !declared_order() {
        resimulate_layout(layout);
    }
}

// ── generic / template instantiation ──────────────────────────────────────────

static CONFIG_INSTANTIATIONS: std::sync::RwLock<Vec<(String, Vec<String>)>> =
    std::sync::RwLock::new(Vec::new());

/// Split a comma-separated argument list at depth 0 (`"Map<K, V>, int"` →
/// `["Map<K, V>", "int"]`).
pub fn split_type_args(text: &str) -> Vec<String> {
    let (mut depth, mut start, mut out) = (0i32, 0usize, Vec::new());
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(text[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        out.push(last.to_string());
    }
    out
}

/// Instantiations to analyze for generic types (C++ class templates, Rust and
/// Go generic structs, Zig type-returning functions) besides the ones found in
/// the source itself, as `(TypeName, [arg, …])`. Typically from `[instantiate]`
/// in `.padlock.toml`. Process-global; replaces the previous list.
pub fn set_configured_instantiations(list: Vec<(String, Vec<String>)>) {
    bump_settings_generation();
    *CONFIG_INSTANTIATIONS
        .write()
        .unwrap_or_else(|e| e.into_inner()) = list;
    publish_instantiations(Vec::new());
}

/// Additionally analyze these instantiations (e.g. uses found in other files of
/// the same run), on top of the configured ones.
pub fn set_discovered_instantiations(list: Vec<(String, Vec<String>)>) {
    bump_settings_generation();
    publish_instantiations(list);
}

static EXTRA_INSTANTIATIONS: std::sync::RwLock<Vec<(String, Vec<String>)>> =
    std::sync::RwLock::new(Vec::new());

fn publish_instantiations(mut discovered: Vec<(String, Vec<String>)>) {
    let mut all = CONFIG_INSTANTIATIONS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    all.append(&mut discovered);
    *EXTRA_INSTANTIATIONS
        .write()
        .unwrap_or_else(|e| e.into_inner()) = all;
}

/// Configured and cross-file instantiations of the generic type `name`.
pub(crate) fn extra_instantiations(name: &str) -> Vec<Vec<String>> {
    EXTRA_INSTANTIATIONS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, a)| a.clone())
        .collect()
}

/// The configured instantiations as `Name<args>` strings (for cache fingerprints).
pub fn configured_instantiations_fingerprint() -> Vec<String> {
    CONFIG_INSTANTIATIONS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(n, a)| format!("{n}<{}>", a.join(",")))
        .collect()
}

/// Concrete uses of generic types in `source` (`Buf<char, 8>`, `Pair[int, string]`,
/// `Foo<u32>`, `List(u32)`), as `(TypeName, [arg, …])`. Used to instantiate generic
/// types defined in *other* files of the same run.
pub fn generic_uses(source: &str, lang: &SourceLanguage) -> Vec<(String, Vec<String>)> {
    match lang {
        SourceLanguage::Cpp => frontends::c_cpp::template_uses(source),
        SourceLanguage::Rust => frontends::rust::generic_uses(source),
        SourceLanguage::Go => frontends::go::generic_uses(source),
        SourceLanguage::Zig => frontends::zig::generic_uses(source),
        _ => Vec::new(),
    }
}

/// True for a skipped type that analysis could pick up once concrete type
/// arguments are known (a template / generic without a found instantiation).
pub fn skipped_for_lack_of_instantiation(s: &SkippedStruct) -> bool {
    s.reason.contains(INSTANTIATION_HINT)
}

/// Marker included in the skip reason of every generic type that was skipped only
/// because no instantiation was found.
pub(crate) const INSTANTIATION_HINT: &str = "no instantiation found";

// ── preprocessor defines ──────────────────────────────────────────────────────

static DEFINES: std::sync::RwLock<Vec<(String, String)>> = std::sync::RwLock::new(Vec::new());

/// Set the macros treated as defined when the C/C++ frontend chooses between
/// `#if` / `#ifdef` branches inside struct bodies (like `-D` for a compiler).
/// Each entry is `NAME` or `NAME=VALUE`; `NAME` alone means `NAME=1`.
///
/// Process-global (not thread-local), so it applies to every parallel parse.
pub fn set_preprocessor_defines<S: AsRef<str>>(defines: &[S]) {
    bump_settings_generation();
    let parsed = defines
        .iter()
        .map(|d| match d.as_ref().split_once('=') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => (d.as_ref().trim().to_string(), "1".to_string()),
        })
        .filter(|(k, _)| !k.is_empty())
        .collect();
    *DEFINES.write().unwrap_or_else(|e| e.into_inner()) = parsed;
}

/// The active defines as `NAME=VALUE` strings (for cache fingerprints).
pub fn preprocessor_defines_fingerprint() -> Vec<String> {
    preprocessor_defines()
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect()
}

pub(crate) fn preprocessor_defines() -> Vec<(String, String)> {
    DEFINES.read().unwrap_or_else(|e| e.into_inner()).clone()
}

// ── skipped-struct side channel ───────────────────────────────────────────────
//
// Frontends call `record_skipped` (via `crate::record_skipped`) when they skip
// a type they cannot accurately size (generics, templates, etc.).  `parse_source`
// drains the buffer after each file so callers receive structured skip data
// alongside the layouts.

thread_local! {
    static SKIPPED_COLLECTOR: std::cell::RefCell<Vec<SkippedStruct>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Record a skipped struct/type into the per-thread buffer.
///
/// Called by language frontends when they encounter a type they cannot size
/// (e.g. generic/template types).  The buffer is drained by `parse_source`.
pub fn record_skipped(name: &str, reason: &str) {
    SKIPPED_COLLECTOR.with(|c| {
        c.borrow_mut().push(SkippedStruct {
            name: name.to_string(),
            reason: reason.to_string(),
            source_file: None,
        });
    });
}

pub(crate) fn take_skipped() -> Vec<SkippedStruct> {
    SKIPPED_COLLECTOR.with(|c| c.take())
}

/// Put skipped entries back into the collector (after a nested collection).
pub(crate) fn restore_skipped(items: Vec<SkippedStruct>) {
    SKIPPED_COLLECTOR.with(|c| c.borrow_mut().extend(items));
}

/// Output from a single source-file parse: the extracted layouts plus any
/// types that were skipped (e.g. generics whose layout is unknown).
pub struct ParseOutput {
    pub layouts: Vec<StructLayout>,
    pub skipped: Vec<SkippedStruct>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceLanguage {
    C,
    Cpp,
    Rust,
    Go,
    Zig,
}

/// Detect language from file extension.
pub fn detect_language(path: &Path) -> Option<SourceLanguage> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("c") | Some("h") => Some(SourceLanguage::C),
        Some("cpp") | Some("cc") | Some("cxx") | Some("c++") | Some("hpp") | Some("hh")
        | Some("hxx") | Some("h++") | Some("ipp") | Some("tpp") | Some("inl") | Some("cppm")
        | Some("ixx") => Some(SourceLanguage::Cpp),
        Some("rs") => Some(SourceLanguage::Rust),
        Some("go") => Some(SourceLanguage::Go),
        Some("zig") => Some(SourceLanguage::Zig),
        _ => None,
    }
}

/// Like [`detect_language`], but a `.h` header whose contents are C++ (classes,
/// templates, namespaces, access specifiers, `std::`) is reported as C++ — the
/// C grammar cannot parse those and would silently lose the types.
pub fn detect_language_from_source(path: &Path, source: &str) -> Option<SourceLanguage> {
    let lang = detect_language(path)?;
    if lang == SourceLanguage::C
        && path.extension().and_then(|e| e.to_str()) == Some("h")
        && looks_like_cpp(source)
    {
        return Some(SourceLanguage::Cpp);
    }
    Some(lang)
}

fn looks_like_cpp(source: &str) -> bool {
    source.lines().any(|line| {
        let l = line.trim_start();
        if l.starts_with("//") || l.starts_with('*') || l.starts_with("/*") {
            return false;
        }
        l.starts_with("class ")
            || l.starts_with("namespace ")
            || l.starts_with("template<")
            || l.starts_with("template <")
            || l.starts_with("public:")
            || l.starts_with("private:")
            || l.starts_with("protected:")
            || l.starts_with("using namespace ")
            || l.contains("std::")
            || l.contains(" virtual ")
            || l.starts_with("virtual ")
    })
}

/// Parse C/C++ translation-unit roots (files no other analyzed file includes)
/// in parallel but deterministically: each shared header is laid out in the
/// context of the first root, in sorted order, that reaches it. Results are
/// in the order of `roots`.
pub fn parse_c_roots(
    roots: &[PathBuf],
    arch: &'static ArchConfig,
) -> Vec<anyhow::Result<ParseOutput>> {
    c_includes::parse_units_in_order(roots, arch)
}

/// The headers a C/C++ file includes directly (resolved, canonical); empty for
/// other languages. Lets callers analyze files that nothing else includes
/// first, so headers are processed in the context of their includer.
pub fn direct_includes(path: &Path, arch: &'static ArchConfig) -> Vec<PathBuf> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "inl" | "ipp") => {
            c_includes::direct_includes(path, arch)
        }
        _ => Vec::new(),
    }
}

/// Other files whose content the parse of `path` depends on: the headers a
/// C/C++ file includes (transitively). Callers caching results by file mtime
/// should also check these.
pub fn source_dependencies(path: &Path, arch: &'static ArchConfig) -> Vec<PathBuf> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match detect_language_from_source(path, &source) {
        Some(SourceLanguage::C) => c_includes::dependencies(path, arch, false),
        Some(SourceLanguage::Cpp) => c_includes::dependencies(path, arch, true),
        _ => Vec::new(),
    }
}

/// Parse a source file and return layouts plus any skipped types.
pub fn parse_source(path: &Path, arch: &'static ArchConfig) -> anyhow::Result<ParseOutput> {
    let source = std::fs::read_to_string(path)?;
    let lang = detect_language_from_source(path, &source)
        .ok_or_else(|| anyhow::anyhow!("unsupported file type: {}", path.display()))?;
    // Clear any leftover thread-local state from previous direct parse_source_str calls.
    let _ = take_skipped();
    // A Go file is laid out together with the other files of its package.
    // …and a C/C++ file with what its `#include`s declare.
    let package = match lang {
        SourceLanguage::Go => go_package::parse_in_package(path, &source, arch).map(Ok),
        // …and a Rust file with the rest of its crate.
        SourceLanguage::Rust => rust_crate::parse_in_crate(path, &source, arch).map(Ok),
        SourceLanguage::C | SourceLanguage::Cpp => {
            c_includes::parse_with_includes(path, &source, &lang, arch)
        }
        _ => None,
    };
    let mut layouts = match package {
        Some(layouts) => finish_layouts(&source, &lang, layouts?),
        None => parse_source_str(&source, &lang, arch)?,
    };
    let mut skipped = take_skipped();
    let file_str = path.to_string_lossy().into_owned();
    for layout in &mut layouts {
        layout.source_file = Some(file_str.clone());
    }
    for s in &mut skipped {
        s.source_file = Some(file_str.clone());
    }
    Ok(ParseOutput { layouts, skipped })
}

/// Parse source text directly (useful for tests and piped input).
pub fn parse_source_str(
    source: &str,
    lang: &SourceLanguage,
    arch: &'static ArchConfig,
) -> anyhow::Result<Vec<StructLayout>> {
    let mut layouts = match lang {
        SourceLanguage::C => frontends::c_cpp::parse_c(source, arch)?,
        SourceLanguage::Cpp => frontends::c_cpp::parse_cpp(source, arch)?,
        SourceLanguage::Rust => frontends::rust::parse_rust(source, arch)?,
        SourceLanguage::Go => frontends::go::parse_go(source, arch)?,
        SourceLanguage::Zig => frontends::zig::parse_zig(source, arch)?,
    };
    // Resolve fields whose type names match other structs in this file.
    // This makes nested struct sizes accurate (instead of defaulting to pointer size).
    resolve_nested_structs(&mut layouts);
    Ok(finish_layouts(source, lang, layouts))
}

/// The steps after nested-struct resolution: hide anonymous aggregates,
/// annotate concurrency, drop `// padlock:ignore`d structs.
fn finish_layouts(
    source: &str,
    lang: &SourceLanguage,
    mut layouts: Vec<StructLayout>,
) -> Vec<StructLayout> {
    hide_anonymous_aggregates(&mut layouts);

    // Annotate concurrency patterns
    for layout in &mut layouts {
        concurrency::annotate_concurrency(layout, lang);
    }

    // Remove structs explicitly opted out via `// padlock:ignore`.
    // Pre-scan the source once instead of rescanning per struct (O(M) vs O(N×M)).
    let ignored = collect_padlock_ignored_names(source);
    layouts.retain(|layout| !ignored.contains(&layout.name));
    layouts
}

// ── nested struct resolution ──────────────────────────────────────────────────

/// Returns true if `name` is a well-known primitive type name in any supported
/// language. These must never be shadowed by a user-defined struct name.
fn is_known_primitive(name: &str) -> bool {
    matches!(
        name,
        // Rust primitives
        "bool" | "u8" | "i8" | "u16" | "i16" | "u32" | "i32" | "f32" | "u64" | "i64" | "f64"
            | "u128" | "i128" | "usize" | "isize" | "char" | "str"
            // C/C++ primitives
            | "int" | "long" | "short" | "float" | "double" | "void"
            | "int8_t" | "uint8_t" | "int16_t" | "uint16_t" | "int32_t" | "uint32_t"
            | "int64_t" | "uint64_t" | "size_t" | "ssize_t" | "ptrdiff_t"
            | "intptr_t" | "uintptr_t" | "_Bool"
            // Go primitives
            | "int8" | "uint8" | "byte" | "int16" | "uint16" | "int32" | "uint32"
            | "int64" | "uint64" | "float32" | "float64" | "complex64" | "complex128"
            | "rune" | "string" | "error"
            // SIMD
            | "__m64" | "__m128" | "__m128d" | "__m128i"
            | "__m256" | "__m256d" | "__m256i"
            | "__m512" | "__m512d" | "__m512i"
    )
}

// ── C++ inheritance (Itanium C++ ABI) ─────────────────────────────────────────

/// Snapshot of one class as the Itanium layout of its derived classes sees it.
#[derive(Clone)]
struct CppView {
    size: usize,
    align: usize,
    /// Size without virtual-base subobjects (`nvsize`), also the data size
    /// (`dsize`) a non-POD base contributes before tail-padding reuse.
    nvsize: usize,
    nvalign: usize,
    info: Option<padlock_core::ir::CppClassInfo>,
    has_members: bool,
}

fn is_cpp_slot(name: &str) -> bool {
    name == "__vptr" || name.starts_with("__base_") || name.starts_with("__vbase_")
}

fn cpp_views(layouts: &[StructLayout]) -> HashMap<String, CppView> {
    layouts
        .iter()
        .map(|l| {
            let nv = l.fields.iter().filter(|f| !f.name.starts_with("__vbase_"));
            let nvsize = nv.clone().map(|f| f.offset + f.size).max().unwrap_or(0);
            // Never stricter than the class itself (`#pragma pack` / `packed` cap it).
            let nvalign = nv
                .map(|f| f.align)
                .max()
                .unwrap_or(1)
                .clamp(1, l.align.max(1));
            (
                l.name.clone(),
                CppView {
                    size: l.total_size,
                    align: l.align,
                    nvsize,
                    nvalign,
                    info: l.cpp.clone(),
                    has_members: l.fields.iter().any(|f| !is_cpp_slot(&f.name)),
                },
            )
        })
        .collect()
}

/// Has a vtable pointer somewhere: virtual functions or a virtual base, itself or
/// through any base.
fn cpp_dynamic(name: &str, views: &HashMap<String, CppView>, depth: u8) -> bool {
    let Some(info) = views.get(name).and_then(|v| v.info.as_ref()) else {
        return false;
    };
    depth < 16
        && (info.has_virtual_methods
            || info
                .bases
                .iter()
                .any(|b| b.is_virtual || cpp_dynamic(&b.name, views, depth + 1)))
}

/// No data at all: an empty base occupies no space in a derived class.
fn cpp_empty(name: &str, views: &HashMap<String, CppView>, depth: u8) -> bool {
    let Some(v) = views.get(name) else {
        return false;
    };
    depth < 16
        && !v.has_members
        && !cpp_dynamic(name, views, 0)
        && v.info.as_ref().is_none_or(|i| {
            i.bases
                .iter()
                .all(|b| !b.is_virtual && cpp_empty(&b.name, views, depth + 1))
        })
}

/// Virtual bases reachable from `name`, in depth-first declaration order.
fn cpp_virtual_bases(
    name: &str,
    views: &HashMap<String, CppView>,
    out: &mut Vec<String>,
    depth: u8,
) {
    let Some(info) = views.get(name).and_then(|v| v.info.as_ref()) else {
        return;
    };
    if depth > 16 {
        return;
    }
    for b in &info.bases {
        if b.is_virtual && !out.contains(&b.name) {
            out.push(b.name.clone());
        }
        cpp_virtual_bases(&b.name, views, out, depth + 1);
    }
}

/// `(size, align)` a subobject of class `name` occupies inside another class:
/// nothing when empty; `nvsize` for a non-POD class (its tail padding is
/// reusable); the full size otherwise. Unknown classes keep a pointer-sized
/// placeholder.
fn cpp_subobject(
    name: &str,
    views: &HashMap<String, CppView>,
    ps: usize,
    msvc: bool,
) -> (usize, usize, bool) {
    let Some(v) = views.get(name) else {
        return (ps, ps, false);
    };
    if cpp_empty(name, views, 0) {
        return (0, 1, true);
    }
    // MSVC never lays a derived class's members into a base's tail padding.
    if msvc {
        return (v.size, v.align, true);
    }
    let non_pod = v
        .info
        .as_ref()
        .is_some_and(|i| i.non_pod || !i.bases.is_empty())
        || cpp_dynamic(name, views, 0);
    if non_pod {
        (v.nvsize, v.nvalign, true)
    } else {
        (v.size, v.align, true)
    }
}

fn cpp_slot(name: String, ty: &str, size: usize, align: usize) -> padlock_core::ir::Field {
    padlock_core::ir::Field {
        name,
        ty: if ty == "__vptr" {
            TypeInfo::Pointer { size, align }
        } else {
            TypeInfo::Opaque {
                name: ty.to_string(),
                size,
                align,
            }
        },
        offset: 0,
        size,
        align,
        source_file: None,
        source_line: None,
        access: padlock_core::ir::AccessPattern::Unknown,
    }
}

/// Rebuild every C++ class's base, vtable-pointer and virtual-base slots from the
/// current sizes of the classes it derives from, following the Itanium ABI:
/// the primary base (first non-virtual dynamic base) or else the vptr at offset 0,
/// then the other non-virtual bases, the data members, and finally each virtual
/// base once. Empty bases and `[[no_unique_address]]` members of empty type take
/// no space; members may start inside a non-POD base's tail padding. Returns
/// whether anything changed.
fn apply_cpp_inheritance(layouts: &mut [StructLayout]) -> bool {
    let views = cpp_views(layouts);
    let mut changed_any = false;
    for layout in layouts.iter_mut() {
        let Some(info) = layout.cpp.clone() else {
            continue;
        };
        let ps = layout.arch.pointer_size;
        let msvc = layout.arch.name.ends_with("msvc");
        let mut slots = Vec::new();
        let non_virtual: Vec<&padlock_core::ir::CppBase> =
            info.bases.iter().filter(|b| !b.is_virtual).collect();
        let primary = non_virtual
            .iter()
            .position(|b| cpp_dynamic(&b.name, &views, 0));
        if let Some(p) = primary {
            let (sz, al, _) = cpp_subobject(&non_virtual[p].name, &views, ps, msvc);
            slots.push(cpp_slot(
                format!("__base_{}", non_virtual[p].name),
                &non_virtual[p].name,
                sz,
                al,
            ));
        } else if cpp_dynamic(&layout.name, &views, 0) {
            slots.push(cpp_slot("__vptr".to_string(), "__vptr", ps, ps));
        }
        for (i, b) in non_virtual.iter().enumerate() {
            if Some(i) == primary {
                continue;
            }
            let (sz, al, _) = cpp_subobject(&b.name, &views, ps, msvc);
            slots.push(cpp_slot(format!("__base_{}", b.name), &b.name, sz, al));
        }
        let mut members: Vec<padlock_core::ir::Field> = layout
            .fields
            .iter()
            .filter(|f| !is_cpp_slot(&f.name))
            .cloned()
            .collect();
        for m in members.iter_mut() {
            if !info.no_unique_address.contains(&m.name) {
                continue;
            }
            let ty_name = match &m.ty {
                TypeInfo::Primitive { name, .. } | TypeInfo::Opaque { name, .. } => name.clone(),
                _ => continue,
            };
            if let (sz, al, true) = cpp_subobject(&ty_name, &views, ps, msvc) {
                m.size = sz;
                m.align = al;
            }
        }
        let mut vbases = Vec::new();
        cpp_virtual_bases(&layout.name, &views, &mut vbases, 0);
        let mut tail = Vec::new();
        for vb in vbases {
            let (sz, al) = match views.get(&vb) {
                Some(_) if cpp_empty(&vb, &views, 0) => (0, 1),
                Some(v) => (v.nvsize, v.nvalign),
                None => (ps, ps),
            };
            tail.push(cpp_slot(format!("__vbase_{vb}"), &vb, sz, al));
        }

        let rebuilt: Vec<padlock_core::ir::Field> =
            slots.into_iter().chain(members).chain(tail).collect();
        let same = rebuilt.len() == layout.fields.len()
            && rebuilt
                .iter()
                .zip(&layout.fields)
                .all(|(a, b)| a.name == b.name && a.size == b.size && a.align == b.align);
        if same {
            continue;
        }
        layout.fields = rebuilt;
        // `packed` applies to data members, not to base subobjects or the vptr.
        for f in &layout.fields {
            if is_cpp_slot(&f.name) && !layout.explicit_align.contains(&f.name) {
                layout.explicit_align.push(f.name.clone());
            }
        }
        resimulate_layout(layout);
        if layout.total_size == 0 {
            // C++ objects are never zero-sized.
            layout.total_size = 1;
        }
        changed_any = true;
    }
    changed_any
}

/// Drop the synthetic layouts of anonymous member aggregates once they have done
/// their job (sizing their parents), and show the members that use them as
/// `struct { … }` / `union { … }`.
pub(crate) fn hide_anonymous_aggregates(layouts: &mut Vec<StructLayout>) {
    let prefix = frontends::c_cpp::ANON_PREFIX;
    layouts.retain(|l| !l.name.starts_with(prefix));
    for l in layouts.iter_mut() {
        for f in l.fields.iter_mut() {
            if let TypeInfo::Primitive { name, .. } | TypeInfo::Opaque { name, .. } = &mut f.ty
                && let Some(rest) = name.strip_prefix(prefix)
            {
                let kind = rest.split('#').next().unwrap_or("struct");
                let dims = name
                    .find('[')
                    .map(|i| name[i..].to_string())
                    .unwrap_or_default();
                *name = format!("{kind} {{ … }}{dims}");
            }
        }
    }
}

/// Split `Name[2][3]` into (`Name`, 6). `[]` counts as 0 elements and an
/// unevaluated `[?]` as 1; a name without a suffix is (`Name`, 1).
pub(crate) fn split_c_array_suffix(ty: &str) -> (&str, usize) {
    let Some(open) = ty.find('[') else {
        return (ty, 1);
    };
    // `Pair[int, string]` (a Go instantiation) is a name, not an array.
    let first = ty[open + 1..].split(']').next().unwrap_or("").trim();
    if !(first.is_empty() || first == "?" || first.bytes().all(|b| b.is_ascii_digit())) {
        return (ty, 1);
    }
    let mut count = 1usize;
    for dim in ty[open..].split('[').skip(1) {
        let n = dim.trim_end_matches(']').trim();
        count = count.saturating_mul(match n {
            "" => 0,
            n => n.parse().unwrap_or(1),
        });
    }
    (ty[..open].trim_end(), count)
}

/// Resolve fields whose type name matches another parsed struct.
///
/// Runs in a loop until stable to handle transitive nesting (struct A contains
/// B which contains C). In practice, 2–3 iterations suffice for typical code.
pub(crate) fn resolve_nested_structs(layouts: &mut [StructLayout]) {
    // Bound iterations to guard against size oscillation from parse-error trees.
    // Real transitive nesting resolves in ≤ depth iterations; depth is bounded by
    // the number of distinct struct types in the file.
    let max_iters = layouts.len().max(4) * 2;
    let mut iters = 0;
    loop {
        if iters >= max_iters {
            break;
        }
        iters += 1;
        // Build name → (total_size, align) from whatever we have so far.
        let known: HashMap<String, (usize, usize)> = layouts
            .iter()
            .map(|l| (l.name.clone(), (l.total_size, l.align)))
            .collect();
        // C++ classes that are not POD: GCC ignores `packed` for members of such
        // a type ("ignoring packed attribute because of unpacked non-POD field").
        let mut non_pod: std::collections::HashSet<String> = layouts
            .iter()
            .filter(|l| {
                l.cpp
                    .as_ref()
                    .is_some_and(|i| i.non_pod || i.has_virtual_methods || !i.bases.is_empty())
            })
            .map(|l| l.name.clone())
            .collect();
        let packed_types: std::collections::HashSet<String> = layouts
            .iter()
            .filter(|l| l.is_packed)
            .map(|l| l.name.clone())
            .collect();
        // …and so is any C++ class or union with a non-POD member.
        loop {
            let before = non_pod.len();
            for l in layouts.iter().filter(|l| l.cpp.is_some()) {
                let has_non_pod_member = l.fields.iter().any(|f| match &f.ty {
                    TypeInfo::Primitive { name, .. } | TypeInfo::Opaque { name, .. } => {
                        non_pod.contains(split_c_array_suffix(name).0)
                    }
                    _ => false,
                });
                if has_non_pod_member {
                    non_pod.insert(l.name.clone());
                }
            }
            if non_pod.len() == before {
                break;
            }
        }

        let mut changed_any = apply_cpp_inheritance(layouts);

        for layout in layouts.iter_mut() {
            let mut changed = false;

            for field in layout.fields.iter_mut() {
                // Extract the type name from Primitive or Opaque variants.
                // Struct/Pointer/Array variants are already correctly sized.
                let type_name: String = match &field.ty {
                    TypeInfo::Primitive { name, .. } | TypeInfo::Opaque { name, .. } => {
                        name.clone()
                    }
                    _ => continue,
                };

                // C++ base/virtual-base slots and `[[no_unique_address]]` members are
                // sized by `apply_cpp_inheritance`, not as whole embedded objects.
                if let Some(info) = &layout.cpp
                    && (field.name.starts_with("__base_")
                        || field.name.starts_with("__vbase_")
                        || info.no_unique_address.contains(&field.name))
                {
                    continue;
                }

                // `Inner[2][3]` is an array of a named struct: resolve the element
                // type and scale by the element count.
                let (elem_name, elem_count) = split_c_array_suffix(&type_name);

                // Never shadow built-in primitives.
                if is_known_primitive(elem_name) {
                    continue;
                }

                // Don't resolve a struct to itself (circular).
                if elem_name == layout.name {
                    continue;
                }

                if let Some(&(elem_size, struct_align)) = known.get(elem_name) {
                    let struct_size = elem_size.saturating_mul(elem_count);
                    // An explicitly aligned member keeps its alignment (never below
                    // the type's own); in a packed struct others drop to 1.
                    // (A type that is itself packed can be packed into another.)
                    if layout.is_packed
                        && non_pod.contains(elem_name)
                        && !packed_types.contains(elem_name)
                    {
                        // Track the member type's alignment as it gets resolved.
                        if field.align != struct_align {
                            field.align = struct_align;
                            changed = true;
                        }
                        if !layout.explicit_align.contains(&field.name) {
                            layout.explicit_align.push(field.name.clone());
                            changed = true;
                        }
                    }
                    let eff_align = if layout.explicit_align.contains(&field.name) {
                        // In a packed struct the explicit value stands as written.
                        if layout.is_packed || layout.pack_limit.is_some() {
                            field.align
                        } else {
                            field.align.max(struct_align)
                        }
                    } else if layout.is_packed {
                        1
                    } else {
                        struct_align
                    };
                    // Only update if something would change — avoids infinite loops
                    // for pointer-sized structs that already have the right size.
                    if field.size == struct_size && field.align == eff_align {
                        continue;
                    }
                    field.ty = TypeInfo::Opaque {
                        name: type_name,
                        size: struct_size,
                        align: struct_align,
                    };
                    field.size = struct_size;
                    field.align = eff_align;
                    changed = true;
                }
            }

            if changed {
                resimulate_layout(layout);
                changed_any = true;
            }
        }

        if !changed_any {
            break;
        }
    }

    // Post-pass: any field still TypeInfo::Opaque whose type name is not among
    // the parsed structs was not resolved — its size is a pointer-sized fallback.
    // Flag these so output layers can warn the user.
    let known_names: std::collections::HashSet<String> =
        layouts.iter().map(|l| l.name.clone()).collect();

    for layout in layouts.iter_mut() {
        for field in layout.fields.iter() {
            if let TypeInfo::Opaque {
                name: type_name, ..
            } = &field.ty
            {
                let elem_name = split_c_array_suffix(type_name).0;
                if is_known_primitive(elem_name)
                    || elem_name == layout.name
                    || known_names.contains(elem_name)
                {
                    continue;
                }
                if !layout.uncertain_fields.contains(&field.name) {
                    layout.uncertain_fields.push(field.name.clone());
                }
            }
        }
    }

    // A member whose own type has an uncertain size is uncertain too, however
    // deeply nested (e.g. a struct holding a struct with an external field).
    loop {
        let uncertain_types: std::collections::HashSet<String> = layouts
            .iter()
            .filter(|l| !l.uncertain_fields.is_empty())
            .map(|l| l.name.clone())
            .collect();
        let mut changed = false;
        for layout in layouts.iter_mut() {
            for field in &layout.fields {
                let (TypeInfo::Primitive { name, .. } | TypeInfo::Opaque { name, .. }) = &field.ty
                else {
                    continue;
                };
                let elem = split_c_array_suffix(name).0;
                if elem != layout.name
                    && uncertain_types.contains(elem)
                    && !layout.uncertain_fields.contains(&field.name)
                {
                    layout.uncertain_fields.push(field.name.clone());
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Re-simulate field offsets and total_size after field sizes have been updated.
fn resimulate_layout(layout: &mut StructLayout) {
    if layout.is_union {
        for field in layout.fields.iter_mut() {
            field.offset = 0;
        }
        let max_size = layout.fields.iter().map(|f| f.size).max().unwrap_or(0);
        // `union U { … } __attribute__((aligned(64)))` keeps its alignment.
        let max_align = layout
            .fields
            .iter()
            .map(|f| f.align)
            .max()
            .unwrap_or(1)
            .max(layout.forced_align.unwrap_or(1));
        layout.total_size = if max_align > 0 {
            max_size.next_multiple_of(max_align)
        } else {
            max_size
        };
        layout.align = max_align;
        return;
    }

    if layout.is_repr_rust && !layout.is_packed && !declared_order() {
        // Stable: equal alignments keep their current (declaration) order.
        layout.fields.sort_by_key(|f| std::cmp::Reverse(f.align));
    }
    let packed = layout.is_packed;
    let cap = if packed { Some(1) } else { layout.pack_limit };
    let mut offset = 0usize;
    let mut struct_align = 1usize;

    for field in layout.fields.iter_mut() {
        let eff_align = if layout.explicit_align.contains(&field.name) {
            // `#pragma pack(N)` caps even explicit alignment (the attribute does not).
            layout
                .pack_limit
                .map_or(field.align, |p| field.align.min(p))
        } else {
            cap.map_or(field.align, |c| field.align.min(c))
        };
        if eff_align > 0 {
            offset = offset.next_multiple_of(eff_align);
        }
        field.offset = offset;
        offset += field.size;
        struct_align = struct_align.max(eff_align);
    }

    // `alignas(N)` / `repr(align(N))` / a Zig packed struct's backing integer:
    // the struct's own alignment survives re-layout, and pads even a packed struct.
    if let Some(fa) = layout.forced_align {
        struct_align = struct_align.max(fa);
    }
    // Packed structs have alignment 1 unless a member is explicitly aligned, so
    // rounding is a no-op for them.
    if struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }

    layout.total_size = offset;
    layout.align = struct_align;
}

/// Scan `source` once and return all struct/type names annotated with
/// `// padlock:ignore`.  O(source_len) regardless of struct count.
///
/// Handles both forms:
/// - inline: `struct Foo { … }; // padlock:ignore`
/// - preceding line: `// padlock:ignore\nstruct Foo { … }`
fn collect_padlock_ignored_names(source: &str) -> std::collections::HashSet<String> {
    let mut ignored = std::collections::HashSet::new();
    let mut pos = 0;
    while let Some(rel) = source[pos..].find("padlock:ignore") {
        let abs = pos + rel;
        let after = abs + "padlock:ignore".len();
        // Skip per-finding annotations: `padlock:ignore[...]` (bare `padlock:ignore` only)
        if source[after..].starts_with('[') {
            pos = after;
            continue;
        }
        let line_start = source[..abs].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = source[abs..]
            .find('\n')
            .map(|i| abs + i)
            .unwrap_or(source.len());
        let line_trimmed = source[line_start..line_end].trim();

        if let Some(name) = struct_name_on_line(source, line_start, line_end) {
            // Inline annotation on a struct declaration line.
            ignored.insert(name);
        } else if line_trimmed.starts_with("//") && line_end + 1 < source.len() {
            // Pure comment line — look at the immediately following line.
            let next_start = line_end + 1;
            let next_end = source[next_start..]
                .find('\n')
                .map(|i| next_start + i)
                .unwrap_or(source.len());
            if let Some(name) = struct_name_on_line(source, next_start, next_end) {
                ignored.insert(name);
            }
        }
        pos = after;
    }
    ignored
}

/// Extract the declared struct/union/type name from a source line, if present.
fn struct_name_on_line(source: &str, line_start: usize, line_end: usize) -> Option<String> {
    let line = &source[line_start..line_end];
    for kw in &["struct ", "union ", "type "] {
        if let Some(kw_pos) = line.find(kw) {
            let rest = &line[kw_pos + kw.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let after = kw_pos + kw.len() + name.len();
            let is_boundary = line[after..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if is_boundary {
                return Some(name);
            }
        }
    }
    None
}

/// Returns `true` if a `// padlock:ignore` comment appears on the line
/// immediately before (or inline on the same line as) the struct/union/type
/// declaration for `struct_name`.
///
/// This allows callers to suppress analysis for a specific struct by writing:
/// ```c
/// // padlock:ignore
/// struct MySpecialLayout { ... };
/// ```
#[cfg(test)]
fn is_padlock_ignored(source: &str, struct_name: &str) -> bool {
    // Keywords that introduce named type definitions across all supported languages
    for keyword in &["struct", "union", "type"] {
        let needle = format!("{keyword} {struct_name}");
        let mut search = 0usize;
        while let Some(rel) = source[search..].find(&needle) {
            let abs = search + rel;
            // Ensure the character after the name is a word boundary (not part of a longer name)
            let after_name = abs + needle.len();
            let is_boundary = source[after_name..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if is_boundary {
                let line_start = source[..abs].rfind('\n').map(|i| i + 1).unwrap_or(0);
                // Check the line containing the struct keyword for an inline annotation
                let line_end = source[abs..]
                    .find('\n')
                    .map(|i| abs + i)
                    .unwrap_or(source.len());
                if source[line_start..line_end].contains("padlock:ignore") {
                    return true;
                }
                // Check the immediately preceding line for an annotation comment.
                // Only accept it if the preceding line is a pure comment (starts with `//`
                // after trimming), so that an inline annotation on a prior struct's closing
                // line doesn't accidentally suppress the following struct.
                if line_start > 0 {
                    let prev_end = line_start - 1;
                    let prev_start = source[..prev_end].rfind('\n').map(|i| i + 1).unwrap_or(0);
                    let prev_trimmed = source[prev_start..prev_end].trim();
                    if prev_trimmed.starts_with("//") && prev_trimmed.contains("padlock:ignore") {
                        return true;
                    }
                }
            }
            search = abs + 1;
        }
    }
    false
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn detect_c_extensions() {
        assert_eq!(detect_language(Path::new("foo.c")), Some(SourceLanguage::C));
        assert_eq!(detect_language(Path::new("foo.h")), Some(SourceLanguage::C));
    }

    #[test]
    fn detect_cpp_extensions() {
        assert_eq!(
            detect_language(Path::new("foo.cpp")),
            Some(SourceLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.cc")),
            Some(SourceLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.hpp")),
            Some(SourceLanguage::Cpp)
        );
    }

    #[test]
    fn detect_rust_extension() {
        assert_eq!(
            detect_language(Path::new("foo.rs")),
            Some(SourceLanguage::Rust)
        );
    }

    #[test]
    fn detect_go_extension() {
        assert_eq!(
            detect_language(Path::new("foo.go")),
            Some(SourceLanguage::Go)
        );
    }

    #[test]
    fn detect_zig_extension() {
        assert_eq!(
            detect_language(Path::new("foo.zig")),
            Some(SourceLanguage::Zig)
        );
    }

    #[test]
    fn detect_unknown_is_none() {
        assert_eq!(detect_language(Path::new("foo.py")), None);
        assert_eq!(detect_language(Path::new("foo")), None);
    }

    #[test]
    fn parse_source_str_c_roundtrip() {
        let src = "struct Point { int x; int y; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
    }

    #[test]
    fn parse_source_str_rust_roundtrip() {
        let src = "struct Foo { x: u32, y: u64 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Foo");
    }

    #[test]
    fn padlock_ignore_suppresses_c_struct() {
        let src = "// padlock:ignore\nstruct Hidden { int x; int y; };\nstruct Visible { int a; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
    }

    #[test]
    fn padlock_ignore_inline_suppresses_c_struct() {
        // Inline annotation on the struct's own line suppresses it, but must NOT
        // suppress the struct that follows (the next struct's preceding line is a
        // code line with a trailing comment, not a pure `//` comment line).
        let src = "struct Hidden { int x; }; // padlock:ignore\nstruct Visible { int a; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1, "only Visible should remain");
        assert_eq!(layouts[0].name, "Visible");
    }

    #[test]
    fn padlock_ignore_suppresses_rust_struct() {
        let src = "// padlock:ignore\nstruct Hidden { x: u32 }\nstruct Visible { a: u32 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
    }

    #[test]
    fn padlock_ignore_without_annotation_keeps_struct() {
        let src = "struct Visible { int x; int y; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
    }

    // ── nested struct resolution ───────────────────────────────────────────────

    #[test]
    fn nested_rust_struct_size_resolved() {
        // Inner is 8 bytes. Outer has a field of type Inner.
        // Without resolution, Inner's field size would be pointer_size (8) — coincidentally
        // correct here, but offset placement still validates the pass runs.
        let src = "struct Inner { x: u64 }\nstruct Outer { a: u8, b: Inner }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let outer = layouts.iter().find(|l| l.name == "Outer").unwrap();
        let b = outer.fields.iter().find(|f| f.name == "b").unwrap();
        assert_eq!(b.size, 8, "Inner is 8 bytes");
        assert_eq!(b.align, 8, "Inner aligns to 8");
        // Outer: u8 at 0, [7 pad], Inner at 8 → total 16
        assert_eq!(outer.total_size, 16);
    }

    #[test]
    fn nested_rust_struct_non_pointer_size_resolved() {
        // Point is 8 bytes (two i32). Line contains two Points — should be 16 bytes, not
        // 2 * pointer_size = 16 (same here, but alignment is distinct).
        let src = "struct Point { x: i32, y: i32 }\nstruct Line { a: Point, b: Point }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let line = layouts.iter().find(|l| l.name == "Line").unwrap();
        assert_eq!(line.total_size, 16);
        assert_eq!(line.fields[0].size, 8);
        assert_eq!(line.fields[1].size, 8);
        assert_eq!(line.fields[1].offset, 8);
    }

    #[test]
    fn nested_rust_struct_large_inner_triggers_padding() {
        // SmallHeader: bool (1 byte). BigPayload: [u64; 4] = 32 bytes.
        // Wrapper { flag: SmallHeader, data: BigPayload }
        // Without resolution: SmallHeader is pointer-sized (8), total 8+32=40 → wrong.
        // With resolution: SmallHeader is 1 byte, then 7 pad, then BigPayload at 8 → total 40.
        // Actually u64 array: [u64;4] parsed as Array of 4 u64 = 32 bytes, align 8.
        let src = "struct SmallHeader { flag: bool }\n#[repr(C)] struct Wrapper { h: SmallHeader, data: u64 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let wrapper = layouts.iter().find(|l| l.name == "Wrapper").unwrap();
        let h = wrapper.fields.iter().find(|f| f.name == "h").unwrap();
        // SmallHeader has total_size=1, align=1
        assert_eq!(h.size, 1, "SmallHeader resolved to 1 byte");
        assert_eq!(h.align, 1);
        // data (u64, align 8) should be at offset 8 (7 bytes padding after SmallHeader)
        let data = wrapper.fields.iter().find(|f| f.name == "data").unwrap();
        assert_eq!(data.offset, 8);
        assert_eq!(wrapper.total_size, 16);
    }

    #[test]
    fn nested_c_struct_resolved() {
        let src =
            "struct Vec2 { float x; float y; };\nstruct Rect { struct Vec2 tl; struct Vec2 br; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        let rect = layouts.iter().find(|l| l.name == "Rect").unwrap();
        // Each Vec2 is 8 bytes (two floats). Rect = 16 bytes, no padding.
        assert_eq!(rect.total_size, 16, "Rect should be 16 bytes");
        assert_eq!(rect.fields[0].size, 8);
        assert_eq!(rect.fields[1].size, 8);
        assert_eq!(rect.fields[1].offset, 8);
    }

    #[test]
    fn nested_go_struct_resolved() {
        let src = "package p\ntype Vec2 struct { X float32; Y float32 }\ntype Rect struct { TL Vec2; BR Vec2 }";
        let layouts = parse_source_str(src, &SourceLanguage::Go, &X86_64_SYSV).unwrap();
        let rect = layouts.iter().find(|l| l.name == "Rect").unwrap();
        assert_eq!(rect.total_size, 16);
        assert_eq!(rect.fields[0].size, 8);
        assert_eq!(rect.fields[1].size, 8);
        assert_eq!(rect.fields[1].offset, 8);
    }

    #[test]
    fn primitive_types_not_shadowed_by_struct_resolution() {
        // A struct named "u64" would be very unusual, but primitives must not be overwritten.
        let src = "struct Wrapper { x: u64, y: bool }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let w = &layouts[0];
        let x = w.fields.iter().find(|f| f.name == "x").unwrap();
        assert_eq!(x.size, 8, "u64 must stay 8 bytes");
    }

    #[test]
    fn is_padlock_ignored_does_not_match_partial_names() {
        // "struct Foo" annotation must not suppress "struct FooBar"
        assert!(!is_padlock_ignored(
            "// padlock:ignore\nstruct FooBar { int x; };",
            "Foo"
        ));
    }

    // ── per-finding suppression integration ───────────────────────────────────

    #[test]
    fn per_finding_suppress_reorder_in_c() {
        // The struct has padding waste (bool before u64) — without suppression
        // this would produce PaddingWaste + ReorderSuggestion. With the annotation
        // on the preceding line, only PaddingWaste should survive.
        let src = "// padlock: ignore[ReorderSuggestion]\nstruct Foo { char a; long b; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].suppressed_findings, vec!["ReorderSuggestion"]);
    }

    #[test]
    fn per_finding_suppress_multiple_kinds_in_c() {
        let src =
            "// padlock: ignore[PaddingWaste, ReorderSuggestion]\nstruct Bar { char a; long b; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(
            layouts[0].suppressed_findings,
            vec!["PaddingWaste", "ReorderSuggestion"]
        );
    }

    #[test]
    fn per_finding_suppress_in_rust() {
        let src = "// padlock: ignore[FalseSharing]\nstruct Foo { x: u64, y: u64 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].suppressed_findings, vec!["FalseSharing"]);
    }

    #[test]
    fn per_finding_suppress_in_go() {
        let src =
            "package p\n// padlock: ignore[LocalityIssue]\ntype Foo struct { X int64; Y int64 }";
        let layouts = parse_source_str(src, &SourceLanguage::Go, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].suppressed_findings, vec!["LocalityIssue"]);
    }

    #[test]
    fn unannotated_struct_has_no_suppressed_findings() {
        let src = "struct Clean { int x; int y; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert!(layouts[0].suppressed_findings.is_empty());
    }

    // ── C++ inheritance base-size resolution ─────────────────────────────────

    #[test]
    fn cpp_inheritance_base_size_resolved_via_parse_source_str() {
        // Base has two ints = 8 bytes. Derived inherits Base and adds one int.
        // After resolve_nested_structs, __base_Base must be 8 bytes (not
        // pointer-sized 8B by coincidence — we use a 4-byte base to verify).
        let src = r#"
class SmallBase { int x; };
class BigDerived : public SmallBase { int a; int b; int c; };
"#;
        let layouts = parse_source_str(src, &SourceLanguage::Cpp, &X86_64_SYSV).unwrap();
        let derived = layouts.iter().find(|l| l.name == "BigDerived").unwrap();
        let base_field = derived
            .fields
            .iter()
            .find(|f| f.name == "__base_SmallBase")
            .unwrap();
        // SmallBase is 4 bytes (single int, no padding), so after resolution
        // the synthetic field must be 4 bytes, not 8 (pointer size).
        assert_eq!(
            base_field.size, 4,
            "__base_SmallBase should be resolved to 4 bytes (sizeof SmallBase)"
        );
        // BigDerived total: 4 (base) + 4*3 (a,b,c) = 16 bytes
        assert_eq!(derived.total_size, 16);
    }

    #[test]
    fn cpp_multi_level_inheritance_resolved() {
        let src = r#"
class A { int x; };
class B : public A { int y; };
class C : public B { int z; };
"#;
        let layouts = parse_source_str(src, &SourceLanguage::Cpp, &X86_64_SYSV).unwrap();
        let c = layouts.iter().find(|l| l.name == "C").unwrap();
        // C has __base_B (which is 8 bytes: A(4)+y(4)) + z(4) = 12 bytes
        let base_b = c.fields.iter().find(|f| f.name == "__base_B").unwrap();
        assert_eq!(base_b.size, 8, "B is 8 bytes (A's int + B's int)");
        assert_eq!(c.total_size, 12);
    }

    // ── end-to-end: Arc<T> through the real pipeline ───────────────────────
    //
    // The name-enrichment fix (rust.rs) and the concurrency classification
    // fix (concurrency.rs) were each tested in isolation at their own unit
    // level — rust.rs tests call `parse_rust` directly (which never runs
    // `annotate_concurrency`), and concurrency.rs tests hand-build a
    // `StructLayout` with an already-enriched type name string. Neither
    // proves the two halves actually compose correctly through the real
    // path a user hits (`parse_source_str`, which chains parse_rust ->
    // annotate_concurrency). These tests close that gap.

    #[test]
    fn end_to_end_plain_arc_fields_not_flagged_concurrent() {
        let src = "struct Config { name: std::sync::Arc<String>, logger: std::sync::Arc<Logger> }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let config = &layouts[0];
        assert!(
            !concurrency::has_concurrent_fields(config),
            "plain Arc<T> fields must not be classified as a concurrency hazard \
             through the real parse pipeline, not just in isolated unit tests"
        );
    }

    #[test]
    fn end_to_end_arc_mutex_fields_still_flagged_concurrent() {
        let src = "struct Shared { a: std::sync::Arc<std::sync::Mutex<u64>>, b: std::sync::Arc<std::sync::Mutex<u64>> }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let shared = &layouts[0];
        assert!(
            concurrency::has_concurrent_fields(shared),
            "Arc<Mutex<T>> must still be classified as concurrent through the real pipeline"
        );
    }

    #[test]
    fn end_to_end_arc_rwlock_field_is_concurrent() {
        // Not covered by any existing unit test: RwLock wrapped in Arc,
        // exercising the `.contains("RwLock")` branch (changed from
        // `starts_with` alongside the Mutex fix) through the real pipeline.
        let src = "struct S { data: std::sync::Arc<std::sync::RwLock<Vec<u8>>> }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert!(concurrency::has_concurrent_fields(&layouts[0]));
    }

    #[test]
    fn end_to_end_rc_refcell_is_not_flagged_concurrent() {
        // Rc<RefCell<T>> is the standard single-threaded shared-mutable-state
        // pattern in Rust — RefCell provides no thread synchronization at
        // all, so this must stay Unknown, not Concurrent, regardless of the
        // Arc/Rc enrichment change.
        let src = "struct S { shared: std::rc::Rc<std::cell::RefCell<Vec<u8>>> }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert!(!concurrency::has_concurrent_fields(&layouts[0]));
    }

    // ── C++ shared_ptr<T>/unique_ptr<T>: lock in that this bug class does
    // not apply here ──────────────────────────────────────────────────────
    //
    // C++'s frontend extracts type names as raw source text (tree-sitter
    // byte ranges), not a collapsed AST leaf identifier like Rust's `syn`
    // does — so `std::shared_ptr<std::mutex>` naturally keeps "mutex" in
    // its recorded name with no enrichment needed. Verified against the
    // real binary (0.24.0) before writing this: plain shared_ptr<T> scores
    // 100 with no finding; shared_ptr<std::mutex> still correctly triggers
    // FalseSharing. This test locks that in — it would regress silently if
    // the C++ type-name extraction ever changed to a simplified form.

    #[test]
    fn end_to_end_cpp_shared_ptr_plain_not_flagged_concurrent() {
        let src = r#"
struct Config {
    std::shared_ptr<std::string> name;
    std::shared_ptr<int> logger;
};
"#;
        let layouts = parse_source_str(src, &SourceLanguage::Cpp, &X86_64_SYSV).unwrap();
        assert!(
            !concurrency::has_concurrent_fields(&layouts[0]),
            "plain std::shared_ptr<T> must not be classified as a concurrency hazard"
        );
    }

    #[test]
    fn end_to_end_cpp_shared_ptr_mutex_still_flagged_concurrent() {
        let src = r#"
struct Shared {
    std::shared_ptr<std::mutex> a;
    std::shared_ptr<std::mutex> b;
};
"#;
        let layouts = parse_source_str(src, &SourceLanguage::Cpp, &X86_64_SYSV).unwrap();
        assert!(concurrency::has_concurrent_fields(&layouts[0]));
    }

    // ── re-layout after nested resolution keeps struct-level attributes ──────

    fn find<'a>(ls: &'a [StructLayout], name: &str) -> &'a StructLayout {
        ls.iter().find(|l| l.name == name).unwrap()
    }

    #[test]
    fn nested_resolution_keeps_alignas_and_pragma_pack() {
        let src = "struct In { char x; int y; };
struct alignas(64) A { In i; char c; };
#pragma pack(push, 2)
struct P { char a; In i; long l; };
#pragma pack(pop)";
        let ls =
            parse_source_str(src, &SourceLanguage::Cpp, &padlock_core::arch::X86_64_SYSV).unwrap();
        // g++: sizeof(A) == 64, sizeof(P) == 18
        assert_eq!(find(&ls, "A").total_size, 64);
        assert_eq!(find(&ls, "P").total_size, 18);
    }

    #[test]
    fn nested_resolution_keeps_repr_align_and_packed_n() {
        let src = "#[repr(C)] struct In { x: u8, y: u32 }
#[repr(C, align(64))] struct A { i: In, c: u8 }
#[repr(C, packed(2))] struct P { a: u8, i: In, l: u64 }";
        let ls =
            parse_source_str(src, &SourceLanguage::Rust, &padlock_core::arch::X86_64_SYSV).unwrap();
        assert_eq!(find(&ls, "A").total_size, 64);
        assert_eq!(find(&ls, "P").total_size, 18);
    }

    #[test]
    fn zig_packed_struct_nested_in_packed_struct_counts_bits() {
        let src = "const In = packed struct { a: u3, b: u2 };
const P = packed struct { x: In, y: u8, z: In };";
        let ls =
            parse_source_str(src, &SourceLanguage::Zig, &padlock_core::arch::X86_64_SYSV).unwrap();
        // 5 + 8 + 5 = 18 bits → u18 → 4 bytes (zig @sizeOf)
        assert_eq!(find(&ls, "P").total_size, 4);
    }

    // ── C++ inheritance (values from g++ -std=c++17) ──────────────────────────

    fn cpp(src: &str) -> Vec<StructLayout> {
        parse_source_str(src, &SourceLanguage::Cpp, &padlock_core::arch::X86_64_SYSV).unwrap()
    }

    fn field_off(l: &StructLayout, name: &str) -> usize {
        l.fields.iter().find(|f| f.name == name).unwrap().offset
    }

    #[test]
    fn non_pod_base_tail_padding_is_reused() {
        let ls = cpp("struct Pod { int a; char b; }; struct D1 : Pod { char c; };
class Priv { int a; char b; public: int get(); }; struct D2 : Priv { char c; };
struct Ctor { int a; char b; Ctor(); }; struct D3 : Ctor { char c; };");
        assert_eq!(find(&ls, "D1").total_size, 12);
        assert_eq!(find(&ls, "D2").total_size, 8);
        assert_eq!(field_off(find(&ls, "D2"), "c"), 5);
        assert_eq!(find(&ls, "D3").total_size, 8);
    }

    #[test]
    fn empty_bases_and_no_unique_address_take_no_space() {
        let ls = cpp("struct Empty {};
struct E1 : Empty { int x; };
struct E2 : Empty {};
struct N { char a; [[no_unique_address]] Empty e; int x; };");
        assert_eq!(find(&ls, "Empty").total_size, 1);
        assert_eq!(find(&ls, "E1").total_size, 4);
        assert_eq!(find(&ls, "E2").total_size, 1);
        assert_eq!(find(&ls, "N").total_size, 8);
    }

    #[test]
    fn virtual_bases_are_placed_once_after_the_non_virtual_part() {
        let ls = cpp("struct A { int a; };
struct B : virtual A { int b; };
struct C : virtual A { int c; };
struct D : B, C { int d; };");
        assert_eq!(find(&ls, "B").total_size, 16);
        let d = find(&ls, "D");
        assert_eq!(d.total_size, 40);
        assert_eq!(field_off(d, "d"), 28);
        assert_eq!(field_off(d, "__vbase_A"), 32);
    }

    #[test]
    fn primary_base_is_the_first_dynamic_base() {
        let ls = cpp("struct NonDyn { char z; };
struct Vf { virtual void f(); char b; };
struct P : NonDyn, Vf { char q; };");
        let p = find(&ls, "P");
        assert_eq!(field_off(p, "__base_Vf"), 0);
        assert_eq!(field_off(p, "__base_NonDyn"), 9);
        assert_eq!(p.total_size, 16);
    }

    #[test]
    fn cpp_content_in_a_dot_h_header_is_parsed_as_cpp() {
        let p = Path::new("x.h");
        assert_eq!(
            detect_language_from_source(p, "struct S { int a; };\n"),
            Some(SourceLanguage::C)
        );
        assert_eq!(
            detect_language_from_source(p, "class S {\npublic:\n  int a;\n};\n"),
            Some(SourceLanguage::Cpp)
        );
        assert_eq!(
            detect_language_from_source(p, "namespace n {\nstruct S { std::string s; };\n}\n"),
            Some(SourceLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("x.hh")),
            Some(SourceLanguage::Cpp)
        );
    }

    #[test]
    fn cpp_namespaces_keep_same_named_types_apart() {
        let ls = cpp("namespace na { struct Same { char a; long b; }; }
namespace nb { struct Same { char a; char b; }; struct UseB { char x; Same s; char z; }; }
struct UseA { char x; na::Same s; char z; };
struct Outer { struct Inner { char c; int i; }; char a; Inner in; char z; };");
        assert_eq!(find(&ls, "na::Same").total_size, 16);
        assert_eq!(find(&ls, "nb::Same").total_size, 2);
        // `Same` inside nb means nb::Same (g++: sizeof(nb::UseB) == 4).
        assert_eq!(find(&ls, "nb::UseB").total_size, 4);
        assert_eq!(find(&ls, "UseA").total_size, 32);
        assert_eq!(find(&ls, "Outer::Inner").total_size, 8);
        assert_eq!(find(&ls, "Outer").total_size, 16);
    }

    #[test]
    fn typedef_names_the_right_anonymous_struct() {
        let ls = parse_source_str(
            "struct A { struct { int x; } n; };\ntypedef struct { char c; long l; } T;",
            &SourceLanguage::C,
            &padlock_core::arch::X86_64_SYSV,
        )
        .unwrap();
        assert_eq!(find(&ls, "T").total_size, 16);
    }
}
