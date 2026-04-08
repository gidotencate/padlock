// padlock-core/src/analysis/impact.rs
//
// Estimates the concrete memory and cache-line impact of struct layout
// inefficiencies at different instance-count scales.
//
// All estimates are approximations intended to give engineers a concrete sense
// of magnitude, not cycle-accurate benchmarks.

/// Estimated impact of a struct layout inefficiency at various scales.
///
/// Built from `estimate_impact`. All "extra" figures are relative to the
/// layout produced by `reorder::reorder_savings` (the optimal field order).
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactEstimate {
    /// Bytes saved per instance by applying the optimal field ordering.
    pub savings_per_instance: usize,
    /// Cache-line size used for this estimate (bytes).
    pub cache_line_size: usize,
    /// Cache lines occupied by the *current* layout per instance.
    pub current_cache_lines: usize,
    /// Cache lines that would be occupied by the *optimal* layout per instance.
    pub optimal_cache_lines: usize,
    /// Extra bytes across 1 000 instances (`savings × 1 000`).
    pub extra_bytes_1k: usize,
    /// Extra bytes across 1 000 000 instances (`savings × 1 000 000`).
    pub extra_bytes_1m: usize,
    /// Approximate extra cache lines loaded for a sequential scan of 1 000 instances.
    pub extra_cache_lines_1k: usize,
    /// Approximate extra cache lines loaded for a sequential scan of 1 000 000 instances.
    pub extra_cache_lines_1m: usize,
}

impl ImpactEstimate {
    /// Returns `true` when the current layout occupies more cache lines per
    /// instance than the optimal layout, meaning a reorder would reduce
    /// cache-line crossings on random access.
    pub fn reduces_cache_line_crossings(&self) -> bool {
        self.current_cache_lines > self.optimal_cache_lines
    }
}

/// Compute the impact estimate for a struct layout inefficiency.
///
/// # Parameters
/// - `savings` — bytes saved per instance by reordering (from `reorder::reorder_savings`)
/// - `current_size` — current total struct size in bytes
/// - `optimal_size` — total size after optimal reordering
/// - `cache_line` — cache-line size in bytes (typically 64; use `ArchConfig.cache_line_size`)
///
/// # Example
/// ```
/// use padlock_core::analysis::impact::estimate_impact;
///
/// // 24-byte struct that can shrink to 16 bytes by reordering
/// let est = estimate_impact(8, 24, 16, 64);
/// assert_eq!(est.savings_per_instance, 8);
/// assert_eq!(est.extra_bytes_1m, 8_000_000);
/// assert_eq!(est.extra_cache_lines_1m, 125_000);
/// assert!(!est.reduces_cache_line_crossings()); // both fit in one cache line
/// ```
pub fn estimate_impact(
    savings: usize,
    current_size: usize,
    optimal_size: usize,
    cache_line: usize,
) -> ImpactEstimate {
    let cl = cache_line.max(1);
    let current_cache_lines = current_size.div_ceil(cl);
    let optimal_cache_lines = optimal_size.div_ceil(cl);

    ImpactEstimate {
        savings_per_instance: savings,
        cache_line_size: cl,
        current_cache_lines,
        optimal_cache_lines,
        extra_bytes_1k: savings * 1_000,
        extra_bytes_1m: savings * 1_000_000,
        // Ceiling division: conservative estimate (rounds up)
        extra_cache_lines_1k: (savings * 1_000).div_ceil(cl),
        extra_cache_lines_1m: (savings * 1_000_000).div_ceil(cl),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_layout_impact() {
        // 24-byte → 16-byte, cache line 64 (both fit in one cache line)
        let est = estimate_impact(8, 24, 16, 64);
        assert_eq!(est.savings_per_instance, 8);
        assert_eq!(est.cache_line_size, 64);
        assert_eq!(est.current_cache_lines, 1);
        assert_eq!(est.optimal_cache_lines, 1);
        assert!(!est.reduces_cache_line_crossings());
        assert_eq!(est.extra_bytes_1k, 8_000);
        assert_eq!(est.extra_bytes_1m, 8_000_000);
        assert_eq!(est.extra_cache_lines_1k, 125);
        assert_eq!(est.extra_cache_lines_1m, 125_000);
    }

    #[test]
    fn large_struct_reduces_cache_line_crossings() {
        // 128-byte → 64-byte: spans 2 cache lines, optimal spans 1
        let est = estimate_impact(64, 128, 64, 64);
        assert_eq!(est.current_cache_lines, 2);
        assert_eq!(est.optimal_cache_lines, 1);
        assert!(est.reduces_cache_line_crossings());
        assert_eq!(est.extra_bytes_1m, 64_000_000);
        assert_eq!(est.extra_cache_lines_1m, 1_000_000);
    }

    #[test]
    fn zero_savings_produces_zero_impact() {
        let est = estimate_impact(0, 16, 16, 64);
        assert_eq!(est.savings_per_instance, 0);
        assert_eq!(est.extra_bytes_1k, 0);
        assert_eq!(est.extra_bytes_1m, 0);
        assert_eq!(est.extra_cache_lines_1k, 0);
        assert_eq!(est.extra_cache_lines_1m, 0);
        assert!(!est.reduces_cache_line_crossings());
    }

    #[test]
    fn apple_silicon_128_byte_cache_line() {
        // Apple M-series: 128-byte cache line
        let est = estimate_impact(8, 24, 16, 128);
        assert_eq!(est.cache_line_size, 128);
        assert_eq!(est.current_cache_lines, 1);
        assert_eq!(est.optimal_cache_lines, 1);
        // Sequential scan: 8 * 1M / 128 = 62500
        assert_eq!(est.extra_cache_lines_1m, 62_500);
    }

    #[test]
    fn struct_spanning_boundary_in_current_but_not_optimal() {
        // 72-byte struct (spans 2 cache lines of 64) → optimal 64 bytes (fits in 1)
        let est = estimate_impact(8, 72, 64, 64);
        assert_eq!(est.current_cache_lines, 2); // ceil(72/64) = 2
        assert_eq!(est.optimal_cache_lines, 1); // ceil(64/64) = 1
        assert!(est.reduces_cache_line_crossings());
    }

    #[test]
    fn small_savings_cache_lines_round_up() {
        // 1 byte savings × 1000 instances = 1000 bytes; 1000/64 rounds up to 16
        let est = estimate_impact(1, 8, 7, 64);
        assert_eq!(est.extra_cache_lines_1k, 16); // ceil(1000/64)
    }
}
