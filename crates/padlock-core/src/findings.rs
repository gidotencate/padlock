// padlock-core/src/findings.rs

use crate::analysis::{false_sharing, locality, padding, reorder, scorer};
use crate::ir::{PaddingGap, SharingConflict, StructLayout};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Severity {
    Low,
    Medium,
    High,
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
}

#[derive(Debug, serde::Serialize)]
pub struct StructReport {
    pub struct_name: String,
    pub source_file: Option<String>,
    pub source_line: Option<u32>,
    pub total_size: usize,
    pub wasted_bytes: usize,
    pub score: f64,
    pub findings: Vec<Finding>,
}

#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub structs: Vec<StructReport>,
    pub total_structs: usize,
    pub total_wasted_bytes: usize,
}

impl Report {
    /// Run all analysis passes over `layouts` and assemble the full report.
    pub fn from_layouts(layouts: &[StructLayout]) -> Report {
        let structs: Vec<StructReport> = layouts.iter().map(analyze_one).collect();
        let total_wasted_bytes = structs.iter().map(|s| s.wasted_bytes).sum();
        Report {
            total_structs: structs.len(),
            total_wasted_bytes,
            structs,
        }
    }
}

fn analyze_one(layout: &StructLayout) -> StructReport {
    let mut findings = Vec::new();

    // ── padding waste ────────────────────────────────────────────────────────
    let gaps = padding::find_padding(layout);
    let wasted: usize = gaps.iter().map(|g| g.bytes).sum();
    // Unions: is_union suppresses padding at the find_padding level; no extra check needed.
    if wasted > 0 {
        let waste_pct = wasted as f64 / layout.total_size as f64 * 100.0;
        let severity = if waste_pct >= 30.0 {
            Severity::High
        } else if waste_pct >= 10.0 {
            Severity::Medium
        } else {
            Severity::Low
        };
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
        findings.push(Finding::ReorderSuggestion {
            struct_name: layout.name.clone(),
            original_size: layout.total_size,
            optimized_size,
            savings,
            suggested_order,
            severity: if savings >= 8 {
                Severity::High
            } else {
                Severity::Medium
            },
        });
    }

    // ── false sharing ────────────────────────────────────────────────────────
    // Unions place all fields at offset 0 by definition; that is not false sharing.
    if !layout.is_union && false_sharing::has_false_sharing(layout) {
        let conflicts = false_sharing::find_sharing_conflicts(layout);
        findings.push(Finding::FalseSharing {
            struct_name: layout.name.clone(),
            conflicts,
            severity: Severity::High,
        });
    }

    // ── locality ─────────────────────────────────────────────────────────────
    if locality::has_locality_issue(layout) {
        let (hot, cold) = locality::partition_hot_cold(layout);
        findings.push(Finding::LocalityIssue {
            struct_name: layout.name.clone(),
            hot_fields: hot,
            cold_fields: cold,
            severity: Severity::Medium,
        });
    }

    let score = scorer::score(layout);

    StructReport {
        struct_name: layout.name.clone(),
        source_file: layout.source_file.clone(),
        source_line: layout.source_line,
        total_size: layout.total_size,
        wasted_bytes: wasted,
        score,
        findings,
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
        assert!(sr
            .findings
            .iter()
            .any(|f| matches!(f, Finding::PaddingWaste { .. })));
    }

    #[test]
    fn report_from_packed_has_no_padding_finding() {
        let report = Report::from_layouts(&[packed_layout()]);
        let sr = &report.structs[0];
        assert_eq!(sr.wasted_bytes, 0);
        assert!(!sr
            .findings
            .iter()
            .any(|f| matches!(f, Finding::PaddingWaste { .. })));
    }

    #[test]
    fn report_from_misaligned_has_reorder_suggestion() {
        let report = Report::from_layouts(&[connection_layout()]);
        let sr = &report.structs[0];
        assert!(sr
            .findings
            .iter()
            .any(|f| matches!(f, Finding::ReorderSuggestion { .. })));
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
}
