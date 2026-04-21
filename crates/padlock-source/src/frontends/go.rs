// padlock-source/src/frontends/go.rs
//
// Extracts struct layouts from Go source using tree-sitter-go.
// Sizes use Go's platform-native alignment rules (same as C on the target arch).

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
use std::collections::HashSet;
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
        // Interface types: two-word fat pointer (type pointer + data pointer).
        // `error` and `any` are the two universally-known interface names; inline
        // anonymous interface bodies (`interface{ Method() }`) are caught by the
        // `starts_with("interface")` arm.
        //
        // Locally-declared named interfaces (e.g. `type Reader interface { … }`) are
        // resolved to 16B by `parse_struct_type` using the phase-1 interface name set
        // collected by `collect_go_interface_names` — they do not reach this function.
        //
        // Qualified names from external packages (e.g. `io.Reader`, `driver.Connector`)
        // fall through to the `_` arm (pointer_size) and are flagged as `uncertain_fields`
        // by `parse_struct_type` so the output layer can warn the user.
        "error" | "any" => (arch.pointer_size * 2, arch.pointer_size),
        ty if ty.starts_with("interface") => (arch.pointer_size * 2, arch.pointer_size),
        _ => (arch.pointer_size, arch.pointer_size),
    }
}

// ── phase-1: local interface name collection ──────────────────────────────────

/// Scan a Go source tree for `type X interface { ... }` declarations and
/// return the set of locally-defined interface names.
///
/// These names are used in `parse_struct_type` to size named-interface fields
/// as two-word fat pointers (type-pointer + data-pointer, 16 bytes on 64-bit)
/// instead of falling through to the generic pointer-sized unknown catch-all.
fn collect_go_interface_names(source: &str, root: Node<'_>) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
        if node.kind() != "type_spec" {
            continue;
        }
        // type_spec: name=type_identifier, type=interface_type
        let mut iface_name: Option<String> = None;
        let mut is_interface = false;
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else { continue };
            match child.kind() {
                "type_identifier" => {
                    iface_name = Some(source[child.byte_range()].to_string());
                }
                "interface_type" => {
                    is_interface = true;
                }
                _ => {}
            }
        }
        if is_interface && let Some(name) = iface_name {
            names.insert(name);
        }
    }
    names
}

// ── tree-sitter walker ────────────────────────────────────────────────────────

fn extract_structs(source: &str, root: Node<'_>, arch: &'static ArchConfig) -> Vec<StructLayout> {
    // Phase 1: collect locally-defined interface names for accurate fat-pointer sizing.
    let local_interfaces = collect_go_interface_names(source, root);

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
            && let Some(layout) = parse_type_declaration(source, node, arch, &local_interfaces)
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
    local_interfaces: &HashSet<String>,
) -> Option<StructLayout> {
    let source_line = node.start_position().row as u32 + 1;
    let decl_start_byte = node.start_byte();
    // type_declaration has a type_spec child
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        if child.kind() == "type_spec" {
            return parse_type_spec(
                source,
                child,
                arch,
                source_line,
                decl_start_byte,
                local_interfaces,
            );
        }
    }
    None
}

fn parse_type_spec(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    source_line: u32,
    decl_start_byte: usize,
    local_interfaces: &HashSet<String>,
) -> Option<StructLayout> {
    let mut name: Option<String> = None;
    let mut struct_node: Option<Node> = None;
    let mut is_generic = false;

    for i in 0..node.child_count() {
        let child = node.child(i)?;
        match child.kind() {
            "type_identifier" => name = Some(source[child.byte_range()].to_string()),
            "struct_type" => struct_node = Some(child),
            "type_parameter_list" => is_generic = true,
            _ => {}
        }
    }

    let name = name?;

    if is_generic {
        eprintln!(
            "padlock: note: skipping '{name}' — generic struct \
             (layout depends on type arguments; use binary analysis for accurate results)"
        );
        crate::record_skipped(
            &name,
            "generic struct — layout depends on type arguments; \
             use binary analysis for accurate results",
        );
        return None;
    }

    let struct_node = struct_node?;
    parse_struct_type(
        source,
        struct_node,
        name,
        arch,
        source_line,
        decl_start_byte,
        local_interfaces,
    )
}

fn parse_struct_type(
    source: &str,
    node: Node<'_>,
    name: String,
    arch: &'static ArchConfig,
    source_line: u32,
    decl_start_byte: usize,
    local_interfaces: &HashSet<String>,
) -> Option<StructLayout> {
    let mut raw_fields: Vec<(String, String, Option<String>, u32)> = Vec::new();

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
    let mut uncertain_fields: Vec<String> = Vec::new();

    for (fname, ty_name, guard, field_line) in raw_fields {
        let (mut size, mut align) = go_type_size_align(&ty_name, arch);

        // Override: a locally-declared interface type is a fat pointer (16B on 64-bit).
        // go_type_size_align does not know about local names, so we patch here.
        if local_interfaces.contains(ty_name.as_str()) {
            size = arch.pointer_size * 2;
            align = arch.pointer_size;
        }

        // Qualified types (e.g. `driver.Connector`, `io.Reader`) come from external
        // packages. Without type information we cannot determine whether they are
        // interfaces (16B fat pointer) or structs (arbitrary size). Flag them as
        // uncertain so the output layer can warn the user.
        let is_pointer = ty_name.starts_with('*');
        let base_ty = ty_name.trim_start_matches('*');
        if !is_pointer && base_ty.contains('.') {
            uncertain_fields.push(fname.clone());
        }

        if align > 0 {
            offset = offset.next_multiple_of(align);
        }
        struct_align = struct_align.max(align);
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
            offset,
            size,
            align,
            source_file: None,
            source_line: Some(field_line),
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
        is_repr_rust: false,
        suppressed_findings: super::suppress::suppressed_from_preceding_source(
            source,
            decl_start_byte,
        ),
        uncertain_fields,
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
    out: &mut Vec<(String, String, Option<String>, u32)>,
) {
    // field_declaration: field_identifier+ type [comment]
    // OR embedded type (anonymous field): TypeName [comment]
    let mut field_names: Vec<String> = Vec::new();
    let mut ty_text: Option<String> = None;
    let field_line = node.start_position().row as u32 + 1;

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

    let guard =
        trailing_comment_on_line(source, node).and_then(|c| extract_guard_from_go_comment(&c));

    if !field_names.is_empty() {
        if let Some(ty) = ty_text {
            // Normal named fields
            for name in field_names {
                out.push((name, ty.clone(), guard.clone(), field_line));
            }
        }
    } else if let Some(ty) = ty_text {
        // Embedded (anonymous) field: `sync.Mutex` or `Base`.
        // Go field name is the unqualified type name.
        // The nested-struct resolution pass in lib.rs will later fill in
        // the correct size/align from other parsed struct layouts.
        let simple_name = ty.split('.').next_back().unwrap_or(&ty).to_string();
        out.push((simple_name, ty, guard, field_line));
    }
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_go(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::LANGUAGE.into())?;
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

    #[test]
    fn inline_interface_with_methods_is_two_words() {
        // An anonymous interface with methods (e.g. `interface{ Close() error }`) is a
        // two-word fat pointer — same as `interface{}`.  The tree-sitter node kind is
        // `interface_type` in both cases so the `ty.starts_with("interface")` match handles
        // all inline interface bodies.
        let src = "package p\ntype S struct { Conn interface{ Close() error } }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16);
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn named_cross_package_interface_falls_back_to_pointer_size() {
        // Named interfaces from other packages (driver.Connector, io.ReadCloser, …)
        // appear in the AST as `qualified_type` nodes with text like "driver.Connector".
        // Without go/types resolution we cannot distinguish an interface from a concrete
        // struct, so they fall back to pointer_size (8B on x86-64) — the same as an
        // opaque pointer.  This is a known source-analysis limitation; binary (DWARF)
        // analysis always returns the correct compiler layout.
        let src = "package p\ntype DB struct { connector driver.Connector }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        // Known limitation: reports 8B, not the actual 16B.
        assert_eq!(
            layouts[0].fields[0].size, 8,
            "named cross-package interface falls back to pointer_size (known limitation)"
        );
        // The field must be flagged as uncertain so the output layer can warn the user.
        assert!(
            layouts[0]
                .uncertain_fields
                .contains(&"connector".to_string()),
            "qualified-type field should be in uncertain_fields"
        );
    }

    // ── local interface type resolution ───────────────────────────────────────

    #[test]
    fn local_interface_field_is_fat_pointer() {
        // A named interface declared in the same file must be sized as a two-word
        // fat pointer (16B on 64-bit), not as a single pointer (8B).
        let src = r#"package p
type Reader interface {
    Read(p []byte) (n int, err error)
}
type Buf struct {
    R Reader
    N int32
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Buf").expect("Buf");
        let r = l.fields.iter().find(|f| f.name == "R").expect("R field");
        assert_eq!(
            r.size, 16,
            "local interface must be sized as 16B fat pointer"
        );
        assert_eq!(r.align, 8);
    }

    #[test]
    fn local_interface_field_not_marked_uncertain() {
        // A locally-declared interface is resolved; it must NOT appear in uncertain_fields.
        let src = r#"package p
type Closer interface { Close() error }
type File struct { C Closer }
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "File").expect("File");
        assert!(
            !l.uncertain_fields.contains(&"C".to_string()),
            "local interface field must not be uncertain"
        );
    }

    #[test]
    fn qualified_type_field_marked_uncertain() {
        // A qualified type (e.g. `io.Reader`) from an external package cannot be
        // resolved without go/types; the field must appear in uncertain_fields.
        let src = "package p\ntype S struct { R io.Reader; N int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert!(
            l.uncertain_fields.contains(&"R".to_string()),
            "qualified-type field must be in uncertain_fields"
        );
        // Non-qualified field must not be uncertain
        assert!(
            !l.uncertain_fields.contains(&"N".to_string()),
            "plain int32 field must not be uncertain"
        );
    }

    #[test]
    fn pointer_to_qualified_type_not_uncertain() {
        // `*pkg.Type` is an explicit pointer — size is always pointer_size (8B).
        // No need to flag it as uncertain since the pointer indirection makes the
        // type's internal layout irrelevant for padding analysis.
        let src = "package p\ntype S struct { P *io.Reader }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert!(
            !l.uncertain_fields.contains(&"P".to_string()),
            "*qualified.Type pointer must not be uncertain"
        );
    }

    // ── embedded struct support ───────────────────────────────────────────────

    #[test]
    fn embedded_struct_field_uses_type_name_as_field_name() {
        // `Base` is an embedded field — Go uses the type name as the field name.
        let src = r#"package p
type Base struct { X int32 }
type Derived struct {
    Base
    Y int32
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let derived = layouts
            .iter()
            .find(|l| l.name == "Derived")
            .expect("Derived");
        // Must have a field named "Base"
        assert!(
            derived.fields.iter().any(|f| f.name == "Base"),
            "embedded field should be named 'Base'"
        );
    }

    #[test]
    fn embedded_qualified_type_uses_unqualified_name() {
        // `sync.Mutex` embedded — field name should be "Mutex"
        let src = r#"package p
type Safe struct {
    sync.Mutex
    Value int64
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Safe").expect("Safe");
        assert!(
            l.fields.iter().any(|f| f.name == "Mutex"),
            "embedded sync.Mutex should produce field named 'Mutex'"
        );
    }

    #[test]
    fn embedded_field_has_non_zero_size_from_resolution() {
        // After lib.rs nested-struct resolution, Base's size should be filled in.
        // We test via parse_source_str which triggers resolution.
        let src = r#"package p
type Inner struct { A int64; B int64 }
type Outer struct {
    Inner
    C int32
}
"#;
        use crate::{SourceLanguage, parse_source_str};
        let layouts = parse_source_str(src, &SourceLanguage::Go, &X86_64_SYSV).unwrap();
        let outer = layouts.iter().find(|l| l.name == "Outer").expect("Outer");
        let inner_field = outer
            .fields
            .iter()
            .find(|f| f.name == "Inner")
            .expect("Inner field");
        // Inner struct is 16 bytes (two int64s)
        assert_eq!(
            inner_field.size, 16,
            "embedded Inner field should be resolved to 16 bytes"
        );
    }

    #[test]
    fn struct_with_no_embedded_fields_unaffected() {
        let src = "package p\ntype S struct { A int32; B int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields.len(), 2);
        assert_eq!(l.fields[0].name, "A");
        assert_eq!(l.fields[1].name, "B");
    }

    // ── Go generics ───────────────────────────────────────────────────────────

    #[test]
    fn go_generic_struct_is_skipped() {
        // Generic structs cannot be sized without type instantiation.
        let src = "package p\ntype Pair[T any] struct { First T; Second T }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Pair"),
            "generic struct must be skipped"
        );
    }

    #[test]
    fn go_concrete_struct_alongside_generic_is_parsed() {
        // The generic is skipped but a concrete sibling struct is still parsed.
        let src = "package p\ntype Pair[T any] struct { First T }\ntype Point struct { X int32; Y int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Pair"),
            "Pair must be skipped"
        );
        assert!(
            layouts.iter().any(|l| l.name == "Point"),
            "Point must be parsed"
        );
    }

    // ── bad weather: embedded fields ──────────────────────────────────────────

    #[test]
    fn embedded_unknown_type_falls_back_to_pointer_size() {
        // If the embedded type is not defined in the file, size = pointer_size
        let src = "package p\ntype S struct { external.Type\nX int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let emb = l
            .fields
            .iter()
            .find(|f| f.name == "Type")
            .expect("Type field");
        // Falls back to pointer size (8 on x86_64) since type is unknown
        assert_eq!(emb.size, 8);
    }
}
