// padlock-output/src/summary.rs

use padlock_core::findings::{Finding, Report, Severity, StructReport};

/// Render a full report as a human-readable multi-line string.
pub fn render_report(report: &Report) -> String {
    let mut out = String::new();

    // When multiple files were analyzed, show the file count first.
    if report.analyzed_paths.len() > 1 {
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
    out.push('\n');

    for sr in &report.structs {
        out.push_str(&render_struct(sr));
        out.push('\n');
    }

    out
}

pub fn render_struct(sr: &StructReport) -> String {
    let mut out = String::new();

    let score_label = match sr.score as u32 {
        90..=100 => "✓",
        60..=89 => "~",
        _ => "✗",
    };

    let location = match (&sr.source_file, sr.source_line) {
        (Some(f), Some(l)) => format!(" ({}:{})", f, l),
        (Some(f), None) => format!(" ({})", f),
        _ => String::new(),
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
            ..
        } => format!(
            "[{sev}] Reorder fields to save {savings}B → {optimized_size}B: {}",
            suggested_order.join(", ")
        ),
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
        let out = render_struct(&report.structs[0]);
        assert!(out.contains("holes=2"));
    }

    #[test]
    fn render_struct_omits_holes_when_zero() {
        let report = Report::from_layouts(&[packed_layout()]);
        let out = render_struct(&report.structs[0]);
        assert!(!out.contains("holes="));
    }

    #[test]
    fn render_struct_shows_field_count() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_struct(&report.structs[0]);
        assert!(out.contains("fields=4"));
    }

    #[test]
    fn render_report_multi_file_header() {
        let mut report = Report::from_layouts(&[connection_layout()]);
        report.analyzed_paths = vec!["a.rs".into(), "b.rs".into()];
        let out = render_report(&report);
        assert!(out.contains("2 files"));
    }
}
