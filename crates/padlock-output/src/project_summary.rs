// padlock-output/src/project_summary.rs
//
// Renders a project-level health summary designed for large codebases.
// Shows aggregate score, severity distribution with a bar chart, worst files,
// and worst structs — all fitting in one terminal screen.

use padlock_core::findings::{Report, Severity, StructReport};

/// Input for the project summary renderer.
pub struct SummaryInput<'a> {
    pub report: &'a Report,
    /// Number of worst files and structs to show (default 5).
    pub top: usize,
}

/// Render a project health summary to a `String`.
pub fn render_summary(input: &SummaryInput<'_>) -> String {
    let report = input.report;
    let top = input.top.max(1);

    let total = report.structs.len();
    if total == 0 {
        return "No structs found.\n".to_string();
    }

    // ── aggregate score (weighted by struct size) ──────────────────────────────
    let total_weight: f64 = report
        .structs
        .iter()
        .map(|s| s.total_size as f64)
        .sum::<f64>()
        .max(1.0);
    let weighted_score: f64 = report
        .structs
        .iter()
        .map(|s| s.score * s.total_size as f64)
        .sum::<f64>()
        / total_weight;
    let score_int = weighted_score.round() as usize;
    let grade = letter_grade(score_int);

    // ── severity counts ────────────────────────────────────────────────────────
    let mut n_high = 0usize;
    let mut n_medium = 0usize;
    let mut n_low = 0usize;
    let mut n_clean = 0usize;

    for sr in &report.structs {
        let worst = sr
            .findings
            .iter()
            .map(|f| f.severity())
            .max_by_key(|s| severity_rank(s));
        match worst {
            Some(s) if *s == Severity::High => n_high += 1,
            Some(s) if *s == Severity::Medium => n_medium += 1,
            Some(_) => n_low += 1,
            None => n_clean += 1,
        }
    }

    // ── file scores ───────────────────────────────────────────────────────────
    // Group structs by source file and compute per-file aggregate score.
    let mut file_map: std::collections::HashMap<String, Vec<&StructReport>> =
        std::collections::HashMap::new();
    for sr in &report.structs {
        let file = sr
            .source_file
            .clone()
            .unwrap_or_else(|| "<unknown>".to_string());
        file_map.entry(file).or_default().push(sr);
    }

    let mut file_scores: Vec<(String, f64, usize, usize)> = file_map
        .iter()
        .map(|(file, structs)| {
            let w: f64 = structs
                .iter()
                .map(|s| s.total_size as f64)
                .sum::<f64>()
                .max(1.0);
            let score = structs
                .iter()
                .map(|s| s.score * s.total_size as f64)
                .sum::<f64>()
                / w;
            let high_count = structs
                .iter()
                .filter(|s| {
                    s.findings
                        .iter()
                        .any(|f| matches!(f.severity(), Severity::High))
                })
                .count();
            let wasted: usize = structs.iter().map(|s| s.wasted_bytes).sum();
            (file.clone(), score, high_count, wasted)
        })
        .collect();
    // Sort worst first (lowest score, then most high findings)
    file_scores.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.2.cmp(&a.2))
    });

    // ── worst structs (by score, then wasted bytes) ───────────────────────────
    let mut worst_structs: Vec<&StructReport> = report.structs.iter().collect();
    worst_structs.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.wasted_bytes.cmp(&a.wasted_bytes))
    });

    // ── render ────────────────────────────────────────────────────────────────
    let mut out = String::new();
    let bar_width = 20usize;
    let divider = "━".repeat(57);

    // Header line
    out.push_str(&format!(
        "{divider}\n  Score   {score_int} / 100   {grade}    {} structs · {} files · {}B wasted\n{divider}\n\n",
        total,
        file_scores.len(),
        report.total_wasted_bytes
    ));

    // Severity distribution bar chart
    let bar = |n: usize| {
        let filled = if total > 0 {
            (n * bar_width / total).min(bar_width)
        } else {
            0
        };
        let empty = bar_width - filled;
        format!("{}{}", "█".repeat(filled), "░".repeat(empty))
    };

    out.push_str(&format!(
        "  🔴 High     {}  {:>4}  ({:.0}%)\n",
        bar(n_high),
        n_high,
        pct(n_high, total)
    ));
    out.push_str(&format!(
        "  🟡 Medium   {}  {:>4}  ({:.0}%)\n",
        bar(n_medium),
        n_medium,
        pct(n_medium, total)
    ));
    out.push_str(&format!(
        "  🔵 Low      {}  {:>4}  ({:.0}%)\n",
        bar(n_low),
        n_low,
        pct(n_low, total)
    ));
    out.push_str(&format!(
        "  ✅ Clean    {}  {:>4}  ({:.0}%)\n",
        bar(n_clean),
        n_clean,
        pct(n_clean, total)
    ));

    // Worst files
    if !file_scores.is_empty() {
        out.push_str(&format!(
            "\n  {:<44} {:>5}   {:>5}   {}\n",
            "Worst files", "score", "High", "wasted"
        ));
        out.push_str(&format!("  {}\n", "─".repeat(68)));
        for (file, score, high, wasted) in file_scores.iter().take(top) {
            let name = truncate(file, 44);
            out.push_str(&format!(
                "  {:<44} {:>5.0}   {:>5}   {}B\n",
                name, score, high, wasted
            ));
        }
    }

    // Worst structs
    if !worst_structs.is_empty() {
        out.push_str(&format!(
            "\n  {:<30} {:>5}   {}\n",
            "Worst structs", "score", "location"
        ));
        out.push_str(&format!("  {}\n", "─".repeat(68)));
        for sr in worst_structs.iter().take(top) {
            let loc = match (&sr.source_file, sr.source_line) {
                (Some(f), Some(l)) => format!("{f}:{l}"),
                (Some(f), None) => f.clone(),
                _ => String::new(),
            };
            out.push_str(&format!(
                "  {:<30} {:>5.0}   {}\n",
                truncate(&sr.struct_name, 30),
                sr.score,
                loc
            ));
        }
    }

    // Next-step hint
    if let Some((worst_file, _, _, _)) = file_scores.first() {
        out.push_str(&format!(
            "\n  Run `padlock analyze {worst_file}` for full detail.\n"
        ));
    }

    out
}

fn letter_grade(score: usize) -> &'static str {
    match score {
        90..=100 => "A",
        80..=89 => "B",
        70..=79 => "C",
        60..=69 => "D",
        _ => "F",
    }
}

fn severity_rank(s: &Severity) -> u8 {
    match s {
        Severity::Low => 1,
        Severity::Medium => 2,
        Severity::High => 3,
    }
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        n as f64 / total as f64 * 100.0
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::findings::Report;
    use padlock_core::ir::test_fixtures::{connection_layout, packed_layout};

    fn make_report() -> Report {
        let mut r = Report::from_layouts(&[connection_layout(), packed_layout()]);
        // Annotate source files for file grouping
        r.structs[0].source_file = Some("src/conn.rs".to_string());
        r.structs[1].source_file = Some("src/packed.rs".to_string());
        r
    }

    #[test]
    fn summary_contains_score() {
        let report = make_report();
        let out = render_summary(&SummaryInput {
            report: &report,
            top: 5,
        });
        assert!(out.contains("/ 100"), "must show score out of 100");
    }

    #[test]
    fn summary_contains_grade() {
        let report = make_report();
        let out = render_summary(&SummaryInput {
            report: &report,
            top: 5,
        });
        // Grade must be one of A-F
        assert!(
            out.contains('A')
                || out.contains('B')
                || out.contains('C')
                || out.contains('D')
                || out.contains('F'),
            "must contain a letter grade"
        );
    }

    #[test]
    fn summary_contains_severity_bars() {
        let report = make_report();
        let out = render_summary(&SummaryInput {
            report: &report,
            top: 5,
        });
        assert!(out.contains("High"), "must show High severity");
        assert!(out.contains("Medium"), "must show Medium severity");
        assert!(out.contains("Clean"), "must show Clean count");
    }

    #[test]
    fn summary_contains_worst_file() {
        let report = make_report();
        let out = render_summary(&SummaryInput {
            report: &report,
            top: 5,
        });
        assert!(
            out.contains("src/conn.rs") || out.contains("src/packed.rs"),
            "must show at least one file"
        );
    }

    #[test]
    fn summary_contains_struct_names() {
        let report = make_report();
        let out = render_summary(&SummaryInput {
            report: &report,
            top: 5,
        });
        assert!(out.contains("Connection") || out.contains("Packed"));
    }

    #[test]
    fn summary_empty_report() {
        let report = Report::from_layouts(&[]);
        let out = render_summary(&SummaryInput {
            report: &report,
            top: 5,
        });
        assert!(out.contains("No structs"));
    }

    #[test]
    fn letter_grade_boundaries() {
        assert_eq!(letter_grade(100), "A");
        assert_eq!(letter_grade(90), "A");
        assert_eq!(letter_grade(89), "B");
        assert_eq!(letter_grade(80), "B");
        assert_eq!(letter_grade(79), "C");
        assert_eq!(letter_grade(70), "C");
        assert_eq!(letter_grade(69), "D");
        assert_eq!(letter_grade(60), "D");
        assert_eq!(letter_grade(59), "F");
        assert_eq!(letter_grade(0), "F");
    }
}
