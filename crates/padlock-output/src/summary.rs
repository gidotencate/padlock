// padlock-output/src/summary.rs

use padlock_core::findings::{Finding, Report, Severity, SkippedStruct, StructReport};

/// Render a full report as a human-readable multi-line string.
///
/// `show_skipped`: when `true`, list every skipped type; when `false`, show only
/// a count + category breakdown with a hint to pass `--show-skipped`.
pub fn render_report(report: &Report, show_skipped: bool) -> String {
    let mut out = String::new();
    let multi_file = report.analyzed_paths.len() > 1;

    // Coverage suffix: shown when any types were skipped so the engineer knows
    // what fraction of the type surface padlock could actually see.
    let coverage_suffix = if !report.skipped.is_empty() {
        let total = report.total_structs + report.skipped.len();
        let pct = report.total_structs * 100 / total;
        if pct < 70 {
            format!(
                " [{} of {} types, {}% source coverage — consider binary analysis for the rest]",
                report.total_structs, total, pct
            )
        } else {
            format!(
                " [{} of {} types, {}% source coverage]",
                report.total_structs, total, pct
            )
        }
    } else {
        String::new()
    };

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
            " — {} bytes wasted across all structs{}\n",
            report.total_wasted_bytes, coverage_suffix
        ));
    } else {
        out.push_str(&format!(" — no padding waste found{}\n", coverage_suffix));
    }

    if multi_file {
        render_grouped(&mut out, report);
    } else {
        out.push('\n');
        for sr in &report.structs {
            out.push_str(&render_struct_with_embed(sr, true, &report.embedded_in));
            out.push('\n');
        }
    }

    if !report.skipped.is_empty() {
        out.push_str(&render_skipped(&report.skipped, show_skipped));
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
                out.push_str(&render_struct_with_embed(sr, false, &report.embedded_in));
                out.push('\n');
            }
        }
    }
}

/// Render one struct report (public API, no embedding hints).
///
/// `show_filename`: when `true`, the `source_file` is included in the location hint;
/// when `false` (inside a file-grouped section), only the line number is shown.
pub fn render_struct(sr: &StructReport, show_filename: bool) -> String {
    render_struct_with_embed(sr, show_filename, &std::collections::HashMap::new())
}

/// Render one struct report with optional embedding-context hints.
fn render_struct_with_embed(
    sr: &StructReport,
    show_filename: bool,
    embedded_in: &std::collections::HashMap<String, Vec<String>>,
) -> String {
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

    if sr.is_repr_rust && !sr.findings.is_empty() {
        out.push_str(
            "    note: repr(Rust) — compiler may reorder fields; \
             use binary analysis for actual layout\n",
        );
    }

    if !sr.uncertain_fields.is_empty() {
        let fields = sr.uncertain_fields.join(", ");
        out.push_str(&format!(
            "    note: uncertain field size(s): {fields} — \
             use binary analysis (DWARF/BTF) or provide type info for accurate results\n"
        ));
    }

    // Embedding context: if this struct has padding waste and is embedded in
    // other structs, note that fixing this struct would shrink those too.
    let has_waste = sr
        .findings
        .iter()
        .any(|f| matches!(f, Finding::PaddingWaste { .. }));
    if has_waste
        && let Some(outer_structs) = embedded_in.get(&sr.struct_name)
        && !outer_structs.is_empty()
    {
        let mut names = outer_structs.clone();
        names.sort();
        names.dedup();
        out.push_str(&format!(
            "    note: embedded in [{}] — fixing layout would reduce size of each\n",
            names.join(", ")
        ));
    }

    out
}

const SKIPPED_INLINE_LIMIT: usize = 10;

/// Categorise a skipped entry into a short human-readable label.
fn skip_category(s: &SkippedStruct) -> &'static str {
    let r = s.reason.as_str();
    if r.starts_with("C++ template") {
        "C++ template"
    } else if r.starts_with("comptime-generic") {
        "Zig comptime-generic"
    } else if r.starts_with("generic enum") {
        "Rust generic enum"
    } else if r.starts_with("generic struct") {
        let is_go = s
            .source_file
            .as_deref()
            .map(|f| f.ends_with(".go"))
            .unwrap_or(false);
        if is_go { "Go generic" } else { "Rust generic" }
    } else {
        "other"
    }
}

/// Render the skipped-types section.
///
/// Always shows a count + breakdown by category.  When `show_all` is false,
/// lists at most `SKIPPED_INLINE_LIMIT` entries and appends a hint; when true,
/// lists all entries.
fn render_skipped(skipped: &[SkippedStruct], show_all: bool) -> String {
    let n = skipped.len();
    let mut out = String::new();

    // Build category counts (sorted for stable output).
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for s in skipped {
        *counts.entry(skip_category(s)).or_insert(0) += 1;
    }
    let breakdown: Vec<String> = counts
        .iter()
        .map(|(cat, cnt)| format!("{cnt} {cat}"))
        .collect();

    out.push_str(&format!(
        "note: {n} type{} skipped (layout cannot be determined from source alone): {}\n",
        if n == 1 { "" } else { "s" },
        breakdown.join(", "),
    ));

    let limit = if show_all { n } else { SKIPPED_INLINE_LIMIT };
    for s in skipped.iter().take(limit) {
        let loc = s
            .source_file
            .as_deref()
            .map(|f| format!(" ({f})"))
            .unwrap_or_default();
        out.push_str(&format!("  skipped '{}'{loc}: {}\n", s.name, s.reason));
    }

    if !show_all && n > SKIPPED_INLINE_LIMIT {
        out.push_str(&format!(
            "  … and {} more (use --show-skipped to list all, or --json for full data)\n",
            n - SKIPPED_INLINE_LIMIT,
        ));
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
        } => {
            // Show up to 3 gap locations so the engineer knows exactly where to look.
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
            format!("[{sev}] Padding waste: {wasted_bytes}B ({waste_pct:.0}%) — {detail}")
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
                "[{sev}] Reorder fields: {original_size}B → {optimized_size}B (saves {savings}B): {}",
                suggested_order.join(", ")
            );
            if *severity == Severity::High {
                format!("{base}  (~{savings} MB/1M instances)")
            } else {
                base
            }
        }
        Finding::FalseSharing {
            conflicts,
            is_inferred,
            ..
        } => {
            // Show the field names involved so the engineer knows what to look at.
            let field_lists: Vec<String> = conflicts
                .iter()
                .map(|c| format!("cache line {}: [{}]", c.cache_line, c.fields.join(", ")))
                .collect();
            let inferred_note = if *is_inferred {
                "  (inferred from type names — add guard annotations or verify with profiling)"
            } else {
                ""
            };
            format!(
                "[{sev}] False sharing: {}{}",
                field_lists.join("; "),
                inferred_note
            )
        }
        Finding::LocalityIssue {
            hot_fields,
            cold_fields,
            is_inferred,
            ..
        } => {
            let inferred_note = if *is_inferred {
                "  (inferred from type names — verify with profiling)"
            } else {
                ""
            };
            format!(
                "[{sev}] Locality: hot [{}] mixed with cold [{}] on same cache line(s){}",
                hot_fields.join(", "),
                cold_fields.join(", "),
                inferred_note
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
    fn render_report_contains_struct_name() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report, false);
        assert!(out.contains("Connection"));
    }

    #[test]
    fn render_report_mentions_wasted_bytes() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report, false);
        assert!(out.contains("waste") || out.contains("Padding"));
    }

    #[test]
    fn render_report_shows_reorder_suggestion() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report, false);
        assert!(out.contains("Reorder") || out.contains("saves"));
    }

    #[test]
    fn render_report_no_issues_on_packed() {
        let report = Report::from_layouts(&[packed_layout()]);
        let out = render_report(&report, false);
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
        let out = render_report(&report, false);
        assert!(out.contains("2 files"));
    }

    #[test]
    fn high_reorder_finding_shows_mb_hint() {
        // Connection saves 8B (High severity) → should show MB/1M hint
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report, false);
        assert!(out.contains("MB/1M instances"));
    }

    #[test]
    fn mb_hint_absent_for_packed_struct() {
        let report = Report::from_layouts(&[packed_layout()]);
        let out = render_report(&report, false);
        assert!(!out.contains("MB/1M instances"));
    }

    #[test]
    fn padding_waste_shows_gap_locations() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report, false);
        // Should show "XB after `field` (offset N)" for each gap.
        assert!(out.contains("after `"), "gap location detail missing");
        assert!(out.contains("offset "), "gap offset missing");
    }

    #[test]
    fn reorder_shows_before_and_after_sizes() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_report(&report, false);
        // New format: "NB → NB (saves NB)"
        assert!(out.contains("saves"), "savings clause missing");
    }

    // ── uncertain_fields note ─────────────────────────────────────────────────

    #[test]
    fn uncertain_fields_note_shown_when_non_empty() {
        let mut layout = connection_layout();
        layout.uncertain_fields = vec!["connector".to_string()];

        let report = Report::from_layouts(&[layout]);
        let out = render_struct(&report.structs[0], true);
        assert!(
            out.contains("uncertain field size"),
            "uncertain_fields note must appear in output: {out}"
        );
        assert!(
            out.contains("connector"),
            "uncertain field name must appear in output: {out}"
        );
        assert!(
            out.contains("DWARF/BTF"),
            "output must mention binary analysis: {out}"
        );
    }

    #[test]
    fn uncertain_fields_note_absent_when_empty() {
        let report = Report::from_layouts(&[connection_layout()]);
        let out = render_struct(&report.structs[0], true);
        assert!(
            !out.contains("uncertain field size"),
            "uncertain_fields note must not appear when fields are empty"
        );
    }
}
