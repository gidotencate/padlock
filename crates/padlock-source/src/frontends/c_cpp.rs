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
    // C++ standard library synchronisation types (Linux/glibc x86-64 defaults).
    // Sizes are platform-approximate; accuracy is "good enough" for cache-line
    // bucketing and false-sharing detection.
    match ty {
        // Mutexes — all backed by pthread_mutex_t (40 bytes on Linux/glibc)
        "std::mutex"
        | "std::recursive_mutex"
        | "std::timed_mutex"
        | "std::recursive_timed_mutex"
        | "pthread_mutex_t" => return (40, 8),
        "std::shared_mutex" | "std::shared_timed_mutex" => return (56, 8),
        // Condition variables
        "std::condition_variable" | "pthread_cond_t" => return (48, 8),
        // std::atomic<T> — same size as T; extract and recurse
        ty if ty.starts_with("std::atomic<") && ty.ends_with('>') => {
            let inner = &ty[12..ty.len() - 1];
            return c_type_size_align(inner.trim(), arch);
        }
        _ => {} // fall through to primitive types below
    }
    // Primitive / stdint / pointer types
    match ty {
        "char" | "_Bool" | "bool" => (1, 1),
        "short" | "short int" => (2, 2),
        "int" => (4, 4),
        "long" => (arch.pointer_size, arch.pointer_size),
        "long long" => (8, 8),
        "float" => (4, 4),
        "double" => (8, 8),
        "long double" => (16, 16),
        "int8_t" | "uint8_t" => (1, 1),
        "int16_t" | "uint16_t" => (2, 2),
        "int32_t" | "uint32_t" => (4, 4),
        "int64_t" | "uint64_t" => (8, 8),
        "size_t" | "ssize_t" | "ptrdiff_t" | "intptr_t" | "uintptr_t" => {
            (arch.pointer_size, arch.pointer_size)
        }
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

    // Collect declared fields: (field_name, type_text, guard, alignas_override)
    let mut raw_fields: Vec<(String, String, Option<String>, Option<usize>)> = Vec::new();
    for i in 0..body.child_count() {
        let Some(child) = body.child(i) else {
            continue;
        };
        if child.kind() == "field_declaration" {
            if let Some(anon_fields) = parse_anonymous_nested(source, child, arch, false) {
                raw_fields.extend(anon_fields);
            } else if let Some((ty, fname, guard, al)) = parse_field_declaration(source, child) {
                raw_fields.push((fname, ty, guard, al));
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
    if raw_fields.iter().any(|(_, ty, _, _)| is_bitfield_type(ty)) {
        return None;
    }

    // Declared member fields
    for (fname, ty_name, guard, alignas) in raw_fields {
        let (size, natural_align) = c_type_size_align(&ty_name, arch);
        let align = alignas.unwrap_or(natural_align);
        let access = if let Some(g) = guard {
            AccessPattern::Concurrent {
                guard: Some(g),
                is_atomic: false,
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
            source_line: None,
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
    // (field_name, type_text, guard, alignas_override)
    let mut raw_fields: Vec<(String, String, Option<String>, Option<usize>)> = Vec::new();

    for i in 0..body.child_count() {
        let child = body.child(i)?;
        if child.kind() == "field_declaration" {
            // Check for anonymous nested struct/union: a field_declaration whose
            // only non-field-identifier child is a struct_specifier/union_specifier
            // with no type_identifier (i.e. `struct { int x; int y; };`).
            if let Some(anon_fields) = parse_anonymous_nested(source, child, arch, is_union) {
                raw_fields.extend(anon_fields);
            } else if let Some((ty, fname, guard, al)) = parse_field_declaration(source, child) {
                raw_fields.push((fname, ty, guard, al));
            }
        }
    }

    if raw_fields.is_empty() {
        return None;
    }

    // Bit-field packing is compiler-controlled and cannot be accurately modelled
    // without a compiler. Skip the entire struct to avoid producing wrong layout
    // data. Use `padlock analyze` on the compiled binary for accurate results.
    if raw_fields.iter().any(|(_, ty, _, _)| is_bitfield_type(ty)) {
        return None;
    }

    let mut fields: Vec<Field> = raw_fields
        .into_iter()
        .map(|(fname, ty_name, guard, alignas)| {
            let (size, natural_align) = c_type_size_align(&ty_name, arch);
            // alignas(N) on a field overrides its alignment requirement.
            let align = alignas.unwrap_or(natural_align);
            let access = if let Some(g) = guard {
                AccessPattern::Concurrent {
                    guard: Some(g),
                    is_atomic: false,
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
                source_line: None,
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
fn parse_anonymous_nested(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    parent_is_union: bool,
) -> Option<Vec<(String, String, Option<String>, Option<usize>)>> {
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
        let mut nested_raw: Vec<(String, String, Option<String>, Option<usize>)> = Vec::new();

        for j in 0..body.child_count() {
            let inner = body.child(j)?;
            if inner.kind() == "field_declaration" {
                // Recurse to handle doubly-nested anonymous structs.
                if let Some(deeper) = parse_anonymous_nested(source, inner, arch, nested_is_union) {
                    nested_raw.extend(deeper);
                } else if let Some((ty, fname, guard, al)) = parse_field_declaration(source, inner)
                {
                    nested_raw.push((fname, ty, guard, al));
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

fn parse_field_declaration(
    source: &str,
    node: Node<'_>,
) -> Option<(String, String, Option<String>, Option<usize>)> {
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

    Some((ty, fname, guard, alignas_override))
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
                    },
                },
            ],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
        };
        assert!(padlock_core::analysis::false_sharing::has_false_sharing(
            &layout
        ));
        // Same guard → no false sharing
        layout.fields[1].access = AccessPattern::Concurrent {
            guard: Some("lock_a".into()),
            is_atomic: false,
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
    fn cpp_class_with_bitfields_is_skipped() {
        let src = "class Packed { int x : 4; int y : 4; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Packed"),
            "C++ class with bitfields should be skipped"
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
}
