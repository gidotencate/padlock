// padlock-source/src/fixgen.rs
//
// Generate reordered struct source text, unified diffs, and in-place rewrites.

use padlock_core::ir::{StructLayout, optimal_order};
use similar::{ChangeTag, TextDiff};

/// Render a reordered C/C++ struct definition as source text.
///
/// Uses the field names already present in the layout — type names come from
/// the `TypeInfo::Primitive/Opaque` name stored during source parsing.
pub fn generate_c_fix(layout: &StructLayout) -> String {
    let optimal = optimal_order(layout);
    let mut out = format!("struct {} {{\n", layout.name);
    for field in &optimal {
        let ty = field_type_name(field);
        out.push_str(&format!("    {ty} {};\n", field.name));
    }
    out.push_str("};\n");
    out
}

/// Render a reordered Rust struct definition as source text.
pub fn generate_rust_fix(layout: &StructLayout) -> String {
    let optimal = optimal_order(layout);
    let mut out = format!("struct {} {{\n", layout.name);
    for field in &optimal {
        let ty = field_type_name(field);
        out.push_str(&format!("    {}: {ty},\n", field.name));
    }
    out.push_str("}\n");
    out
}

/// Render a reordered Go struct definition as source text.
pub fn generate_go_fix(layout: &StructLayout) -> String {
    let optimal = optimal_order(layout);
    let mut out = format!("type {} struct {{\n", layout.name);
    for field in &optimal {
        let ty = field_type_name(field);
        out.push_str(&format!("\t{}\t{ty}\n", field.name));
    }
    out.push_str("}\n");
    out
}

/// Produce a unified diff between `original` and `fixed` source text.
pub fn unified_diff(original: &str, fixed: &str, context_lines: usize) -> String {
    if original == fixed {
        return String::from("(no changes)\n");
    }
    let diff = TextDiff::from_lines(original, fixed);
    let mut out = String::new();
    for (idx, group) in diff.grouped_ops(context_lines).iter().enumerate() {
        if idx > 0 {
            out.push_str("...\n");
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let prefix = match change.tag() {
                    ChangeTag::Delete => "-",
                    ChangeTag::Insert => "+",
                    ChangeTag::Equal => " ",
                };
                out.push_str(&format!("{prefix} {}", change.value()));
                if !change.value().ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    out
}

// ── span finders ──────────────────────────────────────────────────────────────

/// Count matching braces from the start of `s` (which must begin with `{`).
/// Returns the byte index one past the matching `}`.
fn match_braces(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Consume an optional trailing semicolon (after optional whitespace) at `pos`.
fn consume_semicolon(source: &str, pos: usize) -> usize {
    let rest = &source[pos..];
    let ws = rest.len()
        - rest
            .trim_start_matches(|c: char| c.is_whitespace() && c != '\n')
            .len();
    let after_ws = &rest[ws..];
    if after_ws.starts_with(';') {
        pos + ws + 1
    } else {
        pos
    }
}

/// Find the byte range of a named struct/union in C/C++ source.
/// The range covers from `struct/union Name` through the closing `};`.
pub fn find_c_struct_span(source: &str, struct_name: &str) -> Option<std::ops::Range<usize>> {
    for kw in &["struct", "union"] {
        let needle = format!("{kw} {struct_name}");
        let mut search_from = 0usize;
        while let Some(rel) = source[search_from..].find(&needle) {
            let start = search_from + rel;
            let after_name = start + needle.len();
            // Ensure the character after the name is a boundary (space, `{`, newline)
            let boundary = source[after_name..].chars().next();
            if matches!(
                boundary,
                Some('{') | Some('\n') | Some('\r') | Some(' ') | Some('\t') | None
            ) {
                // Find the opening brace (may have whitespace between name and `{`)
                if let Some(brace_rel) = source[after_name..].find('{') {
                    let brace_start = after_name + brace_rel;
                    // Verify no word characters between name end and brace
                    if source[after_name..brace_start]
                        .chars()
                        .all(|c| c.is_whitespace())
                    {
                        if let Some(body_len) = match_braces(&source[brace_start..]) {
                            let end = consume_semicolon(source, brace_start + body_len);
                            return Some(start..end);
                        }
                    }
                }
            }
            search_from = start + 1;
        }
    }
    None
}

/// Find the byte range of a named struct in Rust source (`struct Name { ... }`).
pub fn find_rust_struct_span(source: &str, struct_name: &str) -> Option<std::ops::Range<usize>> {
    let needle = format!("struct {struct_name}");
    let mut search_from = 0usize;
    while let Some(rel) = source[search_from..].find(&needle) {
        let start = search_from + rel;
        let after_name = start + needle.len();
        let boundary = source[after_name..].chars().next();
        if matches!(
            boundary,
            Some('{') | Some('\n') | Some('\r') | Some(' ') | Some('\t') | None
        ) {
            if let Some(brace_rel) = source[after_name..].find('{') {
                let brace_start = after_name + brace_rel;
                if source[after_name..brace_start]
                    .chars()
                    .all(|c| c.is_whitespace())
                {
                    if let Some(body_len) = match_braces(&source[brace_start..]) {
                        // Rust structs have no trailing `;` (unit structs do, but we skip those)
                        return Some(start..brace_start + body_len);
                    }
                }
            }
        }
        search_from = start + 1;
    }
    None
}

/// Find the byte range of a named struct in Go source (`type Name struct { ... }`).
pub fn find_go_struct_span(source: &str, struct_name: &str) -> Option<std::ops::Range<usize>> {
    let needle = format!("type {struct_name} struct");
    let mut search_from = 0usize;
    while let Some(rel) = source[search_from..].find(&needle) {
        let start = search_from + rel;
        let after_kw = start + needle.len();
        if let Some(brace_rel) = source[after_kw..].find('{') {
            let brace_start = after_kw + brace_rel;
            if source[after_kw..brace_start]
                .chars()
                .all(|c| c.is_whitespace())
            {
                if let Some(body_len) = match_braces(&source[brace_start..]) {
                    return Some(start..brace_start + body_len);
                }
            }
        }
        search_from = start + 1;
    }
    None
}

// ── in-place rewriters ────────────────────────────────────────────────────────

/// Apply C/C++ struct reorderings in-place, returning the modified source.
/// Each layout in `layouts` is looked up by name; matched structs are replaced
/// with the optimally-ordered definition. Replacements are applied back-to-front
/// so byte offsets remain valid.
pub fn apply_fixes_c(source: &str, layouts: &[&StructLayout]) -> String {
    apply_fixes(source, layouts, find_c_struct_span, generate_c_fix)
}

/// Apply Rust struct reorderings in-place, returning the modified source.
pub fn apply_fixes_rust(source: &str, layouts: &[&StructLayout]) -> String {
    apply_fixes(source, layouts, find_rust_struct_span, generate_rust_fix)
}

/// Apply Go struct reorderings in-place, returning the modified source.
pub fn apply_fixes_go(source: &str, layouts: &[&StructLayout]) -> String {
    apply_fixes(source, layouts, find_go_struct_span, generate_go_fix)
}

fn apply_fixes(
    source: &str,
    layouts: &[&StructLayout],
    find_span: fn(&str, &str) -> Option<std::ops::Range<usize>>,
    generate: fn(&StructLayout) -> String,
) -> String {
    // Collect (start, end, replacement) for each matching layout
    let mut replacements: Vec<(usize, usize, String)> = layouts
        .iter()
        .filter_map(|layout| {
            let span = find_span(source, &layout.name)?;
            let fixed = generate(layout);
            Some((span.start, span.end, fixed))
        })
        .collect();

    // Sort by start offset ascending, then apply in reverse so offsets stay valid
    replacements.sort_by_key(|(start, _, _)| *start);

    let mut result = source.to_string();
    for (start, end, fixed) in replacements.into_iter().rev() {
        result.replace_range(start..end, &fixed);
    }
    result
}

fn field_type_name(field: &padlock_core::ir::Field) -> &str {
    match &field.ty {
        padlock_core::ir::TypeInfo::Primitive { name, .. }
        | padlock_core::ir::TypeInfo::Opaque { name, .. } => name.as_str(),
        padlock_core::ir::TypeInfo::Pointer { .. } => "void*",
        padlock_core::ir::TypeInfo::Array { .. } => "/* array */",
        padlock_core::ir::TypeInfo::Struct(l) => l.name.as_str(),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::ir::test_fixtures::connection_layout;

    #[test]
    fn c_fix_starts_with_struct() {
        let out = generate_c_fix(&connection_layout());
        assert!(out.starts_with("struct Connection {"));
    }

    #[test]
    fn c_fix_contains_all_fields() {
        let out = generate_c_fix(&connection_layout());
        assert!(out.contains("timeout"));
        assert!(out.contains("port"));
        assert!(out.contains("is_active"));
        assert!(out.contains("is_tls"));
    }

    #[test]
    fn c_fix_puts_largest_align_first() {
        let out = generate_c_fix(&connection_layout());
        let timeout_pos = out.find("timeout").unwrap();
        let is_active_pos = out.find("is_active").unwrap();
        assert!(timeout_pos < is_active_pos);
    }

    #[test]
    fn rust_fix_uses_colon_syntax() {
        let out = generate_rust_fix(&connection_layout());
        assert!(out.contains(": f64"));
    }

    #[test]
    fn unified_diff_marks_changes() {
        let orig = "struct T { char a; double b; };\n";
        let fixed = "struct T { double b; char a; };\n";
        let diff = unified_diff(orig, fixed, 1);
        assert!(diff.contains('-') || diff.contains('+'));
    }

    #[test]
    fn unified_diff_identical_is_no_changes() {
        assert_eq!(unified_diff("x\n", "x\n", 3), "(no changes)\n");
    }

    // ── span finders ──────────────────────────────────────────────────────────

    #[test]
    fn find_c_struct_span_basic() {
        let src = "struct Foo { int x; char y; };\nstruct Bar { double z; };\n";
        let span = find_c_struct_span(src, "Foo").unwrap();
        let text = &src[span];
        assert!(text.starts_with("struct Foo"));
        assert!(!text.contains("Bar"));
    }

    #[test]
    fn find_c_struct_span_missing_returns_none() {
        let src = "struct Other { int x; };";
        assert!(find_c_struct_span(src, "Missing").is_none());
    }

    #[test]
    fn find_rust_struct_span_basic() {
        let src = "struct Foo {\n    x: u32,\n    y: u8,\n}\n";
        let span = find_rust_struct_span(src, "Foo").unwrap();
        assert!(src[span].starts_with("struct Foo"));
    }

    #[test]
    fn find_go_struct_span_basic() {
        let src = "type Foo struct {\n\tX int32\n\tY bool\n}\n";
        let span = find_go_struct_span(src, "Foo").unwrap();
        assert!(src[span].starts_with("type Foo struct"));
    }

    // ── apply_fixes ───────────────────────────────────────────────────────────

    #[test]
    fn apply_fixes_c_reorders_in_place() {
        // Connection has char/double/char/int — after fix, double should come first
        let src = "struct Connection { bool is_active; double timeout; bool is_tls; int port; };\n";
        let layout = connection_layout();
        let fixed = apply_fixes_c(src, &[&layout]);
        let timeout_pos = fixed.find("timeout").unwrap();
        let is_active_pos = fixed.find("is_active").unwrap();
        assert!(
            timeout_pos < is_active_pos,
            "double should appear before bool after reorder"
        );
    }

    #[test]
    fn apply_fixes_rust_reorders_in_place() {
        let src = "struct Connection {\n    is_active: bool,\n    timeout: f64,\n    is_tls: bool,\n    port: i32,\n}\n";
        let layout = connection_layout();
        let fixed = apply_fixes_rust(src, &[&layout]);
        let timeout_pos = fixed.find("timeout").unwrap();
        let is_active_pos = fixed.find("is_active").unwrap();
        assert!(timeout_pos < is_active_pos);
    }

    #[test]
    fn go_fix_uses_tab_syntax() {
        let layout = connection_layout();
        let out = generate_go_fix(&layout);
        assert!(out.starts_with("type Connection struct"));
        assert!(out.contains('\t'));
    }
}
