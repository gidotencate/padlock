// padlock-core/src/findings.rs

use crate::analysis::{false_sharing, locality, padding, reorder, scorer};
use crate::ir::{AccessPattern, PaddingGap, SharingConflict, StructLayout};
use rayon::prelude::*;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl Severity {
    /// Return the next lower severity level (High→Medium, Medium→Low, Low→Low).
    pub fn downgrade(self) -> Self {
        match self {
            Severity::High => Severity::Medium,
            Severity::Medium => Severity::Low,
            Severity::Low => Severity::Low,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind")]
pub enum Finding {
    PaddingWaste {
        struct_name: String,
        total_size: usize,
        wasted_bytes: usize,
        waste_pct: f64,
        gaps: Vec<PaddingGap>,
        severity: Severity,
    },
    FalseSharing {
        struct_name: String,
        conflicts: Vec<SharingConflict>,
        severity: Severity,
        /// True when every conflicting field's access pattern was inferred from its
        /// type name rather than an explicit source annotation (`GUARDED_BY`,
        /// `#[lock_protected_by]`, `// padlock:guard=`, etc.).
        /// Engineers should verify inferred findings with profiling before acting.
        is_inferred: bool,
    },
    ReorderSuggestion {
        struct_name: String,
        original_size: usize,
        optimized_size: usize,
        savings: usize,
        suggested_order: Vec<String>,
        severity: Severity,
    },
    LocalityIssue {
        struct_name: String,
        hot_fields: Vec<String>,
        cold_fields: Vec<String>,
        severity: Severity,
        /// True when all hot-field classifications came from the type-name heuristic.
        is_inferred: bool,
    },
}

impl Finding {
    pub fn severity(&self) -> &Severity {
        match self {
            Finding::PaddingWaste { severity, .. } => severity,
            Finding::FalseSharing { severity, .. } => severity,
            Finding::ReorderSuggestion { severity, .. } => severity,
            Finding::LocalityIssue { severity, .. } => severity,
        }
    }

    pub fn struct_name(&self) -> &str {
        match self {
            Finding::PaddingWaste { struct_name, .. } => struct_name,
            Finding::FalseSharing { struct_name, .. } => struct_name,
            Finding::ReorderSuggestion { struct_name, .. } => struct_name,
            Finding::LocalityIssue { struct_name, .. } => struct_name,
        }
    }

    /// The name of the finding variant as a string, used for per-finding suppression.
    ///
    /// Matches the variant names used in source annotations:
    /// `"PaddingWaste"`, `"ReorderSuggestion"`, `"FalseSharing"`, `"LocalityIssue"`.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Finding::PaddingWaste { .. } => "PaddingWaste",
            Finding::FalseSharing { .. } => "FalseSharing",
            Finding::ReorderSuggestion { .. } => "ReorderSuggestion",
            Finding::LocalityIssue { .. } => "LocalityIssue",
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct StructReport {
    pub struct_name: String,
    pub source_file: Option<String>,
    pub source_line: Option<u32>,
    pub total_size: usize,
    /// Number of data fields (excludes padding pseudo-fields).
    pub num_fields: usize,
    /// Number of byte-level padding gaps (holes) in the layout.
    pub num_holes: usize,
    pub wasted_bytes: usize,
    pub score: f64,
    pub findings: Vec<Finding>,
    /// Mirrors `StructLayout::is_repr_rust`. When true, findings describe
    /// declared-order waste; the compiler may have already eliminated it.
    pub is_repr_rust: bool,
    /// Field names whose type size could not be accurately determined from
    /// source alone (e.g. a qualified Go type like `driver.Connector` whose
    /// package is not in the analyzed source set).  When non-empty, padding
    /// and reorder findings on this struct may be inaccurate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertain_fields: Vec<String>,
}

/// A struct/type that was skipped during analysis because its layout cannot be
/// accurately determined from source alone (e.g. generic/template types).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkippedStruct {
    pub name: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub structs: Vec<StructReport>,
    pub total_structs: usize,
    pub total_wasted_bytes: usize,
    /// Weighted-by-size aggregate score across every struct (same formula as
    /// `padlock summary`'s headline number). The single source of truth for
    /// this computation — consumers (the project-summary renderer, the VS
    /// Code extension's status bar via JSON) read it here rather than
    /// reimplementing the weighted-average formula themselves.
    pub aggregate_score: f64,
    /// Letter grade (A-F) for `aggregate_score`, using the same thresholds
    /// everywhere: A >= 90, B >= 80, C >= 70, D >= 60, F otherwise.
    pub aggregate_grade: char,
    /// Paths that were analyzed to produce this report (populated by the CLI).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub analyzed_paths: Vec<String>,
    /// Structs that were skipped during analysis (e.g. generics/templates whose
    /// layout cannot be determined from source alone). Populated by the CLI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedStruct>,
    /// Maps each struct name to the list of outer struct names that embed it
    /// as a field.  Used to surface "fixing this struct also shrinks Foo/Bar"
    /// hints in the output.  Omitted from JSON serialisation (internal only).
    #[serde(skip)]
    pub embedded_in: std::collections::HashMap<String, Vec<String>>,
}

/// Weighted-by-`total_size` aggregate score across `structs`. Structs with no
/// findings score 100, so an empty report (no structs) is defined as a clean
/// 100.0 rather than 0 — "nothing to flag" should not read as "everything is
/// broken".
pub fn weighted_score(structs: &[StructReport]) -> f64 {
    let total_weight: f64 = structs.iter().map(|s| s.total_size as f64).sum();
    if total_weight <= 0.0 {
        return 100.0;
    }
    structs
        .iter()
        .map(|s| s.score * s.total_size as f64)
        .sum::<f64>()
        / total_weight
}

/// Letter grade (A-F) for a 0-100 score, rounded to the nearest integer
/// before bucketing: A >= 90, B >= 80, C >= 70, D >= 60, F otherwise.
pub fn letter_grade(score: f64) -> char {
    match score.round() as i64 {
        s if s >= 90 => 'A',
        80..=89 => 'B',
        70..=79 => 'C',
        60..=69 => 'D',
        _ => 'F',
    }
}

impl Report {
    /// Run all analysis passes over `layouts` and assemble the full report.
    pub fn from_layouts(layouts: &[StructLayout]) -> Report {
        let structs: Vec<StructReport> = layouts.par_iter().map(analyze_one).collect();
        let total_wasted_bytes = structs.iter().map(|s| s.wasted_bytes).sum();
        let aggregate_score = weighted_score(&structs);
        let aggregate_grade = letter_grade(aggregate_score);

        // Build reverse-embedding map: inner_struct_name → [outer_struct_names].
        // Any field whose TypeInfo::Opaque name matches a known struct is an embedding.
        let struct_names: std::collections::HashSet<&str> =
            structs.iter().map(|s| s.struct_name.as_str()).collect();
        let mut embedded_in: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for layout in layouts {
            for field in &layout.fields {
                let inner_name = match &field.ty {
                    crate::ir::TypeInfo::Opaque { name, .. } => name.as_str(),
                    _ => continue,
                };
                if struct_names.contains(inner_name) {
                    embedded_in
                        .entry(inner_name.to_owned())
                        .or_default()
                        .push(layout.name.clone());
                }
            }
        }

        Report {
            total_structs: structs.len(),
            total_wasted_bytes,
            aggregate_score,
            aggregate_grade,
            structs,
            analyzed_paths: Vec::new(),
            skipped: Vec::new(),
            embedded_in,
        }
    }
}

fn analyze_one(layout: &StructLayout) -> StructReport {
    let mut findings = Vec::new();

    // ── padding waste ────────────────────────────────────────────────────────
    let gaps = padding::find_padding(layout);
    let num_holes = gaps.len();
    let wasted: usize = gaps.iter().map(|g| g.bytes).sum();
    // Unions: is_union suppresses padding at the find_padding level; no extra check needed.
    if wasted > 0 {
        let waste_pct = wasted as f64 / layout.total_size as f64 * 100.0;
        // Percentage thresholds catch high-density waste in small structs.
        // Absolute thresholds catch large structs where the percentage looks
        // modest but the real cache-bandwidth cost is significant.
        let mut severity = if waste_pct >= 30.0 || wasted >= 32 {
            Severity::High
        } else if waste_pct >= 10.0 || wasted >= 8 {
            Severity::Medium
        } else {
            Severity::Low
        };
        // repr(Rust) structs have no guaranteed layout — the compiler may already
        // eliminate this padding.  Downgrade by one level so the finding remains
        // visible without over-alarming on code the compiler has already handled.
        if layout.is_repr_rust {
            severity = severity.downgrade();
        }
        findings.push(Finding::PaddingWaste {
            struct_name: layout.name.clone(),
            total_size: layout.total_size,
            wasted_bytes: wasted,
            waste_pct,
            gaps,
            severity,
        });
    }

    // ── reorder suggestion ───────────────────────────────────────────────────
    // Packed structs have no padding to eliminate; union field order is irrelevant.
    let (optimized_size, savings) = reorder::reorder_savings(layout);
    if savings > 0 && !layout.is_packed && !layout.is_union {
        let suggested_order = reorder::optimal_order(layout)
            .iter()
            .map(|f| f.name.clone())
            .collect();
        // repr(Rust): the compiler likely already reorders fields, so cap at Medium —
        // the suggestion is still actionable (especially when adding repr(C) later)
        // but should not block a High-only CI gate.
        let severity = if layout.is_repr_rust {
            Severity::Medium
        } else if savings >= 8 {
            Severity::High
        } else {
            Severity::Medium
        };
        findings.push(Finding::ReorderSuggestion {
            struct_name: layout.name.clone(),
            original_size: layout.total_size,
            optimized_size,
            savings,
            suggested_order,
            severity,
        });
    }

    // ── false sharing ────────────────────────────────────────────────────────
    // Unions place all fields at offset 0 by definition; that is not false sharing.
    if !layout.is_union && false_sharing::has_false_sharing(layout) {
        let conflicts = false_sharing::find_sharing_conflicts(layout);
        // is_inferred = true when no conflicting field carries an explicit annotation.
        let is_inferred = !layout.fields.iter().any(|f| {
            matches!(
                f.access,
                AccessPattern::Concurrent {
                    is_annotated: true,
                    ..
                }
            )
        });
        findings.push(Finding::FalseSharing {
            struct_name: layout.name.clone(),
            conflicts,
            severity: Severity::High,
            is_inferred,
        });
    }

    // ── locality ─────────────────────────────────────────────────────────────
    if locality::has_locality_issue(layout) {
        let (hot, cold) = locality::partition_hot_cold(layout);
        // is_inferred = true when no hot field has an explicit annotation.
        // ReadMostly is always set by the heuristic; Concurrent is annotated when
        // is_annotated = true.
        let is_inferred = !layout.fields.iter().any(|f| {
            matches!(
                f.access,
                AccessPattern::Concurrent {
                    is_annotated: true,
                    ..
                }
            )
        });
        findings.push(Finding::LocalityIssue {
            struct_name: layout.name.clone(),
            hot_fields: hot,
            cold_fields: cold,
            severity: Severity::Medium,
            is_inferred,
        });
    }

    // ── per-finding suppression ──────────────────────────────────────────────
    // Drop any findings whose kind_name matches a suppression directive placed
    // in the source file (e.g. `// padlock: ignore[ReorderSuggestion]`).
    if !layout.suppressed_findings.is_empty() {
        findings.retain(|f| {
            !layout
                .suppressed_findings
                .contains(&f.kind_name().to_string())
        });
    }

    let score = scorer::score(layout);

    StructReport {
        struct_name: layout.name.clone(),
        source_file: layout.source_file.clone(),
        source_line: layout.source_line,
        total_size: layout.total_size,
        num_fields: layout.fields.len(),
        num_holes,
        wasted_bytes: wasted,
        score,
        findings,
        is_repr_rust: layout.is_repr_rust,
        uncertain_fields: layout.uncertain_fields.clone(),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::test_fixtures::{connection_layout, packed_layout};

    #[test]
    fn report_from_misaligned_has_padding_finding() {
        let report = Report::from_layouts(&[connection_layout()]);
        assert_eq!(report.total_structs, 1);
        let sr = &report.structs[0];
        assert!(sr.wasted_bytes > 0);
        assert!(
            sr.findings
                .iter()
                .any(|f| matches!(f, Finding::PaddingWaste { .. }))
        );
    }

    #[test]
    fn report_from_packed_has_no_padding_finding() {
        let report = Report::from_layouts(&[packed_layout()]);
        let sr = &report.structs[0];
        assert_eq!(sr.wasted_bytes, 0);
        assert!(
            !sr.findings
                .iter()
                .any(|f| matches!(f, Finding::PaddingWaste { .. }))
        );
    }

    #[test]
    fn letter_grade_boundaries() {
        assert_eq!(letter_grade(100.0), 'A');
        assert_eq!(letter_grade(90.0), 'A');
        assert_eq!(letter_grade(89.0), 'B');
        assert_eq!(letter_grade(80.0), 'B');
        assert_eq!(letter_grade(79.0), 'C');
        assert_eq!(letter_grade(70.0), 'C');
        assert_eq!(letter_grade(69.0), 'D');
        assert_eq!(letter_grade(60.0), 'D');
        assert_eq!(letter_grade(59.0), 'F');
        assert_eq!(letter_grade(0.0), 'F');
        // Rounds before bucketing: 89.6 -> 90 -> A, not B.
        assert_eq!(letter_grade(89.6), 'A');
    }

    #[test]
    fn weighted_score_of_empty_structs_is_a_clean_hundred() {
        assert_eq!(weighted_score(&[]), 100.0);
    }

    #[test]
    fn weighted_score_matches_report_aggregate_score() {
        let report = Report::from_layouts(&[connection_layout(), packed_layout()]);
        assert_eq!(report.aggregate_score, weighted_score(&report.structs));
        assert_eq!(report.aggregate_grade, letter_grade(report.aggregate_score));
    }

    #[test]
    fn report_from_misaligned_has_reorder_suggestion() {
        let report = Report::from_layouts(&[connection_layout()]);
        let sr = &report.structs[0];
        assert!(
            sr.findings
                .iter()
                .any(|f| matches!(f, Finding::ReorderSuggestion { .. }))
        );
    }

    #[test]
    fn severity_high_when_waste_over_30_pct() {
        let report = Report::from_layouts(&[connection_layout()]);
        let sr = &report.structs[0];
        // Connection wastes 10/24 = 41% → High
        let padding_finding = sr
            .findings
            .iter()
            .find(|f| matches!(f, Finding::PaddingWaste { .. }))
            .unwrap();
        assert_eq!(padding_finding.severity(), &Severity::High);
    }

    #[test]
    fn total_wasted_bytes_sums_across_structs() {
        let report = Report::from_layouts(&[connection_layout(), packed_layout()]);
        assert_eq!(report.total_structs, 2);
        assert_eq!(report.total_wasted_bytes, 10); // only Connection wastes bytes
    }

    #[test]
    fn suppressed_finding_kind_not_in_report() {
        let mut layout = connection_layout();
        layout.suppressed_findings = vec!["ReorderSuggestion".to_string()];
        let report = Report::from_layouts(&[layout]);
        let sr = &report.structs[0];
        // PaddingWaste should still appear
        assert!(
            sr.findings
                .iter()
                .any(|f| matches!(f, Finding::PaddingWaste { .. }))
        );
        // ReorderSuggestion must be suppressed
        assert!(
            !sr.findings
                .iter()
                .any(|f| matches!(f, Finding::ReorderSuggestion { .. }))
        );
    }

    #[test]
    fn suppressing_all_findings_yields_empty_findings() {
        let mut layout = connection_layout();
        layout.suppressed_findings = vec![
            "PaddingWaste".to_string(),
            "ReorderSuggestion".to_string(),
            "FalseSharing".to_string(),
            "LocalityIssue".to_string(),
        ];
        let report = Report::from_layouts(&[layout]);
        assert!(report.structs[0].findings.is_empty());
    }
}
