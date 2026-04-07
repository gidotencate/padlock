// padlock-core/src/analysis/scorer.rs

use crate::analysis::{false_sharing, locality, padding, reorder};
use crate::ir::StructLayout;

#[derive(Debug, Clone)]
pub struct ScoreBreakdown {
    /// Final score in [0.0, 100.0]. Higher = better (less to fix).
    pub total: f64,
    /// Points deducted for padding waste (max 40).
    pub padding_deduction: f64,
    /// Points deducted for confirmed false sharing (30) or potential (10).
    pub false_sharing_deduction: f64,
    /// Points deducted for hot/cold locality issues (max 15).
    pub locality_deduction: f64,
}

/// Score a layout on a 0–100 scale (100 = perfect, 0 = very bad).
pub fn score(layout: &StructLayout) -> f64 {
    score_with_breakdown(layout).total
}

pub fn score_with_breakdown(layout: &StructLayout) -> ScoreBreakdown {
    let mut deductions = 0.0f64;

    // Padding: up to 40 points
    let gaps = padding::find_padding(layout);
    let wasted: usize = gaps.iter().map(|g| g.bytes).sum();
    let padding_deduction = if layout.total_size > 0 {
        (wasted as f64 / layout.total_size as f64 * 40.0).min(40.0)
    } else {
        0.0
    };
    deductions += padding_deduction;

    // False sharing: 30 confirmed, 10 potential.
    // Unions place all fields at offset 0 by definition — that is not a sharing hazard.
    let false_sharing_deduction = if layout.is_union {
        0.0
    } else if false_sharing::has_false_sharing(layout) {
        30.0
    } else if !false_sharing::find_sharing_conflicts(layout).is_empty() {
        10.0
    } else {
        0.0
    };
    deductions += false_sharing_deduction;

    // Locality: up to 15 points
    let locality_deduction = if locality::has_locality_issue(layout) {
        15.0
    } else {
        0.0
    };
    deductions += locality_deduction;

    // Reorder potential: up to 15 points
    let (_, savings) = reorder::reorder_savings(layout);
    let reorder_deduction = if layout.total_size > 0 && savings > 0 {
        (savings as f64 / layout.total_size as f64 * 15.0).min(15.0)
    } else {
        0.0
    };
    deductions += reorder_deduction;

    let total = (100.0 - deductions).clamp(0.0, 100.0);

    ScoreBreakdown {
        total,
        padding_deduction,
        false_sharing_deduction,
        locality_deduction,
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::test_fixtures::{connection_layout, packed_layout};

    #[test]
    fn score_in_range() {
        let s = score(&connection_layout());
        assert!((0.0..=100.0).contains(&s), "score out of range: {s}");
    }

    #[test]
    fn packed_scores_higher_than_misaligned() {
        assert!(score(&packed_layout()) > score(&connection_layout()));
    }

    #[test]
    fn perfect_layout_scores_100() {
        // Single 8-byte field — no padding, no sharing, no locality issues
        use crate::arch::X86_64_SYSV;
        use crate::ir::{AccessPattern, Field, StructLayout, TypeInfo};
        let layout = StructLayout {
            name: "Single".into(),
            total_size: 8,
            align: 8,
            fields: vec![Field {
                name: "x".into(),
                ty: TypeInfo::Primitive {
                    name: "u64".into(),
                    size: 8,
                    align: 8,
                },
                offset: 0,
                size: 8,
                align: 8,
                source_file: None,
                source_line: None,
                access: AccessPattern::Unknown,
            }],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
        };
        assert!((score(&layout) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn connection_loses_padding_and_reorder_points() {
        let bd = score_with_breakdown(&connection_layout());
        assert!(bd.padding_deduction > 0.0);
        assert!(bd.total < 100.0);
    }
}
