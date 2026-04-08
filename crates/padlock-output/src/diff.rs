// padlock-output/src/diff.rs

use padlock_core::ir::{StructLayout, optimal_order};
use similar::{ChangeTag, TextDiff};

/// Render a unified diff of a struct's current field order vs the optimal order.
pub fn render_diff(layout: &StructLayout) -> String {
    let original = fields_to_text(layout.fields.iter().map(|f| f.name.as_str()));
    let optimal = optimal_order(layout);
    let optimized = fields_to_text(optimal.iter().map(|f| f.name.as_str()));
    text_diff(&original, &optimized)
}

/// Render a unified diff between two arbitrary text blocks.
pub fn text_diff(original: &str, updated: &str) -> String {
    if original == updated {
        return String::from("(no changes)\n");
    }

    let diff = TextDiff::from_lines(original, updated);
    let mut out = String::new();

    for change in diff.iter_all_changes() {
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
    out
}

fn fields_to_text<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.map(|n| format!("{n}\n")).collect()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::ir::test_fixtures::{connection_layout, packed_layout};

    #[test]
    fn diff_misaligned_is_nonempty() {
        let out = render_diff(&connection_layout());
        // Connection is not in optimal order so a diff should exist
        assert_ne!(out, "(no changes)\n");
        assert!(out.contains("timeout") || out.contains("port"));
    }

    #[test]
    fn diff_already_optimal_is_no_changes() {
        // packed_layout has fields in optimal order (i32 > i16 > i16)
        let out = render_diff(&packed_layout());
        assert_eq!(out, "(no changes)\n");
    }

    #[test]
    fn text_diff_shows_plus_minus() {
        let out = text_diff("a\nb\n", "a\nc\n");
        assert!(out.contains("- b") || out.contains("-b"));
        assert!(out.contains("+ c") || out.contains("+c"));
    }

    #[test]
    fn text_diff_identical_is_no_changes() {
        assert_eq!(text_diff("x\n", "x\n"), "(no changes)\n");
    }
}
