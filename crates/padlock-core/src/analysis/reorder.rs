// padlock-core/src/analysis/reorder.rs

use crate::ir::{Field, StructLayout};

pub use crate::ir::optimal_order;

/// Simulate the packed size of `fields` laid out in order with natural alignment.
fn simulated_size(fields: &[&Field], struct_align: usize) -> usize {
    let mut offset = 0usize;
    for f in fields {
        if f.align > 0 {
            offset = offset.next_multiple_of(f.align);
        }
        offset += f.size;
    }
    if struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }
    offset
}

/// Return `(optimized_total_size, bytes_saved)` for reordering `layout`'s fields
/// into the optimal order (largest alignment first).
pub fn reorder_savings(layout: &StructLayout) -> (usize, usize) {
    if layout.fields.is_empty() {
        return (layout.total_size, 0);
    }
    let ordered = optimal_order(layout);
    let optimized = simulated_size(&ordered, layout.align);
    let savings = layout.total_size.saturating_sub(optimized);
    (optimized, savings)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::test_fixtures::{connection_layout, packed_layout};

    #[test]
    fn connection_saves_8_bytes() {
        // Original: 24 bytes.  Optimal: timeout(8) port(4) is_active(1) is_tls(1) = 14 → align8 → 16
        let (optimized, savings) = reorder_savings(&connection_layout());
        assert_eq!(optimized, 16);
        assert_eq!(savings, 8);
    }

    #[test]
    fn packed_layout_saves_nothing() {
        let (optimized, savings) = reorder_savings(&packed_layout());
        assert_eq!(savings, 0);
        assert_eq!(optimized, packed_layout().total_size);
    }

    #[test]
    fn optimal_order_puts_largest_align_first() {
        let layout = connection_layout();
        let order = optimal_order(&layout);
        // timeout has align 8, everything else ≤ 4
        assert_eq!(order[0].name, "timeout");
    }

    #[test]
    fn simulated_size_matches_manual_calculation() {
        let layout = connection_layout();
        let ordered = optimal_order(&layout);
        // timeout@0(size8) port@8(size4) is_active@12(size1) is_tls@13(size1) → 14 → pad to 16
        let sz = simulated_size(&ordered, layout.align);
        assert_eq!(sz, 16);
    }
}
