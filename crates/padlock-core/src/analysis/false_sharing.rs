// padlock-core/src/analysis/false_sharing.rs

use crate::ir::{AccessPattern, SharingConflict, StructLayout};

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
pub fn has_false_sharing(layout: &StructLayout) -> bool {
    let line = layout.arch.cache_line_size;
    if line == 0 {
        return false;
    }

    let concurrent: Vec<(usize, Option<&str>)> = layout
        .fields
        .iter()
        .filter_map(|f| {
            if let AccessPattern::Concurrent { guard, .. } = &f.access {
                Some((f.offset / line, guard.as_deref()))
            } else {
                None
            }
        })
        .collect();

    for i in 0..concurrent.len() {
        for j in (i + 1)..concurrent.len() {
            let (cl_a, guard_a) = concurrent[i];
            let (cl_b, guard_b) = concurrent[j];
            if cl_a == cl_b && guard_a != guard_b {
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
}
