// padlock-output/src/summary.rs

use padlock_core::findings::{Finding, Report, Severity, StructReport};

/// Render a full report as a human-readable multi-line string.
pub fn render_report(report: &Report) -> String {
    let mut out = String::new();
    let multi_file = report.analyzed_paths.len() > 1;

    // Header line
    if multi_file {
        out.push_str(&format!("Analyzed {} files, ", report.analyzed_paths.len()));
        out.push_str(&format!(
            "{} struct{}",
            report.total_structs,
            if report.total_structs == 1 { "" } else { "s" }
        ));
    } else {
        out.push_str(&format!(
            "Analyzed {} struct{}",
            report.total_structs,
            if report.total_structs == 1 { "" } else { "s" }
        ));
    }

    if report.total_wasted_bytes > 0 {
        out.push_str(&format!(
            " — {} bytes wasted across all structs\n",
            report.total_wasted_bytes
        ));
    } else {
        out.push_str(" — no padding waste found\n");
    }

    if multi_file {
        render_grouped(&mut out, report);
    } else {
        out.push('\n');
        for sr in &report.structs {
            out.push_str(&render_struct(sr, true));
            out.push('\n');
        }
    }

    out
}

/// Render structs grouped by source file with a `── file ──` separator header.
fn render_grouped(out: &mut String, report: &Report) {
    // Collect distinct source files in encounter order, preserving struct order.
    let mut file_order: Vec<Option<String>> = Vec::new();
    let mut groups: std::collections::HashMap<Option<String>, Vec<&StructReport>> =
        std::collections::HashMap::new();

    for sr in &report.structs {
        let key = sr.source_file.clone();
        if !groups.contains_key(&key) {
            file_order.push(key.clone());
        }
        groups.entry(key).or_default().push(sr);
    }

    for key in &file_order {
        // File separator header
        let label = key.as_deref().unwrap_or("<binary>");
        let bar = "─".repeat(60usize.saturating_sub(label.len() + 4));
        out.push_str(&format!("\n── {label} {bar}\n\n"));

        if let Some(structs) = groups.get(key) {
            for sr in structs {
                // Within a group, suppress the filename (show only line number).
                out.push_str(&render_struct(sr, false));
                out.push('\n');
            }
        }
    }
}

/// Render one struct report.
///
/// `show_filename`: when `true`, the `source_file` is included in the location hint;
/// when `false` (inside a file-grouped section), only the line number is shown.
pub fn render_struct(sr: &StructReport, show_filename: bool) -> String {
    let mut out = String::new();

    let score_label = match sr.score as u32 {
        90..=100 => "✓",
        60..=89 => "~",
        _ => "✗",
    };

    let location = if show_filename {
        match (&sr.source_file, sr.source_line) {
            (Some(f), Some(l)) => format!(" ({}:{})", f, l),
            (Some(f), None) => format!(" ({})", f),
            _ => String::new(),
        }
    } else {
        match sr.source_line {
            Some(l) => format!(" :{l}"),
            None => String::new(),
        }
    };

    let holes_hint = if sr.num_holes > 0 {
        format!("  holes={}", sr.num_holes)
    } else {
        String::new()
    };

    out.push_str(&format!(
        "[{score_label}] {name}{location}  {size}B  fields={fields}{holes}  score={score:.0}\n",
        name = sr.struct_name,
        size = sr.total_size,
        fields = sr.num_fields,
        holes = holes_hint,
        score = sr.score,
    ));

    for finding in &sr.findings {
        out.push_str(&format!("    {}\n", render_finding(finding)));
    }

    if sr.findings.is_empty() {
        out.push_str("    (no issues found)\n");
    }

    out
}

fn render_finding(f: &Finding) -> String {
    let sev = match f.severity() {
        Severity::High => "HIGH",
        Severity::Medium => "MEDIUM",
        Severity::Low => "LOW",
    };
    match f {
        Finding::PaddingWaste {
            wasted_bytes,
            waste_pct,
            gaps,
            ..
        } => format!(
            "[{sev}] Padding waste: {wasted_bytes}B ({waste_pct:.0}%) across {} gap(s)",
            gaps.len()
        ),
        Finding::ReorderSuggestion {
            savings,
            optimized_size,
            suggested_order,
            severity,
            ..
        } => {
            let base = format!(
                "[{sev}] Reorder fields to save {savings}B → {optimized_size}B: {}",
                suggested_order.join(", ")
            );
            // For High-severity suggestions (≥8B savings) append a concrete
            // scale hint so engineers immediately see why the reorder matters.
            if *severity == Severity::High {
                format!("{base}  (~{savings} MB/1M instances)")
            } else {
                base
            }
        }
        Finding::FalseSharing { conflicts, .. } => format!(
            "[{sev}] False sharing: {} cache-line conflict(s)",
            conflicts.len()
        ),
        Finding::LocalityIssue {
            hot_fields,
            cold_fields,
            ..
        } => format!(
            "[{sev}] Locality: hot [{}] interleaved with cold [{}]",
            hot_fields.join(", "),
            cold_fields.join(", ")
        ),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::findings::Report;
    use padlock_core::ir::test_fixtures::{connection_layout, packed_layout};

    #[test]
    fn render_report_contains_struct_name() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report);
        assert!(out.contains("Connection"));
    }

    #[test]
    fn render_report_mentions_wasted_bytes() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report);
        assert!(out.contains("waste") || out.contains("Padding"));
    }

    #[test]
    fn render_report_shows_reorder_suggestion() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report);
        assert!(out.contains("Reorder") || out.contains("save"));
    }

    #[test]
    fn render_report_no_issues_on_packed() {
        let report = Report::from_layouts(&[packed_layout()]);
        let out = render_report(&report);
        assert!(out.contains("no issues"));
    }

    #[test]
    fn render_struct_shows_hole_count_when_nonzero() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_struct(&report.structs[0], true);
        assert!(out.contains("holes=2"));
    }

    #[test]
    fn render_struct_omits_holes_when_zero() {
        let report = Report::from_layouts(&[packed_layout()]);
        let out = render_struct(&report.structs[0], true);
        assert!(!out.contains("holes="));
    }

    #[test]
    fn render_struct_shows_field_count() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_struct(&report.structs[0], true);
        assert!(out.contains("fields=4"));
    }

    #[test]
    fn render_report_multi_file_header() {
        let mut report = Report::from_layouts(&[connection_layout()]);
        report.analyzed_paths = vec!["a.rs".into(), "b.rs".into()];
        let out = render_report(&report);
        assert!(out.contains("2 files"));
    }

    #[test]
    fn high_reorder_finding_shows_mb_hint() {
        // Connection saves 8B (High severity) → should show MB/1M hint
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report);
        assert!(out.contains("MB/1M instances"));
    }

    #[test]
    fn mb_hint_absent_for_packed_struct() {
        let report = Report::from_layouts(&[packed_layout()]);
        let out = render_report(&report);
        assert!(!out.contains("MB/1M instances"));
    }
}
