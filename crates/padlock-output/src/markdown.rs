// padlock-output/src/markdown.rs
//
// Renders a padlock Report as GitHub-Flavored Markdown.
// Suitable for CI step summaries, PR comment bots, and generated reports.

use padlock_core::findings::{Finding, Report, Severity, StructReport};

/// Render a full report as a GitHub-Flavored Markdown string.
pub fn to_markdown(report: &Report) -> String {
    let mut out = String::new();

    let struct_word = if report.total_structs == 1 {
        "struct"
    } else {
        "structs"
    };
    if report.total_wasted_bytes > 0 {
        out.push_str(&format!(
            "# padlock: {} {}, {} bytes wasted\n\n",
            report.total_structs, struct_word, report.total_wasted_bytes
        ));
    } else {
        out.push_str(&format!(
            "# padlock: {} {} — no padding waste\n\n",
            report.total_structs, struct_word
        ));
    }

    if report.structs.is_empty() {
        out.push_str("No structs analyzed.\n");
        return out;
    }

    for sr in &report.structs {
        out.push_str(&render_struct_md(sr));
        out.push('\n');
    }

    out
}

fn render_struct_md(sr: &StructReport) -> String {
    let mut out = String::new();

    let score_icon = match sr.score as u32 {
        90..=100 => "✅",
        60..=89 => "⚠️",
        _ => "❌",
    };

    let location = match (&sr.source_file, sr.source_line) {
        (Some(f), Some(l)) => format!(" `{}:{}`", f, l),
        (Some(f), None) => format!(" `{}`", f),
        _ => String::new(),
    };

    out.push_str(&format!(
        "## {} `{}`{} — {}B, score {:.0}\n\n",
        score_icon, sr.struct_name, location, sr.total_size, sr.score
    ));

    if sr.findings.is_empty() {
        out.push_str("No issues found.\n");
        return out;
    }

    out.push_str("| Severity | Finding |\n");
    out.push_str("|----------|--------|\n");
    for finding in &sr.findings {
        let sev = match finding.severity() {
            Severity::High => "🔴 High",
            Severity::Medium => "🟡 Medium",
            Severity::Low => "🔵 Low",
        };
        out.push_str(&format!("| {} | {} |\n", sev, render_finding_md(finding)));
    }

    out
}

fn render_finding_md(f: &Finding) -> String {
    match f {
        Finding::PaddingWaste {
            wasted_bytes,
            waste_pct,
            gaps,
            ..
        } => {
            let gap_detail: Vec<String> = gaps
                .iter()
                .take(3)
                .map(|g| {
                    format!(
                        "{}B after `{}` (offset {})",
                        g.bytes, g.after_field, g.at_offset
                    )
                })
                .collect();
            let detail = if gaps.len() > 3 {
                format!("{} … and {} more", gap_detail.join(", "), gaps.len() - 3)
            } else {
                gap_detail.join(", ")
            };
            format!("Padding waste: {wasted_bytes}B ({waste_pct:.0}%) — {detail}")
        }
        Finding::ReorderSuggestion {
            savings,
            original_size,
            optimized_size,
            suggested_order,
            severity,
            ..
        } => {
            let base = format!(
                "Reorder fields: {original_size}B → {optimized_size}B (saves {savings}B): `{}`",
                suggested_order.join(", ")
            );
            if *severity == Severity::High {
                format!("{base} (~{savings} MB/1M instances)")
            } else {
                base
            }
        }
        Finding::FalseSharing {
            conflicts,
            is_inferred,
            ..
        } => {
            let field_lists: Vec<String> = conflicts
                .iter()
                .map(|c| format!("cache line {}: `[{}]`", c.cache_line, c.fields.join(", ")))
                .collect();
            let inferred = if *is_inferred {
                " _(inferred from type names — add guard annotations or verify with profiling)_"
            } else {
                ""
            };
            format!("False sharing: {}{}", field_lists.join("; "), inferred)
        }
        Finding::LocalityIssue {
            hot_fields,
            cold_fields,
            is_inferred,
            ..
        } => {
            let inferred = if *is_inferred {
                " _(inferred from type names — verify with profiling)_"
            } else {
                ""
            };
            format!(
                "Locality: hot `[{}]` interleaved with cold `[{}]`{}",
                hot_fields.join(", "),
                cold_fields.join(", "),
                inferred
            )
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::findings::Report;
    use padlock_core::ir::test_fixtures::{connection_layout, packed_layout};

    #[test]
    fn markdown_contains_struct_name() {
        let report = Report::from_layouts(&[connection_layout()]);
        let md = to_markdown(&report);
        assert!(md.contains("Connection"));
    }

    #[test]
    fn markdown_contains_severity_emoji() {
        let report = Report::from_layouts(&[connection_layout()]);
        let md = to_markdown(&report);
        assert!(md.contains("🔴 High"));
    }

    #[test]
    fn markdown_no_issues_for_packed() {
        let report = Report::from_layouts(&[packed_layout()]);
        let md = to_markdown(&report);
        assert!(md.contains("No issues found"));
    }

    #[test]
    fn markdown_header_mentions_wasted_bytes() {
        let report = Report::from_layouts(&[connection_layout()]);
        let md = to_markdown(&report);
        assert!(md.contains("bytes wasted"));
    }

    #[test]
    fn markdown_has_table_structure() {
        let report = Report::from_layouts(&[connection_layout()]);
        let md = to_markdown(&report);
        assert!(md.contains("| Severity | Finding |"));
        assert!(md.contains("|----------|"));
    }
}
