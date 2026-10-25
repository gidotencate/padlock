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

/// The concurrently accessed fields of each cache line that holds two or more
/// of them with *different* guards — written under different locks, or atomics
/// updated independently. That is false sharing: threads touching unrelated
/// data still bounce the line between their cores. Fields under the same guard
/// do not conflict. Only the conflicting fields are listed (not every field
/// that happens to share the line).
pub fn find_concurrent_conflicts(layout: &StructLayout) -> Vec<SharingConflict> {
    let line = layout.arch.cache_line_size;
    if line == 0 || layout.is_union {
        return Vec::new();
    }
    let mut buckets: std::collections::BTreeMap<usize, Vec<(&str, Option<&str>)>> =
        std::collections::BTreeMap::new();
    for f in &layout.fields {
        if let AccessPattern::Concurrent { guard, .. } = &f.access {
            // A field spanning several lines conflicts on each of them.
            let first = f.offset / line;
            let last = (f.offset + f.size.max(1) - 1) / line;
            for cl in first..=last {
                buckets
                    .entry(cl)
                    .or_default()
                    .push((f.name.as_str(), guard.as_deref().map(normalize_guard)));
            }
        }
    }
    buckets
        .into_iter()
        .filter(|(_, fields)| fields.len() > 1 && fields.iter().any(|(_, g)| *g != fields[0].1))
        .map(|(cache_line, fields)| SharingConflict {
            fields: fields.into_iter().map(|(n, _)| n.to_string()).collect(),
            cache_line,
        })
        .collect()
}

/// Return `true` if any cache line contains two or more `Concurrent` fields
/// with *different* guards (see [`find_concurrent_conflicts`]): two locks, a
/// lock and an atomic, or two independently updated atomics.
///
/// Guards come from explicit annotations (`GUARDED_BY`, `#[lock_protected_by]`,
/// `// padlock:guard=`) or, without them, from the type-name heuristic, which
/// gives every lock or atomic its own guard. Findings from the heuristic are
/// reported as inferred, at lower severity.
pub fn has_false_sharing(layout: &StructLayout) -> bool {
    !find_concurrent_conflicts(layout).is_empty()
}

/// Whether some conflicting field carries an explicit guard annotation (as
/// opposed to every conflict being inferred from type names).
pub fn is_annotated(layout: &StructLayout) -> bool {
    let conflicting: std::collections::HashSet<String> = find_concurrent_conflicts(layout)
        .into_iter()
        .flat_map(|c| c.fields)
        .collect();
    layout.fields.iter().any(|f| {
        conflicting.contains(&f.name)
            && matches!(
                f.access,
                AccessPattern::Concurrent {
                    is_annotated: true,
                    ..
                }
            )
    })
}

/// Return `true` if any cache line holds 2+ fields where at least one is
/// `Concurrent`, but the pair didn't meet `has_false_sharing`'s stricter bar
/// (same guard, or only one of the pair is concurrent, or both are pure
/// atomics). A weaker, "worth a human look" signal — as opposed to
/// `find_sharing_conflicts`, which flags *any* two fields sharing a cache
/// line regardless of concurrency and is meant only as display detail once
/// `has_false_sharing` has already confirmed a real hazard.
pub fn has_potential_sharing_risk(layout: &StructLayout) -> bool {
    let line = layout.arch.cache_line_size;
    if line == 0 || layout.fields.is_empty() {
        return false;
    }

    let mut buckets: std::collections::BTreeMap<usize, Vec<&AccessPattern>> =
        std::collections::BTreeMap::new();
    for field in &layout.fields {
        if matches!(field.access, AccessPattern::Padding) {
            continue;
        }
        buckets
            .entry(field.offset / line)
            .or_default()
            .push(&field.access);
    }

    buckets.values().any(|accesses| {
        accesses.len() > 1
            && accesses
                .iter()
                .any(|a| matches!(a, AccessPattern::Concurrent { .. }))
    })
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
            forced_align: None,
            pack_limit: None,
            cpp: None,
            explicit_align: Vec::new(),
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

    // Two independently updated atomics on one line are the textbook case:
    // threads bumping different counters still bounce the line.
    #[test]
    fn two_atomics_on_one_line_are_false_sharing() {
        let layout = make_layout(vec![atomic("counter_a", 0), atomic("counter_b", 8)]);
        assert!(has_false_sharing(&layout));
        let conflicts = find_concurrent_conflicts(&layout);
        assert_eq!(conflicts[0].fields, vec!["counter_a", "counter_b"]);
    }

    // …but not once each has a cache line of its own.
    #[test]
    fn atomics_on_separate_lines_are_not_false_sharing() {
        let layout = make_layout(vec![atomic("counter_a", 0), atomic("counter_b", 64)]);
        assert!(!has_false_sharing(&layout));
    }

    // Only the conflicting fields are reported, not everything on the line.
    #[test]
    fn conflicts_list_only_concurrent_fields() {
        let mut layout = make_layout(vec![concurrent("a", 0, "mu_a"), atomic("b", 16)]);
        let mut plain = layout.fields[0].clone();
        plain.name = "plain".into();
        plain.offset = 8;
        plain.access = AccessPattern::Unknown;
        layout.fields.insert(1, plain);
        let conflicts = find_concurrent_conflicts(&layout);
        assert_eq!(conflicts[0].fields, vec!["a", "b"]);
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

    // ── has_potential_sharing_risk ───────────────────────────────────────────

    #[test]
    fn no_potential_risk_for_two_plain_unrelated_fields() {
        // Two ordinary, non-concurrent fields sharing a cache line is just
        // normal struct layout, not a sharing risk of any kind.
        let layout = make_layout(vec![plain("a", 0), plain("b", 8)]);
        assert!(!has_potential_sharing_risk(&layout));
    }

    #[test]
    fn no_potential_risk_for_single_field_alone_on_its_line() {
        let layout = make_layout(vec![concurrent("a", 0, "mu")]);
        assert!(!has_potential_sharing_risk(&layout));
    }

    #[test]
    fn potential_risk_for_concurrent_field_sharing_line_with_plain_field() {
        // Not confirmed false sharing (has_false_sharing requires both sides
        // to be Concurrent), but still worth a lower-confidence flag.
        let layout = make_layout(vec![concurrent("mu_guarded", 0, "mu"), plain("b", 8)]);
        assert!(has_potential_sharing_risk(&layout));
    }

    #[test]
    fn potential_risk_for_same_guard_concurrent_pair() {
        // Same guard means no *confirmed* false sharing, but two
        // lock-protected fields sharing a line is still worth flagging.
        let layout = make_layout(vec![concurrent("a", 0, "mu"), concurrent("b", 8, "mu")]);
        assert!(!has_false_sharing(&layout));
        assert!(has_potential_sharing_risk(&layout));
    }
}
