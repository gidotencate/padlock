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
        "bool" => (1, 1),
        "int8" | "uint8" | "byte" => (1, 1),
        "int16" | "uint16" => (2, 2),
        "int32" | "uint32" | "rune" | "float32" => (4, 4),
        "int64" | "uint64" | "float64" | "complex64" => (8, 8),
        "complex128" => (16, 16),
        "int" | "uint" => (arch.pointer_size, arch.pointer_size),
        "uintptr" => (arch.pointer_size, arch.pointer_size),
        "string" => (arch.pointer_size * 2, arch.pointer_size), // ptr + len
        ty if ty.starts_with("[]") => (arch.pointer_size * 3, arch.pointer_size), // ptr+len+cap
        ty if ty.starts_with("map[") || ty.starts_with("chan ") => {
            (arch.pointer_size, arch.pointer_size)
        }
        ty if ty.starts_with('*') => (arch.pointer_size, arch.pointer_size),
        // Interface types: two-word fat pointer (type pointer + data pointer)
        "error" | "interface{}" | "any" => (arch.pointer_size * 2, arch.pointer_size),
        _ => (arch.pointer_size, arch.pointer_size),
    }
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

        // type_declaration → type_spec → struct_type
        if node.kind() == "type_declaration"
            && let Some(layout) = parse_type_declaration(source, node, arch)
        {
            layouts.push(layout);
        }
    }
    layouts
}

fn parse_type_declaration(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
) -> Option<StructLayout> {
    let source_line = node.start_position().row as u32 + 1;
    // type_declaration has a type_spec child
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        if child.kind() == "type_spec" {
            return parse_type_spec(source, child, arch, source_line);
        }
    }
    None
}

fn parse_type_spec(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    source_line: u32,
) -> Option<StructLayout> {
    let mut name: Option<String> = None;
    let mut struct_node: Option<Node> = None;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "type_identifier" => name = Some(source[child.byte_range()].to_string()),
            "struct_type" => struct_node = Some(child),
            _ => {}
        }
    }

    let name = name?;
    let struct_node = struct_node?;
    parse_struct_type(source, struct_node, name, arch, source_line)
}

fn parse_struct_type(
    source: &str,
    node: Node<'_>,
    name: String,
    arch: &'static ArchConfig,
    source_line: u32,
) -> Option<StructLayout> {
    let mut raw_fields: Vec<(String, String, Option<String>)> = Vec::new();

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

    for (fname, ty_name, guard) in raw_fields {
        let (size, align) = go_type_size_align(&ty_name, arch);
        if align > 0 {
            offset = offset.next_multiple_of(align);
        }
        struct_align = struct_align.max(align);
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
            offset,
            size,
            align,
            source_file: None,
            source_line: None,
            access,
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
        source_line: Some(source_line),
        arch,
        is_packed: false,
        is_union: false,
    })
}

/// Extract a guard name from a Go field's trailing line comment.
///
/// Recognised forms (must appear after the field type on the same line):
/// - `// padlock:guard=mu`
/// - `// guarded_by: mu`
/// - `// +checklocksprotects:mu` (gVisor-style)
pub fn extract_guard_from_go_comment(comment: &str) -> Option<String> {
    let c = comment.trim();
    // Strip leading `//` and optional whitespace
    let body = c.strip_prefix("//").map(str::trim)?;

    // padlock:guard=mu
    if let Some(rest) = body.strip_prefix("padlock:guard=") {
        let guard = rest.trim();
        if !guard.is_empty() {
            return Some(guard.to_string());
        }
    }
    // guarded_by: mu
    if let Some(rest) = body
        .strip_prefix("guarded_by:")
        .or_else(|| body.strip_prefix("guarded_by ="))
    {
        let guard = rest.trim();
        if !guard.is_empty() {
            return Some(guard.to_string());
        }
    }
    // +checklocksprotects:mu (gVisor)
    if let Some(rest) = body.strip_prefix("+checklocksprotects:") {
        let guard = rest.trim();
        if !guard.is_empty() {
            return Some(guard.to_string());
        }
    }
    None
}

/// Find the trailing line comment on the same source line as `node`.
fn trailing_comment_on_line(source: &str, node: Node<'_>) -> Option<String> {
    // The node's end byte is just past the last token on the field line.
    // Read the rest of that line from the source.
    let end = node.end_byte();
    if end >= source.len() {
        return None;
    }
    let rest = &source[end..];
    // Take only up to the next newline
    let line = rest.lines().next().unwrap_or("");
    // Look for `//` in that remainder
    line.find("//").map(|pos| line[pos..].to_string())
}

fn collect_field_declarations(
    source: &str,
    node: Node<'_>,
    out: &mut Vec<(String, String, Option<String>)>,
) {
    // field_declaration: field_identifier+ type [comment]
    let mut field_names: Vec<String> = Vec::new();
    let mut ty_text: Option<String> = None;

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "field_identifier" => field_names.push(source[child.byte_range()].to_string()),
                "type_identifier" | "pointer_type" | "qualified_type" | "slice_type"
                | "map_type" | "channel_type" | "array_type" | "interface_type" => {
                    ty_text = Some(source[child.byte_range()].trim().to_string());
                }
                _ => {}
            }
        }
    }

    if let Some(ty) = ty_text {
        // Check for trailing guard comment on this field's line
        let guard =
            trailing_comment_on_line(source, node).and_then(|c| extract_guard_from_go_comment(&c));
        for name in field_names {
            out.push((name, ty.clone(), guard.clone()));
        }
    }
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_go(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::language())?;
    let tree = parser
        .parse(source, None)
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

    // ── Go guard comment extraction ────────────────────────────────────────────

    #[test]
    fn extract_guard_padlock_form() {
        assert_eq!(
            extract_guard_from_go_comment("// padlock:guard=mu"),
            Some("mu".to_string())
        );
    }

    #[test]
    fn extract_guard_guarded_by_form() {
        assert_eq!(
            extract_guard_from_go_comment("// guarded_by: counter_lock"),
            Some("counter_lock".to_string())
        );
    }

    #[test]
    fn extract_guard_checklocksprotects_form() {
        assert_eq!(
            extract_guard_from_go_comment("// +checklocksprotects:mu"),
            Some("mu".to_string())
        );
    }

    #[test]
    fn extract_guard_no_match_returns_none() {
        assert!(extract_guard_from_go_comment("// just a comment").is_none());
        assert!(extract_guard_from_go_comment("// TODO: fix this").is_none());
    }

    #[test]
    fn go_struct_padlock_guard_annotation_sets_concurrent() {
        let src = r#"package p
type Cache struct {
    Readers int64 // padlock:guard=mu
    Writers int64 // padlock:guard=other_mu
    Mu      sync.Mutex
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        // Readers and Writers should be Concurrent with different guards
        if let AccessPattern::Concurrent { guard, .. } = &l.fields[0].access {
            assert_eq!(guard.as_deref(), Some("mu"));
        } else {
            panic!(
                "expected Concurrent for Readers, got {:?}",
                l.fields[0].access
            );
        }
        if let AccessPattern::Concurrent { guard, .. } = &l.fields[1].access {
            assert_eq!(guard.as_deref(), Some("other_mu"));
        } else {
            panic!(
                "expected Concurrent for Writers, got {:?}",
                l.fields[1].access
            );
        }
    }

    #[test]
    fn go_struct_different_guards_same_cache_line_is_false_sharing() {
        let src = r#"package p
type HotPath struct {
    Readers int64 // padlock:guard=lock_a
    Writers int64 // padlock:guard=lock_b
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(padlock_core::analysis::false_sharing::has_false_sharing(
            &layouts[0]
        ));
    }

    #[test]
    fn go_struct_same_guard_is_not_false_sharing() {
        let src = r#"package p
type Safe struct {
    A int64 // padlock:guard=mu
    B int64 // padlock:guard=mu
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(!padlock_core::analysis::false_sharing::has_false_sharing(
            &layouts[0]
        ));
    }

    // ── interface{} / any sizing ───────────────────────────────────────────────

    #[test]
    fn interface_field_is_two_words() {
        // interface{} is a fat pointer: (type pointer, data pointer) = 2×pointer
        let src = "package p\ntype S struct { V interface{} }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // 2 × 8B on x86-64
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn any_field_is_two_words() {
        // `any` is an alias for `interface{}` since Go 1.18
        let src = "package p\ntype S struct { V any }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // 2 × 8B on x86-64
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn interface_field_same_size_as_error() {
        // `error` was already two-word; interface{} must match
        let src_iface = "package p\ntype S struct { V interface{} }";
        let src_err = "package p\ntype S struct { V error }";
        let iface = parse_go(src_iface, &X86_64_SYSV).unwrap();
        let err = parse_go(src_err, &X86_64_SYSV).unwrap();
        assert_eq!(iface[0].fields[0].size, err[0].fields[0].size);
    }

    #[test]
    fn struct_with_mixed_interface_and_ints_has_correct_layout() {
        // interface{} at offset 0 (size 16, align 8) then int64 at offset 16
        let src = "package p\ntype S struct { V interface{}; N int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[0].size, 16);
        assert_eq!(l.fields[1].offset, 16);
        assert_eq!(l.total_size, 24);
    }
}
