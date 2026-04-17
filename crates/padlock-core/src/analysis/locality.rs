// padlock-core/src/analysis/locality.rs

use crate::ir::{AccessPattern, Field, StructLayout};

pub struct FieldLocality<'a> {
    pub field: &'a Field,
    pub is_hot: bool,
}

fn is_hot(f: &Field) -> bool {
    matches!(
        f.access,
        AccessPattern::ReadMostly | AccessPattern::Concurrent { .. }
    )
}

/// Classify every field as hot or cold.
pub fn classify_fields(layout: &StructLayout) -> Vec<FieldLocality<'_>> {
    layout
        .fields
        .iter()
        .map(|f| FieldLocality {
            field: f,
            is_hot: is_hot(f),
        })
        .collect()
}

/// Returns `true` if the layout has a hot/cold locality problem.
///
/// Two conditions are checked:
///
/// 1. **Interleaving** — a hot→cold→hot transition exists (classic case).
/// 2. **Cache-line mixing** — when the struct spans more than one cache line,
///    any cache line that contains both hot and cold fields is a problem even
///    without interleaving.  Architectures with `cache_line_size == 0` (e.g.
///    Cortex-M with no cache) skip check 2.
pub fn has_locality_issue(layout: &StructLayout) -> bool {
    let classified = classify_fields(layout);
    let has_hot = classified.iter().any(|c| c.is_hot);
    let has_cold = classified.iter().any(|c| !c.is_hot);
    if !has_hot || !has_cold {
        return false;
    }

    // Check 1: hot→cold→hot interleaving.
    let mut saw_cold_after_hot = false;
    let mut last_was_hot = false;
    for c in &classified {
        if c.is_hot {
            if saw_cold_after_hot {
                return true;
            }
            last_was_hot = true;
        } else if last_was_hot {
            saw_cold_after_hot = true;
        }
    }

    // Check 2: hot and cold fields share a cache line when the struct spans
    // multiple cache lines.  If the whole struct fits in one cache line it all
    // gets loaded together anyway, so this check adds no signal.
    let cl = layout.arch.cache_line_size;
    if cl > 0 && layout.total_size > cl {
        let mut line_has_hot = std::collections::HashMap::<usize, bool>::new();
        let mut line_has_cold = std::collections::HashMap::<usize, bool>::new();
        for c in &classified {
            let line = c.field.offset / cl;
            if c.is_hot {
                line_has_hot.insert(line, true);
            } else {
                line_has_cold.insert(line, true);
            }
        }
        if line_has_hot
            .keys()
            .any(|line| line_has_cold.contains_key(line))
        {
            return true;
        }
    }

    false
}

/// Split fields into (hot_names, cold_names) preserving original order.
pub fn partition_hot_cold(layout: &StructLayout) -> (Vec<String>, Vec<String>) {
    let mut hot = Vec::new();
    let mut cold = Vec::new();
    for f in &layout.fields {
        if is_hot(f) {
            hot.push(f.name.clone());
        } else {
            cold.push(f.name.clone());
        }
    }
    (hot, cold)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::X86_64_SYSV;
    use crate::ir::{Field, StructLayout, TypeInfo};

    fn field(name: &str, offset: usize, access: AccessPattern) -> Field {
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
            access,
        }
    }

    fn layout(fields: Vec<Field>) -> StructLayout {
        StructLayout {
            name: "T".into(),
            total_size: fields.len() * 8,
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

    #[test]
    fn interleaved_hot_cold_is_issue() {
        // hot cold hot — locality issue
        let l = layout(vec![
            field("a", 0, AccessPattern::ReadMostly),
            field("b", 8, AccessPattern::Unknown),
            field("c", 16, AccessPattern::ReadMostly),
        ]);
        assert!(has_locality_issue(&l));
    }

    #[test]
    fn hot_first_then_cold_is_fine() {
        let l = layout(vec![
            field("a", 0, AccessPattern::ReadMostly),
            field("b", 8, AccessPattern::ReadMostly),
            field("c", 16, AccessPattern::Unknown),
            field("d", 24, AccessPattern::Unknown),
        ]);
        assert!(!has_locality_issue(&l));
    }

    #[test]
    fn all_unknown_no_issue() {
        let l = layout(vec![
            field("a", 0, AccessPattern::Unknown),
            field("b", 8, AccessPattern::Unknown),
        ]);
        assert!(!has_locality_issue(&l));
    }

    #[test]
    fn hot_then_cold_sharing_cache_line_is_issue() {
        // 9 fields × 8B = 72B > 64B cache line.
        // hot field at offset 0 and cold fields at offsets 8–63 all share cache
        // line 0, even though the layout is hot-first (no interleaving).
        let mut fields = vec![field("hot0", 0, AccessPattern::ReadMostly)];
        for i in 1..9usize {
            fields.push(field(&format!("cold{i}"), i * 8, AccessPattern::Unknown));
        }
        let l = layout(fields);
        assert!(
            has_locality_issue(&l),
            "hot and cold sharing a cache line must be flagged"
        );
    }

    #[test]
    fn hot_and_cold_on_separate_cache_lines_is_fine() {
        // All hot fields fit within the first cache line; all cold fields start
        // on the second cache line.  No mixing → no issue.
        // 8 hot fields × 8B = 64B exactly fills cache line 0.
        let mut fields: Vec<Field> = (0usize..8)
            .map(|i| field(&format!("hot{i}"), i * 8, AccessPattern::ReadMostly))
            .collect();
        // cold field starts at offset 64 — exactly cache line 1.
        fields.push(field("cold0", 64, AccessPattern::Unknown));
        // Manually set total_size to 72 (not fields.len()*8 which the helper sets).
        let mut l = layout(fields);
        l.total_size = 72;
        assert!(
            !has_locality_issue(&l),
            "hot/cold on separate cache lines must not be flagged"
        );
    }

    #[test]
    fn hot_then_cold_within_one_cache_line_not_flagged() {
        // Struct fits entirely within one 64B cache line — check 2 does not apply.
        let l = layout(vec![
            field("a", 0, AccessPattern::ReadMostly),
            field("b", 8, AccessPattern::ReadMostly),
            field("c", 16, AccessPattern::Unknown),
            field("d", 24, AccessPattern::Unknown),
        ]);
        assert!(!has_locality_issue(&l));
    }

    #[test]
    fn partition_separates_correctly() {
        let l = layout(vec![
            field("a", 0, AccessPattern::ReadMostly),
            field("b", 8, AccessPattern::Unknown),
            field(
                "c",
                16,
                AccessPattern::Concurrent {
                    guard: None,
                    is_atomic: true,
                    is_annotated: false,
                },
            ),
        ]);
        let (hot, cold) = partition_hot_cold(&l);
        assert_eq!(hot, vec!["a", "c"]);
        assert_eq!(cold, vec!["b"]);
    }
}
