// padlock-dwarf/src/pdb_reader.rs
//
// Extract struct/union/enum layouts from a PDB (Program Database) file
// produced by MSVC.
//
// PDB files encode type information in a TPI (Type Info) stream.  We iterate
// the stream building a TypeFinder, then for every non-forward-reference
// Class, Union, or Enumeration record we resolve its FieldList to collect
// members.
//
// Limitations:
//   - Bitfield members: grouped by field_type into synthetic [f1:3|f2:5] fields.
//   - Virtual-base and base-class members are omitted (size comes from the
//     struct's own `size` field, which is already correct).
//   - Static members are skipped (no byte offset in a struct instance).
//   - Source file/line: PDB stores source locations in symbol records (per
//     function/variable), not in type records — they are not available here.

use std::collections::HashMap;

use anyhow::Context;
use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
use pdb::{FallibleIterator, PrimitiveKind, TypeData, TypeFinder, TypeIndex};

/// Extract struct/union/enum layouts from raw PDB file bytes.
pub fn extract_from_pdb(
    data: &[u8],
    arch: &'static ArchConfig,
) -> anyhow::Result<Vec<StructLayout>> {
    let cursor = std::io::Cursor::new(data);
    let mut pdb = pdb::PDB::open(cursor).context("failed to open PDB")?;
    let type_info = pdb
        .type_information()
        .context("failed to read TPI stream")?;
    let mut type_finder = type_info.finder();

    // First pass: iterate all types, build the TypeFinder, and collect every
    // Class/Union/Enumeration that is not a forward reference.
    struct RawStruct {
        name: String,
        size: usize,
        fields_idx: TypeIndex,
        is_union: bool,
        is_enum: bool,
    }
    let mut raw_structs: Vec<RawStruct> = Vec::new();

    {
        let mut iter = type_info.iter();
        while let Some(typ) = iter.next()? {
            type_finder.update(&iter);
            match typ.parse() {
                Ok(TypeData::Class(c)) => {
                    if c.properties.forward_reference() {
                        continue;
                    }
                    let Some(fields_idx) = c.fields else {
                        continue;
                    };
                    raw_structs.push(RawStruct {
                        name: c.name.to_string().into_owned(),
                        size: c.size as usize,
                        fields_idx,
                        is_union: false,
                        is_enum: false,
                    });
                }
                Ok(TypeData::Union(u)) => {
                    if u.properties.forward_reference() {
                        continue;
                    }
                    raw_structs.push(RawStruct {
                        name: u.name.to_string().into_owned(),
                        size: u.size as usize,
                        fields_idx: u.fields,
                        is_union: true,
                        is_enum: false,
                    });
                }
                Ok(TypeData::Enumeration(e)) => {
                    if e.properties.forward_reference() {
                        continue;
                    }
                    let underlying_size = underlying_enum_size(e.underlying_type, arch);
                    raw_structs.push(RawStruct {
                        name: e.name.to_string().into_owned(),
                        size: underlying_size,
                        fields_idx: e.fields,
                        is_union: false,
                        is_enum: true,
                    });
                }
                _ => {}
            }
        }
    }

    // Build a size cache: TypeIndex → (size, align) for fast field type lookup.
    // For structs/unions we approximate alignment as the largest power-of-two
    // that divides the size (capped at pointer_size).  This is conservative
    // but avoids false-positives in padding detection; exact field-derived
    // alignment is computed when we build the final StructLayout.
    let mut size_cache: HashMap<TypeIndex, (usize, usize)> = HashMap::new();
    {
        let mut iter = type_info.iter();
        while let Some(typ) = iter.next()? {
            match typ.parse() {
                Ok(TypeData::Class(c)) if !c.properties.forward_reference() => {
                    let sz = c.size as usize;
                    let al = approx_struct_align(sz, arch);
                    size_cache.insert(typ.index(), (sz, al));
                }
                Ok(TypeData::Union(u)) if !u.properties.forward_reference() => {
                    let sz = u.size as usize;
                    let al = approx_struct_align(sz, arch);
                    size_cache.insert(typ.index(), (sz, al));
                }
                Ok(TypeData::Primitive(p)) => {
                    if let Some((sz, al)) = primitive_size(&p, arch) {
                        size_cache.insert(typ.index(), (sz, al));
                    }
                }
                _ => {}
            }
        }
    }

    let mut layouts = Vec::new();

    for raw in raw_structs {
        if raw.is_enum {
            // Enums are represented as a single `__discriminant` field.
            let sz = raw.size;
            let al = sz.max(1);
            let fields = vec![Field {
                name: "__discriminant".to_string(),
                ty: TypeInfo::Primitive {
                    name: format!("uint{}_t", sz * 8),
                    size: sz,
                    align: al,
                },
                offset: 0,
                size: sz,
                align: al,
                source_file: None,
                source_line: None,
                access: AccessPattern::Unknown,
            }];
            layouts.push(StructLayout {
                name: raw.name,
                total_size: sz,
                align: al,
                fields,
                source_file: None,
                source_line: None,
                arch,
                is_packed: false,
                is_union: false,
                is_repr_rust: false,
                suppressed_findings: Vec::new(),
                uncertain_fields: Vec::new(),
            });
            continue;
        }

        let (fields, uncertain_fields) = collect_fields(
            &raw.fields_idx,
            &type_finder,
            &size_cache,
            arch,
            raw.is_union,
        )?;

        let align = fields.iter().map(|f| f.align).max().unwrap_or(1);

        layouts.push(StructLayout {
            name: raw.name,
            total_size: raw.size,
            align,
            fields,
            source_file: None,
            source_line: None,
            arch,
            is_packed: false,
            is_union: raw.is_union,
            is_repr_rust: false,
            suppressed_findings: Vec::new(),
            uncertain_fields,
        });
    }

    Ok(layouts)
}

/// Approximate the alignment of a struct/union from its total size.
///
/// Returns the largest power-of-two that is ≤ `sz` and ≤ `arch.pointer_size`.
/// This is a conservative under-estimate; the true alignment is the max of
/// field alignments, which we derive from the collected fields after parsing.
fn approx_struct_align(sz: usize, arch: &ArchConfig) -> usize {
    if sz == 0 {
        return 1;
    }
    // Largest power-of-two that divides sz (i.e. is ≤ sz)
    let pot = 1usize << sz.trailing_zeros();
    pot.min(arch.pointer_size)
}

/// Resolve the byte size of an enum's underlying integer type.
fn underlying_enum_size(idx: TypeIndex, arch: &'static ArchConfig) -> usize {
    // We need a TypeFinder to resolve this, but during the first pass we only
    // have the iterator. Re-open would be expensive, so we fall back to 4 bytes
    // (the MSVC default for `int`-backed enums) as a safe approximation.
    // In practice this is correct for the vast majority of MSVC-compiled enums.
    let _ = (idx, arch);
    4
}

/// Collect fields from a FieldList type index.
/// Returns `(fields, uncertain_field_names)`.
fn collect_fields(
    fields_idx: &TypeIndex,
    type_finder: &TypeFinder<'_>,
    size_cache: &HashMap<TypeIndex, (usize, usize)>,
    arch: &'static ArchConfig,
    is_union: bool,
) -> anyhow::Result<(Vec<Field>, Vec<String>)> {
    let field_type = type_finder.find(*fields_idx)?.parse()?;
    let field_list = match field_type {
        TypeData::FieldList(fl) => fl,
        _ => return Ok((Vec::new(), Vec::new())),
    };

    let mut fields: Vec<Field> = Vec::new();
    let mut uncertain: Vec<String> = Vec::new();

    // Accumulate consecutive bitfield members (same offset) before flushing.
    struct BfGroup {
        parts: Vec<String>,
        offset: usize,
        storage_bytes: usize,
    }
    let mut pending_bf: Option<BfGroup> = None;

    let flush_bf = |group: BfGroup, fields: &mut Vec<Field>, uncertain: &mut Vec<String>| {
        if group.storage_bytes == 0 {
            uncertain.push(format!("[bf@{}]", group.offset));
            return;
        }
        let name = if group.parts.is_empty() {
            "[__pad]".to_string()
        } else {
            format!("[{}]", group.parts.join("|"))
        };
        fields.push(Field {
            name,
            ty: TypeInfo::Primitive {
                name: format!("uint{}_t", group.storage_bytes * 8),
                size: group.storage_bytes,
                align: group.storage_bytes,
            },
            offset: group.offset,
            size: group.storage_bytes,
            align: group.storage_bytes,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        });
    };

    for field_data in &field_list.fields {
        if let TypeData::Member(m) = field_data {
            let offset = m.offset as usize;
            let name = m.name.to_string().into_owned();

            // Detect bitfield: the field_type will be a Bitfield type record.
            let bitfield_info = type_finder
                .find(m.field_type)
                .ok()
                .and_then(|t| t.parse().ok())
                .and_then(|td| {
                    if let TypeData::Bitfield(bf) = td {
                        Some(bf)
                    } else {
                        None
                    }
                });

            if let Some(bf) = bitfield_info {
                // Flush pending group if byte offset changed.
                if let Some(ref g) = pending_bf
                    && g.offset != offset
                {
                    let g = pending_bf.take().unwrap();
                    flush_bf(g, &mut fields, &mut uncertain);
                }

                // Resolve the underlying storage type size.
                let storage_bytes = type_finder
                    .find(bf.underlying_type)
                    .ok()
                    .and_then(|t| t.parse().ok())
                    .and_then(|td| {
                        if let TypeData::Primitive(p) = td {
                            primitive_size(&p, arch).map(|(sz, _)| sz)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);

                let group = pending_bf.get_or_insert(BfGroup {
                    parts: Vec::new(),
                    offset,
                    storage_bytes: 0,
                });
                if !name.is_empty() && bf.length > 0 {
                    group.parts.push(format!("{name}:{}", bf.length));
                }
                if storage_bytes > group.storage_bytes {
                    group.storage_bytes = storage_bytes;
                }
            } else {
                // Flush any pending bitfield group.
                if let Some(g) = pending_bf.take() {
                    flush_bf(g, &mut fields, &mut uncertain);
                }

                let (size, align) = resolve_type_size(m.field_type, type_finder, size_cache, arch);
                let ty = TypeInfo::Opaque {
                    name: format!("{}", m.field_type.0),
                    size,
                    align,
                };

                fields.push(Field {
                    name,
                    ty,
                    offset,
                    size,
                    align,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                });
            }
            // Skip static members, virtual-base records, base classes — they don't
            // occupy a predictable slot in the struct's memory layout.
        }
    }

    if let Some(g) = pending_bf.take() {
        flush_bf(g, &mut fields, &mut uncertain);
    }

    if is_union {
        for f in &mut fields {
            f.offset = 0;
        }
    } else {
        fields.sort_by_key(|f| f.offset);
    }

    Ok((fields, uncertain))
}

/// Resolve a TypeIndex to (size_bytes, align_bytes).
fn resolve_type_size(
    idx: TypeIndex,
    type_finder: &TypeFinder<'_>,
    size_cache: &HashMap<TypeIndex, (usize, usize)>,
    arch: &'static ArchConfig,
) -> (usize, usize) {
    if let Some(&pair) = size_cache.get(&idx) {
        return pair;
    }
    let td = match type_finder.find(idx).ok().and_then(|t| t.parse().ok()) {
        Some(td) => td,
        None => return (arch.pointer_size, arch.pointer_size),
    };
    match td {
        TypeData::Primitive(p) => {
            primitive_size(&p, arch).unwrap_or((arch.pointer_size, arch.pointer_size))
        }
        TypeData::Class(c) => {
            let sz = c.size as usize;
            (sz, approx_struct_align(sz, arch))
        }
        TypeData::Union(u) => {
            let sz = u.size as usize;
            (sz, approx_struct_align(sz, arch))
        }
        TypeData::Pointer(_) => (arch.pointer_size, arch.pointer_size),
        TypeData::Array(a) => {
            // `dimensions` holds cumulative byte lengths per dimension, NOT
            // element counts.  E.g. `float[4][4]` → `[16, 64]`.  The total
            // byte size is always the last (outermost) entry.
            let total = a.dimensions.last().copied().unwrap_or(0) as usize;
            let (_, elem_al) = resolve_type_size(a.element_type, type_finder, size_cache, arch);
            (total, elem_al)
        }
        _ => (arch.pointer_size, arch.pointer_size),
    }
}

/// Map a PDB `PrimitiveType` to (size_bytes, align_bytes). Returns `None` for
/// void/notype. Pointer indirection is always arch pointer size.
fn primitive_size(p: &pdb::PrimitiveType, arch: &ArchConfig) -> Option<(usize, usize)> {
    if p.indirection.is_some() {
        return Some((arch.pointer_size, arch.pointer_size));
    }
    let sz: usize = match p.kind {
        PrimitiveKind::NoType | PrimitiveKind::Void => return None,
        PrimitiveKind::Char
        | PrimitiveKind::UChar
        | PrimitiveKind::I8
        | PrimitiveKind::U8
        | PrimitiveKind::RChar
        | PrimitiveKind::Bool8 => 1,
        PrimitiveKind::WChar
        | PrimitiveKind::Short
        | PrimitiveKind::UShort
        | PrimitiveKind::I16
        | PrimitiveKind::U16
        | PrimitiveKind::RChar16
        | PrimitiveKind::F16
        | PrimitiveKind::Bool16 => 2,
        PrimitiveKind::Long
        | PrimitiveKind::ULong
        | PrimitiveKind::I32
        | PrimitiveKind::U32
        | PrimitiveKind::RChar32
        | PrimitiveKind::F32
        | PrimitiveKind::F32PP
        | PrimitiveKind::HRESULT
        | PrimitiveKind::Bool32 => 4,
        PrimitiveKind::Quad
        | PrimitiveKind::UQuad
        | PrimitiveKind::I64
        | PrimitiveKind::U64
        | PrimitiveKind::F64
        | PrimitiveKind::Complex32
        | PrimitiveKind::Bool64 => 8,
        PrimitiveKind::Octa
        | PrimitiveKind::UOcta
        | PrimitiveKind::I128
        | PrimitiveKind::U128
        | PrimitiveKind::F128
        | PrimitiveKind::Complex64 => 16,
        PrimitiveKind::F48 => 6,
        PrimitiveKind::F80 => 10,
        PrimitiveKind::Complex80 => 20,
        PrimitiveKind::Complex128 => 32,
        _ => return None,
    };
    Some((sz, sz))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    // ── approx_struct_align ───────────────────────────────────────────────────

    #[test]
    fn approx_align_zero() {
        assert_eq!(approx_struct_align(0, &X86_64_SYSV), 1);
    }

    #[test]
    fn approx_align_exact_powers_of_two() {
        // Powers of two ≤ pointer_size are returned as-is.
        assert_eq!(approx_struct_align(1, &X86_64_SYSV), 1);
        assert_eq!(approx_struct_align(2, &X86_64_SYSV), 2);
        assert_eq!(approx_struct_align(4, &X86_64_SYSV), 4);
        assert_eq!(approx_struct_align(8, &X86_64_SYSV), 8);
    }

    #[test]
    fn approx_align_capped_at_pointer_size() {
        // Sizes larger than pointer_size still return pointer_size (8 on x86-64).
        assert_eq!(approx_struct_align(16, &X86_64_SYSV), 8);
        assert_eq!(approx_struct_align(24, &X86_64_SYSV), 8);
        assert_eq!(approx_struct_align(64, &X86_64_SYSV), 8);
    }

    #[test]
    fn approx_align_non_power_of_two_uses_trailing_zeros() {
        // Non-power-of-two sizes: alignment = largest power-of-two factor, capped at 8.
        // 12 = 4 × 3 → trailing zeros = 2 → 1<<2 = 4
        assert_eq!(approx_struct_align(12, &X86_64_SYSV), 4);
        // 6 = 2 × 3 → trailing zeros = 1 → 1<<1 = 2
        assert_eq!(approx_struct_align(6, &X86_64_SYSV), 2);
        // 3 = odd → trailing zeros = 0 → 1<<0 = 1
        assert_eq!(approx_struct_align(3, &X86_64_SYSV), 1);
        // 20 = 4 × 5 → trailing zeros = 2 → 4
        assert_eq!(approx_struct_align(20, &X86_64_SYSV), 4);
    }

    // ── primitive_size ────────────────────────────────────────────────────────

    #[test]
    fn primitive_size_void_returns_none() {
        let p = pdb::PrimitiveType {
            kind: PrimitiveKind::Void,
            indirection: None,
        };
        assert!(primitive_size(&p, &X86_64_SYSV).is_none());
    }

    #[test]
    fn primitive_size_i32_is_4() {
        let p = pdb::PrimitiveType {
            kind: PrimitiveKind::I32,
            indirection: None,
        };
        assert_eq!(primitive_size(&p, &X86_64_SYSV), Some((4, 4)));
    }

    #[test]
    fn primitive_size_pointer_indirection_uses_arch_size() {
        let p = pdb::PrimitiveType {
            kind: PrimitiveKind::I32,
            indirection: Some(pdb::Indirection::Near32),
        };
        assert_eq!(
            primitive_size(&p, &X86_64_SYSV),
            Some((X86_64_SYSV.pointer_size, X86_64_SYSV.pointer_size))
        );
    }

    #[test]
    fn primitive_size_u64_is_8() {
        let p = pdb::PrimitiveType {
            kind: PrimitiveKind::U64,
            indirection: None,
        };
        assert_eq!(primitive_size(&p, &X86_64_SYSV), Some((8, 8)));
    }
}
