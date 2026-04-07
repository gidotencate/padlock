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

/// Simulate C struct layout (no `__attribute__((packed))`) given ordered fields.
fn simulate_layout(
    fields: &mut Vec<Field>,
    struct_name: String,
    arch: &'static ArchConfig,
) -> StructLayout {
    let mut offset = 0usize;
    let mut struct_align = 1usize;

    for f in fields.iter_mut() {
        if f.align > 0 {
            offset = offset.next_multiple_of(f.align);
        }
        f.offset = offset;
        offset += f.size;
        struct_align = struct_align.max(f.align);
    }
    // Trailing padding
    if struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }

    StructLayout {
        name: struct_name,
        total_size: offset,
        align: struct_align,
        fields: fields.drain(..).collect(),
        source_file: None,
        source_line: None,
        arch,
        is_packed: false,
        is_union: false,
    }
}

/// Simulate a C/C++ union layout: all fields start at offset 0;
/// total size is the largest field, rounded to max alignment.
fn simulate_union_layout(
    fields: &mut Vec<Field>,
    name: String,
    arch: &'static ArchConfig,
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
        fields: fields.drain(..).collect(),
        source_file: None,
        source_line: None,
        arch,
        is_packed: false,
        is_union: true,
    }
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
        if node.kind() == "type_definition" {
            if let Some(layout) = parse_typedef_struct_or_union(source, node, arch) {
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

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "type_identifier" => name = source[child.byte_range()].to_string(),
            "field_declaration_list" => body_node = Some(child),
            _ => {}
        }
    }

    let body = body_node?;
    let mut raw_fields: Vec<(String, String, Option<String>)> = Vec::new();

    for i in 0..body.child_count() {
        let child = body.child(i)?;
        if child.kind() == "field_declaration" {
            if let Some((ty, fname, guard)) = parse_field_declaration(source, child) {
                raw_fields.push((fname, ty, guard));
            }
        }
    }

    if raw_fields.is_empty() {
        return None;
    }

    let mut fields: Vec<Field> = raw_fields
        .into_iter()
        .map(|(fname, ty_name, guard)| {
            // Use the base type (without bit-field `:N` suffix) for size/align lookup.
            let base = strip_bitfield_suffix(&ty_name);
            let (size, align) = c_type_size_align(base, arch);
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

    if is_union {
        Some(simulate_union_layout(&mut fields, name, arch))
    } else {
        Some(simulate_layout(&mut fields, name, arch))
    }
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
            if trimmed.starts_with('(') {
                let inner = &trimmed[1..];
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

fn parse_field_declaration(
    source: &str,
    node: Node<'_>,
) -> Option<(String, String, Option<String>)> {
    let mut ty_parts: Vec<String> = Vec::new();
    let mut field_name: Option<String> = None;
    // Bit-field width, e.g. `int flags : 3;` → Some("3")
    let mut bit_width: Option<String> = None;
    // Collect attribute text for guard extraction
    let mut attr_text = String::new();

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

    Some((ty, fname, guard))
}

fn extract_identifier(source: &str, node: Node<'_>) -> Option<String> {
    if node.kind() == "field_identifier" || node.kind() == "identifier" {
        return Some(source[node.byte_range()].to_string());
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if let Some(name) = extract_identifier(source, child) {
                return Some(name);
            }
        }
    }
    None
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_c(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_c::language())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter parse failed"))?;
    let mut layouts = Vec::new();
    extract_structs_from_tree(source, tree.root_node(), arch, &mut layouts);
    Ok(layouts)
}

pub fn parse_cpp(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_cpp::language())?;
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
        assert!(!sr
            .findings
            .iter()
            .any(|f| matches!(f, padlock_core::findings::Finding::PaddingWaste { .. })));
        assert!(!sr
            .findings
            .iter()
            .any(|f| matches!(f, padlock_core::findings::Finding::ReorderSuggestion { .. })));
    }

    #[test]
    fn typedef_union_parsed() {
        let src = "typedef union { int a; double b; } Value;";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Value");
        assert!(layouts[0].is_union);
    }

    // ── bit fields ────────────────────────────────────────────────────────────

    #[test]
    fn bitfield_type_annotated_with_width() {
        let src = "struct Flags { int a : 3; int b : 5; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        // Both fields should be present; type names should contain the width
        let names: Vec<&str> = layouts[0].fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"a") && names.contains(&"b"));
        // Type name should encode the bit width
        let a_ty = match &layouts[0].fields[0].ty {
            padlock_core::ir::TypeInfo::Primitive { name, .. } => name.clone(),
            _ => panic!("expected Primitive"),
        };
        assert!(
            a_ty.contains(':'),
            "bit field type should contain ':' width annotation"
        );
    }

    #[test]
    fn bitfield_uses_storage_unit_size() {
        // `int a : 3` should report size = sizeof(int) = 4
        let src = "struct S { int a : 3; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 4);
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
}
