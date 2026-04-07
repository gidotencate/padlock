//padlock-core/src/ir.rs

pub use crate::arch::{ArchConfig, X86_64_SYSV};

/// The type of a single field. Recursive for nested structs.
#[derive(Debug, Clone)]
pub enum TypeInfo {
    Primitive {
        name: String,
        size: usize,
        align: usize,
    },
    Pointer {
        size: usize,
        align: usize,
    },
    Array {
        element: Box<TypeInfo>,
        count: usize,
        size: usize,
        align: usize,
    },
    Struct(Box<StructLayout>),
    Opaque {
        name: String,
        size: usize,
        align: usize,
    },
}

impl TypeInfo {
    pub fn size(&self) -> usize {
        match self {
            TypeInfo::Primitive { size, .. } => *size,
            TypeInfo::Pointer { size, .. } => *size,
            TypeInfo::Array { size, .. } => *size,
            TypeInfo::Struct(l) => l.total_size,
            TypeInfo::Opaque { size, .. } => *size,
        }
    }

    pub fn align(&self) -> usize {
        match self {
            TypeInfo::Primitive { align, .. } => *align,
            TypeInfo::Pointer { align, .. } => *align,
            TypeInfo::Array { align, .. } => *align,
            TypeInfo::Struct(l) => l.align,
            TypeInfo::Opaque { align, .. } => *align,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum AccessPattern {
    Unknown,
    Concurrent {
        guard: Option<String>,
        is_atomic: bool,
    },
    ReadMostly,
    Padding,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: TypeInfo,
    pub offset: usize,
    pub size: usize,
    pub align: usize,
    pub source_file: Option<String>,
    pub source_line: Option<u32>,
    pub access: AccessPattern,
}

/// One complete struct as read from DWARF or source and enriched by analysis.
#[derive(Debug, Clone)]
pub struct StructLayout {
    pub name: String,
    pub total_size: usize,
    pub align: usize,
    pub fields: Vec<Field>,
    pub source_file: Option<String>,
    pub source_line: Option<u32>,
    pub arch: &'static ArchConfig,
    pub is_packed: bool,
    /// True when this layout was parsed from a C/C++ `union` declaration.
    /// All fields share the same base offset (0); analysis suppresses reorder
    /// and padding findings that do not apply to unions.
    pub is_union: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaddingGap {
    pub after_field: String,
    pub bytes: usize,
    pub at_offset: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SharingConflict {
    pub fields: Vec<String>,
    pub cache_line: usize,
}

/// Find all padding gaps between consecutive fields.
///
/// Returns an empty vec for union layouts — all fields share offset 0 by
/// definition, so the concept of inter-field padding does not apply.
pub fn find_padding(layout: &StructLayout) -> Vec<PaddingGap> {
    if layout.is_union {
        return Vec::new();
    }
    let mut gaps = Vec::new();
    for window in layout.fields.windows(2) {
        let current = &window[0];
        let next = &window[1];
        let end = current.offset + current.size;
        if next.offset > end {
            gaps.push(PaddingGap {
                after_field: current.name.clone(),
                bytes: next.offset - end,
                at_offset: end,
            });
        }
    }
    // Trailing padding: struct total_size > last field end
    if let Some(last) = layout.fields.last() {
        let end = last.offset + last.size;
        if layout.total_size > end {
            gaps.push(PaddingGap {
                after_field: last.name.clone(),
                bytes: layout.total_size - end,
                at_offset: end,
            });
        }
    }
    gaps
}

/// Return fields sorted by descending alignment then descending size (optimal order).
pub fn optimal_order(layout: &StructLayout) -> Vec<&Field> {
    let mut sorted: Vec<&Field> = layout.fields.iter().collect();
    sorted.sort_by(|a, b| {
        b.align
            .cmp(&a.align)
            .then(b.size.cmp(&a.size))
            .then(a.name.cmp(&b.name))
    });
    sorted
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_fixtures {
    use super::*;
    use crate::arch::X86_64_SYSV;

    /// The canonical misaligned layout used across crate tests.
    ///   is_active: bool  offset 0,  size 1, align 1
    ///   [7 bytes padding]
    ///   timeout:   f64   offset 8,  size 8, align 8
    ///   is_tls:    bool  offset 16, size 1, align 1
    ///   [3 bytes padding]
    ///   port:      i32   offset 20, size 4, align 4
    ///   total_size 24
    pub fn connection_layout() -> StructLayout {
        StructLayout {
            name: "Connection".to_string(),
            total_size: 24,
            align: 8,
            fields: vec![
                Field {
                    name: "is_active".into(),
                    ty: TypeInfo::Primitive {
                        name: "bool".into(),
                        size: 1,
                        align: 1,
                    },
                    offset: 0,
                    size: 1,
                    align: 1,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "timeout".into(),
                    ty: TypeInfo::Primitive {
                        name: "f64".into(),
                        size: 8,
                        align: 8,
                    },
                    offset: 8,
                    size: 8,
                    align: 8,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "is_tls".into(),
                    ty: TypeInfo::Primitive {
                        name: "bool".into(),
                        size: 1,
                        align: 1,
                    },
                    offset: 16,
                    size: 1,
                    align: 1,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "port".into(),
                    ty: TypeInfo::Primitive {
                        name: "i32".into(),
                        size: 4,
                        align: 4,
                    },
                    offset: 20,
                    size: 4,
                    align: 4,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
            ],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
        }
    }

    /// A perfectly packed layout (no padding anywhere).
    pub fn packed_layout() -> StructLayout {
        StructLayout {
            name: "Packed".to_string(),
            total_size: 8,
            align: 4,
            fields: vec![
                Field {
                    name: "a".into(),
                    ty: TypeInfo::Primitive {
                        name: "i32".into(),
                        size: 4,
                        align: 4,
                    },
                    offset: 0,
                    size: 4,
                    align: 4,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "b".into(),
                    ty: TypeInfo::Primitive {
                        name: "i16".into(),
                        size: 2,
                        align: 2,
                    },
                    offset: 4,
                    size: 2,
                    align: 2,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "c".into(),
                    ty: TypeInfo::Primitive {
                        name: "i16".into(),
                        size: 2,
                        align: 2,
                    },
                    offset: 6,
                    size: 2,
                    align: 2,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
            ],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
        }
    }

    #[test]
    fn test_find_padding_connection() {
        let layout = connection_layout();
        let gaps = find_padding(&layout);
        assert_eq!(
            gaps,
            vec![
                PaddingGap {
                    after_field: "is_active".into(),
                    bytes: 7,
                    at_offset: 1
                },
                PaddingGap {
                    after_field: "is_tls".into(),
                    bytes: 3,
                    at_offset: 17
                },
            ]
        );
    }

    #[test]
    fn test_find_padding_packed() {
        let layout = packed_layout();
        assert!(find_padding(&layout).is_empty());
    }

    #[test]
    fn test_optimal_order() {
        let layout = connection_layout();
        let order: Vec<&str> = optimal_order(&layout)
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        // timeout (align 8) first, then port (align 4), then bools (align 1)
        assert_eq!(order[0], "timeout");
        assert_eq!(order[1], "port");
        assert!(order[2] == "is_active" || order[2] == "is_tls");
    }
}
