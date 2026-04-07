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
        let (_, entry) = entries.next_dfs()?.ok_or_else(|| {
            anyhow::anyhow!("no DIE at offset")
        })?;

        match entry.tag() {
            gimli::DW_TAG_base_type => {
                let name = self.attr_string(unit, entry, gimli::DW_AT_name)?
                    .unwrap_or_default();
                let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?
                    .unwrap_or(0);
                let align = self.attr_usize(entry, gimli::DW_AT_alignment)?
                    .unwrap_or(size);

                Ok((size, align, TypeInfo::Primitive { name, size, align }))
            }

            gimli::DW_TAG_pointer_type |
            gimli::DW_TAG_reference_type => {
                let size = self.arch.pointer_size;
                Ok((size, size, TypeInfo::Pointer { size, align: size }))
            }

            gimli::DW_TAG_typedef => {
                let inner_offset = match entry.attr_value(gimli::DW_AT_type)? {
                    Some(gimli::AttributeValue::UnitRef(off)) => off,
                    _ => return Err(anyhow::anyhow!("typedef with no type")),
                };
                self.resolve_type(unit, inner_offset)
            }

            gimli::DW_TAG_const_type |
            gimli::DW_TAG_volatile_type |
            gimli::DW_TAG_restrict_type => {
                let inner_offset = match entry.attr_value(gimli::DW_AT_type)? {
                    Some(gimli::AttributeValue::UnitRef(off)) => off,
                    _ => return Err(anyhow::anyhow!("qualifier with no type")),
                };
                self.resolve_type(unit, inner_offset)
            }

            gimli::DW_TAG_array_type => {
                let elem_offset = match entry.attr_value(gimli::DW_AT_type)? {
                    Some(gimli::AttributeValue::UnitRef(off)) => off,
                    _ => return Err(anyhow::anyhow!("array with no element type")),
                };
                let (elem_size, elem_align, elem_ty) =
                    self.resolve_type(unit, elem_offset)?;

                let count = self.extract_array_count(unit, entry)?;
                let total = elem_size * count;

                Ok((total, elem_align, TypeInfo::Array {
                    element: Box::new(elem_ty),
                    count,
                    size: total,
                    align: elem_align,
                }))
            }

            gimli::DW_TAG_structure_type => {
                if let Some(layout) = self.extract_struct(unit, entry)? {
                    let size = layout.total_size;
                    let align = layout.align;
                    Ok((size, align, TypeInfo::Struct(Box::new(layout))))
                } else {
                    let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?
                        .unwrap_or(0);
                    Ok((size, size, TypeInfo::Opaque {
                        name: "<incomplete>".to_string(),
                        size,
                        align: size,
                    }))
                }
            }

            gimli::DW_TAG_enumeration_type => {
                let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?
                    .unwrap_or(4);
                let name = self.attr_string(unit, entry, gimli::DW_AT_name)?
                    .unwrap_or_default();
                Ok((size, size, TypeInfo::Opaque { name, size, align: size }))
            }

            _ => {
                let size = self.attr_usize(entry, gimli::DW_AT_byte_size)?
                    .unwrap_or(0);
                Ok((size, size, TypeInfo::Opaque {
                    name: format!("<{:?}>", entry.tag()),
                    size,
                    align: size,
                }))
            }
        }
    }
}
