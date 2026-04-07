// padlock-source/src/frontends/go.rs
//
// Extracts struct layouts from Go source using tree-sitter-go.
// Sizes use Go's platform-native alignment rules (same as C on the target arch).

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
use tree_sitter::{Node, Parser};

// ── type resolution ───────────────────────────────────────────────────────────

fn go_type_size_align(ty: &str, arch: &'static ArchConfig) -> (usize, usize) {
    match ty.trim() {
        "bool"                  => (1, 1),
        "int8"  | "uint8"  | "byte" => (1, 1),
        "int16" | "uint16"      => (2, 2),
        "int32" | "uint32" | "rune" | "float32" => (4, 4),
        "int64" | "uint64" | "float64" | "complex64" => (8, 8),
        "complex128"            => (16, 16),
        "int"  | "uint"         => (arch.pointer_size, arch.pointer_size),
        "uintptr"               => (arch.pointer_size, arch.pointer_size),
        "string"                => (arch.pointer_size * 2, arch.pointer_size), // ptr + len
        ty if ty.starts_with("[]") => (arch.pointer_size * 3, arch.pointer_size), // ptr+len+cap
        ty if ty.starts_with("map[") || ty.starts_with("chan ") => {
            (arch.pointer_size, arch.pointer_size)
        }
        ty if ty.starts_with('*') => (arch.pointer_size, arch.pointer_size),
        // Interface types: two-word fat pointer
        "error" => (arch.pointer_size * 2, arch.pointer_size),
        _ => (arch.pointer_size, arch.pointer_size),
    }
}

// ── tree-sitter walker ────────────────────────────────────────────────────────

fn extract_structs(source: &str, root: Node<'_>, arch: &'static ArchConfig) -> Vec<StructLayout> {
    let mut layouts = Vec::new();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(c) = node.child(i) { stack.push(c); }
        }

        // type_declaration → type_spec → struct_type
        if node.kind() == "type_declaration" {
            if let Some(layout) = parse_type_declaration(source, node, arch) {
                layouts.push(layout);
            }
        }
    }
    layouts
}

fn parse_type_declaration(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
) -> Option<StructLayout> {
    // type_declaration has a type_spec child
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        if child.kind() == "type_spec" {
            return parse_type_spec(source, child, arch);
        }
    }
    None
}

fn parse_type_spec(source: &str, node: Node<'_>, arch: &'static ArchConfig) -> Option<StructLayout> {
    let mut name: Option<String> = None;
    let mut struct_node: Option<Node> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "type_identifier" => name = Some(source[child.byte_range()].to_string()),
            "struct_type"     => struct_node = Some(child),
            _ => {}
        }
    }

    let name = name?;
    let struct_node = struct_node?;
    parse_struct_type(source, struct_node, name, arch)
}

fn parse_struct_type(
    source: &str,
    node: Node<'_>,
    name: String,
    arch: &'static ArchConfig,
) -> Option<StructLayout> {
    let mut raw_fields: Vec<(String, String)> = Vec::new();

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        if child.kind() == "field_declaration_list" {
            for j in 0..child.child_count() {
                let field_node = child.child(j)?;
                if field_node.kind() == "field_declaration" {
                    collect_field_declarations(source, field_node, &mut raw_fields);
                }
            }
        }
    }

    if raw_fields.is_empty() {
        return None;
    }

    // Simulate layout
    let mut offset = 0usize;
    let mut struct_align = 1usize;
    let mut fields: Vec<Field> = Vec::new();

    for (fname, ty_name) in raw_fields {
        let (size, align) = go_type_size_align(&ty_name, arch);
        if align > 0 {
            offset = offset.next_multiple_of(align);
        }
        struct_align = struct_align.max(align);
        fields.push(Field {
            name: fname,
            ty: TypeInfo::Primitive { name: ty_name, size, align },
            offset,
            size,
            align,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        });
        offset += size;
    }
    if struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }

    Some(StructLayout {
        name,
        total_size: offset,
        align: struct_align,
        fields,
        source_file: None,
        source_line: None,
        arch,
        is_packed: false,
        is_union: false,
    })
}

fn collect_field_declarations(source: &str, node: Node<'_>, out: &mut Vec<(String, String)>) {
    // field_declaration: field_identifier+ type
    let mut field_names: Vec<String> = Vec::new();
    let mut ty_text: Option<String> = None;

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "field_identifier" => field_names.push(source[child.byte_range()].to_string()),
                "type_identifier" | "pointer_type" | "qualified_type"
                | "slice_type" | "map_type" | "channel_type" | "array_type" => {
                    ty_text = Some(source[child.byte_range()].trim().to_string());
                }
                _ => {}
            }
        }
    }

    if let Some(ty) = ty_text {
        for name in field_names {
            out.push((name, ty.clone()));
        }
    }
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_go(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::language())?;
    let tree = parser.parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter-go parse failed"))?;
    Ok(extract_structs(source, tree.root_node(), arch))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn parse_simple_go_struct() {
        let src = r#"
package main
type Point struct {
    X int32
    Y int32
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
        assert_eq!(layouts[0].fields.len(), 2);
    }

    #[test]
    fn go_layout_with_padding() {
        let src = "package p\ntype T struct { A bool; B int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 8); // bool (1) + 7 pad → 8
    }

    #[test]
    fn go_string_is_two_words() {
        let src = "package p\ntype S struct { Name string }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // ptr + len
    }
}
