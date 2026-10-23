// padlock-dwarf/src/type_resolver.rs

use gimli::{Reader, Unit, UnitOffset};
use padlock_core::ir::TypeInfo;

use crate::extractor::Extractor;

impl<'a, R: Reader> Extractor<'a, R> {
    /// Follow a type reference and return (size, align, TypeInfo).
    pub fn resolve_type(
        &self,
        unit: &Unit<R>,
        offset: UnitOffset<R::Offset>,
    ) -> anyhow::Result<(usize, usize, TypeInfo)> {
        let mut entries = unit.entries_at_offset(offset)?;
        let entry = entries
            .next_dfs()?
            .ok_or_else(|| anyhow::anyhow!("no DIE at offset"))?;

        match entry.tag() {
            gimli::DW_TAG_base_type => {
                let name = self
                    .attr_string(unit, entry, gimli::DW_AT_name)?
                    .unwrap_or_default();
                let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?.unwrap_or(0);
                let align = self
                    .attr_usize(entry, gimli::DW_AT_alignment)?
                    .unwrap_or(size);

                Ok((size, align, TypeInfo::Primitive { name, size, align }))
            }

            gimli::DW_TAG_pointer_type
            | gimli::DW_TAG_reference_type
            | gimli::DW_TAG_rvalue_reference_type => {
                let size = self.arch.pointer_size;
                Ok((size, size, TypeInfo::Pointer { size, align: size }))
            }

            gimli::DW_TAG_typedef => self
                .resolve_type_attr(unit, entry)?
                .ok_or_else(|| anyhow::anyhow!("typedef with no type")),

            gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type => {
                // _Atomic T (C11): DW_TAG_atomic_type wraps the base type exactly like
                // const/volatile. Without this arm it fell to the `_` catch-all which
                // reads DW_AT_byte_size on the atomic wrapper itself — that attribute is
                // absent on qualifier tags, so the field was sized as 0 bytes and the
                // gap after it appeared 4–8 bytes too large.
                self.resolve_type_attr(unit, entry)?
                    .ok_or_else(|| anyhow::anyhow!("qualifier with no type"))
            }

            gimli::DW_TAG_array_type => {
                let (elem_size, elem_align, elem_ty) = self
                    .resolve_type_attr(unit, entry)?
                    .ok_or_else(|| anyhow::anyhow!("array with no element type"))?;

                let count = self.extract_array_count(unit, entry)?;
                let total = elem_size * count;

                Ok((
                    total,
                    elem_align,
                    TypeInfo::Array {
                        element: Box::new(elem_ty),
                        count,
                        size: total,
                        align: elem_align,
                    },
                ))
            }

            // DW_TAG_class_type is the DWARF tag for C++ `class` declarations;
            // it has identical layout semantics to DW_TAG_structure_type.
            gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type => {
                if let Some(layout) = self.extract_struct(unit, entry)? {
                    let size = layout.total_size;
                    let align = layout.align;
                    Ok((size, align, TypeInfo::Struct(Box::new(layout))))
                } else {
                    let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?.unwrap_or(0);
                    Ok((
                        size,
                        size,
                        TypeInfo::Opaque {
                            name: "<incomplete>".to_string(),
                            size,
                            align: size,
                        },
                    ))
                }
            }

            // A union's alignment is its strictest member's, not its size
            // (`union { char c[13]; long l; }` is 16 bytes, aligned to 8).
            gimli::DW_TAG_union_type => {
                let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?.unwrap_or(0);
                let mut align = self.attr_usize(entry, gimli::DW_AT_alignment)?.unwrap_or(0);
                if align == 0 {
                    let mut tree = unit.entries_tree(Some(entry.offset()))?;
                    let root = tree.root()?;
                    let mut children = root.children();
                    while let Some(child) = children.next()? {
                        let e = child.entry();
                        if e.tag() == gimli::DW_TAG_member
                            && let Some((_, a, _)) = self.resolve_type_attr(unit, e)?
                        {
                            align = align.max(a);
                        }
                    }
                }
                let name = self
                    .attr_string(unit, entry, gimli::DW_AT_name)?
                    .unwrap_or_default();
                let align = align.max(1);
                Ok((size, align, TypeInfo::Opaque { name, size, align }))
            }

            // C++ pointer to member: a data-member pointer is one word, a
            // member-function pointer two (function pointer + `this` adjustment).
            gimli::DW_TAG_ptr_to_member_type => {
                let ps = self.arch.pointer_size;
                let to_function = match entry.attr_value(gimli::DW_AT_type) {
                    Some(gimli::AttributeValue::UnitRef(off)) => unit
                        .entries_at_offset(off)?
                        .next_dfs()?
                        .is_some_and(|e| e.tag() == gimli::DW_TAG_subroutine_type),
                    _ => false,
                };
                let size = if to_function { ps * 2 } else { ps };
                Ok((size, ps, TypeInfo::Pointer { size, align: ps }))
            }

            gimli::DW_TAG_enumeration_type => {
                let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?.unwrap_or(4);
                let name = self
                    .attr_string(unit, entry, gimli::DW_AT_name)?
                    .unwrap_or_default();
                Ok((
                    size,
                    size,
                    TypeInfo::Opaque {
                        name,
                        size,
                        align: size,
                    },
                ))
            }

            _ => {
                let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?.unwrap_or(0);
                Ok((
                    size,
                    size,
                    TypeInfo::Opaque {
                        name: format!("<{:?}>", entry.tag()),
                        size,
                        align: size,
                    },
                ))
            }
        }
    }
}
