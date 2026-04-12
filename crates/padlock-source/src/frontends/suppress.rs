// padlock-source/src/frontends/suppress.rs
//
// Shared helpers for parsing per-finding suppression annotations from source.
//
// All supported languages accept a comment immediately before the struct/type
// declaration:
//
//   // padlock: ignore[ReorderSuggestion]
//   // padlock: ignore[PaddingWaste, FalseSharing]
//
// The comment can appear on the line(s) directly preceding the declaration;
// blank lines between the comment and the struct are skipped.
//
// Valid finding kind names:
//   PaddingWaste, ReorderSuggestion, FalseSharing, LocalityIssue

/// Parse `padlock: ignore[Kind1, Kind2]` from a single comment line.
/// The input may be a whole line (including leading `//` or `/*`) or
/// just the content of a doc attribute string (Rust `#[doc = "..."]`).
///
/// Returns the list of finding kind names, or an empty vec if the line does
/// not contain a suppression directive.
pub fn extract_suppressed_kinds(line: &str) -> Vec<String> {
    // Strip leading comment markers and whitespace
    let body = line
        .trim()
        .trim_start_matches("///")
        .trim_start_matches("//")
        .trim_start_matches("/*")
        .trim_start_matches('*')
        .trim();

    // Accept both `padlock:ignore[...]` and `padlock: ignore[...]`
    let rest = if let Some(r) = body.strip_prefix("padlock:") {
        r.trim_start()
    } else {
        return Vec::new();
    };

    let rest = if let Some(r) = rest.strip_prefix("ignore[") {
        r
    } else {
        return Vec::new();
    };

    if let Some(end) = rest.find(']') {
        rest[..end]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    }
}

/// Scan the source text *before* `node_start_byte` for suppression directives.
///
/// Walks backward through lines immediately preceding the struct declaration
/// (skipping blank lines), stops as soon as a non-comment, non-blank line is seen.
/// Returns all suppressed finding kinds found.
pub fn suppressed_from_preceding_source(source: &str, node_start_byte: usize) -> Vec<String> {
    let before = &source[..node_start_byte.min(source.len())];
    let mut result = Vec::new();
    let mut found_any_comment = false;
    for line in before.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue; // blank lines don't break the scan
        }
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            let kinds = extract_suppressed_kinds(trimmed);
            if !kinds.is_empty() {
                result.extend(kinds);
                found_any_comment = true;
                continue;
            }
            // A comment that is NOT a suppression directive stops the scan —
            // it's a doc comment or other comment, not related to this struct.
            break;
        }
        break; // non-comment, non-blank — stop
    }
    let _ = found_any_comment;
    result
}

/// Variant for parsers (like the Rust `syn` frontend) that know the 1-indexed
/// line number of the struct rather than its byte offset.
///
/// Scans the comment line(s) immediately *before* `struct_line` (skipping blank
/// lines) for `// padlock: ignore[...]` directives.
pub fn suppressed_from_source_line(source: &str, struct_line: u32) -> Vec<String> {
    if struct_line == 0 {
        return Vec::new();
    }
    // Compute the byte offset of the start of `struct_line` (1-indexed).
    let mut line_start = 0usize;
    for (i, line) in source.lines().enumerate() {
        if i + 1 == struct_line as usize {
            break;
        }
        line_start += line.len() + 1; // +1 for the '\n'
    }
    suppressed_from_preceding_source(source, line_start)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_kind() {
        let kinds = extract_suppressed_kinds("// padlock: ignore[ReorderSuggestion]");
        assert_eq!(kinds, vec!["ReorderSuggestion"]);
    }

    #[test]
    fn parses_multiple_kinds() {
        let kinds = extract_suppressed_kinds("// padlock: ignore[PaddingWaste, FalseSharing]");
        assert_eq!(kinds, vec!["PaddingWaste", "FalseSharing"]);
    }

    #[test]
    fn parses_no_space_variant() {
        let kinds = extract_suppressed_kinds("// padlock:ignore[LocalityIssue]");
        assert_eq!(kinds, vec!["LocalityIssue"]);
    }

    #[test]
    fn returns_empty_for_unrelated_comment() {
        assert!(extract_suppressed_kinds("// some other comment").is_empty());
        assert!(extract_suppressed_kinds("// padlock:guard=mu").is_empty());
        assert!(extract_suppressed_kinds("// padlock:ignore").is_empty()); // no brackets
    }

    #[test]
    fn parses_doc_comment_style() {
        let kinds = extract_suppressed_kinds("/// padlock: ignore[FalseSharing]");
        assert_eq!(kinds, vec!["FalseSharing"]);
    }

    #[test]
    fn preceding_source_finds_immediately_before() {
        let source =
            "struct Other { int x; };\n// padlock: ignore[ReorderSuggestion]\nstruct Foo {";
        let byte = source.find("struct Foo").unwrap();
        let kinds = suppressed_from_preceding_source(source, byte);
        assert_eq!(kinds, vec!["ReorderSuggestion"]);
    }

    #[test]
    fn preceding_source_skips_blank_lines() {
        let source = "// padlock: ignore[FalseSharing]\n\nstruct Foo {";
        let byte = source.find("struct Foo").unwrap();
        let kinds = suppressed_from_preceding_source(source, byte);
        assert_eq!(kinds, vec!["FalseSharing"]);
    }

    #[test]
    fn preceding_source_stops_at_non_suppress_comment() {
        // A doc comment between the suppress directive and the struct breaks the scan
        let source = "// padlock: ignore[ReorderSuggestion]\n// Some other doc\nstruct Foo {";
        let byte = source.find("struct Foo").unwrap();
        let kinds = suppressed_from_preceding_source(source, byte);
        // The "Some other doc" comment is between the directive and the struct → scan stops
        assert!(kinds.is_empty());
    }

    #[test]
    fn preceding_source_returns_empty_when_no_directive() {
        let source = "struct Bar { int x; };\nstruct Foo {";
        let byte = source.find("struct Foo").unwrap();
        let kinds = suppressed_from_preceding_source(source, byte);
        assert!(kinds.is_empty());
    }
}
