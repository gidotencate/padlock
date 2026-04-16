// padlock-core/src/analysis/false_sharing.rs

use crate::ir::{AccessPattern, SharingConflict, StructLayout};

/// Normalise a guard name for comparison.
///
/// Strips language-specific prefixes that refer to the same object:
/// - `self.mu` → `mu`  (Rust / Python)
/// - `this->mu` → `mu` (C++ member)
/// - `this.mu` → `mu`  (Go/Java style)
/// - Leading `&` / `*` dereference operators
///
/// Only the outermost layer of each prefix is stripped so that deeply qualified
/// names (e.g. `self.inner.mu`) still compare differently from `self.outer.mu`.
pub fn normalize_guard(guard: &str) -> &str {
    let s = guard
        .strip_prefix("self.")
        .or_else(|| guard.strip_prefix("this->"))
        .or_else(|| guard.strip_prefix("this."))
        .unwrap_or(guard);
    s.trim_start_matches(['&', '*'])
}

/// Return all groups of fields that share a cache line.
/// Any cache line with two or more fields is a potential false-sharing hazard.
pub fn find_sharing_conflicts(layout: &StructLayout) -> Vec<SharingConflict> {
    let line = layout.arch.cache_line_size;
    if line == 0 || layout.fields.is_empty() {
        return Vec::new();
    }

    let mut buckets: std::collections::BTreeMap<usize, Vec<String>> =
        std::collections::BTreeMap::new();
    for field in &layout.fields {
        if matches!(field.access, AccessPattern::Padding) {
            continue;
        }
        let cl = field.offset / line;
        buckets.entry(cl).or_default().push(field.name.clone());
    }

    buckets
        .into_iter()
        .filter(|(_, fields)| fields.len() > 1)
        .map(|(cache_line, fields)| SharingConflict { fields, cache_line })
        .collect()
}

/// Return `true` if any cache line contains two or more `Concurrent` fields
/// with *different* lock guards — a confirmed false-sharing hazard.
///
/// ## Heuristic tightening
///
/// The type-name heuristic assigns each field's own name as its guard, so two
/// `AtomicU64` fields always receive different guards and would naively trigger
/// this check. However, two purely-atomic fields sharing a cache line is a
/// performance concern (cache-line bouncing) rather than *false sharing* in the
/// classical lock-based sense. To avoid noisy findings from the heuristic, we
/// only flag a conflict when **at least one** of the two fields has
/// `is_atomic: false` (i.e. it is a mutex/lock type, or was explicitly
/// annotated as lock-protected data).
///
/// Explicit guard annotations (`GUARDED_BY`, `#[lock_protected_by]`, etc.) always
/// set `is_atomic: false`, so annotated conflicts are always reported.
pub fn has_false_sharing(layout: &StructLayout) -> bool {
    let line = layout.arch.cache_line_size;
    if line == 0 {
        return false;
    }

    let concurrent: Vec<(usize, Option<&str>, bool)> = layout
        .fields
        .iter()
        .filter_map(|f| {
            if let AccessPattern::Concurrent {
                guard, is_atomic, ..
            } = &f.access
            {
                Some((f.offset / line, guard.as_deref(), *is_atomic))
            } else {
                None
            }
        })
        .collect();

    for i in 0..concurrent.len() {
        for j in (i + 1)..concurrent.len() {
            let (cl_a, guard_a, atomic_a) = concurrent[i];
            let (cl_b, guard_b, atomic_b) = concurrent[j];
            if cl_a == cl_b && guard_a.map(normalize_guard) != guard_b.map(normalize_guard) {
                // Skip if both fields are purely atomic with no lock involvement —
                // that pattern is handled by the locality analysis, not false sharing.
                if atomic_a && atomic_b {
                    continue;
                }
                return true;
            }
        }
    }
    false
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::X86_64_SYSV;
    use crate::ir::{Field, StructLayout, TypeInfo};

    fn make_layout(fields: Vec<Field>) -> StructLayout {
        StructLayout {
            name: "T".into(),
            total_size: 128,
            align: 8,
            fields,
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
            is_repr_rust: false,
            suppressed_findings: Vec::new(),
            uncertain_fields: Vec::new(),
        }
    }

    fn concurrent(name: &str, offset: usize, guard: &str) -> Field {
        Field {
            name: name.into(),
            ty: TypeInfo::Primitive {
                name: "u64".into(),
                size: 8,
                align: 8,
            },
            offset,
            size: 8,
            align: 8,
            source_file: None,
            source_line: None,
            access: AccessPattern::Concurrent {
                guard: Some(guard.into()),
                is_atomic: false,
                is_annotated: false,
            },
        }
    }

    fn atomic(name: &str, offset: usize) -> Field {
        Field {
            name: name.into(),
            ty: TypeInfo::Primitive {
                name: "AtomicU64".into(),
                size: 8,
                align: 8,
            },
            offset,
            size: 8,
            align: 8,
            source_file: None,
            source_line: None,
            access: AccessPattern::Concurrent {
                guard: Some(name.into()),
                is_atomic: true,
                is_annotated: false,
            },
        }
    }

    fn plain(name: &str, offset: usize) -> Field {
        Field {
            name: name.into(),
            ty: TypeInfo::Primitive {
                name: "u64".into(),
                size: 8,
                align: 8,
            },
            offset,
            size: 8,
            align: 8,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        }
    }

    #[test]
    fn two_fields_on_same_line_is_conflict() {
        let layout = make_layout(vec![plain("a", 0), plain("b", 8)]);
        let conflicts = find_sharing_conflicts(&layout);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].cache_line, 0);
    }

    #[test]
    fn fields_on_different_lines_no_conflict() {
        let layout = make_layout(vec![plain("a", 0), plain("b", 64)]);
        assert!(find_sharing_conflicts(&layout).is_empty());
    }

    #[test]
    fn has_false_sharing_when_different_guards_same_line() {
        let layout = make_layout(vec![
            concurrent("readers", 0, "lock_a"),
            concurrent("writers", 8, "lock_b"),
        ]);
        assert!(has_false_sharing(&layout));
    }

    #[test]
    fn no_false_sharing_when_same_guard() {
        let layout = make_layout(vec![concurrent("a", 0, "mu"), concurrent("b", 8, "mu")]);
        assert!(!has_false_sharing(&layout));
    }

    #[test]
    fn no_false_sharing_when_all_unknown() {
        let layout = make_layout(vec![plain("a", 0), plain("b", 8)]);
        assert!(!has_false_sharing(&layout));
    }

    #[test]
    fn no_false_sharing_when_different_lines() {
        let layout = make_layout(vec![
            concurrent("a", 0, "lock_a"),
            concurrent("b", 64, "lock_b"),
        ]);
        assert!(!has_false_sharing(&layout));
    }

    // Heuristic tightening: two pure atomics sharing a cache line is cache-line
    // bouncing (a locality concern), not classical false sharing.
    #[test]
    fn no_false_sharing_for_two_pure_atomics_same_line() {
        let layout = make_layout(vec![atomic("counter_a", 0), atomic("counter_b", 8)]);
        assert!(!has_false_sharing(&layout));
    }

    // A mutex-protected field paired with an atomic on the same line IS false sharing.
    #[test]
    fn false_sharing_for_atomic_and_mutex_same_line() {
        let layout = make_layout(vec![
            atomic("hot_counter", 0),
            concurrent("protected_data", 8, "mu"),
        ]);
        assert!(has_false_sharing(&layout));
    }

    // Three fields: two atomics (same line) plus one mutex-protected — the mutex
    // conflicts with both atomics, so false sharing should be detected.
    #[test]
    fn false_sharing_detected_with_mixed_atomics_and_mutex() {
        let layout = make_layout(vec![
            atomic("reads", 0),
            atomic("writes", 8),
            concurrent("state", 16, "mu"),
        ]);
        assert!(has_false_sharing(&layout));
    }

    // Two atomics on the same line, all with the same guard — no false sharing.
    #[test]
    fn no_false_sharing_for_pure_atomics_on_different_lines() {
        let layout = make_layout(vec![atomic("counter_a", 0), atomic("counter_b", 64)]);
        assert!(!has_false_sharing(&layout));
    }

    // ── normalize_guard ───────────────────────────────────────────────────────

    #[test]
    fn normalize_strips_self_prefix() {
        assert_eq!(normalize_guard("self.mu"), "mu");
    }

    #[test]
    fn normalize_strips_this_arrow_prefix() {
        assert_eq!(normalize_guard("this->mu"), "mu");
    }

    #[test]
    fn normalize_strips_this_dot_prefix() {
        assert_eq!(normalize_guard("this.mu"), "mu");
    }

    #[test]
    fn normalize_strips_leading_ampersand() {
        assert_eq!(normalize_guard("&mu"), "mu");
    }

    #[test]
    fn normalize_strips_leading_star() {
        assert_eq!(normalize_guard("*mu"), "mu");
    }

    #[test]
    fn normalize_no_change_for_plain_name() {
        assert_eq!(normalize_guard("mu"), "mu");
    }

    // Guards with different receiver prefixes but the same base name should NOT
    // trigger false sharing.
    #[test]
    fn no_false_sharing_when_guards_differ_only_by_self_prefix() {
        // "self.mu" and "mu" normalise to the same base name.
        let layout = make_layout(vec![
            concurrent("readers", 0, "self.mu"),
            concurrent("writers", 8, "mu"),
        ]);
        assert!(!has_false_sharing(&layout));
    }

    #[test]
    fn no_false_sharing_when_guards_differ_only_by_this_arrow_prefix() {
        let layout = make_layout(vec![
            concurrent("readers", 0, "this->lock"),
            concurrent("writers", 8, "lock"),
        ]);
        assert!(!has_false_sharing(&layout));
    }
}
