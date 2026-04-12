// padlock-dwarf/src/extractor.rs

use std::collections::HashMap;

use gimli::{DebuggingInformationEntry, Reader, Unit, UnitOffset};
use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout};

pub struct Extractor<'a, R: Reader> {
    pub(crate) dwarf: &'a gimli::Dwarf<R>,
    pub(crate) arch: &'static ArchConfig,
}

impl<'a, R: Reader> Extractor<'a, R> {
    pub fn new(dwarf: &'a gimli::Dwarf<R>, arch: &'static ArchConfig) -> Self {
        Self { dwarf, arch }
    }

    pub fn extract_all(&self) -> anyhow::Result<Vec<StructLayout>> {
        let mut layouts = Vec::new();

        let mut iter = self.dwarf.units();
        while let Some(header) = iter.next()? {
            let unit = self.dwarf.unit(header)?;
            self.extract_from_unit(&unit, &mut layouts)?;
        }

        Ok(layouts)
    }

    fn extract_from_unit(&self, unit: &Unit<R>, out: &mut Vec<StructLayout>) -> anyhow::Result<()> {
        // First pass: build a map from struct offset → typedef name.
        // Handles `typedef struct { ... } Foo` where the struct has no tag name.
        let typedef_names = self.collect_typedef_names(unit)?;

        let mut entries = unit.entries();
        while let Some((_, entry)) = entries.next_dfs()? {
            if entry.tag() == gimli::DW_TAG_structure_type
                && let Some(mut layout) = self.extract_struct(unit, entry)?
            {
                if layout.name == "<anonymous>"
                    && let Some(name) = typedef_names.get(&entry.offset())
                {
                    layout.name = name.clone();
                }
                out.push(layout);
            }
        }
        Ok(())
    }

    /// Walk all top-level DIEs and collect DW_TAG_typedef entries that point
    /// directly to a DW_TAG_structure_type, returning struct_offset → typedef_name.
    fn collect_typedef_names(
        &self,
        unit: &Unit<R>,
    ) -> anyhow::Result<HashMap<UnitOffset<R::Offset>, String>> {
        let mut map = HashMap::new();
        let mut entries = unit.entries();
        while let Some((_, entry)) = entries.next_dfs()? {
            if entry.tag() != gimli::DW_TAG_typedef {
                continue;
            }
            let typedef_name = match self.attr_string(unit, entry, gimli::DW_AT_name)? {
                Some(n) => n,
                None => continue,
            };
            let struct_offset = match entry.attr_value(gimli::DW_AT_type)? {
                Some(gimli::AttributeValue::UnitRef(off)) => off,
                _ => continue,
            };
            map.insert(struct_offset, typedef_name);
        }
        Ok(map)
    }

    pub(crate) fn extract_struct(
        &self,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
    ) -> anyhow::Result<Option<StructLayout>> {
        if entry.attr(gimli::DW_AT_declaration)?.is_some() {
            return Ok(None);
        }

        let name = self
            .attr_string(unit, entry, gimli::DW_AT_name)?
            .unwrap_or_else(|| "<anonymous>".to_string());

        let total_size = match entry.attr_value(gimli::DW_AT_byte_size)? {
            Some(gimli::AttributeValue::Udata(s)) => s as usize,
            _ => return Ok(None),
        };

        let source_file = self.attr_string(unit, entry, gimli::DW_AT_decl_file)?;
        let source_line = entry.attr_value(gimli::DW_AT_decl_line)?.and_then(|v| {
            if let gimli::AttributeValue::Udata(n) = v {
                Some(n as u32)
            } else {
                None
            }
        });

        let mut fields = Vec::new();
        let mut children = unit.entries_tree(Some(entry.offset()))?;
        let root = children.root()?;
        let mut child_iter = root.children();

        while let Some(child) = child_iter.next()? {
            let child_entry = child.entry();
            if child_entry.tag() == gimli::DW_TAG_member
                && let Some(field) = self.extract_field(unit, child_entry)?
            {
                fields.push(field);
            }
        }

        fields.sort_by_key(|f| f.offset);

        Ok(Some(StructLayout {
            name,
            total_size,
            align: fields.iter().map(|f| f.align).max().unwrap_or(1),
            fields,
            source_file,
            source_line,
            arch: self.arch,
            is_packed: false,
            is_union: false,
            is_repr_rust: false,
        }))
    }

    fn extract_field(
        &self,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
    ) -> anyhow::Result<Option<Field>> {
        let name = self
            .attr_string(unit, entry, gimli::DW_AT_name)?
            .unwrap_or_else(|| "<unnamed>".to_string());

        let offset = match entry.attr_value(gimli::DW_AT_data_member_location)? {
            Some(gimli::AttributeValue::Udata(n)) => n as usize,
            Some(gimli::AttributeValue::Sdata(n)) => n as usize,
            _ => return Ok(None),
        };

        // Bit-field members carry DW_AT_bit_size. They share byte offsets with
        // adjacent fields and cannot be represented in the byte-level IR without
        // losing accuracy. Skip them entirely — use source analysis for structs
        // that contain bit-fields.
        if entry.attr(gimli::DW_AT_bit_size)?.is_some() {
            return Ok(None);
        }

        let type_offset = match entry.attr_value(gimli::DW_AT_type)? {
            Some(gimli::AttributeValue::UnitRef(off)) => off,
            _ => return Ok(None),
        };

        let (size, align, ty) = self.resolve_type(unit, type_offset)?;

        Ok(Some(Field {
            name,
            ty,
            offset,
            size,
            align,
            source_file: None,
            source_line: entry.attr_value(gimli::DW_AT_decl_line)?.and_then(|v| {
                if let gimli::AttributeValue::Udata(n) = v {
                    Some(n as u32)
                } else {
                    None
                }
            }),
            access: AccessPattern::Unknown,
        }))
    }

    pub(crate) fn attr_string(
        &self,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
        attr: gimli::DwAt,
    ) -> anyhow::Result<Option<String>> {
        match entry.attr(attr)? {
            Some(a) => match self.dwarf.attr_string(unit, a.value()) {
                Ok(s) => Ok(Some(s.to_string_lossy()?.into_owned())),
                Err(_) => Ok(None),
            },
            None => Ok(None),
        }
    }

    pub(crate) fn attr_usize(
        &self,
        entry: &DebuggingInformationEntry<R>,
        attr: gimli::DwAt,
    ) -> anyhow::Result<Option<usize>> {
        match entry.attr_value(attr)? {
            Some(gimli::AttributeValue::Udata(n)) => Ok(Some(n as usize)),
            Some(gimli::AttributeValue::Data1(n)) => Ok(Some(n as usize)),
            Some(gimli::AttributeValue::Data2(n)) => Ok(Some(n as usize)),
            Some(gimli::AttributeValue::Data4(n)) => Ok(Some(n as usize)),
            Some(gimli::AttributeValue::Data8(n)) => Ok(Some(n as usize)),
            _ => Ok(None),
        }
    }

    pub(crate) fn extract_array_count(
        &self,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
    ) -> anyhow::Result<usize> {
        let mut children = unit.entries_tree(Some(entry.offset()))?;
        let root = children.root()?;
        let mut child_iter = root.children();

        while let Some(child) = child_iter.next()? {
            let child_entry = child.entry();
            if child_entry.tag() == gimli::DW_TAG_subrange_type {
                if let Some(count) = self.attr_usize(child_entry, gimli::DW_AT_count)? {
                    return Ok(count);
                }
                if let Some(upper) = self.attr_usize(child_entry, gimli::DW_AT_upper_bound)? {
                    return Ok(upper + 1);
                }
            }
        }

        Ok(0)
    }
}
