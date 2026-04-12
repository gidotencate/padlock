// padlock-source/src/concurrency.rs
//
// Heuristic pass: annotate field AccessPatterns based on known concurrency
// type names in Rust, C++, and Go.

use padlock_core::ir::{AccessPattern, StructLayout};

use crate::SourceLanguage;

/// Update `AccessPattern` for fields whose type names suggest concurrent access.
///
/// This is a best-effort heuristic: it matches well-known synchronisation
/// wrapper names (`Mutex<T>`, `std::atomic<T>`, `sync.Mutex`, etc.) found in
/// the `TypeInfo` name of each field.
pub fn annotate_concurrency(layout: &mut StructLayout, language: &SourceLanguage) {
    for field in &mut layout.fields {
        let ty_name = match &field.ty {
            padlock_core::ir::TypeInfo::Primitive { name, .. }
            | padlock_core::ir::TypeInfo::Opaque { name, .. } => name.clone(),
            _ => continue,
        };

        if is_concurrent_type(&ty_name, language) {
            let is_atomic = is_atomic_type(&ty_name, language);
            if matches!(field.access, AccessPattern::Unknown) {
                // Use the field name as the guard so that two concurrent fields
                // with different names on the same cache line are flagged as
                // false-sharing candidates (different guards → different data).
                field.access = AccessPattern::Concurrent {
                    guard: Some(field.name.clone()),
                    is_atomic,
                };
            }
        } else if is_read_mostly_type(&ty_name, language)
            && matches!(field.access, AccessPattern::Unknown)
        {
            field.access = AccessPattern::ReadMostly;
        }
    }
}

/// Returns `true` if any field has a `Concurrent` access pattern.
pub fn has_concurrent_fields(layout: &StructLayout) -> bool {
    layout
        .fields
        .iter()
        .any(|f| matches!(f.access, AccessPattern::Concurrent { .. }))
}

fn is_concurrent_type(name: &str, lang: &SourceLanguage) -> bool {
    match lang {
        SourceLanguage::Rust => {
            name.starts_with("Mutex")
                || name.starts_with("RwLock")
                || name.starts_with("Arc")
                || name.contains("Atomic")
                || name.starts_with("Condvar")
                || name.starts_with("Once")
        }
        SourceLanguage::C | SourceLanguage::Cpp => {
            name.contains("mutex")
                || name.contains("atomic")
                || name.contains("spinlock")
                || name.contains("critical_section")
                || name.contains("pthread_mutex")
        }
        SourceLanguage::Go => {
            name == "sync.Mutex"
                || name == "sync.RWMutex"
                || name == "Mutex"
                || name == "RWMutex"
                || name.contains("atomic")
        }
        SourceLanguage::Zig => {
            name.contains("Mutex")
                || name.contains("RwLock")
                || name.contains("atomic.Value")
                || name.contains("Atomic")
        }
    }
}

fn is_atomic_type(name: &str, lang: &SourceLanguage) -> bool {
    match lang {
        SourceLanguage::Rust => name.contains("Atomic"),
        SourceLanguage::C | SourceLanguage::Cpp => name.contains("atomic"),
        SourceLanguage::Go => name.contains("atomic"),
        SourceLanguage::Zig => name.contains("atomic.Value") || name.contains("Atomic"),
    }
}

fn is_read_mostly_type(name: &str, lang: &SourceLanguage) -> bool {
    match lang {
        SourceLanguage::Rust => name.starts_with("RwLock"),
        SourceLanguage::C | SourceLanguage::Cpp => {
            name.contains("rwlock") || name.contains("shared_mutex")
        }
        SourceLanguage::Go => name == "sync.RWMutex" || name == "RWMutex",
        SourceLanguage::Zig => name.contains("RwLock"),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;
    use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};

    fn field_with_type(name: &str, ty_name: &str) -> Field {
        Field {
            name: name.into(),
            ty: TypeInfo::Primitive {
                name: ty_name.into(),
                size: 8,
                align: 8,
            },
            offset: 0,
            size: 8,
            align: 8,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        }
    }

    fn layout_with_fields(fields: Vec<Field>) -> StructLayout {
        StructLayout {
            name: "T".into(),
            total_size: 64,
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

    #[test]
    fn rust_mutex_field_is_annotated() {
        let mut layout = layout_with_fields(vec![field_with_type("counter", "Mutex")]);
        annotate_concurrency(&mut layout, &SourceLanguage::Rust);
        assert!(matches!(
            layout.fields[0].access,
            AccessPattern::Concurrent { .. }
        ));
    }

    #[test]
    fn rust_atomic_is_atomic() {
        let mut layout = layout_with_fields(vec![field_with_type("count", "AtomicU64")]);
        annotate_concurrency(&mut layout, &SourceLanguage::Rust);
        if let AccessPattern::Concurrent { is_atomic, .. } = &layout.fields[0].access {
            assert!(is_atomic);
        } else {
            panic!("expected Concurrent");
        }
    }

    #[test]
    fn cpp_mutex_annotated() {
        let mut layout = layout_with_fields(vec![field_with_type("mu", "std::mutex")]);
        annotate_concurrency(&mut layout, &SourceLanguage::Cpp);
        assert!(has_concurrent_fields(&layout));
    }

    #[test]
    fn unknown_field_stays_unknown() {
        let mut layout = layout_with_fields(vec![field_with_type("x", "int")]);
        annotate_concurrency(&mut layout, &SourceLanguage::C);
        assert!(matches!(layout.fields[0].access, AccessPattern::Unknown));
    }

    #[test]
    fn has_concurrent_fields_false_when_none() {
        let layout = layout_with_fields(vec![field_with_type("x", "int")]);
        assert!(!has_concurrent_fields(&layout));
    }
}
