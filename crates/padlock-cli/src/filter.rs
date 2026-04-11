// padlock-cli/src/filter.rs
//
// Filtering and sorting options shared across subcommands.
//
// Filters are applied in two phases:
//   1. Pre-analysis (apply_to_layouts): name pattern, min_size, min_holes — cheap,
//      avoids running analysis passes on structs the user doesn't care about.
//   2. Post-analysis (apply_to_report): packable — requires knowing which structs
//      have a ReorderSuggestion finding. Also performs sorting.

use anyhow::Context;
use clap::{Args, ValueEnum};
use padlock_core::findings::{Finding, Report, Severity};
use padlock_core::ir::{StructLayout, find_padding};

/// Severity level for the `--fail-on-severity` flag.
#[derive(Clone, ValueEnum)]
pub enum FailSeverity {
    High,
    Medium,
    Low,
}

impl FailSeverity {
    /// Returns true if `sev` meets or exceeds this threshold.
    pub fn matches(&self, sev: &Severity) -> bool {
        match self {
            FailSeverity::High => matches!(sev, Severity::High),
            FailSeverity::Medium => matches!(sev, Severity::High | Severity::Medium),
            FailSeverity::Low => true,
        }
    }
}

/// How to order the output structs.
#[derive(Clone, ValueEnum, Default)]
pub enum SortBy {
    /// Worst score first (default)
    #[default]
    Score,
    /// Largest struct first
    Size,
    /// Most wasted bytes first
    Waste,
    /// Alphabetical by struct name
    Name,
}

/// Filtering and sorting options shared across `analyze`, `list`, and `report`.
#[derive(Args, Clone)]
pub struct FilterArgs {
    /// Include only structs whose names match this regex pattern
    #[arg(long, short = 'F', value_name = "PATTERN")]
    pub filter: Option<String>,

    /// Exclude structs whose names match this regex pattern
    #[arg(long, short = 'X', value_name = "PATTERN")]
    pub exclude: Option<String>,

    /// Show only structs with at least this many padding holes
    #[arg(long, value_name = "N")]
    pub min_holes: Option<usize>,

    /// Show only structs with total size >= N bytes
    #[arg(long, value_name = "N")]
    pub min_size: Option<usize>,

    /// Show only structs that have reorganizable padding (a reorder suggestion exists)
    #[arg(long)]
    pub packable: bool,

    /// Sort results by: score (default), size, waste, name
    #[arg(long, value_enum, default_value = "score", value_name = "FIELD")]
    pub sort_by: SortBy,
}

impl FilterArgs {
    /// Apply name/size/holes filters to layouts before running analysis passes.
    pub fn apply_to_layouts(&self, layouts: &mut Vec<StructLayout>) -> anyhow::Result<()> {
        if let Some(ref pat) = self.filter {
            let re = regex::Regex::new(pat)
                .with_context(|| format!("invalid --filter pattern: {pat:?}"))?;
            layouts.retain(|l| re.is_match(&l.name));
        }
        if let Some(ref pat) = self.exclude {
            let re = regex::Regex::new(pat)
                .with_context(|| format!("invalid --exclude pattern: {pat:?}"))?;
            layouts.retain(|l| !re.is_match(&l.name));
        }
        if let Some(min_size) = self.min_size {
            layouts.retain(|l| l.total_size >= min_size);
        }
        if let Some(min_holes) = self.min_holes {
            layouts.retain(|l| find_padding(l).len() >= min_holes);
        }
        Ok(())
    }

    /// Apply post-analysis filters (packable) and sort to the assembled report.
    /// Re-synchronises `total_structs` and `total_wasted_bytes` after filtering.
    pub fn apply_to_report(&self, report: &mut Report) {
        if self.packable {
            report.structs.retain(|sr| {
                sr.findings
                    .iter()
                    .any(|f| matches!(f, Finding::ReorderSuggestion { .. }))
            });
        }

        match self.sort_by {
            SortBy::Score => report.structs.sort_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortBy::Size => report
                .structs
                .sort_by(|a, b| b.total_size.cmp(&a.total_size)),
            SortBy::Waste => report
                .structs
                .sort_by(|a, b| b.wasted_bytes.cmp(&a.wasted_bytes)),
            SortBy::Name => report
                .structs
                .sort_by(|a, b| a.struct_name.cmp(&b.struct_name)),
        }

        // Re-sync summary counters after any retention changes.
        report.total_structs = report.structs.len();
        report.total_wasted_bytes = report.structs.iter().map(|s| s.wasted_bytes).sum();
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::findings::Report;
    use padlock_core::ir::test_fixtures::{connection_layout, packed_layout};

    // ── FailSeverity::matches ─────────────────────────────────────────────────

    #[test]
    fn fail_severity_high_only_matches_high() {
        assert!(FailSeverity::High.matches(&Severity::High));
        assert!(!FailSeverity::High.matches(&Severity::Medium));
        assert!(!FailSeverity::High.matches(&Severity::Low));
    }

    #[test]
    fn fail_severity_medium_matches_high_and_medium() {
        assert!(FailSeverity::Medium.matches(&Severity::High));
        assert!(FailSeverity::Medium.matches(&Severity::Medium));
        assert!(!FailSeverity::Medium.matches(&Severity::Low));
    }

    #[test]
    fn fail_severity_low_matches_all() {
        assert!(FailSeverity::Low.matches(&Severity::High));
        assert!(FailSeverity::Low.matches(&Severity::Medium));
        assert!(FailSeverity::Low.matches(&Severity::Low));
    }

    fn args(
        filter: Option<&str>,
        exclude: Option<&str>,
        min_holes: Option<usize>,
        min_size: Option<usize>,
        packable: bool,
        sort_by: SortBy,
    ) -> FilterArgs {
        FilterArgs {
            filter: filter.map(str::to_owned),
            exclude: exclude.map(str::to_owned),
            min_holes,
            min_size,
            packable,
            sort_by,
        }
    }

    fn default_args() -> FilterArgs {
        args(None, None, None, None, false, SortBy::Score)
    }

    #[test]
    fn filter_keeps_matching_name() {
        let mut layouts = vec![connection_layout(), packed_layout()];
        args(Some("Connection"), None, None, None, false, SortBy::Score)
            .apply_to_layouts(&mut layouts)
            .unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Connection");
    }

    #[test]
    fn filter_regex_works() {
        let mut layouts = vec![connection_layout(), packed_layout()];
        // Matches both
        args(
            Some("^(Connection|Packed)$"),
            None,
            None,
            None,
            false,
            SortBy::Score,
        )
        .apply_to_layouts(&mut layouts)
        .unwrap();
        assert_eq!(layouts.len(), 2);
    }

    #[test]
    fn exclude_removes_matching() {
        let mut layouts = vec![connection_layout(), packed_layout()];
        args(None, Some("Packed"), None, None, false, SortBy::Score)
            .apply_to_layouts(&mut layouts)
            .unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Connection");
    }

    #[test]
    fn min_holes_removes_zero_hole_structs() {
        let mut layouts = vec![connection_layout(), packed_layout()];
        // Connection has 2 holes, Packed has 0
        args(None, None, Some(1), None, false, SortBy::Score)
            .apply_to_layouts(&mut layouts)
            .unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Connection");
    }

    #[test]
    fn min_size_removes_small_structs() {
        let mut layouts = vec![connection_layout(), packed_layout()];
        // Connection = 24B, Packed = 8B
        args(None, None, None, Some(16), false, SortBy::Score)
            .apply_to_layouts(&mut layouts)
            .unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Connection");
    }

    #[test]
    fn packable_keeps_only_reorderable() {
        let mut report = Report::from_layouts(&[connection_layout(), packed_layout()]);
        args(None, None, None, None, true, SortBy::Score).apply_to_report(&mut report);
        // All remaining structs must have a ReorderSuggestion
        assert!(report.structs.iter().all(|sr| {
            sr.findings
                .iter()
                .any(|f| matches!(f, Finding::ReorderSuggestion { .. }))
        }));
        // Packed has no reorder suggestion so it should be gone
        assert!(report.structs.iter().all(|sr| sr.struct_name != "Packed"));
    }

    #[test]
    fn sort_by_name_is_alphabetical() {
        let mut report = Report::from_layouts(&[connection_layout(), packed_layout()]);
        args(None, None, None, None, false, SortBy::Name).apply_to_report(&mut report);
        let names: Vec<&str> = report
            .structs
            .iter()
            .map(|s| s.struct_name.as_str())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn sort_by_size_is_descending() {
        let mut report = Report::from_layouts(&[packed_layout(), connection_layout()]);
        args(None, None, None, None, false, SortBy::Size).apply_to_report(&mut report);
        let sizes: Vec<usize> = report.structs.iter().map(|s| s.total_size).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn sort_by_waste_is_descending() {
        let mut report = Report::from_layouts(&[packed_layout(), connection_layout()]);
        args(None, None, None, None, false, SortBy::Waste).apply_to_report(&mut report);
        let waste: Vec<usize> = report.structs.iter().map(|s| s.wasted_bytes).collect();
        assert!(waste.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn report_counters_resynced_after_filter() {
        let mut report = Report::from_layouts(&[connection_layout(), packed_layout()]);
        assert_eq!(report.total_structs, 2);
        args(None, None, None, None, true, SortBy::Score).apply_to_report(&mut report);
        assert_eq!(report.total_structs, report.structs.len());
        assert_eq!(
            report.total_wasted_bytes,
            report.structs.iter().map(|s| s.wasted_bytes).sum::<usize>()
        );
    }

    #[test]
    fn invalid_filter_regex_returns_error() {
        let mut layouts = vec![connection_layout()];
        let result = args(Some("[invalid"), None, None, None, false, SortBy::Score)
            .apply_to_layouts(&mut layouts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--filter"));
    }

    #[test]
    fn invalid_exclude_regex_returns_error() {
        let mut layouts = vec![connection_layout()];
        let result = args(None, Some("(unclosed"), None, None, false, SortBy::Score)
            .apply_to_layouts(&mut layouts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--exclude"));
    }
}
