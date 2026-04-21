// padlock-source/src/fixgen.rs
//
// Generate reordered struct source text, unified diffs, and in-place rewrites.
//
// Fix quality: when the original source is available, field declarations
// (including attributes, doc-comments, visibility modifiers, and guard
// annotations) are extracted verbatim and reordered — nothing is
// synthesised from IR type names. IR-based generation is used only as a
// fallback when the original text cannot be parsed into per-field chunks.

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
    // Detect tuple struct: all field names are `_N` (digit-only suffix)
    let is_tuple = optimal
        .iter()
        .all(|f| f.name.starts_with('_') && f.name[1..].chars().all(|c| c.is_ascii_digit()));
    if is_tuple {
        let types: Vec<String> = optimal
            .iter()
            .map(|f| field_type_name(f).to_string())
            .collect();
        return format!("struct {}({});\n", layout.name, types.join(", "));
    }
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

// ── source-aware field chunk extraction ───────────────────────────────────────
//
// Each language extracts "field chunks" — the verbatim source text for one
// field declaration, including any preceding doc comments and attributes.
// The list is keyed by field name so callers can look up chunks by the IR
// field names and reorder them.

/// Split a Rust struct body (the text between `{` and `}`, exclusive) into
/// field chunks, preserving attributes, doc comments, and visibility modifiers.
///
/// Returns `Vec<(field_name, raw_chunk_text)>` in declaration order.
/// The `raw_chunk_text` includes the field declaration line and its trailing
/// comma; attributes/doc-comments that appear immediately before the field
/// are included in that field's chunk.
///
/// Chunk boundaries are determined by `,` at bracket depth 0, matching how
/// Rust struct fields are separated. The `>` character is tracked conservatively:
/// if depth goes negative it is reset to 0 (handles `->` and comparison operators
/// in default expressions, though those are rare in struct bodies).
pub fn extract_rust_field_chunks(body: &str) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();
    let mut depth: i32 = 0; // tracks < ( [ nesting within a field declaration
    let mut chunk_start = 0usize;
    let bytes = body.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            // Line comments: skip to EOL (don't count brackets inside them)
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            // Block comments: skip to */
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            // String literals: skip to closing quote
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'<' | b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'>' | b')' | b']' => {
                depth = (depth - 1).max(0);
                i += 1;
            }
            // Curly braces (e.g. struct default field values, closure syntax):
            // just skip past them; they don't appear in normal struct bodies
            b'{' | b'}' => {
                i += 1;
            }
            b',' if depth == 0 => {
                i += 1; // include the comma in the chunk
                let chunk = &body[chunk_start..i];
                if let Some(name) = rust_field_name_from_chunk(chunk) {
                    result.push((name, chunk.to_string()));
                }
                chunk_start = i;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Handle last field (may not have trailing comma)
    let tail = body[chunk_start..].trim();
    if !tail.is_empty() {
        // Use the full slice (including leading whitespace) for the chunk
        let chunk = &body[chunk_start..];
        if let Some(name) = rust_field_name_from_chunk(chunk) {
            result.push((name, chunk.to_string()));
        }
    }

    result
}

/// Extract the field name from a Rust field chunk.
/// Handles leading attributes (`#[...]`), doc comments (`///`), and pub
/// visibility (`pub`, `pub(crate)`, `pub(super)`, `pub(in path::to::mod)`).
fn rust_field_name_from_chunk(chunk: &str) -> Option<String> {
    for line in chunk.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with("//") || s.starts_with("#[") || s.starts_with("#![") {
            continue;
        }
        return rust_field_name_from_decl_line(s);
    }
    None
}

/// Parse `[pub[(...)]] field_name: Type` and return the field name.
fn rust_field_name_from_decl_line(line: &str) -> Option<String> {
    let mut s = line.trim();

    // Strip visibility
    if let Some(rest) = s.strip_prefix("pub") {
        let rest = rest.trim_start();
        if rest.starts_with('(') {
            // pub(crate), pub(super), pub(in path) — find the closing ')'
            let end = rest.find(')')?;
            s = rest[end + 1..].trim_start();
        } else {
            s = rest;
        }
    }

    // The field name ends at the first ':' not followed by ':'
    let mut depth: i32 = 0;
    for (idx, c) in s.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = (depth - 1).max(0),
            ':' if depth == 0 => {
                // Make sure this ':' is the field separator, not '::'
                if s[idx + 1..].starts_with(':') {
                    continue; // qualified path
                }
                let name = s[..idx].trim().to_string();
                if !name.is_empty()
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                    && !name.starts_with(|c: char| c.is_ascii_digit())
                {
                    return Some(name);
                }
                return None;
            }
            _ => {}
        }
    }
    None
}

/// Split a C/C++ struct body (text between `{` and `}`, exclusive) into
/// field chunks separated by `;` at depth 0.
///
/// Each chunk includes any preceding `//` or `/* */` comments.
pub fn extract_c_field_chunks(body: &str) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();
    let mut depth: i32 = 0;
    let mut chunk_start = 0usize;
    let bytes = body.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'<' | b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b'>' | b')' | b']' | b'}' => {
                depth = (depth - 1).max(0);
                i += 1;
            }
            b';' if depth == 0 => {
                i += 1;
                let chunk = &body[chunk_start..i];
                if !chunk.trim().is_empty()
                    && let Some(name) = c_field_name_from_chunk(chunk)
                {
                    result.push((name, chunk.to_string()));
                }
                chunk_start = i;
            }
            _ => {
                i += 1;
            }
        }
    }
    result
}

/// Extract a C/C++ field name from a chunk (everything up to and including `;`).
/// The field name is the last identifier before the `;`, stripping pointer
/// declarators and array declarators.
fn c_field_name_from_chunk(chunk: &str) -> Option<String> {
    // Strip comments to get just the code text
    let code: String = chunk
        .lines()
        .filter(|l| !l.trim().starts_with("//"))
        .collect::<Vec<_>>()
        .join(" ");

    // Tokenise by whitespace and punctuation; look for the last identifier-like
    // token before `;`, skipping keywords and type names
    let stripped = code.trim_end_matches(';').trim();
    // Strip array declarator: `field[N]` → `field`
    let stripped = if let Some(bracket) = stripped.rfind('[') {
        stripped[..bracket].trim()
    } else {
        stripped
    };
    // Strip pointer declarators at the end
    let stripped = stripped
        .trim_start_matches('*')
        .trim_end_matches('*')
        .trim();

    // The last whitespace-separated token is the field name
    let last = stripped.split_whitespace().next_back()?;
    // Strip leading `*` (pointer declarator attached to name)
    let last = last.trim_start_matches('*').trim_end_matches('*');

    if last.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !last.is_empty()
        && !last.starts_with(|c: char| c.is_ascii_digit())
        && !is_c_keyword(last)
    {
        Some(last.to_string())
    } else {
        None
    }
}

fn is_c_keyword(s: &str) -> bool {
    matches!(
        s,
        "const"
            | "volatile"
            | "restrict"
            | "unsigned"
            | "signed"
            | "short"
            | "long"
            | "int"
            | "char"
            | "float"
            | "double"
            | "void"
            | "struct"
            | "union"
            | "enum"
            | "typedef"
            | "extern"
            | "static"
            | "inline"
            | "auto"
            | "register"
            | "bool"
            | "_Bool"
            | "uint8_t"
            | "uint16_t"
            | "uint32_t"
            | "uint64_t"
            | "int8_t"
            | "int16_t"
            | "int32_t"
            | "int64_t"
            | "size_t"
            | "ssize_t"
            | "ptrdiff_t"
            | "uintptr_t"
            | "intptr_t"
    )
}

/// Split a Go struct body (text between `{` and `}`, exclusive) into
/// field chunks, one per non-blank non-comment line.
pub fn extract_go_field_chunks(body: &str) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with("//") {
            continue;
        }
        if let Some(name) = go_field_name_from_line(s) {
            result.push((name, format!("{line}\n")));
        }
    }
    result
}

fn go_field_name_from_line(line: &str) -> Option<String> {
    // field_name[, field_name] Type [// comment]
    // OR: EmbeddedType [// comment]
    let code = if let Some(pos) = line.find("//") {
        line[..pos].trim()
    } else {
        line.trim()
    };
    let first = code.split_whitespace().next()?;
    let name = first.trim_end_matches(',');
    if name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !name.is_empty()
    {
        // Use unqualified name for qualified embedded types (e.g. sync.Mutex → Mutex)
        let simple = name.split('.').next_back().unwrap_or(name);
        Some(simple.to_string())
    } else {
        None
    }
}

/// Split a Zig struct body (text between `{` and `}`, exclusive) into
/// field chunks separated by `,` at depth 0.
pub fn extract_zig_field_chunks(body: &str) -> Vec<(String, String)> {
    // Zig field declarations end with `,` — same tokenisation as Rust
    let mut result: Vec<(String, String)> = Vec::new();
    let mut depth: i32 = 0;
    let mut chunk_start = 0usize;
    let bytes = body.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'<' | b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'>' | b')' | b']' => {
                depth = (depth - 1).max(0);
                i += 1;
            }
            b'{' | b'}' => {
                i += 1;
            }
            b',' if depth == 0 => {
                i += 1;
                let chunk = &body[chunk_start..i];
                if let Some(name) = zig_field_name_from_chunk(chunk) {
                    result.push((name, chunk.to_string()));
                }
                chunk_start = i;
            }
            _ => {
                i += 1;
            }
        }
    }
    let tail = body[chunk_start..].trim();
    if !tail.is_empty() {
        let chunk = &body[chunk_start..];
        if let Some(name) = zig_field_name_from_chunk(chunk) {
            result.push((name, chunk.to_string()));
        }
    }
    result
}

fn zig_field_name_from_chunk(chunk: &str) -> Option<String> {
    for line in chunk.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with("//") {
            continue;
        }
        // field_name: Type
        let colon = s.find(':')?;
        let name = s[..colon].trim().to_string();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(name);
        }
        return None;
    }
    None
}

// ── source-aware fix generators ───────────────────────────────────────────────
//
// These functions generate reordered struct source by extracting the original
// field chunks and reordering them rather than synthesising from IR type names.
// They fall back to the IR-based generators when chunk extraction fails.

/// Generate a source-preserving Rust fix: reorder field chunks extracted from
/// `struct_source` (the original `struct Name { ... }` text) according to the
/// optimal field order.
///
/// Preserves `#[serde(...)]`, `pub`, `pub(crate)`, doc comments (`///`), and
/// any other leading attribute/comment lines verbatim.
pub fn generate_rust_fix_from_source(layout: &StructLayout, struct_source: &str) -> String {
    if let Some(result) = try_source_aware_rust(layout, struct_source) {
        return result;
    }
    generate_rust_fix(layout)
}

fn try_source_aware_rust(layout: &StructLayout, struct_source: &str) -> Option<String> {
    // Detect tuple struct: `struct Name(...)` — body delimited by parens not braces.
    let is_tuple = layout
        .fields
        .iter()
        .all(|f| f.name.starts_with('_') && f.name[1..].chars().all(|c| c.is_ascii_digit()));

    if is_tuple {
        return try_source_aware_rust_tuple(layout, struct_source);
    }

    let brace_open = struct_source.find('{')?;
    // Find the matching close brace using match_braces
    let body_with_close = &struct_source[brace_open..];
    let body_len = match_braces(body_with_close)?;
    let body = &body_with_close[1..body_len - 1]; // between { and }

    let chunks = extract_rust_field_chunks(body);
    if chunks.is_empty() {
        return None;
    }

    let chunk_map: std::collections::HashMap<&str, &str> = chunks
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_str()))
        .collect();

    let optimal = optimal_order(layout);
    // Verify all optimal fields have chunks; if any is missing, fall back
    if optimal
        .iter()
        .any(|f| !chunk_map.contains_key(f.name.as_str()))
    {
        return None;
    }

    let header = &struct_source[..=brace_open];
    let mut result = header.to_string();
    if !body.starts_with('\n') {
        result.push('\n');
    }
    for field in &optimal {
        result.push_str(chunk_map[field.name.as_str()]);
    }
    // Ensure there's a newline before the closing brace
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push('}');
    // Preserve anything after the closing brace (e.g. impl blocks on next lines)
    let after = &struct_source[brace_open + body_len..];
    result.push_str(after);
    Some(result)
}

/// Source-aware fix for tuple structs: `struct Name(T0, T1, ...);`
/// Field names are `_0`, `_1`, … matching the IR names.
fn try_source_aware_rust_tuple(layout: &StructLayout, struct_source: &str) -> Option<String> {
    let paren_open = struct_source.find('(')?;
    let body_with_close = &struct_source[paren_open..];
    // Find matching closing paren
    let paren_len = match_parens(body_with_close)?;
    let body = &body_with_close[1..paren_len - 1]; // between ( and )

    // Split by `,` at depth 0 to get individual type chunks (in order)
    let type_chunks = extract_tuple_type_chunks(body);
    if type_chunks.is_empty() {
        return None;
    }

    // The original chunks are in declaration order: chunk[0] → `_0`, etc.
    // Build a map from index-name to the type chunk text.
    let chunk_map: std::collections::HashMap<String, &str> = type_chunks
        .iter()
        .enumerate()
        .map(|(i, c)| (format!("_{i}"), c.as_str()))
        .collect();

    let optimal = optimal_order(layout);
    if optimal.iter().any(|f| !chunk_map.contains_key(&f.name)) {
        return None;
    }

    // Reconstruct: preserve header up to and including `(`
    let header = &struct_source[..=paren_open];
    let mut result = header.to_string();
    let reordered: Vec<&str> = optimal.iter().map(|f| chunk_map[&f.name]).collect();
    result.push_str(&reordered.join(", "));
    result.push(')');
    // Preserve trailing `;` and anything after
    let after = &struct_source[paren_open + paren_len..];
    result.push_str(after);
    Some(result)
}

/// Split a tuple struct body (text between `(` and `)`) by `,` at depth 0.
fn extract_tuple_type_chunks(body: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut depth: i32 = 0;
    let mut chunk_start = 0usize;
    let bytes = body.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'<' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'>' | b']' => {
                depth = (depth - 1).max(0);
                i += 1;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = (depth - 1).max(0);
                i += 1;
            }
            b',' if depth == 0 => {
                let chunk = body[chunk_start..i].trim().to_string();
                if !chunk.is_empty() {
                    result.push(chunk);
                }
                i += 1;
                chunk_start = i;
            }
            _ => {
                i += 1;
            }
        }
    }
    let tail = body[chunk_start..].trim().to_string();
    if !tail.is_empty() {
        result.push(tail);
    }
    result
}

/// Find the matching `)` from the start of `s` (which must begin with `(`).
/// Returns byte index one past the closing `)`.
fn match_parens(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
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

/// Generate a source-preserving C/C++ fix.
pub fn generate_c_fix_from_source(layout: &StructLayout, struct_source: &str) -> String {
    if let Some(result) = try_source_aware_c(layout, struct_source) {
        return result;
    }
    generate_c_fix(layout)
}

fn try_source_aware_c(layout: &StructLayout, struct_source: &str) -> Option<String> {
    let brace_open = struct_source.find('{')?;
    let body_with_close = &struct_source[brace_open..];
    let body_len = match_braces(body_with_close)?;
    let body = &body_with_close[1..body_len - 1];

    let chunks = extract_c_field_chunks(body);
    if chunks.is_empty() {
        return None;
    }

    let chunk_map: std::collections::HashMap<&str, &str> = chunks
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_str()))
        .collect();

    let optimal = optimal_order(layout);
    if optimal
        .iter()
        .any(|f| !chunk_map.contains_key(f.name.as_str()))
    {
        return None;
    }

    let header = &struct_source[..=brace_open];
    let mut result = header.to_string();
    if !body.starts_with('\n') {
        result.push('\n');
    }
    for field in &optimal {
        result.push_str(chunk_map[field.name.as_str()]);
    }
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push('}');
    let close_end = brace_open + body_len;
    let after = &struct_source[close_end..];
    result.push_str(after);
    Some(result)
}

/// Generate a source-preserving Go fix.
pub fn generate_go_fix_from_source(layout: &StructLayout, struct_source: &str) -> String {
    if let Some(result) = try_source_aware_go(layout, struct_source) {
        return result;
    }
    generate_go_fix(layout)
}

fn try_source_aware_go(layout: &StructLayout, struct_source: &str) -> Option<String> {
    let brace_open = struct_source.find('{')?;
    let body_with_close = &struct_source[brace_open..];
    let body_len = match_braces(body_with_close)?;
    let body = &body_with_close[1..body_len - 1];

    let chunks = extract_go_field_chunks(body);
    if chunks.is_empty() {
        return None;
    }

    let chunk_map: std::collections::HashMap<&str, &str> = chunks
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_str()))
        .collect();

    let optimal = optimal_order(layout);
    if optimal
        .iter()
        .any(|f| !chunk_map.contains_key(f.name.as_str()))
    {
        return None;
    }

    let header = &struct_source[..=brace_open];
    let mut result = header.to_string();
    if !body.starts_with('\n') {
        result.push('\n');
    }
    for field in &optimal {
        result.push_str(chunk_map[field.name.as_str()]);
    }
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push('}');
    let close_end = brace_open + body_len;
    let after = &struct_source[close_end..];
    result.push_str(after);
    Some(result)
}

/// Generate a source-preserving Zig fix.
pub fn generate_zig_fix_from_source(layout: &StructLayout, struct_source: &str) -> String {
    if let Some(result) = try_source_aware_zig(layout, struct_source) {
        return result;
    }
    generate_zig_fix(layout)
}

fn try_source_aware_zig(layout: &StructLayout, struct_source: &str) -> Option<String> {
    let brace_open = struct_source.find('{')?;
    let body_with_close = &struct_source[brace_open..];
    let body_len = match_braces(body_with_close)?;
    let body = &body_with_close[1..body_len - 1];

    let chunks = extract_zig_field_chunks(body);
    if chunks.is_empty() {
        return None;
    }

    let chunk_map: std::collections::HashMap<&str, &str> = chunks
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_str()))
        .collect();

    let optimal = optimal_order(layout);
    if optimal
        .iter()
        .any(|f| !chunk_map.contains_key(f.name.as_str()))
    {
        return None;
    }

    let header = &struct_source[..=brace_open];
    let mut result = header.to_string();
    if !body.starts_with('\n') {
        result.push('\n');
    }
    for field in &optimal {
        result.push_str(chunk_map[field.name.as_str()]);
    }
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push('}');
    let close_end = brace_open + body_len;
    let after = &struct_source[close_end..];
    result.push_str(after);
    Some(result)
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
                        && let Some(body_len) = match_braces(&source[brace_start..])
                    {
                        let end = consume_semicolon(source, brace_start + body_len);
                        return Some(start..end);
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
        ) && let Some(brace_rel) = source[after_name..].find('{')
        {
            let brace_start = after_name + brace_rel;
            if source[after_name..brace_start]
                .chars()
                .all(|c| c.is_whitespace())
                && let Some(body_len) = match_braces(&source[brace_start..])
            {
                // Rust structs have no trailing `;` (unit structs do, but we skip those)
                return Some(start..brace_start + body_len);
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
                && let Some(body_len) = match_braces(&source[brace_start..])
            {
                return Some(start..brace_start + body_len);
            }
        }
        search_from = start + 1;
    }
    None
}

// ── in-place rewriters ────────────────────────────────────────────────────────

/// Apply C/C++ struct reorderings in-place, returning the modified source.
/// Each layout in `layouts` is looked up by name; matched structs are replaced
/// with the optimally-ordered definition. Field declarations (including comments
/// and annotations such as `GUARDED_BY`) are preserved verbatim from the original
/// source when possible; IR-based generation is used as a fallback.
/// Replacements are applied back-to-front so byte offsets remain valid.
pub fn apply_fixes_c(source: &str, layouts: &[&StructLayout]) -> String {
    apply_fixes_with_source(
        source,
        layouts,
        find_c_struct_span,
        generate_c_fix_from_source,
    )
}

/// Apply Rust struct reorderings in-place, returning the modified source.
/// Preserves `pub`, `pub(crate)`, `#[serde(...)]`, `/// doc-comments`, and other
/// attributes verbatim; falls back to IR-based generation when source cannot be parsed.
pub fn apply_fixes_rust(source: &str, layouts: &[&StructLayout]) -> String {
    apply_fixes_with_source(
        source,
        layouts,
        find_rust_struct_span,
        generate_rust_fix_from_source,
    )
}

/// Apply Go struct reorderings in-place, returning the modified source.
/// Preserves field tags and comments verbatim; falls back to IR-based generation.
pub fn apply_fixes_go(source: &str, layouts: &[&StructLayout]) -> String {
    apply_fixes_with_source(
        source,
        layouts,
        find_go_struct_span,
        generate_go_fix_from_source,
    )
}

/// Render a reordered Zig struct definition as source text.
/// Zig structs are declared as `const Name = struct { ... };`.
/// If the layout is packed, the output uses `packed struct`.
pub fn generate_zig_fix(layout: &StructLayout) -> String {
    let optimal = optimal_order(layout);
    let qualifier = if layout.is_packed { "packed " } else { "" };
    let mut out = format!("const {} = {}struct {{\n", layout.name, qualifier);
    for field in &optimal {
        let ty = field_type_name(field);
        out.push_str(&format!("    {}: {ty},\n", field.name));
    }
    out.push_str("};\n");
    out
}

/// Find the byte range of a named Zig struct in source.
/// Matches `const Name = [packed|extern ]struct { ... };`.
pub fn find_zig_struct_span(source: &str, struct_name: &str) -> Option<std::ops::Range<usize>> {
    // Match `const Name =` (with optional whitespace variations)
    let needle = format!("const {struct_name}");
    let mut search_from = 0usize;
    while let Some(rel) = source[search_from..].find(&needle) {
        let start = search_from + rel;
        let after_name = start + needle.len();
        // Must be followed by whitespace then `=`
        let rest = source[after_name..].trim_start();
        if !rest.starts_with('=') {
            search_from = start + 1;
            continue;
        }
        // Find `struct` keyword after `=`
        let after_eq = after_name + source[after_name..].find('=')? + 1;
        let after_eq_rest = &source[after_eq..];
        // Skip optional `packed` or `extern` modifiers
        if let Some(struct_rel) = after_eq_rest.find("struct") {
            // Check no non-whitespace/identifier characters between = and struct
            // (beyond optional packed/extern modifiers)
            let prefix = &after_eq_rest[..struct_rel];
            let prefix_clean = prefix.trim();
            if prefix_clean.is_empty() || prefix_clean == "packed" || prefix_clean == "extern" {
                let struct_kw_end = after_eq + struct_rel + "struct".len();
                if let Some(brace_rel) = source[struct_kw_end..].find('{') {
                    let brace_start = struct_kw_end + brace_rel;
                    if source[struct_kw_end..brace_start]
                        .chars()
                        .all(|c| c.is_whitespace())
                        && let Some(body_len) = match_braces(&source[brace_start..])
                    {
                        let end = consume_semicolon(source, brace_start + body_len);
                        return Some(start..end);
                    }
                }
            }
        }
        search_from = start + 1;
    }
    None
}

/// Apply Zig struct reorderings in-place, returning the modified source.
/// Preserves field comments and annotations verbatim; falls back to IR-based generation.
pub fn apply_fixes_zig(source: &str, layouts: &[&StructLayout]) -> String {
    apply_fixes_with_source(
        source,
        layouts,
        find_zig_struct_span,
        generate_zig_fix_from_source,
    )
}

/// Source-aware variant of `apply_fixes`: passes the original struct source text
/// (extracted from the span) to the generator, enabling verbatim field preservation.
fn apply_fixes_with_source(
    source: &str,
    layouts: &[&StructLayout],
    find_span: fn(&str, &str) -> Option<std::ops::Range<usize>>,
    generate: fn(&StructLayout, &str) -> String,
) -> String {
    // Collect (start, end, replacement) for each matching layout
    let mut replacements: Vec<(usize, usize, String)> = layouts
        .iter()
        .filter_map(|layout| {
            let span = find_span(source, &layout.name)?;
            let struct_source = &source[span.clone()];
            let fixed = generate(layout, struct_source);
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

    #[test]
    fn zig_fix_uses_const_struct_syntax() {
        let out = generate_zig_fix(&connection_layout());
        assert!(out.starts_with("const Connection = struct {"));
        assert!(out.ends_with("};\n"));
    }

    #[test]
    fn find_zig_struct_span_basic() {
        let src = "const S = struct {\n    x: u32,\n    y: u8,\n};\n";
        let span = find_zig_struct_span(src, "S").unwrap();
        assert!(src[span].starts_with("const S = struct"));
    }

    #[test]
    fn find_zig_struct_span_packed() {
        let src = "const S = packed struct {\n    x: u32,\n    y: u8,\n};\n";
        let span = find_zig_struct_span(src, "S").unwrap();
        assert!(src[span].contains("packed struct"));
    }

    #[test]
    fn find_zig_struct_span_missing_returns_none() {
        let src = "const Other = struct { x: u8 };\n";
        assert!(find_zig_struct_span(src, "Missing").is_none());
    }

    #[test]
    fn apply_fixes_zig_reorders_in_place() {
        use crate::parse_source_str;
        use padlock_core::arch::X86_64_SYSV;
        let src = "const S = struct {\n    a: u8,\n    b: u64,\n};\n";
        let layouts = parse_source_str(src, &crate::SourceLanguage::Zig, &X86_64_SYSV).unwrap();
        let layout = &layouts[0];
        let fixed = apply_fixes_zig(src, &[layout]);
        // b (u64, align 8) should come before a (u8)
        let b_pos = fixed.find("b:").unwrap();
        let a_pos = fixed.find("a:").unwrap();
        assert!(
            b_pos < a_pos,
            "u64 field should come before u8 after reorder"
        );
    }

    // ── fix quality: source-aware preservation ────────────────────────────────

    #[test]
    fn rust_fix_preserves_pub_visibility() {
        let src = "struct S {\n    pub a: u8,\n    pub b: u64,\n}\n";
        use crate::parse_source_str;
        use padlock_core::arch::X86_64_SYSV;
        let layouts = parse_source_str(src, &crate::SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let fixed = apply_fixes_rust(src, &[&layouts[0]]);
        // pub keyword must appear before both fields
        assert!(fixed.contains("pub b: u64"), "pub on b must be preserved");
        assert!(fixed.contains("pub a: u8"), "pub on a must be preserved");
        // b (u64, align 8) should appear before a (u8, align 1)
        assert!(fixed.find("pub b").unwrap() < fixed.find("pub a").unwrap());
    }

    #[test]
    fn rust_fix_preserves_doc_comments() {
        let src = concat!(
            "struct S {\n",
            "    /// small field\n",
            "    a: u8,\n",
            "    /// large field\n",
            "    b: u64,\n",
            "}\n"
        );
        use crate::parse_source_str;
        use padlock_core::arch::X86_64_SYSV;
        let layouts = parse_source_str(src, &crate::SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let fixed = apply_fixes_rust(src, &[&layouts[0]]);
        assert!(
            fixed.contains("/// large field"),
            "doc comment for b must survive"
        );
        assert!(
            fixed.contains("/// small field"),
            "doc comment for a must survive"
        );
        // The doc comment for b must appear before the doc comment for a
        assert!(
            fixed.find("large field").unwrap() < fixed.find("small field").unwrap(),
            "doc comment ordering must follow field ordering"
        );
    }

    #[test]
    fn rust_fix_preserves_serde_attributes() {
        let src = concat!(
            "struct S {\n",
            "    #[serde(skip)]\n",
            "    a: u8,\n",
            "    #[serde(rename = \"big\")]\n",
            "    b: u64,\n",
            "}\n"
        );
        use crate::parse_source_str;
        use padlock_core::arch::X86_64_SYSV;
        let layouts = parse_source_str(src, &crate::SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let fixed = apply_fixes_rust(src, &[&layouts[0]]);
        assert!(
            fixed.contains("#[serde(skip)]"),
            "serde attribute on a must survive"
        );
        assert!(
            fixed.contains("#[serde(rename = \"big\")]"),
            "serde attribute on b must survive"
        );
    }

    #[test]
    fn rust_fix_preserves_pub_crate_visibility() {
        let src = "struct S {\n    pub(crate) a: u8,\n    pub(crate) b: u64,\n}\n";
        use crate::parse_source_str;
        use padlock_core::arch::X86_64_SYSV;
        let layouts = parse_source_str(src, &crate::SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let fixed = apply_fixes_rust(src, &[&layouts[0]]);
        assert!(
            fixed.contains("pub(crate) b: u64"),
            "pub(crate) on b must be preserved"
        );
        assert!(
            fixed.contains("pub(crate) a: u8"),
            "pub(crate) on a must be preserved"
        );
    }

    #[test]
    fn c_fix_preserves_guarded_by_comments() {
        let src = concat!(
            "struct S {\n",
            "    char a; // GUARDED_BY(mu)\n",
            "    double b; // large field\n",
            "};\n"
        );
        use crate::parse_source_str;
        use padlock_core::arch::X86_64_SYSV;
        let layouts = parse_source_str(src, &crate::SourceLanguage::C, &X86_64_SYSV).unwrap();
        let fixed = apply_fixes_c(src, &[&layouts[0]]);
        assert!(
            fixed.contains("GUARDED_BY(mu)"),
            "guard annotation comment must survive reorder"
        );
        // double should come before char
        assert!(fixed.find("double b").unwrap() < fixed.find("char a").unwrap());
    }

    #[test]
    fn go_fix_preserves_field_tags() {
        let src = concat!("type S struct {\n", "\ta uint8\n", "\tb uint64\n", "}\n");
        use crate::parse_source_str;
        use padlock_core::arch::X86_64_SYSV;
        let layouts = parse_source_str(src, &crate::SourceLanguage::Go, &X86_64_SYSV).unwrap();
        let fixed = apply_fixes_go(src, &[&layouts[0]]);
        // b (8 bytes) should appear before a (1 byte)
        assert!(fixed.find("\tb uint64").unwrap() < fixed.find("\ta uint8").unwrap());
    }

    #[test]
    fn zig_fix_preserves_field_comments() {
        let src = concat!(
            "const S = struct {\n",
            "    // small\n",
            "    a: u8,\n",
            "    // large\n",
            "    b: u64,\n",
            "};\n"
        );
        use crate::parse_source_str;
        use padlock_core::arch::X86_64_SYSV;
        let layouts = parse_source_str(src, &crate::SourceLanguage::Zig, &X86_64_SYSV).unwrap();
        let fixed = apply_fixes_zig(src, &[&layouts[0]]);
        assert!(fixed.contains("// large"), "comment for b must survive");
        assert!(fixed.contains("// small"), "comment for a must survive");
        // b should appear first
        assert!(fixed.find("// large").unwrap() < fixed.find("// small").unwrap());
    }

    // ── bad weather: fix quality fallback ─────────────────────────────────────

    #[test]
    fn rust_fix_from_source_falls_back_when_no_open_brace() {
        // Struct source that is malformed (no `{`): must not panic, falls back to IR
        let layout = connection_layout();
        let out = generate_rust_fix_from_source(&layout, "struct Connection");
        // IR fallback produces valid Rust syntax
        assert!(out.starts_with("struct Connection {"));
    }

    #[test]
    fn c_fix_from_source_falls_back_when_chunks_empty() {
        // Body with no parseable fields — chunk extraction returns empty vec,
        // triggering IR fallback
        let layout = connection_layout();
        let out = generate_c_fix_from_source(&layout, "struct Connection { /* no fields */ };");
        assert!(out.starts_with("struct Connection {"));
        assert!(out.contains("timeout"));
    }

    #[test]
    fn zig_fix_from_source_falls_back_on_missing_field_name() {
        // IR field names don't match chunk names → fallback to IR
        let layout = connection_layout();
        let out =
            generate_zig_fix_from_source(&layout, "const Connection = struct { x: u8, y: u64, };");
        // IR fallback must still produce all fields from the layout
        assert!(out.contains("timeout"));
    }

    // ── Go fix tests ─────────────────────────────────────────────────────────

    #[test]
    fn go_fix_reorders_fields() {
        let layout = connection_layout();
        let out = generate_go_fix(&layout);
        // timeout (align 8) must come before bools (align 1)
        let pos_timeout = out.find("timeout").unwrap();
        let pos_port = out.find("port").unwrap();
        let pos_bool = out.find("is_active").unwrap();
        assert!(pos_timeout < pos_bool, "timeout must precede booleans");
        assert!(pos_port < pos_bool, "port must precede booleans");
    }

    #[test]
    fn go_fix_from_source_preserves_verbatim_field_lines() {
        let layout = connection_layout();
        let src = r#"type Connection struct {
	is_active bool
	timeout   f64
	is_tls    bool
	port      i32
}"#;
        let out = generate_go_fix_from_source(&layout, src);
        // The reordered output must contain the exact verbatim field lines
        assert!(out.contains("timeout   f64"), "verbatim timeout line");
        assert!(out.contains("port      i32"), "verbatim port line");
        // timeout must appear before is_active in the output
        let pos_timeout = out.find("timeout").unwrap();
        let pos_is_active = out.find("is_active").unwrap();
        assert!(
            pos_timeout < pos_is_active,
            "timeout must come before is_active"
        );
    }

    #[test]
    fn apply_fixes_go_rewrites_struct_in_file() {
        let src = "package p\n\ntype Point struct {\n\tFlag bool\n\tX    int64\n\tY    int32\n}\n";
        // Build a minimal layout: Flag(bool,1), X(i64,8), Y(i32,4)
        // optimal: X(8) → Y(4) → Flag(1)
        use padlock_core::arch::X86_64_SYSV;
        use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
        let layout = StructLayout {
            name: "Point".into(),
            total_size: 16,
            align: 8,
            fields: vec![
                Field {
                    name: "Flag".into(),
                    ty: TypeInfo::Primitive {
                        name: "bool".into(),
                        size: 1,
                        align: 1,
                    },
                    offset: 0,
                    size: 1,
                    align: 1,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "X".into(),
                    ty: TypeInfo::Primitive {
                        name: "int64".into(),
                        size: 8,
                        align: 8,
                    },
                    offset: 8,
                    size: 8,
                    align: 8,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "Y".into(),
                    ty: TypeInfo::Primitive {
                        name: "int32".into(),
                        size: 4,
                        align: 4,
                    },
                    offset: 16,
                    size: 4,
                    align: 4,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
            ],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
            is_repr_rust: false,
            suppressed_findings: vec![],
            uncertain_fields: Vec::new(),
        };
        let fixed = apply_fixes_go(src, &[&layout]);
        // X (int64, align 8) must appear before Flag (bool, align 1)
        let pos_x = fixed.find("\tX ").unwrap();
        let pos_flag = fixed.find("\tFlag").unwrap();
        assert!(pos_x < pos_flag, "X must precede Flag after reorder");
        // Package declaration must be preserved
        assert!(fixed.starts_with("package p\n"), "package line preserved");
    }
}
