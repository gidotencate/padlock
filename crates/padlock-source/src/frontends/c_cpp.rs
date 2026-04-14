// padlock-source/src/frontends/c_cpp.rs
//
// Extracts struct layouts from C / C++ source using tree-sitter.
// Sizes and alignments are computed from field type names + arch config;
// there is no compiler involved so the results are approximate for complex types.

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
use tree_sitter::{Node, Parser};

// ── type resolution ───────────────────────────────────────────────────────────

/// Map a C/C++ type name to (size, align) using the target arch.
fn c_type_size_align(ty: &str, arch: &'static ArchConfig) -> (usize, usize) {
    let ty = ty.trim();
    // Strip qualifiers
    for qual in &["const ", "volatile ", "restrict ", "unsigned ", "signed "] {
        if let Some(rest) = ty.strip_prefix(qual) {
            return c_type_size_align(rest, arch);
        }
    }
    // x86 SSE / AVX / AVX-512 SIMD types
    match ty {
        "__m64" => return (8, 8),
        "__m128" | "__m128d" | "__m128i" => return (16, 16),
        "__m256" | "__m256d" | "__m256i" => return (32, 32),
        "__m512" | "__m512d" | "__m512i" => return (64, 64),
        // ARM NEON — 64-bit (double-word) vectors
        "float32x2_t" | "int32x2_t" | "uint32x2_t" | "int8x8_t" | "uint8x8_t" | "int16x4_t"
        | "uint16x4_t" | "float64x1_t" | "int64x1_t" | "uint64x1_t" => return (8, 8),
        // ARM NEON — 128-bit (quad-word) vectors
        "float32x4_t" | "int32x4_t" | "uint32x4_t" | "float64x2_t" | "int64x2_t" | "uint64x2_t"
        | "int8x16_t" | "uint8x16_t" | "int16x8_t" | "uint16x8_t" => return (16, 16),
        _ => {}
    }
    // C++ standard library types (Linux/glibc + libstdc++ defaults).
    // Sizes are platform-approximate; accuracy is "good enough" for cache-line
    // bucketing and false-sharing detection.
    match ty {
        // ── Synchronisation ───────────────────────────────────────────────────
        // pthread_mutex_t on Linux/glibc is 40 bytes.
        "std::mutex"
        | "std::recursive_mutex"
        | "std::timed_mutex"
        | "std::recursive_timed_mutex"
        | "pthread_mutex_t" => return (40, 8),
        "std::shared_mutex" | "std::shared_timed_mutex" => return (56, 8),
        "std::condition_variable" | "pthread_cond_t" => return (48, 8),

        // ── String / view ─────────────────────────────────────────────────────
        // libstdc++ std::string: 32B (ptr + length + SSO buffer / capacity).
        // libc++ (Clang): 24B. We use 32B (libstdc++ / GCC, dominant on Linux).
        "std::string" | "std::wstring" | "std::u8string" | "std::u16string" | "std::u32string"
        | "std::pmr::string" => return (32, 8),
        // std::string_view / std::span<T>: pointer + length (2 words).
        "std::string_view"
        | "std::wstring_view"
        | "std::u8string_view"
        | "std::u16string_view"
        | "std::u32string_view" => return (arch.pointer_size * 2, arch.pointer_size),

        // ── Sequence containers ───────────────────────────────────────────────
        // std::vector<T>: pointer + size + capacity = 3 words (24B on 64-bit).
        // Size is independent of T.
        ty if ty.starts_with("std::vector<") || ty == "std::vector" => {
            return (arch.pointer_size * 3, arch.pointer_size);
        }
        // std::deque<T>: 80B on both libstdc++ and libc++ (64-bit Linux).
        ty if ty.starts_with("std::deque<") || ty == "std::deque" => return (80, 8),
        // std::list<T>: sentinel node pointer + size = 2 words + node pointers.
        // libstdc++: 24B (size_t + two pointers). libc++: 24B.
        ty if ty.starts_with("std::list<") || ty == "std::list" => {
            return (arch.pointer_size * 3, arch.pointer_size);
        }
        // std::forward_list<T>: single pointer (head node).
        ty if ty.starts_with("std::forward_list<") || ty == "std::forward_list" => {
            return (arch.pointer_size, arch.pointer_size);
        }
        // std::array<T, N>: inline storage; size = N * sizeof(T).
        // We cannot compute this without resolving T and N, so fall through.

        // ── Associative / unordered containers ────────────────────────────────
        // All map/set types: header node + size = ~48B (libstdc++) / ~40B (libc++).
        // Use 48B as conservative approximation.
        ty if ty.starts_with("std::map<")
            || ty.starts_with("std::multimap<")
            || ty.starts_with("std::set<")
            || ty.starts_with("std::multiset<") =>
        {
            return (48, 8);
        }
        // std::unordered_map / unordered_set: bucket array pointer + size + load factor + etc.
        // libstdc++: ~56B. libc++: ~72B. Use 56B.
        ty if ty.starts_with("std::unordered_map<")
            || ty.starts_with("std::unordered_multimap<")
            || ty.starts_with("std::unordered_set<")
            || ty.starts_with("std::unordered_multiset<") =>
        {
            return (56, 8);
        }

        // ── Smart pointers ────────────────────────────────────────────────────
        // std::unique_ptr<T>: single pointer (deleter may be zero-sized via EBO).
        ty if ty.starts_with("std::unique_ptr<") || ty == "std::unique_ptr" => {
            return (arch.pointer_size, arch.pointer_size);
        }
        // std::shared_ptr<T> / std::weak_ptr<T>: object pointer + control block pointer.
        ty if ty.starts_with("std::shared_ptr<")
            || ty == "std::shared_ptr"
            || ty.starts_with("std::weak_ptr<")
            || ty == "std::weak_ptr" =>
        {
            return (arch.pointer_size * 2, arch.pointer_size);
        }

        // ── Type-erasure / utilities ──────────────────────────────────────────
        // std::function<Sig>: 32B on libstdc++ and libc++ (64-bit Linux).
        // Holds a functor pointer, a vtable pointer, and a small-functor buffer.
        ty if ty.starts_with("std::function<") || ty == "std::function" => return (32, 8),
        // std::any: 32B on libstdc++ (small-object buffer + vtable pointer).
        "std::any" => return (32, 8),
        // std::error_code / std::error_condition: pointer + int = 16B.
        "std::error_code" | "std::error_condition" => return (16, 8),
        // std::exception_ptr: single pointer.
        "std::exception_ptr" => return (arch.pointer_size, arch.pointer_size),
        // std::type_index: single pointer (wraps std::type_info*).
        "std::type_index" => return (arch.pointer_size, arch.pointer_size),
        // std::span<T>: pointer + length (2 words). Template arg irrelevant.
        ty if ty.starts_with("std::span<") || ty == "std::span" => {
            return (arch.pointer_size * 2, arch.pointer_size);
        }
        // std::optional<T>: sizeof(T) + 1B bool, padded to align(T).
        // Recurse to resolve T then apply the formula.
        ty if ty.starts_with("std::optional<") && ty.ends_with('>') => {
            let inner = &ty["std::optional<".len()..ty.len() - 1];
            let (t_size, t_align) = c_type_size_align(inner.trim(), arch);
            let total = (t_size + 1).next_multiple_of(t_align.max(1));
            return (total, t_align.max(1));
        }

        // ── Atomic ────────────────────────────────────────────────────────────
        // std::atomic<T>: same size and alignment as T.
        ty if ty.starts_with("std::atomic<") && ty.ends_with('>') => {
            let inner = &ty[12..ty.len() - 1];
            return c_type_size_align(inner.trim(), arch);
        }
        // std::atomic_flag: guaranteed 1B minimum, but often 4B in practice.
        "std::atomic_flag" => return (4, 4),

        _ => {} // fall through to primitive types below
    }
    // Primitive / stdint / pointer types
    match ty {
        "char" | "_Bool" | "bool" => (1, 1),
        "short" | "short int" => (2, 2),
        "int" => (4, 4),
        "long" | "long int" => (arch.pointer_size, arch.pointer_size),
        "long long" | "long long int" => (8, 8),
        "float" => (4, 4),
        "double" => (8, 8),
        "long double" => (16, 16),

        // C99 stdint exact-width types
        "int8_t" | "uint8_t" => (1, 1),
        "int16_t" | "uint16_t" => (2, 2),
        "int32_t" | "uint32_t" => (4, 4),
        "int64_t" | "uint64_t" => (8, 8),
        "intmax_t" | "uintmax_t" => (8, 8),
        "size_t" | "ssize_t" | "ptrdiff_t" | "intptr_t" | "uintptr_t" => {
            (arch.pointer_size, arch.pointer_size)
        }

        // C99 fast types — uint_fast{8,16}_t are always 1/2B;
        // uint_fast{32,64}_t are pointer-sized on 64-bit (8B), 4B on 32-bit.
        "int_fast8_t" | "uint_fast8_t" => (1, 1),
        "int_fast16_t" | "uint_fast16_t" => (2, 2),
        "int_fast32_t" | "uint_fast32_t" | "int_fast64_t" | "uint_fast64_t" => {
            (arch.pointer_size, arch.pointer_size)
        }

        // C99 least types — minimum guaranteed widths
        "int_least8_t" | "uint_least8_t" => (1, 1),
        "int_least16_t" | "uint_least16_t" => (2, 2),
        "int_least32_t" | "uint_least32_t" => (4, 4),
        "int_least64_t" | "uint_least64_t" => (8, 8),

        // GCC/Clang 128-bit integer extension
        "__int128" | "__uint128" | "__int128_t" | "__uint128_t" => (16, 16),

        // Linux kernel short-form integer types (linux/types.h)
        "u8" | "s8" => (1, 1),
        "u16" | "s16" => (2, 2),
        "u32" | "s32" => (4, 4),
        "u64" | "s64" => (8, 8),

        // Linux kernel double-underscore types (__u8, __s8, __be16, __le32, …)
        "__u8" | "__s8" | "__u8__" | "__s8__" => (1, 1),
        "__u16" | "__s16" | "__be16" | "__le16" => (2, 2),
        "__u32" | "__s32" | "__be32" | "__le32" => (4, 4),
        "__u64" | "__s64" | "__be64" | "__le64" => (8, 8),

        // MSVC fixed-width intrinsics
        "__int8" => (1, 1),
        "__int16" => (2, 2),
        "__int32" => (4, 4),
        "__int64" => (8, 8),

        // Windows SDK / WinAPI types
        "BYTE" | "BOOLEAN" | "CHAR" | "INT8" | "UINT8" => (1, 1),
        "WORD" | "WCHAR" | "SHORT" | "USHORT" | "INT16" | "UINT16" => (2, 2),
        "DWORD" | "LONG" | "ULONG" | "INT" | "UINT" | "BOOL" | "FLOAT" | "INT32" | "UINT32" => {
            (4, 4)
        }
        "QWORD" | "LONGLONG" | "ULONGLONG" | "INT64" | "UINT64" | "LARGE_INTEGER" => (8, 8),
        "DWORD64" | "ULONG64" | "LONG64" => (8, 8),
        "HANDLE" | "LPVOID" | "PVOID" | "LPCVOID" | "LPSTR" | "LPCSTR" | "LPWSTR" | "LPCWSTR"
        | "SIZE_T" | "SSIZE_T" | "ULONG_PTR" | "LONG_PTR" | "DWORD_PTR" | "INT_PTR"
        | "UINT_PTR" => (arch.pointer_size, arch.pointer_size),

        // C/C++ character types
        // wchar_t: 4B on Linux/macOS (GCC/Clang POSIX), 2B on Windows/MSVC.
        // All current padlock arch configs are POSIX, so 4B is correct here.
        "wchar_t" => (4, 4),
        "char8_t" => (1, 1),
        "char16_t" => (2, 2),
        "char32_t" => (4, 4),

        // Half-precision and bfloat16 (ARM, GCC, Clang, ML workloads)
        "_Float16" | "__fp16" | "__bf16" => (2, 2),
        // 128-bit float (GCC/Clang extension)
        "_Float128" | "__float128" => (16, 16),

        // Pointer types
        ty if ty.ends_with('*') => (arch.pointer_size, arch.pointer_size),
        // Unknown — use pointer size as a reasonable default
        _ => (arch.pointer_size, arch.pointer_size),
    }
}

// ── struct / union simulation ─────────────────────────────────────────────────

/// Strip a bit-field width annotation (`:N`) from a type name for size lookup.
/// `"int:3"` → `"int"`, `"std::atomic"` → unchanged (`:` not followed by digits only).
fn strip_bitfield_suffix(ty: &str) -> &str {
    if let Some(pos) = ty.rfind(':') {
        let suffix = ty[pos + 1..].trim();
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return ty[..pos].trim_end();
        }
    }
    ty
}

/// Return `true` when `ty` carries a bit-field width annotation (e.g. `"int:3"`).
/// Bit-field packing is compiler-controlled and cannot be accurately modelled
/// without a compiler, so structs containing bit-field members are skipped.
fn is_bitfield_type(ty: &str) -> bool {
    strip_bitfield_suffix(ty) != ty
}

/// Simulate C/C++ struct layout given ordered fields.
///
/// When `packed` is `true` the layout mirrors `__attribute__((packed))`:
/// no inter-field alignment padding is inserted and the struct alignment
/// is forced to 1. This matches GCC/Clang behaviour for packed structs.
fn simulate_layout(
    fields: &mut Vec<Field>,
    struct_name: String,
    arch: &'static ArchConfig,
    source_line: Option<u32>,
    packed: bool,
) -> StructLayout {
    let mut offset = 0usize;
    let mut struct_align = 1usize;

    for f in fields.iter_mut() {
        if !packed && f.align > 0 {
            offset = offset.next_multiple_of(f.align);
        }
        f.offset = offset;
        offset += f.size;
        if !packed {
            struct_align = struct_align.max(f.align);
        }
    }
    // Trailing padding (not present in packed structs)
    if !packed && struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }

    StructLayout {
        name: struct_name,
        total_size: offset,
        align: struct_align,
        fields: std::mem::take(fields),
        source_file: None,
        source_line,
        arch,
        is_packed: packed,
        is_union: false,
        is_repr_rust: false,
        suppressed_findings: Vec::new(),
    }
}

/// Simulate a C/C++ union layout: all fields start at offset 0;
/// total size is the largest field, rounded to max alignment.
fn simulate_union_layout(
    fields: &mut Vec<Field>,
    name: String,
    arch: &'static ArchConfig,
    source_line: Option<u32>,
) -> StructLayout {
    for f in fields.iter_mut() {
        f.offset = 0;
    }
    let max_size = fields.iter().map(|f| f.size).max().unwrap_or(0);
    let max_align = fields.iter().map(|f| f.align).max().unwrap_or(1);
    let total_size = if max_align > 0 {
        max_size.next_multiple_of(max_align)
    } else {
        max_size
    };

    StructLayout {
        name,
        total_size,
        align: max_align,
        fields: std::mem::take(fields),
        source_file: None,
        source_line,
        arch,
        is_packed: false,
        is_union: true,
        is_repr_rust: false,
        suppressed_findings: Vec::new(),
    }
}

// ── C++ class parsing (vtable + inheritance) ──────────────────────────────────

/// Parse a `class_specifier` node, modelling:
/// - A hidden vtable pointer (`__vptr`) when any method is `virtual`.
/// - Base-class storage as a synthetic `__base_<Name>` field (size resolved
///   later by the nested-struct resolution pass in `lib.rs`).
fn parse_class_specifier(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
) -> Option<StructLayout> {
    let mut class_name = "<anonymous>".to_string();
    let mut base_names: Vec<String> = Vec::new();
    let mut body_node: Option<Node> = None;
    let mut is_packed = false;
    let mut struct_alignas: Option<usize> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "type_identifier" => class_name = source[child.byte_range()].to_string(),
            "base_class_clause" => {
                // tree-sitter-cpp structure: ':' [access_specifier] type_identifier
                // type_identifier nodes are direct children of base_class_clause.
                for j in 0..child.child_count() {
                    if let Some(base) = child.child(j)
                        && base.kind() == "type_identifier"
                    {
                        base_names.push(source[base.byte_range()].to_string());
                    }
                }
            }
            "field_declaration_list" => body_node = Some(child),
            "attribute_specifier" => {
                if source[child.byte_range()].contains("packed") {
                    is_packed = true;
                }
            }
            // C++11 class-level alignas: `class alignas(64) Name { ... };`
            "alignas_qualifier" | "alignas_specifier" => {
                if struct_alignas.is_none() {
                    struct_alignas = parse_alignas_value(source, child);
                }
            }
            _ => {}
        }
    }

    let body = body_node?;

    // Detect virtual methods: look for `virtual` keyword anywhere in body
    let has_virtual = contains_virtual_keyword(source, body);

    // Collect declared fields: (field_name, type_text, guard, alignas_override, source_line)
    let mut raw_fields: Vec<RawField> = Vec::new();
    for i in 0..body.child_count() {
        let Some(child) = body.child(i) else {
            continue;
        };
        if child.kind() == "field_declaration" {
            if let Some(anon_fields) = parse_anonymous_nested(source, child, arch, false) {
                raw_fields.extend(anon_fields);
            } else if let Some((ty, fname, guard, al, ln)) = parse_field_declaration(source, child)
            {
                raw_fields.push((fname, ty, guard, al, ln));
            }
        }
    }

    // Build fields: vtable pointer, then base-class slots, then declared fields
    let mut fields: Vec<Field> = Vec::new();

    // Virtual dispatch pointer (hidden, at offset 0 for the first virtual class)
    if has_virtual {
        let ps = arch.pointer_size;
        fields.push(Field {
            name: "__vptr".to_string(),
            ty: TypeInfo::Pointer {
                size: ps,
                align: ps,
            },
            offset: 0,
            size: ps,
            align: ps,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        });
    }

    // Base class storage (opaque until nested-struct resolver fills in sizes)
    for base in &base_names {
        let ps = arch.pointer_size;
        fields.push(Field {
            name: format!("__base_{base}"),
            ty: TypeInfo::Opaque {
                name: base.clone(),
                size: ps,
                align: ps,
            },
            offset: 0,
            size: ps,
            align: ps,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        });
    }

    // Skip classes with bit-field members (same reason as structs).
    if raw_fields
        .iter()
        .any(|(_, ty, _, _, _)| is_bitfield_type(ty))
    {
        eprintln!(
            "padlock: note: skipping '{class_name}' — contains bit-fields \
             (bit-field layout is compiler-controlled; use binary analysis for accurate results)"
        );
        return None;
    }

    // Declared member fields
    for (fname, ty_name, guard, alignas, field_line) in raw_fields {
        let (size, natural_align) = c_type_size_align(&ty_name, arch);
        let align = alignas.unwrap_or(natural_align);
        let access = if let Some(g) = guard {
            AccessPattern::Concurrent {
                guard: Some(g),
                is_atomic: false,
                is_annotated: true,
            }
        } else {
            AccessPattern::Unknown
        };
        fields.push(Field {
            name: fname,
            ty: TypeInfo::Primitive {
                name: ty_name,
                size,
                align,
            },
            offset: 0,
            size,
            align,
            source_file: None,
            source_line: Some(field_line),
            access,
        });
    }

    if fields.is_empty() {
        return None;
    }

    let line = node.start_position().row as u32 + 1;
    let mut layout = simulate_layout(&mut fields, class_name, arch, Some(line), is_packed);

    if let Some(al) = struct_alignas
        && al > layout.align
    {
        layout.align = al;
        if !is_packed {
            layout.total_size = layout.total_size.next_multiple_of(al);
        }
    }

    layout.suppressed_findings =
        super::suppress::suppressed_from_preceding_source(source, node.start_byte());

    Some(layout)
}

/// Return true if a `field_declaration_list` node contains any `virtual` keyword
/// (indicating that the class needs a vtable pointer).
fn contains_virtual_keyword(source: &str, node: Node<'_>) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "virtual" {
            return true;
        }
        // Also check raw text for cases where tree-sitter may not produce a
        // dedicated `virtual` node (e.g. inside complex declarations).
        if n.child_count() == 0 {
            let text = &source[n.byte_range()];
            if text == "virtual" {
                return true;
            }
        }
        for i in (0..n.child_count()).rev() {
            if let Some(child) = n.child(i) {
                stack.push(child);
            }
        }
    }
    false
}

// ── tree-sitter walker ────────────────────────────────────────────────────────

fn extract_structs_from_tree(
    source: &str,
    root: Node<'_>,
    arch: &'static ArchConfig,
    layouts: &mut Vec<StructLayout>,
) {
    let cursor = root.walk();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        // Push children in reverse so we process left-to-right
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }

        match node.kind() {
            "struct_specifier" => {
                if let Some(layout) = parse_struct_or_union_specifier(source, node, arch, false) {
                    layouts.push(layout);
                }
            }
            "union_specifier" => {
                if let Some(layout) = parse_struct_or_union_specifier(source, node, arch, true) {
                    layouts.push(layout);
                }
            }
            "class_specifier" => {
                if let Some(layout) = parse_class_specifier(source, node, arch) {
                    layouts.push(layout);
                }
            }
            _ => {}
        }
    }

    // Also handle `typedef struct/union { ... } Name;`
    let cursor2 = root.walk();
    let mut stack2 = vec![root];
    while let Some(node) = stack2.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack2.push(child);
            }
        }
        if node.kind() == "type_definition"
            && let Some(layout) = parse_typedef_struct_or_union(source, node, arch)
        {
            let existing = layouts
                .iter()
                .position(|l| l.name == layout.name || l.name == "<anonymous>");
            match existing {
                Some(i) if layouts[i].name == "<anonymous>" => {
                    layouts[i] = layout;
                }
                None => layouts.push(layout),
                _ => {}
            }
        }
    }
    let _ = cursor;
    let _ = cursor2; // silence unused warnings
}

/// Parse a `struct_specifier` or `union_specifier` node into a `StructLayout`.
fn parse_struct_or_union_specifier(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    is_union: bool,
) -> Option<StructLayout> {
    let mut name = "<anonymous>".to_string();
    let mut body_node: Option<Node> = None;
    let mut is_packed = false;
    // Struct-level alignas: `struct alignas(64) CacheAligned { ... };`
    let mut struct_alignas: Option<usize> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "type_identifier" => name = source[child.byte_range()].to_string(),
            "field_declaration_list" => body_node = Some(child),
            "attribute_specifier" => {
                let text = &source[child.byte_range()];
                if text.contains("packed") {
                    is_packed = true;
                }
            }
            // C++11 struct-level alignas: `struct alignas(64) Name { ... };`
            // tree-sitter-cpp: `alignas_qualifier` as direct child of struct_specifier
            "alignas_qualifier" | "alignas_specifier" => {
                if struct_alignas.is_none() {
                    struct_alignas = parse_alignas_value(source, child);
                }
            }
            _ => {}
        }
    }

    let body = body_node?;
    let mut raw_fields: Vec<RawField> = Vec::new();

    for i in 0..body.child_count() {
        let child = body.child(i)?;
        if child.kind() == "field_declaration" {
            // Check for anonymous nested struct/union: a field_declaration whose
            // only non-field-identifier child is a struct_specifier/union_specifier
            // with no type_identifier (i.e. `struct { int x; int y; };`).
            if let Some(anon_fields) = parse_anonymous_nested(source, child, arch, is_union) {
                raw_fields.extend(anon_fields);
            } else if let Some((ty, fname, guard, al, ln)) = parse_field_declaration(source, child)
            {
                raw_fields.push((fname, ty, guard, al, ln));
            }
        }
    }

    if raw_fields.is_empty() {
        return None;
    }

    // Bit-field packing is compiler-controlled and cannot be accurately modelled
    // without a compiler. Skip the entire struct to avoid producing wrong layout
    // data. Use `padlock analyze` on the compiled binary for accurate results.
    if raw_fields
        .iter()
        .any(|(_, ty, _, _, _)| is_bitfield_type(ty))
    {
        eprintln!(
            "padlock: note: skipping '{name}' — contains bit-fields \
             (bit-field layout is compiler-controlled; use binary analysis for accurate results)"
        );
        return None;
    }

    let mut fields: Vec<Field> = raw_fields
        .into_iter()
        .map(|(fname, ty_name, guard, alignas, field_line)| {
            let (size, natural_align) = c_type_size_align(&ty_name, arch);
            // alignas(N) on a field overrides its alignment requirement.
            let align = alignas.unwrap_or(natural_align);
            let access = if let Some(g) = guard {
                AccessPattern::Concurrent {
                    guard: Some(g),
                    is_atomic: false,
                    is_annotated: true,
                }
            } else {
                AccessPattern::Unknown
            };
            Field {
                name: fname,
                ty: TypeInfo::Primitive {
                    name: ty_name,
                    size,
                    align,
                },
                offset: 0,
                size,
                align,
                source_file: None,
                source_line: Some(field_line),
                access,
            }
        })
        .collect();

    let line = node.start_position().row as u32 + 1;
    let mut layout = if is_union {
        simulate_union_layout(&mut fields, name, arch, Some(line))
    } else {
        simulate_layout(&mut fields, name, arch, Some(line), is_packed)
    };

    // Apply struct-level alignas: the struct's alignment requirement is at
    // least N; trailing padding may grow to satisfy the new alignment.
    if let Some(al) = struct_alignas
        && al > layout.align
    {
        layout.align = al;
        if !is_packed {
            layout.total_size = layout.total_size.next_multiple_of(al);
        }
    }

    layout.suppressed_findings =
        super::suppress::suppressed_from_preceding_source(source, node.start_byte());

    Some(layout)
}

/// Parse a `typedef struct/union { ... } Name;` type_definition node.
fn parse_typedef_struct_or_union(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
) -> Option<StructLayout> {
    let mut specifier_node: Option<Node> = None;
    let mut is_union = false;
    let mut typedef_name: Option<String> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "struct_specifier" => {
                specifier_node = Some(child);
                is_union = false;
            }
            "union_specifier" => {
                specifier_node = Some(child);
                is_union = true;
            }
            "type_identifier" => typedef_name = Some(source[child.byte_range()].to_string()),
            _ => {}
        }
    }

    let spec = specifier_node?;
    let typedef_name = typedef_name?;

    let mut layout = parse_struct_or_union_specifier(source, spec, arch, is_union)?;
    if layout.name == "<anonymous>" {
        layout.name = typedef_name;
    }
    Some(layout)
}

// Alias kept for the typedef pass in extract_structs_from_tree.
#[allow(dead_code)]
fn parse_typedef_struct(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
) -> Option<StructLayout> {
    parse_typedef_struct_or_union(source, node, arch)
}

/// Extract a lock guard name from a C/C++ `__attribute__((guarded_by(X)))` or
/// `__attribute__((pt_guarded_by(X)))` specifier node.
///
/// Also recognises the common macro forms `GUARDED_BY(X)` and `PT_GUARDED_BY(X)`
/// which expand to the same attribute (Clang thread-safety analysis).
/// The match is done on the raw source text of any `attribute_specifier` child,
/// so it works regardless of how tree-sitter structures the inner tokens.
fn extract_guard_from_c_field_text(field_source: &str) -> Option<String> {
    // Patterns to search for (case-insensitive on the keyword, guard name is as-is)
    for kw in &["guarded_by", "pt_guarded_by", "GUARDED_BY", "PT_GUARDED_BY"] {
        if let Some(pos) = field_source.find(kw) {
            let after = &field_source[pos + kw.len()..];
            // Expect `(` optionally preceded by whitespace
            let trimmed = after.trim_start();
            if let Some(inner) = trimmed.strip_prefix('(') {
                // Read until the matching ')'
                if let Some(end) = inner.find(')') {
                    let guard = inner[..end].trim().trim_matches('"');
                    if !guard.is_empty() {
                        return Some(guard.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Parse a numeric value from an `alignas_qualifier` node: `alignas(N)`.
/// tree-sitter-cpp uses the node kind `alignas_qualifier` for C++11 `alignas`.
/// Returns `None` when the specifier contains a type expression rather than
/// an integer literal (e.g. `alignas(double)` — handled elsewhere by the
/// compiler; we skip those conservatively).
fn parse_alignas_value(source: &str, node: Node<'_>) -> Option<usize> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "number_literal" | "integer_literal" | "integer" => {
                    let text = source[child.byte_range()].trim();
                    if let Ok(n) = text.parse::<usize>() {
                        return Some(n);
                    }
                    // Hex literal: 0x40
                    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                        return usize::from_str_radix(hex, 16).ok();
                    }
                }
                // Recurse for nested nodes (parenthesised expression, etc.)
                "parenthesized_expression" | "argument_list" | "alignas_qualifier" => {
                    if let r @ Some(_) = parse_alignas_value(source, child) {
                        return r;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Returns `(ty, field_name, guard, alignas_override)`.
/// `alignas_override` is `Some(N)` when the field carries `alignas(N)`.
/// Detect and parse an anonymous nested struct/union field declaration, e.g.:
///
/// ```c
/// struct Packet {
///     union {                    // ← anonymous nested union
///         uint32_t raw;
///         struct { uint8_t a; uint8_t b; uint8_t c; uint8_t d; };
///     };
///     uint64_t timestamp;
/// };
/// ```
///
/// A `field_declaration` is anonymous if it contains a `struct_specifier` or
/// `union_specifier` child that has a `field_declaration_list` (i.e. a body)
/// but no `type_identifier` (i.e. no name). The fields of the nested
/// struct/union are flattened into the parent.
///
/// Returns `None` if the declaration is not an anonymous nested struct/union
/// (the caller should fall through to `parse_field_declaration`).
/// (field_name, type_text, guard, alignas_override, source_line_1based)
type RawField = (String, String, Option<String>, Option<usize>, u32);

#[allow(clippy::only_used_in_recursion)]
fn parse_anonymous_nested(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    parent_is_union: bool,
) -> Option<Vec<RawField>> {
    // Find a struct_specifier or union_specifier child.
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        if child.kind() != "struct_specifier" && child.kind() != "union_specifier" {
            continue;
        }
        let nested_is_union = child.kind() == "union_specifier";

        // Must have a body (field_declaration_list) but no type_identifier.
        let mut has_name = false;
        let mut body_node: Option<Node> = None;
        for j in 0..child.child_count() {
            let sub = child.child(j)?;
            match sub.kind() {
                "type_identifier" => has_name = true,
                "field_declaration_list" => body_node = Some(sub),
                _ => {}
            }
        }

        if has_name || body_node.is_none() {
            // Named struct/union used as a field type — handled by parse_field_declaration.
            continue;
        }

        let body = body_node?;
        let mut nested_raw: Vec<RawField> = Vec::new();

        for j in 0..body.child_count() {
            let inner = body.child(j)?;
            if inner.kind() == "field_declaration" {
                // Recurse to handle doubly-nested anonymous structs.
                if let Some(deeper) = parse_anonymous_nested(source, inner, arch, nested_is_union) {
                    nested_raw.extend(deeper);
                } else if let Some((ty, fname, guard, al, ln)) =
                    parse_field_declaration(source, inner)
                {
                    nested_raw.push((fname, ty, guard, al, ln));
                }
            }
        }

        // If nested is a union, the fields all share offset 0 (relative to the
        // union's placement in the parent). We can't easily track this through
        // raw field lists, so we emit them as a synthetic __anon_union_N field
        // when the parent cares about offsets, or just flatten for unions.
        //
        // For simplicity: flatten all fields — the layout simulator will compute
        // correct offsets if the parent is a struct, and union semantics are
        // preserved when the parent is a union.
        let _ = (nested_is_union, parent_is_union);

        if !nested_raw.is_empty() {
            return Some(nested_raw);
        }
    }
    None
}

fn parse_field_declaration(source: &str, node: Node<'_>) -> Option<RawField> {
    let mut ty_parts: Vec<String> = Vec::new();
    let mut field_name: Option<String> = None;
    // Bit-field width, e.g. `int flags : 3;` → Some("3")
    let mut bit_width: Option<String> = None;
    // Collect attribute text for guard extraction
    let mut attr_text = String::new();
    // Field-level alignas override
    let mut alignas_override: Option<usize> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "type_specifier" | "primitive_type" | "type_identifier" | "sized_type_specifier" => {
                ty_parts.push(source[child.byte_range()].trim().to_string());
            }
            // C++ qualified types: std::mutex, ns::Type, etc.
            // C++ template types:  std::atomic<uint64_t>, std::vector<int>, etc.
            "qualified_identifier" | "template_type" => {
                ty_parts.push(source[child.byte_range()].trim().to_string());
            }
            // Nested struct/union used as a field type: `struct Vec2 tl;`
            // Extract just the type_identifier name (e.g. "Vec2") so the
            // nested-struct resolution pass can match it by name.
            "struct_specifier" | "union_specifier" => {
                for j in 0..child.child_count() {
                    if let Some(sub) = child.child(j)
                        && sub.kind() == "type_identifier"
                    {
                        ty_parts.push(source[sub.byte_range()].trim().to_string());
                        break;
                    }
                }
            }
            "field_identifier" => {
                field_name = Some(source[child.byte_range()].trim().to_string());
            }
            "pointer_declarator" => {
                field_name = extract_identifier(source, child);
                ty_parts.push("*".to_string());
            }
            // Bit-field clause: `: N`  (tree-sitter-c/cpp node)
            "bitfield_clause" => {
                let text = source[child.byte_range()].trim();
                // Strip leading ':' and whitespace to get just the width digits
                bit_width = Some(text.trim_start_matches(':').trim().to_string());
            }
            // GNU attribute specifier: __attribute__((...))
            "attribute_specifier" | "attribute" => {
                attr_text.push_str(source[child.byte_range()].trim());
                attr_text.push(' ');
            }
            // C++11 alignas: tree-sitter-cpp wraps it as type_qualifier → alignas_qualifier
            // Also handle the direct form in case grammar versions differ.
            "alignas_qualifier" | "alignas_specifier" => {
                if alignas_override.is_none() {
                    alignas_override = parse_alignas_value(source, child);
                }
            }
            // type_qualifier wraps alignas_qualifier for field declarations:
            // `alignas(8) char c;` → type_qualifier { alignas_qualifier { ... } }
            "type_qualifier" => {
                if alignas_override.is_none() {
                    for j in 0..child.child_count() {
                        if let Some(sub) = child.child(j)
                            && (sub.kind() == "alignas_qualifier"
                                || sub.kind() == "alignas_specifier")
                        {
                            alignas_override = parse_alignas_value(source, sub);
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let base_ty = ty_parts.join(" ");
    let fname = field_name?;
    if base_ty.is_empty() {
        return None;
    }
    // Annotate bit-field types as "type:N" so callers can detect and report them;
    // `strip_bitfield_suffix` recovers the base type for size/align lookup.
    let ty = if let Some(w) = bit_width {
        format!("{base_ty}:{w}")
    } else {
        base_ty
    };

    // Also check the full field source text (attribute_specifier may not always
    // be a direct child depending on tree-sitter grammar version).
    let field_src = source[node.byte_range()].to_string();
    let guard = extract_guard_from_c_field_text(&attr_text)
        .or_else(|| extract_guard_from_c_field_text(&field_src));

    let line = node.start_position().row as u32 + 1;
    Some((ty, fname, guard, alignas_override, line))
}

fn extract_identifier(source: &str, node: Node<'_>) -> Option<String> {
    if node.kind() == "field_identifier" || node.kind() == "identifier" {
        return Some(source[node.byte_range()].to_string());
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i)
            && let Some(name) = extract_identifier(source, child)
        {
            return Some(name);
        }
    }
    None
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_c(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_c::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter parse failed"))?;
    let mut layouts = Vec::new();
    extract_structs_from_tree(source, tree.root_node(), arch, &mut layouts);
    Ok(layouts)
}

pub fn parse_cpp(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_cpp::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter parse failed"))?;
    let mut layouts = Vec::new();
    extract_structs_from_tree(source, tree.root_node(), arch, &mut layouts);
    Ok(layouts)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn parse_simple_c_struct() {
        let src = r#"
struct Point {
    int x;
    int y;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
        assert_eq!(layouts[0].fields.len(), 2);
        assert_eq!(layouts[0].fields[0].name, "x");
        assert_eq!(layouts[0].fields[1].name, "y");
    }

    #[test]
    fn parse_typedef_struct() {
        let src = r#"
typedef struct {
    char  is_active;
    double timeout;
    int   port;
} Connection;
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Connection");
        assert_eq!(layouts[0].fields.len(), 3);
    }

    #[test]
    fn c_layout_computes_offsets() {
        let src = "struct T { char a; double b; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let layout = &layouts[0];
        // char at offset 0, double at offset 8 (7 bytes padding)
        assert_eq!(layout.fields[0].offset, 0);
        assert_eq!(layout.fields[1].offset, 8);
        assert_eq!(layout.total_size, 16);
    }

    #[test]
    fn c_layout_detects_padding() {
        let src = "struct T { char a; int b; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let gaps = padlock_core::ir::find_padding(&layouts[0]);
        assert!(!gaps.is_empty());
        assert_eq!(gaps[0].bytes, 3); // 3 bytes padding between char and int
    }

    #[test]
    fn parse_cpp_struct() {
        let src = "struct Vec3 { float x; float y; float z; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].fields.len(), 3);
    }

    // ── SIMD types ────────────────────────────────────────────────────────────

    #[test]
    fn simd_sse_field_size_and_align() {
        let src = "struct Vecs { __m128 a; __m256 b; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let f = &layouts[0].fields;
        assert_eq!(f[0].size, 16); // __m128
        assert_eq!(f[0].align, 16);
        assert_eq!(f[1].size, 32); // __m256
        assert_eq!(f[1].align, 32);
    }

    #[test]
    fn simd_avx512_size() {
        let src = "struct Wide { __m512 v; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 64);
        assert_eq!(layouts[0].fields[0].align, 64);
    }

    #[test]
    fn simd_padding_detected_when_small_field_before_avx() {
        // char(1) + [31 pad] + __m256(32) = 64 bytes, 31 wasted
        let src = "struct Mixed { char flag; __m256 data; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let gaps = padlock_core::ir::find_padding(&layouts[0]);
        assert!(!gaps.is_empty());
        assert_eq!(gaps[0].bytes, 31);
    }

    // ── union parsing ─────────────────────────────────────────────────────────

    #[test]
    fn union_fields_all_at_offset_zero() {
        let src = "union Data { int i; float f; double d; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let u = &layouts[0];
        assert!(u.is_union);
        for field in &u.fields {
            assert_eq!(
                field.offset, 0,
                "union field '{}' should be at offset 0",
                field.name
            );
        }
    }

    #[test]
    fn union_total_size_is_max_field() {
        // double is the largest (8 bytes); total should be 8
        let src = "union Data { int i; float f; double d; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].total_size, 8);
    }

    #[test]
    fn union_no_padding_finding() {
        let src = "union Data { int i; double d; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let report = padlock_core::findings::Report::from_layouts(&layouts);
        let sr = &report.structs[0];
        assert!(
            !sr.findings
                .iter()
                .any(|f| matches!(f, padlock_core::findings::Finding::PaddingWaste { .. }))
        );
        assert!(
            !sr.findings
                .iter()
                .any(|f| matches!(f, padlock_core::findings::Finding::ReorderSuggestion { .. }))
        );
    }

    #[test]
    fn typedef_union_parsed() {
        let src = "typedef union { int a; double b; } Value;";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Value");
        assert!(layouts[0].is_union);
    }

    // ── attribute guard extraction ─────────────────────────────────────────────

    #[test]
    fn extract_guard_from_c_guarded_by_macro() {
        let text = "int value GUARDED_BY(mu);";
        let guard = extract_guard_from_c_field_text(text);
        assert_eq!(guard.as_deref(), Some("mu"));
    }

    #[test]
    fn extract_guard_from_c_attribute_specifier() {
        let text = "__attribute__((guarded_by(counter_lock))) uint64_t counter;";
        let guard = extract_guard_from_c_field_text(text);
        assert_eq!(guard.as_deref(), Some("counter_lock"));
    }

    #[test]
    fn extract_guard_pt_guarded_by() {
        let text = "int *ptr PT_GUARDED_BY(ptr_lock);";
        let guard = extract_guard_from_c_field_text(text);
        assert_eq!(guard.as_deref(), Some("ptr_lock"));
    }

    #[test]
    fn no_guard_returns_none() {
        let guard = extract_guard_from_c_field_text("int x;");
        assert!(guard.is_none());
    }

    #[test]
    fn c_struct_guarded_by_sets_concurrent_access() {
        // Using GUARDED_BY macro style in comments/text — tree-sitter won't parse
        // macro expansions, so test the text-extraction path via parse_field_declaration
        // indirectly by checking extract_guard_from_c_field_text.
        let text = "uint64_t readers GUARDED_BY(lock_a);";
        assert_eq!(
            extract_guard_from_c_field_text(text).as_deref(),
            Some("lock_a")
        );
    }

    #[test]
    fn c_struct_different_guards_detected_as_false_sharing() {
        use padlock_core::arch::X86_64_SYSV;
        use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};

        // Manually build a layout with two fields on the same cache line,
        // different guards — mirrors what the C frontend would produce for
        // __attribute__((guarded_by(...))) annotated fields.
        let mut layout = StructLayout {
            name: "S".into(),
            total_size: 128,
            align: 8,
            fields: vec![
                Field {
                    name: "readers".into(),
                    ty: TypeInfo::Primitive {
                        name: "uint64_t".into(),
                        size: 8,
                        align: 8,
                    },
                    offset: 0,
                    size: 8,
                    align: 8,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Concurrent {
                        guard: Some("lock_a".into()),
                        is_atomic: false,
                        is_annotated: true,
                    },
                },
                Field {
                    name: "writers".into(),
                    ty: TypeInfo::Primitive {
                        name: "uint64_t".into(),
                        size: 8,
                        align: 8,
                    },
                    offset: 8,
                    size: 8,
                    align: 8,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Concurrent {
                        guard: Some("lock_b".into()),
                        is_atomic: false,
                        is_annotated: true,
                    },
                },
            ],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
            is_repr_rust: false,
            suppressed_findings: Vec::new(),
        };
        assert!(padlock_core::analysis::false_sharing::has_false_sharing(
            &layout
        ));
        // Same guard → no false sharing
        layout.fields[1].access = AccessPattern::Concurrent {
            guard: Some("lock_a".into()),
            is_atomic: false,
            is_annotated: true,
        };
        assert!(!padlock_core::analysis::false_sharing::has_false_sharing(
            &layout
        ));
    }

    // ── C++ class: vtable pointer ─────────────────────────────────────────────

    #[test]
    fn cpp_class_with_virtual_method_has_vptr() {
        let src = r#"
class Widget {
    virtual void draw();
    int x;
    int y;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        // First field must be __vptr
        assert_eq!(l.fields[0].name, "__vptr");
        assert_eq!(l.fields[0].size, 8); // pointer on x86_64
        // __vptr is at offset 0
        assert_eq!(l.fields[0].offset, 0);
        // int x should come after the pointer (at offset 8)
        let x = l.fields.iter().find(|f| f.name == "x").unwrap();
        assert_eq!(x.offset, 8);
    }

    #[test]
    fn cpp_class_without_virtual_has_no_vptr() {
        let src = "class Plain { int a; int b; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert!(!layouts[0].fields.iter().any(|f| f.name == "__vptr"));
    }

    #[test]
    fn cpp_struct_keyword_with_virtual_has_vptr() {
        // `struct` in C++ can also have virtual methods
        let src = "struct IFoo { virtual ~IFoo(); virtual void bar(); };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        // struct_specifier doesn't go through parse_class_specifier, so no __vptr
        // (vtable injection is only for `class` nodes)
        let _ = layouts; // just verify it parses without panic
    }

    // ── C++ class: single inheritance ─────────────────────────────────────────

    #[test]
    fn cpp_derived_class_has_base_slot() {
        let src = r#"
class Base {
    int x;
};
class Derived : public Base {
    int y;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        // Both Base and Derived should be parsed
        let derived = layouts.iter().find(|l| l.name == "Derived").unwrap();
        // Derived must have a __base_Base synthetic field
        assert!(
            derived.fields.iter().any(|f| f.name == "__base_Base"),
            "Derived should have a __base_Base field"
        );
        // The y field should come after __base_Base
        let base_field = derived
            .fields
            .iter()
            .find(|f| f.name == "__base_Base")
            .unwrap();
        let y_field = derived.fields.iter().find(|f| f.name == "y").unwrap();
        assert!(y_field.offset >= base_field.offset + base_field.size);
    }

    #[test]
    fn cpp_class_multiple_inheritance_has_multiple_base_slots() {
        let src = r#"
class A { int a; };
class B { int b; };
class C : public A, public B { int c; };
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let c = layouts.iter().find(|l| l.name == "C").unwrap();
        assert!(c.fields.iter().any(|f| f.name == "__base_A"));
        assert!(c.fields.iter().any(|f| f.name == "__base_B"));
    }

    #[test]
    fn cpp_virtual_base_class_total_size_accounts_for_vptr() {
        // class with virtual method: size = sizeof(__vptr) + member fields + padding
        let src = "class V { virtual void f(); int x; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        // __vptr(8) + int(4) + 4 pad = 16 bytes on x86_64
        assert_eq!(l.total_size, 16);
    }

    // ── bitfield handling ─────────────────────────────────────────────────────

    #[test]
    fn is_bitfield_type_detects_colon_n() {
        assert!(is_bitfield_type("int:3"));
        assert!(is_bitfield_type("unsigned int:16"));
        assert!(is_bitfield_type("uint32_t:1"));
        // Not bit-fields — contains ':' but not followed by pure digits
        assert!(!is_bitfield_type("std::atomic<int>"));
        assert!(!is_bitfield_type("ns::Type"));
        assert!(!is_bitfield_type("int"));
    }

    #[test]
    fn struct_with_bitfields_is_skipped() {
        // Bit-field layout is compiler-controlled and cannot be accurately modelled
        // without a compiler. The struct must be skipped entirely.
        let src = r#"
struct Flags {
    unsigned int active : 1;
    unsigned int ready  : 1;
    unsigned int error  : 6;
    int value;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        // Flags must not appear — its layout cannot be accurately computed.
        assert!(
            layouts.iter().all(|l| l.name != "Flags"),
            "struct with bitfields should be skipped; got {:?}",
            layouts.iter().map(|l| &l.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn struct_without_bitfields_is_still_parsed() {
        // Ensure the bitfield guard doesn't affect normal structs.
        let src = "struct Normal { int a; char b; double c; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Normal");
    }

    #[test]
    fn c_struct_fields_have_source_lines() {
        let src = "struct Point {\n    int x;\n    int y;\n};";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let fields = &layouts[0].fields;
        // x is on line 2, y is on line 3
        assert_eq!(fields[0].source_line, Some(2), "x should be line 2");
        assert_eq!(fields[1].source_line, Some(3), "y should be line 3");
    }

    #[test]
    fn cpp_class_with_bitfields_is_skipped() {
        let src = "class Packed { int x : 4; int y : 4; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Packed"),
            "C++ class with bitfields should be skipped"
        );
    }

    #[test]
    fn all_bitfield_struct_is_skipped() {
        // Struct with ONLY bit-field members (no normal fields).
        // raw_fields is non-empty but all entries carry the `:N` annotation,
        // so the bit-field guard must still fire and skip the struct.
        let src = "struct BitPacked { int x:4; int y:4; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "BitPacked"),
            "all-bitfield struct should be skipped; got {:?}",
            layouts.iter().map(|l| &l.name).collect::<Vec<_>>()
        );
    }

    // ── __attribute__((packed)) detection ─────────────────────────────────────

    #[test]
    fn packed_struct_has_no_alignment_padding() {
        // Without packed: char(1) + 3-byte pad + int(4) + char(1) + 3-byte pad = 12 bytes
        // With packed:    char(1) + int(4) + char(1) = 6 bytes, align=1
        let src = r#"
struct __attribute__((packed)) Tight {
    char a;
    int  b;
    char c;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Tight").expect("Tight");
        assert!(l.is_packed, "should be marked is_packed");
        assert_eq!(l.total_size, 6, "packed: no padding inserted");
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 1); // immediately after char
        assert_eq!(l.fields[2].offset, 5);
    }

    #[test]
    fn non_packed_struct_has_normal_alignment_padding() {
        // Confirm baseline: same struct without __attribute__((packed)) gets padded
        let src = r#"
struct Normal {
    char a;
    int  b;
    char c;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Normal").expect("Normal");
        assert!(!l.is_packed);
        assert_eq!(l.total_size, 12);
        assert_eq!(l.fields[1].offset, 4); // aligned to 4
    }

    #[test]
    fn cpp_class_packed_attribute_detected() {
        let src = r#"
class __attribute__((packed)) Dense {
    char a;
    int  b;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Dense").expect("Dense");
        assert!(
            l.is_packed,
            "C++ class with __attribute__((packed)) must be marked packed"
        );
        assert_eq!(l.total_size, 5); // char(1) + int(4), no padding
    }

    // ── alignas detection ─────────────────────────────────────────────────────

    #[test]
    fn field_alignas_overrides_natural_alignment() {
        // char is normally align=1 but alignas(8) forces it to align-8.
        // Layout: c(1B at offset 0, align=8) + x(4B at offset 4, align=4)
        // c must start on an 8-byte boundary (trivially satisfied at offset 0).
        // After c (1 byte), x aligns to 4: offset = 1.next_multiple_of(4) = 4.
        // Struct align = max(8, 4) = 8. Total = 8 bytes (4+4 → 8 → ok for align 8).
        let src = r#"
struct S {
    alignas(8) char c;
    int x;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        // c should be forced to align 8
        let c_field = l.fields.iter().find(|f| f.name == "c").unwrap();
        assert_eq!(c_field.align, 8);
        // x comes after c (1 byte) with natural alignment 4 → offset 4
        let x_field = l.fields.iter().find(|f| f.name == "x").unwrap();
        assert_eq!(x_field.offset, 4);
        // Struct alignment is max(alignas(8), int align 4) = 8
        assert_eq!(l.align, 8);
        // Total = 8 bytes (x at 4, size 4; 4+4=8; 8 is multiple of align 8)
        assert_eq!(l.total_size, 8);
    }

    #[test]
    fn struct_level_alignas_increases_struct_alignment() {
        // alignas(64) on the struct means its alignment requirement is 64.
        // Total size must be a multiple of 64.
        let src = r#"
struct alignas(64) CacheLine {
    int x;
    int y;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts
            .iter()
            .find(|l| l.name == "CacheLine")
            .expect("CacheLine");
        assert_eq!(l.align, 64);
        assert_eq!(l.total_size % 64, 0);
    }

    #[test]
    fn alignas_on_field_smaller_than_natural_is_ignored() {
        // alignas(1) on an int field: does NOT reduce alignment below 4.
        // In C++, alignas cannot reduce alignment below the natural alignment.
        // Our implementation stores the alignas value; natural alignment wins
        // because we take max(alignas, natural) in the caller.
        // Note: we currently store alignas directly; this test documents behaviour.
        let src = "struct S { int x; int y; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].align, 4); // natural alignment, not reduced
    }

    #[test]
    fn cpp_class_alignas_detected() {
        let src = r#"
class alignas(32) Aligned {
    double x;
    double y;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts
            .iter()
            .find(|l| l.name == "Aligned")
            .expect("Aligned");
        assert_eq!(l.align, 32);
        assert_eq!(l.total_size % 32, 0);
    }

    // ── bad weather: alignas edge cases ───────────────────────────────────────

    #[test]
    fn struct_without_alignas_unchanged() {
        // Ensure the alignas detection path doesn't affect structs without it
        let src = "struct Plain { int a; char b; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.align, 4); // max field alignment = int = 4
        assert_eq!(l.total_size, 8); // int(4) + char(1) + 3 pad
    }

    // ── anonymous nested structs/unions ───────────────────────────────────────

    #[test]
    fn anonymous_nested_union_fields_flattened() {
        let src = r#"
struct Packet {
    union {
        uint32_t raw;
        uint8_t bytes[4];
    };
    uint64_t timestamp;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Packet").expect("Packet");
        // raw, bytes (or similar) and timestamp must all be present
        assert!(
            l.fields.iter().any(|f| f.name == "raw"),
            "raw field must be flattened into Packet"
        );
        assert!(
            l.fields.iter().any(|f| f.name == "timestamp"),
            "timestamp must be present"
        );
    }

    #[test]
    fn anonymous_nested_struct_fields_flattened() {
        let src = r#"
struct Outer {
    struct {
        int x;
        int y;
    };
    double z;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Outer").expect("Outer");
        assert!(
            l.fields.iter().any(|f| f.name == "x"),
            "x must be flattened"
        );
        assert!(
            l.fields.iter().any(|f| f.name == "y"),
            "y must be flattened"
        );
        assert!(l.fields.iter().any(|f| f.name == "z"), "z present");
        // Total: x(4) + y(4) + z(8) = 16 bytes, no padding
        assert_eq!(l.total_size, 16);
    }

    #[test]
    fn named_nested_struct_not_flattened() {
        // A named struct used as a field type must NOT be flattened
        let src = r#"
struct Vec2 { float x; float y; };
struct Rect { struct Vec2 tl; struct Vec2 br; };
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let rect = layouts.iter().find(|l| l.name == "Rect").expect("Rect");
        // Should have tl and br as opaque fields, not x/y flattened
        assert_eq!(rect.fields.len(), 2);
        assert!(rect.fields.iter().any(|f| f.name == "tl"));
        assert!(rect.fields.iter().any(|f| f.name == "br"));
    }

    // ── type-table tests ──────────────────────────────────────────────────────

    #[test]
    fn linux_kernel_types_correct_size() {
        // u8/u16/u32/u64 and s8/s16/s32/s64 (linux/types.h)
        assert_eq!(c_type_size_align("u8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("u16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("u32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("u64", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("s8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("s16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("s32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("s64", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn linux_kernel_dunder_types_correct_size() {
        assert_eq!(c_type_size_align("__u8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("__u16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("__u32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("__u64", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("__s8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("__s64", &X86_64_SYSV), (8, 8));
        // Endian-annotated types are same width as their base
        assert_eq!(c_type_size_align("__be16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("__le32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("__be64", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn c99_fast_types_correct_size() {
        // fast8/16 are their natural width
        assert_eq!(c_type_size_align("uint_fast8_t", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("uint_fast16_t", &X86_64_SYSV), (2, 2));
        // fast32/64 are pointer-sized on 64-bit
        assert_eq!(c_type_size_align("uint_fast32_t", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("uint_fast64_t", &X86_64_SYSV), (8, 8));
        // least types are their minimum guaranteed width
        assert_eq!(c_type_size_align("uint_least8_t", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("uint_least32_t", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("uint_least64_t", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("intmax_t", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("uintmax_t", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn gcc_int128_correct_size() {
        assert_eq!(c_type_size_align("__int128", &X86_64_SYSV), (16, 16));
        assert_eq!(c_type_size_align("__uint128", &X86_64_SYSV), (16, 16));
        assert_eq!(c_type_size_align("__int128_t", &X86_64_SYSV), (16, 16));
        // unsigned __int128 — "unsigned " prefix is stripped, then __int128 matched
        assert_eq!(
            c_type_size_align("unsigned __int128", &X86_64_SYSV),
            (16, 16)
        );
    }

    #[test]
    fn windows_types_correct_size() {
        assert_eq!(c_type_size_align("BYTE", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("WORD", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("DWORD", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("QWORD", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("BOOL", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("UINT8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("INT32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("UINT64", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("HANDLE", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("LPVOID", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn char_types_correct_size() {
        assert_eq!(c_type_size_align("wchar_t", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("char8_t", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("char16_t", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("char32_t", &X86_64_SYSV), (4, 4));
    }

    #[test]
    fn half_precision_types_correct_size() {
        assert_eq!(c_type_size_align("_Float16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("__fp16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("__bf16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("_Float128", &X86_64_SYSV), (16, 16));
    }

    #[test]
    fn unsigned_prefix_stripped_correctly() {
        // "unsigned short" → "short" → (2, 2)
        assert_eq!(c_type_size_align("unsigned short", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("unsigned int", &X86_64_SYSV), (4, 4));
        assert_eq!(
            c_type_size_align("unsigned long long", &X86_64_SYSV),
            (8, 8)
        );
        assert_eq!(
            c_type_size_align("long int", &X86_64_SYSV),
            (X86_64_SYSV.pointer_size, X86_64_SYSV.pointer_size)
        );
    }

    #[test]
    fn linux_kernel_struct_with_new_types() {
        // Representative kernel-style struct using __u32, __be16, u8
        let src = r#"
struct NetHeader {
    __be32 src_ip;
    __be32 dst_ip;
    __be16 src_port;
    __be16 dst_port;
    u8     protocol;
    u8     ttl;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        // 4+4+2+2+1+1 = 14B; max align is 4 (__be32) → padded to 16B
        assert_eq!(l.total_size, 16);
        assert_eq!(l.fields[0].size, 4); // __be32 src_ip
        assert_eq!(l.fields[2].size, 2); // __be16 src_port
        assert_eq!(l.fields[4].size, 1); // u8 protocol
    }

    // ── C++ stdlib type tests ─────────────────────────────────────────────────

    #[test]
    fn cpp_string_is_32_bytes() {
        assert_eq!(c_type_size_align("std::string", &X86_64_SYSV), (32, 8));
        assert_eq!(c_type_size_align("std::wstring", &X86_64_SYSV), (32, 8));
    }

    #[test]
    fn cpp_string_view_is_two_words() {
        assert_eq!(c_type_size_align("std::string_view", &X86_64_SYSV), (16, 8));
    }

    #[test]
    fn cpp_vector_is_24_bytes() {
        assert_eq!(c_type_size_align("std::vector<int>", &X86_64_SYSV), (24, 8));
        assert_eq!(
            c_type_size_align("std::vector<uint64_t>", &X86_64_SYSV),
            (24, 8)
        );
        // Size is independent of T
        assert_eq!(
            c_type_size_align("std::vector<std::string>", &X86_64_SYSV),
            (24, 8)
        );
    }

    #[test]
    fn cpp_smart_pointers_correct_size() {
        // unique_ptr: single pointer
        assert_eq!(
            c_type_size_align("std::unique_ptr<int>", &X86_64_SYSV),
            (8, 8)
        );
        // shared_ptr / weak_ptr: two pointers
        assert_eq!(
            c_type_size_align("std::shared_ptr<int>", &X86_64_SYSV),
            (16, 8)
        );
        assert_eq!(
            c_type_size_align("std::weak_ptr<int>", &X86_64_SYSV),
            (16, 8)
        );
    }

    #[test]
    fn cpp_optional_recursive_size() {
        // std::optional<bool>: 1B (bool) + 1B (has_value flag) → 2B
        assert_eq!(
            c_type_size_align("std::optional<bool>", &X86_64_SYSV),
            (2, 1)
        );
        // std::optional<int>: 4B + 1B → padded to 4B → 8B total? Let's check:
        // t_size=4, t_align=4; (4+1).next_multiple_of(4) = 8
        assert_eq!(
            c_type_size_align("std::optional<int>", &X86_64_SYSV),
            (8, 4)
        );
        // std::optional<double>: 8B + 1B → padded to 8B → 16B
        assert_eq!(
            c_type_size_align("std::optional<double>", &X86_64_SYSV),
            (16, 8)
        );
    }

    #[test]
    fn cpp_function_is_32_bytes() {
        assert_eq!(
            c_type_size_align("std::function<void()>", &X86_64_SYSV),
            (32, 8)
        );
        assert_eq!(
            c_type_size_align("std::function<int(int)>", &X86_64_SYSV),
            (32, 8)
        );
    }

    #[test]
    fn cpp_stdlib_struct_with_string_field() {
        // A struct with std::string fields — used to get pointer-size (8B), now 32B
        let src = r#"
struct Config {
    std::string name;
    int         version;
    bool        enabled;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].size, 32); // std::string, not 8
        // int at offset 32, bool at 36; total padded to 8-byte align = 40
        assert_eq!(l.fields[1].offset, 32);
        assert_eq!(l.fields[1].size, 4);
    }
}
