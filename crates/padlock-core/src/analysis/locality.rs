// padlock-core/src/analysis/locality.rs

use crate::ir::{AccessPattern, Field, StructLayout};

pub struct FieldLocality<'a> {
    pub field: &'a Field,
    pub is_hot: bool,
}

fn is_hot(f: &Field) -> bool {
    matches!(f.access, AccessPattern::ReadMostly | AccessPattern::Concurrent { .. })
}

/// Classify every field as hot or cold.
pub fn classify_fields(layout: &StructLayout) -> Vec<FieldLocality<'_>> {
    layout
        .fields
        .iter()
        .map(|f| FieldLocality { field: f, is_hot: is_hot(f) })
        .collect()
}

/// Returns `true` if hot and cold fields are interleaved (a locality problem).
/// If all fields are hot or all are cold the layout is fine.
pub fn has_locality_issue(layout: &StructLayout) -> bool {
    let classified = classify_fields(layout);
    let has_hot  = classified.iter().any(|c| c.is_hot);
    let has_cold = classified.iter().any(|c| !c.is_hot);
    if !has_hot || !has_cold {
        return false;
    }
    // Scan for a cold→hot or hot→cold→hot transition (interleaving).
    let mut saw_cold_after_hot = false;
    let mut last_was_hot = false;
    for c in &classified {
        if c.is_hot {
            if saw_cold_after_hot {
                return true; // hot field appears after a cold field that followed a hot field
            }
            last_was_hot = true;
        } else {
            if last_was_hot {
                saw_cold_after_hot = true;
            }
        }
    }
    false
}

/// Split fields into (hot_names, cold_names) preserving original order.
pub fn partition_hot_cold(layout: &StructLayout) -> (Vec<String>, Vec<String>) {
    let mut hot  = Vec::new();
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
            ty: TypeInfo::Primitive { name: "u64".into(), size: 8, align: 8 },
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
        }
    }

    #[test]
    fn interleaved_hot_cold_is_issue() {
        // hot cold hot — locality issue
        let l = layout(vec![
            field("a", 0,  AccessPattern::ReadMostly),
            field("b", 8,  AccessPattern::Unknown),
            field("c", 16, AccessPattern::ReadMostly),
        ]);
        assert!(has_locality_issue(&l));
    }

    #[test]
    fn hot_first_then_cold_is_fine() {
        let l = layout(vec![
            field("a", 0,  AccessPattern::ReadMostly),
            field("b", 8,  AccessPattern::ReadMostly),
            field("c", 16, AccessPattern::Unknown),
            field("d", 24, AccessPattern::Unknown),
        ]);
        assert!(!has_locality_issue(&l));
    }

    #[test]
    fn all_unknown_no_issue() {
        let l = layout(vec![
            field("a", 0,  AccessPattern::Unknown),
            field("b", 8,  AccessPattern::Unknown),
        ]);
        assert!(!has_locality_issue(&l));
    }

    #[test]
    fn partition_separates_correctly() {
        let l = layout(vec![
            field("a", 0,  AccessPattern::ReadMostly),
            field("b", 8,  AccessPattern::Unknown),
            field("c", 16, AccessPattern::Concurrent { guard: None, is_atomic: true }),
        ]);
        let (hot, cold) = partition_hot_cold(&l);
        assert_eq!(hot,  vec!["a", "c"]);
        assert_eq!(cold, vec!["b"]);
    }
}
