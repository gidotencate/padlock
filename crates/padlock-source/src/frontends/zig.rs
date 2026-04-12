// padlock-source/src/frontends/zig.rs
//
// Extracts struct layouts from Zig source using tree-sitter-zig.
// Handles regular, extern, and packed struct variants.
// Sizes use Zig's platform-native alignment rules (same as C on the target arch).

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{Field, StructLayout, TypeInfo};
use tree_sitter::{Node, Parser};

// ── type resolution ───────────────────────────────────────────────────────────

fn zig_type_size_align(ty: &str, arch: &'static ArchConfig) -> (usize, usize) {
    let ty = ty.trim();
    match ty {
        "bool" => (1, 1),
        "u8" | "i8" => (1, 1),
        "u16" | "i16" | "f16" => (2, 2),
        "u32" | "i32" | "f32" => (4, 4),
        "u64" | "i64" | "f64" => (8, 8),
        "u128" | "i128" | "f128" => (16, 16),
        // f80 is the x87 80-bit float; stored as 10 bytes, aligned to 16 on x86-64
        "f80" => (10, 16),
        "usize" | "isize" => (arch.pointer_size, arch.pointer_size),
        "void" | "anyopaque" => (0, 1),
        // comptime-only or type-erased — treat as pointer-sized
        "type" | "anytype" | "comptime_int" | "comptime_float" => {
            (arch.pointer_size, arch.pointer_size)
        }

        // Zig C-interop types (std.c / @cImport equivalents)
        "c_char" | "c_uchar" | "c_schar" => (1, 1),
        "c_short" | "c_ushort" => (2, 2),
        "c_int" | "c_uint" => (4, 4),
        "c_long" | "c_ulong" => (arch.pointer_size, arch.pointer_size), // 8B on LP64, 4B on LLP64
        "c_longlong" | "c_ulonglong" => (8, 8),
        "c_float" => (4, 4),
        "c_double" => (8, 8),
        "c_longdouble" => (16, 16),

        // Arbitrary-width integers: uN / iN where N is a decimal number.
        // In an extern struct the field occupies ceil(N/8) bytes, aligned to
        // the next power-of-two (capped at 8). In a packed struct all integers
        // are bit-packed — we cannot model that without knowing the context, so
        // we use the same ceil-and-align heuristic as a reasonable approximation.
        ty if (ty.starts_with('u') || ty.starts_with('i'))
            && ty[1..].bytes().all(|b| b.is_ascii_digit())
            && !ty[1..].is_empty() =>
        {
            if let Ok(bits) = ty[1..].parse::<usize>() {
                let bytes = bits.div_ceil(8).max(1);
                // Align to next power-of-two, capped at 8
                let align = bytes.next_power_of_two().min(8);
                (bytes, align)
            } else {
                (arch.pointer_size, arch.pointer_size)
            }
        }

        _ => (arch.pointer_size, arch.pointer_size),
    }
}

/// Determine size/align of a type node, dispatching by node kind.
fn type_node_size_align(source: &str, node: Node<'_>, arch: &'static ArchConfig) -> (usize, usize) {
    match node.kind() {
        "builtin_type" | "identifier" => {
            let text = source[node.byte_range()].trim();
            zig_type_size_align(text, arch)
        }
        // *T — single pointer
        "pointer_type" => (arch.pointer_size, arch.pointer_size),
        // ?T — optional; if T is a pointer the optional is pointer-sized (null = 0),
        // otherwise it is T + 1 byte tag, rounded up. Approximate as pointer-sized.
        "nullable_type" => {
            // Check if the inner type is a pointer — if so, null-pointer optimisation applies
            if let Some(inner) = find_child_by_kinds(node, &["pointer_type"]) {
                let _ = inner; // pointer optionals are pointer-sized
                (arch.pointer_size, arch.pointer_size)
            } else if let Some(inner) = find_first_type_child(source, node) {
                let (sz, al) = type_node_size_align(source, inner, arch);
                // Add 1 byte tag, round up to alignment
                let tagged = (sz + 1).next_multiple_of(al.max(1));
                (tagged, al.max(1))
            } else {
                (arch.pointer_size, arch.pointer_size)
            }
        }
        // []T — slice = (ptr, len)
        "slice_type" => (arch.pointer_size * 2, arch.pointer_size),
        // [N]T — array; try to parse N and recursively get element size
        "array_type" => {
            if let Some((count, elem_sz, elem_al)) = parse_array_type(source, node, arch) {
                (elem_sz * count, elem_al)
            } else {
                (arch.pointer_size, arch.pointer_size)
            }
        }
        // error union E!T — approximate as two words
        "error_union" => (arch.pointer_size * 2, arch.pointer_size),
        _ => (arch.pointer_size, arch.pointer_size),
    }
}

/// For `[N]T` nodes, return `Some((count, elem_size, elem_align))`.
fn parse_array_type(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
) -> Option<(usize, usize, usize)> {
    // array_type children: [ integer_literal ] type_expr
    let mut count: Option<usize> = None;
    let mut elem: Option<(usize, usize)> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "integer" | "integer_literal" => {
                let text = source[child.byte_range()].trim();
                count = text.parse::<usize>().ok();
            }
            "builtin_type" | "identifier" | "pointer_type" | "slice_type" | "array_type"
            | "nullable_type" => {
                elem = Some(type_node_size_align(source, child, arch));
            }
            _ => {}
        }
    }

    let count = count?;
    let (esz, eal) = elem.unwrap_or((arch.pointer_size, arch.pointer_size));
    Some((count, esz, eal))
}

fn find_child_by_kinds<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    for i in 0..node.child_count() {
        if let Some(c) = node.child(i)
            && kinds.contains(&c.kind())
        {
            return Some(c);
        }
    }
    None
}

fn find_first_type_child<'a>(source: &str, node: Node<'a>) -> Option<Node<'a>> {
    let _ = source;
    for i in 0..node.child_count() {
        if let Some(c) = node.child(i) {
            match c.kind() {
                "builtin_type" | "identifier" | "pointer_type" | "slice_type" | "array_type"
                | "nullable_type" | "error_union" => return Some(c),
                _ => {}
            }
        }
    }
    None
}

// ── tree-sitter walker ────────────────────────────────────────────────────────

fn extract_structs(source: &str, root: Node<'_>, arch: &'static ArchConfig) -> Vec<StructLayout> {
    let mut layouts = Vec::new();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }

        if node.kind() == "variable_declaration"
            && let Some(layout) = parse_variable_declaration(source, node, arch)
        {
            layouts.push(layout);
        }
    }
    layouts
}

fn parse_variable_declaration(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
) -> Option<StructLayout> {
    let source_line = node.start_position().row as u32 + 1;
    let mut name: Option<String> = None;
    let mut struct_node: Option<Node> = None;
    let mut union_node: Option<Node> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "identifier" => {
                // The first identifier after `const`/`var` is the name
                if name.is_none() {
                    name = Some(source[child.byte_range()].to_string());
                }
            }
            "struct_declaration" => struct_node = Some(child),
            "union_declaration" => union_node = Some(child),
            _ => {}
        }
    }

    let name = name?;
    if let Some(sn) = struct_node {
        parse_struct_declaration(source, sn, name, arch, source_line)
    } else if let Some(un) = union_node {
        parse_union_declaration(source, un, name, arch, source_line)
    } else {
        None
    }
}

/// Parse a Zig `union { ... }` or `union(enum) { ... }` declaration.
///
/// Layout rules:
/// - All fields share the same storage (offset 0), total = max(field sizes).
/// - Tagged unions add a synthetic `__tag` discriminant field; its size is
///   the smallest integer that covers the variant count.
/// - The struct is emitted with `is_union = true`.
fn parse_union_declaration(
    source: &str,
    node: Node<'_>,
    name: String,
    arch: &'static ArchConfig,
    source_line: u32,
) -> Option<StructLayout> {
    let mut is_tagged = false;
    let mut raw_fields: Vec<(String, String, usize, usize)> = Vec::new();

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            // `union(enum)` — `enum` keyword is a direct child
            "enum" => is_tagged = true,
            // `union(SomeEnum)` — identifier naming the explicit tag type
            // We detect this by seeing an identifier inside the `(...)` group.
            // Mark it as tagged regardless of the tag type.
            "container_field" => {
                if let Some(f) = parse_container_field(source, child, arch, false) {
                    raw_fields.push(f);
                }
            }
            _ => {}
        }
    }

    if raw_fields.is_empty() {
        return None;
    }

    // Union layout: all fields at offset 0; total = max field size rounded to alignment.
    let max_size = raw_fields
        .iter()
        .map(|(_, _, sz, _)| *sz)
        .max()
        .unwrap_or(0);
    let max_align = raw_fields
        .iter()
        .map(|(_, _, _, al)| *al)
        .max()
        .unwrap_or(1);
    let total_size = if max_align > 0 {
        max_size.next_multiple_of(max_align)
    } else {
        max_size
    };

    let mut fields: Vec<Field> = raw_fields
        .into_iter()
        .map(|(fname, type_text, size, align)| Field {
            name: fname,
            ty: TypeInfo::Primitive {
                name: type_text,
                size,
                align,
            },
            offset: 0,
            size,
            align,
            source_file: None,
            source_line: None,
            access: padlock_core::ir::AccessPattern::Unknown,
        })
        .collect();

    // Tagged union: add a synthetic `__tag` discriminant field.
    // Its size is the smallest integer type that holds all variant indices.
    if is_tagged {
        let n = fields.len();
        let tag_size: usize = if n <= 256 {
            1
        } else if n <= 65536 {
            2
        } else {
            4
        };
        fields.push(Field {
            name: "__tag".to_string(),
            ty: TypeInfo::Primitive {
                name: format!("u{}", tag_size * 8),
                size: tag_size,
                align: tag_size,
            },
            offset: total_size, // tag lives after the union payload
            size: tag_size,
            align: tag_size,
            source_file: None,
            source_line: None,
            access: padlock_core::ir::AccessPattern::Unknown,
        });
    }

    let struct_align = max_align; // tag alignment is usually smaller than payload

    let final_size = if is_tagged {
        let tag_size = fields.last().map(|f| f.size).unwrap_or(0);
        (total_size + tag_size).next_multiple_of(struct_align.max(1))
    } else {
        total_size
    };

    Some(StructLayout {
        name,
        total_size: final_size,
        align: struct_align,
        fields,
        source_file: None,
        source_line: Some(source_line),
        arch,
        is_packed: false,
        is_union: true,
        is_repr_rust: false,
    })
}

fn parse_struct_declaration(
    source: &str,
    node: Node<'_>,
    name: String,
    arch: &'static ArchConfig,
    source_line: u32,
) -> Option<StructLayout> {
    let mut is_packed = false;
    let mut is_extern = false;
    // (field_name, type_text, size, align)
    let mut raw_fields: Vec<(String, String, usize, usize)> = Vec::new();

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "packed" => is_packed = true,
            "extern" => is_extern = true,
            "container_field" => {
                if let Some(f) = parse_container_field(source, child, arch, is_packed) {
                    raw_fields.push(f);
                }
            }
            _ => {}
        }
    }

    if raw_fields.is_empty() {
        return None;
    }

    // Regular Zig structs have implementation-defined layout (reordering allowed).
    // Only extern and packed structs have stable C-compatible / bit-exact layout.
    // For analysis purposes we simulate the declared order for all variants,
    // since that is what the developer sees and intends to reason about.
    let mut offset = 0usize;
    let mut struct_align = 1usize;
    let mut fields: Vec<Field> = Vec::new();

    for (fname, type_text, size, align) in raw_fields {
        let eff_align = if is_packed { 1 } else { align };
        if eff_align > 0 {
            offset = offset.next_multiple_of(eff_align);
        }
        struct_align = struct_align.max(eff_align);
        fields.push(Field {
            name: fname,
            ty: TypeInfo::Primitive {
                name: type_text,
                size,
                align,
            },
            offset,
            size,
            align: eff_align,
            source_file: None,
            source_line: None,
            access: padlock_core::ir::AccessPattern::Unknown,
        });
        offset += size;
    }

    if !is_packed && struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }

    let _ = is_extern; // affects ABI guarantees, not layout simulation

    Some(StructLayout {
        name,
        total_size: offset,
        align: struct_align,
        fields,
        source_file: None,
        source_line: Some(source_line),
        arch,
        is_packed,
        is_union: false,
        is_repr_rust: false,
    })
}

/// Parse a `container_field` node and return `(name, type_text, size, align)`.
fn parse_container_field(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    is_packed: bool,
) -> Option<(String, String, usize, usize)> {
    let mut field_name: Option<String> = None;
    let mut type_text: Option<String> = None;
    let mut size_align: Option<(usize, usize)> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "identifier" if field_name.is_none() => {
                field_name = Some(source[child.byte_range()].to_string());
            }
            "builtin_type" | "pointer_type" | "nullable_type" | "slice_type" | "array_type"
            | "error_union" => {
                let text = source[child.byte_range()].to_string();
                size_align = Some(type_node_size_align(source, child, arch));
                type_text = Some(text);
            }
            "identifier" => {
                // Second identifier = type name (e.g. a named struct type)
                let text = source[child.byte_range()].trim().to_string();
                size_align = Some(zig_type_size_align(&text, arch));
                type_text = Some(text);
            }
            _ => {}
        }
    }

    // Discard fields with empty names — tree-sitter-zig emits a zero-length
    // identifier node for `union {}` (empty union body), which is not a real field.
    let name = field_name.filter(|n| !n.is_empty())?;
    let ty = type_text.unwrap_or_else(|| "anyopaque".to_string());
    let (mut size, align) = size_align.unwrap_or((arch.pointer_size, arch.pointer_size));

    if is_packed && size == 0 {
        size = 0; // void fields in packed structs stay 0
    }

    Some((name, ty, size, align))
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_zig(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_zig::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter-zig parse failed"))?;
    Ok(extract_structs(source, tree.root_node(), arch))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn parse_simple_zig_struct() {
        let src = "const Point = struct { x: u32, y: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
        assert_eq!(layouts[0].fields.len(), 2);
        assert_eq!(layouts[0].total_size, 8);
    }

    #[test]
    fn zig_layout_with_padding() {
        let src = "const T = struct { a: bool, b: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0); // bool at 0
        assert_eq!(l.fields[1].offset, 8); // u64 at 8 (7 bytes padding)
        assert_eq!(l.total_size, 16);
    }

    #[test]
    fn zig_packed_struct_no_padding() {
        let src = "const Packed = packed struct { a: u8, b: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert!(l.is_packed);
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 1); // immediately after u8, no padding
        assert_eq!(l.total_size, 5);
    }

    #[test]
    fn zig_extern_struct_detected() {
        let src = "const Extern = extern struct { x: i32, y: f64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        // extern struct has C layout: x at 0 (4B), 4B pad, y at 8 (8B)
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 8);
        assert_eq!(l.total_size, 16);
    }

    #[test]
    fn zig_pointer_field_is_pointer_sized() {
        let src = "const S = struct { ptr: *u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn zig_optional_pointer_is_pointer_sized() {
        let src = "const S = struct { opt: ?*u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    #[test]
    fn zig_slice_is_two_words() {
        let src = "const S = struct { buf: []u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // ptr + len
    }

    #[test]
    fn zig_usize_follows_arch() {
        let src = "const S = struct { n: usize };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    #[test]
    fn zig_multiple_structs_parsed() {
        let src = "const A = struct { x: u8 };\nconst B = struct { y: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 2);
        assert!(layouts.iter().any(|l| l.name == "A"));
        assert!(layouts.iter().any(|l| l.name == "B"));
    }

    #[test]
    fn zig_array_field_size() {
        let src = "const S = struct { buf: [4]u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // 4 * 4
    }

    // ── union / tagged union ──────────────────────────────────────────────────

    #[test]
    fn zig_bare_union_parsed_as_union() {
        let src = "const U = union { a: u8, b: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.name, "U");
        assert!(l.is_union, "union should have is_union=true");
    }

    #[test]
    fn zig_bare_union_total_size_is_max_field() {
        // a: u8 (1B), b: u32 (4B) → max = 4B, aligned to 4
        let src = "const U = union { a: u8, b: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.total_size, 4);
    }

    #[test]
    fn zig_union_all_fields_at_offset_zero() {
        let src = "const U = union { a: u8, b: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        for field in &l.fields {
            assert_eq!(
                field.offset, 0,
                "union field '{}' should be at offset 0",
                field.name
            );
        }
    }

    #[test]
    fn zig_tagged_union_has_tag_field() {
        let src = "const T = union(enum) { ok: u32, err: void };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert!(
            l.fields.iter().any(|f| f.name == "__tag"),
            "tagged union should have a synthetic __tag field"
        );
    }

    #[test]
    fn zig_tagged_union_size_includes_tag() {
        // ok: u32 (4B), err: void (0B) → payload = 4B, tag = 1B (2 variants ≤ 256)
        // total = (4 + 1).next_multiple_of(4) = 8B
        let src = "const T = union(enum) { ok: u32, err: void };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        // payload (4B) + tag (1B) → 5B → rounded to align 4 = 8B
        assert_eq!(l.total_size, 8);
    }

    #[test]
    fn zig_union_with_largest_field_u64() {
        // a: u8 (1B), b: u64 (8B), c: u32 (4B) → max = 8B, align = 8
        let src = "const U = union { a: u8, b: u64, c: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.total_size, 8);
        assert_eq!(l.align, 8);
    }

    #[test]
    fn zig_struct_and_union_in_same_file() {
        let src = "const S = struct { x: u32 };\nconst U = union { a: u8, b: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 2);
        assert!(layouts.iter().any(|l| l.name == "S" && !l.is_union));
        assert!(layouts.iter().any(|l| l.name == "U" && l.is_union));
    }

    // ── bad weather: unions ───────────────────────────────────────────────────

    #[test]
    fn zig_empty_union_returns_none() {
        // Empty union body → no layout produced
        let src = "const E = union {};";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert!(layouts.is_empty(), "empty union should produce no layout");
    }

    #[test]
    fn zig_union_no_padding_finding() {
        // Unions should never report inter-field padding (all fields at offset 0)
        let src = "const U = union { a: u8, b: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let gaps = padlock_core::ir::find_padding(&layouts[0]);
        assert!(
            gaps.is_empty(),
            "unions should have no padding gaps: {:?}",
            gaps
        );
    }

    // ── type-table tests ──────────────────────────────────────────────────────

    #[test]
    fn zig_c_interop_types_correct_size() {
        assert_eq!(zig_type_size_align("c_char", &X86_64_SYSV), (1, 1));
        assert_eq!(zig_type_size_align("c_short", &X86_64_SYSV), (2, 2));
        assert_eq!(zig_type_size_align("c_ushort", &X86_64_SYSV), (2, 2));
        assert_eq!(zig_type_size_align("c_int", &X86_64_SYSV), (4, 4));
        assert_eq!(zig_type_size_align("c_uint", &X86_64_SYSV), (4, 4));
        // c_long is pointer-sized on LP64 (Linux/macOS x86-64)
        assert_eq!(zig_type_size_align("c_long", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_ulong", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_longlong", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_ulonglong", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_float", &X86_64_SYSV), (4, 4));
        assert_eq!(zig_type_size_align("c_double", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_longdouble", &X86_64_SYSV), (16, 16));
    }

    #[test]
    fn zig_arbitrary_width_integers() {
        // u1 → 1B (ceil(1/8)=1), aligned to 1
        assert_eq!(zig_type_size_align("u1", &X86_64_SYSV), (1, 1));
        // u3 → 1B (ceil(3/8)=1)
        assert_eq!(zig_type_size_align("u3", &X86_64_SYSV), (1, 1));
        // u9 → 2B (ceil(9/8)=2), aligned to 2
        assert_eq!(zig_type_size_align("u9", &X86_64_SYSV), (2, 2));
        // u24 → 3B, aligned to 4 (next power-of-two)
        assert_eq!(zig_type_size_align("u24", &X86_64_SYSV), (3, 4));
        // u48 → 6B, aligned to 8
        assert_eq!(zig_type_size_align("u48", &X86_64_SYSV), (6, 8));
        // i7 → 1B, aligned to 1
        assert_eq!(zig_type_size_align("i7", &X86_64_SYSV), (1, 1));
        // i128 is already in the table; u129 hits the arbitrary-width path
        assert_eq!(zig_type_size_align("u129", &X86_64_SYSV), (17, 8));
    }

    #[test]
    fn zig_struct_with_c_interop_types() {
        // A Zig extern struct using C interop types
        let src = "const Header = extern struct { version: c_uint, length: c_ushort, flags: u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.fields[0].size, 4); // c_uint
        assert_eq!(l.fields[1].size, 2); // c_ushort
        assert_eq!(l.fields[2].size, 1); // u8
    }
}
