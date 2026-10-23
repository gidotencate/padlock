// padlock-dwarf/src/extractor.rs

use std::collections::HashMap;

use gimli::{DebuggingInformationEntry, Reader, Unit, UnitOffset};
use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout};

pub struct Extractor<'a, R: Reader> {
    pub(crate) dwarf: &'a gimli::Dwarf<R>,
    pub(crate) arch: &'static ArchConfig,
    /// DWARF 4 `.debug_types` type units (`-fdebug-types-section`) and DWARF 5
    /// `DW_UT_type` units, with their signatures, so `DW_FORM_ref_sig8`
    /// references can be followed.
    pub(crate) type_units: Vec<Unit<R>>,
    pub(crate) signatures: HashMap<u64, (usize, UnitOffset<R::Offset>)>,
    /// Every unit in `.debug_info`, with its start offset, so `DW_FORM_ref_addr`
    /// references into another compile unit (Go and Zig emit them) can be followed.
    pub(crate) info_units: Vec<(usize, Unit<R>)>,
}

impl<'a, R: Reader> Extractor<'a, R> {
    pub fn new(dwarf: &'a gimli::Dwarf<R>, arch: &'static ArchConfig) -> Self {
        let mut type_units = Vec::new();
        let mut signatures = HashMap::new();
        let mut add = |header: gimli::UnitHeader<R>| {
            if let gimli::UnitType::Type {
                type_signature,
                type_offset,
            }
            | gimli::UnitType::SplitType {
                type_signature,
                type_offset,
            } = header.type_()
                && let Ok(unit) = dwarf.unit(header)
            {
                signatures.insert(type_signature.0, (type_units.len(), type_offset));
                type_units.push(unit);
            }
        };
        let mut v4 = dwarf.type_units();
        while let Ok(Some(h)) = v4.next() {
            add(h);
        }
        let mut v5 = dwarf.units();
        while let Ok(Some(h)) = v5.next() {
            add(h);
        }
        let mut info_units = Vec::new();
        let mut all = dwarf.units();
        while let Ok(Some(h)) = all.next() {
            let start = gimli::ReaderOffset::into_u64(h.offset().0) as usize;
            if let Ok(unit) = dwarf.unit(h) {
                info_units.push((start, unit));
            }
        }
        Self {
            dwarf,
            arch,
            type_units,
            signatures,
            info_units,
        }
    }

    pub fn extract_all(&self) -> anyhow::Result<Vec<StructLayout>> {
        let mut layouts = Vec::new();

        let mut iter = self.dwarf.units();
        while let Some(header) = iter.next()? {
            // Type units are walked below (DWARF 5 lists them among the units).
            if matches!(
                header.type_(),
                gimli::UnitType::Type { .. } | gimli::UnitType::SplitType { .. }
            ) {
                continue;
            }
            let unit = self.dwarf.unit(header)?;
            self.extract_from_unit(&unit, &mut layouts, &HashMap::new())?;
        }
        // `typedef struct { … } Name;` with the struct in a type unit: the typedef
        // (in a compile unit) refers to it by signature.
        let mut sig_typedefs: HashMap<u64, String> = HashMap::new();
        let mut iter = self.dwarf.units();
        while let Some(header) = iter.next()? {
            let unit = self.dwarf.unit(header)?;
            let mut entries = unit.entries();
            while let Some(entry) = entries.next_dfs()? {
                if entry.tag() == gimli::DW_TAG_typedef
                    && let Some(gimli::AttributeValue::DebugTypesRef(sig)) =
                        entry.attr_value(gimli::DW_AT_type)
                    && let Some(name) = self.attr_string(&unit, entry, gimli::DW_AT_name)?
                {
                    sig_typedefs.entry(sig.0).or_insert(name);
                }
            }
        }
        for (i, unit) in self.type_units.iter().enumerate() {
            let names: HashMap<UnitOffset<R::Offset>, String> = self
                .signatures
                .iter()
                .filter(|(_, (u, _))| *u == i)
                .filter_map(|(sig, (_, off))| Some((*off, sig_typedefs.get(sig)?.clone())))
                .collect();
            self.extract_from_unit(unit, &mut layouts, &names)?;
        }

        Ok(layouts)
    }

    /// Size the type an entry's `DW_AT_type` refers to, following references into
    /// type units (`DW_FORM_ref_sig8`). `None` when the entry has no usable type.
    pub(crate) fn resolve_type_attr(
        &self,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
    ) -> anyhow::Result<Option<(usize, usize, padlock_core::ir::TypeInfo)>> {
        match entry.attr_value(gimli::DW_AT_type) {
            Some(gimli::AttributeValue::UnitRef(off)) => self.resolve_type(unit, off).map(Some),
            Some(gimli::AttributeValue::DebugTypesRef(sig)) => match self.signatures.get(&sig.0) {
                Some((i, off)) => self.resolve_type(&self.type_units[*i], *off).map(Some),
                None => Ok(None),
            },
            Some(gimli::AttributeValue::DebugInfoRef(off)) => {
                let target = gimli::ReaderOffset::into_u64(off.0) as usize;
                // The unit with the greatest start offset not past the target.
                let idx = self
                    .info_units
                    .partition_point(|(start, _)| *start <= target);
                let Some((_, owner)) = idx.checked_sub(1).and_then(|i| self.info_units.get(i))
                else {
                    return Ok(None);
                };
                match gimli::DebugInfoOffset(off.0).to_unit_offset(&owner.header) {
                    Some(uoff) => self.resolve_type(owner, uoff).map(Some),
                    None => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    fn extract_from_unit(
        &self,
        unit: &Unit<R>,
        out: &mut Vec<StructLayout>,
        extra_typedef_names: &HashMap<UnitOffset<R::Offset>, String>,
    ) -> anyhow::Result<()> {
        // First pass: build a map from struct offset → typedef name.
        // Handles `typedef struct { ... } Foo` where the struct has no tag name.
        let mut typedef_names = self.collect_typedef_names(unit)?;
        for (k, v) in extra_typedef_names {
            typedef_names.entry(*k).or_insert_with(|| v.clone());
        }

        let mut entries = unit.entries();
        while let Some(entry) = entries.next_dfs()? {
            // DW_TAG_class_type is the DWARF tag for C++ `class` declarations;
            // it has the same layout rules as DW_TAG_structure_type.
            let is_union = entry.tag() == gimli::DW_TAG_union_type;
            let is_struct_like = entry.tag() == gimli::DW_TAG_structure_type
                || entry.tag() == gimli::DW_TAG_class_type
                || is_union;
            if is_struct_like && let Some(mut layout) = self.extract_struct(unit, entry)? {
                layout.is_union = is_union;
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
        while let Some(entry) = entries.next_dfs()? {
            if entry.tag() != gimli::DW_TAG_typedef {
                continue;
            }
            let typedef_name = match self.attr_string(unit, entry, gimli::DW_AT_name)? {
                Some(n) => n,
                None => continue,
            };
            let struct_offset = match entry.attr_value(gimli::DW_AT_type) {
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
        if entry.attr(gimli::DW_AT_declaration).is_some() {
            return Ok(None);
        }

        let name = self
            .attr_string(unit, entry, gimli::DW_AT_name)?
            .unwrap_or_else(|| "<anonymous>".to_string());

        // DW_AT_byte_size is normally normalized to Udata by gimli, but
        // DW_FORM_implicit_const (used when the size is the same for every
        // abbreviation entry, e.g. a pointer-sized struct) arrives as Sdata.
        // Zig describes a `packed struct` as a structure whose `DW_AT_type` is its
        // backing integer (no `DW_AT_byte_size`), with bit-offset members.
        let backing = if entry.attr(gimli::DW_AT_byte_size).is_none() {
            self.resolve_type_attr(unit, entry)?
        } else {
            None
        };
        let total_size = match entry.attr_value(gimli::DW_AT_byte_size) {
            Some(gimli::AttributeValue::Sdata(s)) if s >= 0 => s as usize,
            Some(v) => match v.udata_value() {
                Some(s) => s as usize,
                None => return Ok(None),
            },
            None => match &backing {
                Some((size, _, _)) => *size,
                None => return Ok(None),
            },
        };

        let source_file = self.decl_file(unit, entry);
        let source_line = entry
            .attr_value(gimli::DW_AT_decl_line)
            .and_then(|v| v.udata_value())
            .map(|n| n as u32);

        let mut fields = Vec::new();
        let mut uncertain_fields: Vec<String> = Vec::new();

        // Accumulates consecutive bitfield members at the same byte offset before
        // flushing them as a single synthetic storage-unit field.
        struct BitfieldGroup {
            parts: Vec<String>, // "name:bits" labels
            byte_offset: usize,
            storage_bytes: usize, // from DW_AT_byte_size on member; 0 = unknown
            // Tracks the exclusive upper bit bound for DWARF5 groups that lack
            // DW_AT_byte_size. Relative to the start of the struct in bits.
            max_bit_exclusive: usize,
        }
        let mut pending_bf: Option<BitfieldGroup> = None;

        let flush_bf =
            |group: BitfieldGroup, fields: &mut Vec<Field>, uncertain: &mut Vec<String>| {
                let storage_bytes = if group.storage_bytes > 0 {
                    group.storage_bytes
                } else if group.max_bit_exclusive > group.byte_offset * 8 {
                    // Derive from bit span: smallest number of bytes that covers
                    // all bits from byte_offset*8 through max_bit_exclusive-1.
                    let bits_in_group = group.max_bit_exclusive - group.byte_offset * 8;
                    bits_in_group.div_ceil(8)
                } else {
                    // Storage unit size unknown; flag as uncertain so the user knows.
                    uncertain.push(format!("[bf@{}]", group.byte_offset));
                    return;
                };
                let field_name = if group.parts.is_empty() {
                    "[__pad]".to_string()
                } else {
                    format!("[{}]", group.parts.join("|"))
                };
                use padlock_core::ir::TypeInfo;
                fields.push(Field {
                    name: field_name,
                    ty: TypeInfo::Primitive {
                        name: format!("uint{}_t", storage_bytes * 8),
                        size: storage_bytes,
                        align: storage_bytes,
                    },
                    offset: group.byte_offset,
                    size: storage_bytes,
                    align: storage_bytes,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                });
            };

        let mut children = unit.entries_tree(Some(entry.offset()))?;
        let root = children.root()?;
        let mut child_iter = root.children();

        while let Some(child) = child_iter.next()? {
            let child_entry = child.entry();

            if child_entry.tag() == gimli::DW_TAG_inheritance {
                // Non-virtual base class subobject. Virtual bases use pointer-based
                // thunks (vbtable) to find their offset; we cannot model that accurately
                // without reading vbtable at runtime, so they are skipped.
                let virtuality = self
                    .attr_usize(child_entry, gimli::DW_AT_virtuality)?
                    .unwrap_or(0);
                if virtuality != 0 {
                    continue;
                }

                // Flush any pending bitfield group before the base subobject.
                if let Some(g) = pending_bf.take() {
                    flush_bf(g, &mut fields, &mut uncertain_fields);
                }

                // single-inheritance default: base always at offset 0
                let base_offset =
                    member_location(child_entry.attr_value(gimli::DW_AT_data_member_location))
                        .unwrap_or(0);

                let Some((base_size, base_align, base_ty)) =
                    self.resolve_type_attr(unit, child_entry)?
                else {
                    continue;
                };
                // Extract the name from the resolved type for the synthetic field label.
                let base_name = match &base_ty {
                    padlock_core::ir::TypeInfo::Struct(l) => l.name.clone(),
                    padlock_core::ir::TypeInfo::Opaque { name, .. } => name.clone(),
                    _ => "<base>".to_string(),
                };

                if base_size > 0 {
                    fields.push(Field {
                        name: format!("[{}]", base_name),
                        ty: padlock_core::ir::TypeInfo::Opaque {
                            name: base_name,
                            size: base_size,
                            align: base_align,
                        },
                        offset: base_offset,
                        size: base_size,
                        align: base_align,
                        source_file: None,
                        source_line: None,
                        access: AccessPattern::Unknown,
                    });
                }
                continue;
            }

            if child_entry.tag() != gimli::DW_TAG_member {
                continue;
            }

            let is_bitfield = child_entry.attr(gimli::DW_AT_bit_size).is_some();

            if is_bitfield {
                // DWARF4 uses DW_AT_data_member_location (byte offset).
                // DWARF5 uses DW_AT_data_bit_offset (absolute bit offset from struct start).
                // Returns (byte_offset, abs_bit_offset_if_known).
                let (byte_offset, abs_bit_offset) = match member_location(
                    child_entry.attr_value(gimli::DW_AT_data_member_location),
                ) {
                    Some(n) => (n, None),
                    None => {
                        let raw: Option<u64> =
                            match child_entry.attr_value(gimli::DW_AT_data_bit_offset) {
                                Some(gimli::AttributeValue::Udata(v)) => Some(v),
                                Some(gimli::AttributeValue::Sdata(v)) => Some(v as u64),
                                Some(gimli::AttributeValue::Data1(v)) => Some(v as u64),
                                Some(gimli::AttributeValue::Data2(v)) => Some(v as u64),
                                Some(gimli::AttributeValue::Data4(v)) => Some(v as u64),
                                Some(gimli::AttributeValue::Data8(v)) => Some(v),
                                _ => None,
                            };
                        match raw {
                            Some(bit_off) => ((bit_off / 8) as usize, Some(bit_off as usize)),
                            None => {
                                // No byte offset — flush pending group and skip.
                                if let Some(g) = pending_bf.take() {
                                    flush_bf(g, &mut fields, &mut uncertain_fields);
                                }
                                continue;
                            }
                        }
                    }
                };

                let bit_size = match child_entry.attr_value(gimli::DW_AT_bit_size) {
                    Some(gimli::AttributeValue::Udata(n)) => n as usize,
                    Some(gimli::AttributeValue::Sdata(n)) => n.unsigned_abs() as usize,
                    Some(gimli::AttributeValue::Data1(n)) => n as usize,
                    Some(gimli::AttributeValue::Data2(n)) => n as usize,
                    Some(gimli::AttributeValue::Data4(n)) => n as usize,
                    _ => 0,
                };

                // DW_AT_byte_size on a bitfield member gives the storage unit size.
                // Absent for DWARF5 groups; derived from bit span in flush_bf instead.
                let storage_bytes = match child_entry.attr_value(gimli::DW_AT_byte_size) {
                    Some(gimli::AttributeValue::Udata(n)) => n as usize,
                    Some(gimli::AttributeValue::Data1(n)) => n as usize,
                    Some(gimli::AttributeValue::Data2(n)) => n as usize,
                    Some(gimli::AttributeValue::Data4(n)) => n as usize,
                    _ => 0,
                };

                let member_name = self
                    .attr_string(unit, child_entry, gimli::DW_AT_name)?
                    .unwrap_or_default();

                // If the pending group is at a different byte offset, flush it first.
                if let Some(ref g) = pending_bf
                    && g.byte_offset != byte_offset
                {
                    let g = pending_bf.take().unwrap();
                    flush_bf(g, &mut fields, &mut uncertain_fields);
                }

                let group = pending_bf.get_or_insert(BitfieldGroup {
                    parts: Vec::new(),
                    byte_offset,
                    storage_bytes: 0,
                    max_bit_exclusive: byte_offset * 8,
                });
                if !member_name.is_empty() && bit_size > 0 {
                    group.parts.push(format!("{member_name}:{bit_size}"));
                }
                if storage_bytes > group.storage_bytes {
                    group.storage_bytes = storage_bytes;
                }
                // Track the furthest bit for DWARF5 storage-size derivation.
                let abs_start = abs_bit_offset.unwrap_or(byte_offset * 8);
                let bit_end = abs_start + bit_size;
                if bit_end > group.max_bit_exclusive {
                    group.max_bit_exclusive = bit_end;
                }
            } else {
                // Non-bitfield member — flush any pending bitfield group first.
                if let Some(g) = pending_bf.take() {
                    flush_bf(g, &mut fields, &mut uncertain_fields);
                }
                if let Some(field) = self.extract_field(unit, child_entry)? {
                    fields.push(field);
                }
            }
        }

        // Flush any remaining bitfield group.
        if let Some(g) = pending_bf.take() {
            flush_bf(g, &mut fields, &mut uncertain_fields);
        }

        fields.sort_by_key(|f| f.offset);

        // DW_AT_alignment on the struct itself captures an explicit alignas(N)
        // or __attribute__((aligned(N))) on the type declaration.  If present,
        // it overrides the alignment derived from the maximum field alignment.
        let field_align = fields.iter().map(|f| f.align).max().unwrap_or(1);
        let explicit_align = self.attr_usize(entry, gimli::DW_AT_alignment)?.unwrap_or(0);
        let align = match &backing {
            // Packed: the struct is aligned like its backing integer; fields are not.
            Some((_, backing_align, _)) => *backing_align,
            None => explicit_align.max(field_align),
        };
        if backing.is_some() {
            for f in fields.iter_mut() {
                f.align = 1;
            }
        }

        Ok(Some(StructLayout {
            name,
            total_size,
            align,
            fields,
            source_file,
            source_line,
            arch: self.arch,
            is_packed: backing.is_some(),
            is_union: false,
            is_repr_rust: false,
            suppressed_findings: Vec::new(),
            uncertain_fields,
            forced_align: None,
            pack_limit: None,
            cpp: None,
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

        // A union member may omit the location entirely: it is at offset 0. A
        // member of a Zig packed struct has only a bit offset.
        let offset = match member_location(entry.attr_value(gimli::DW_AT_data_member_location)) {
            Some(n) => n,
            None if entry.attr(gimli::DW_AT_data_member_location).is_none() => {
                member_location(entry.attr_value(gimli::DW_AT_data_bit_offset)).unwrap_or(0) / 8
            }
            None => return Ok(None),
        };

        let Some((size, type_align, ty)) = self.resolve_type_attr(unit, entry)? else {
            return Ok(None);
        };

        // DW_AT_alignment on a member DIE captures an explicit alignment
        // override on the field declaration (e.g. __attribute__((aligned(N)))).
        // It overrides the alignment inferred from the field's type.
        let member_align = self.attr_usize(entry, gimli::DW_AT_alignment)?.unwrap_or(0);
        let align = member_align.max(type_align);

        Ok(Some(Field {
            name,
            ty,
            offset,
            size,
            align,
            source_file: None,
            source_line: entry.attr_value(gimli::DW_AT_decl_line).and_then(|v| {
                if let gimli::AttributeValue::Udata(n) = v {
                    Some(n as u32)
                } else {
                    None
                }
            }),
            access: AccessPattern::Unknown,
        }))
    }

    /// The path named by an entry's `DW_AT_decl_file` (an index into the unit's
    /// line-program file table), joined with its include directory and the
    /// unit's compilation directory when relative.
    pub(crate) fn decl_file(
        &self,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
    ) -> Option<String> {
        let index = match entry.attr_value(gimli::DW_AT_decl_file)? {
            gimli::AttributeValue::FileIndex(i) => i,
            v => v.udata_value()?,
        };
        let program = unit.line_program.as_ref()?;
        let header = program.header();
        let file = header.file(index)?;
        let text = |v: gimli::AttributeValue<R>| -> Option<String> {
            let s = self.dwarf.attr_string(unit, v).ok()?;
            Some(s.to_string_lossy().ok()?.into_owned())
        };
        let name = text(file.path_name())?;
        let mut path = std::path::PathBuf::new();
        if let Some(dir) = unit.comp_dir.as_ref()
            && let Ok(d) = dir.to_string_lossy()
        {
            path.push(d.as_ref());
        }
        if let Some(dir) = file.directory(header).and_then(text) {
            path.push(dir);
        }
        path.push(name);
        Some(path.to_string_lossy().into_owned())
    }

    pub(crate) fn attr_string(
        &self,
        unit: &Unit<R>,
        entry: &DebuggingInformationEntry<R>,
        attr: gimli::DwAt,
    ) -> anyhow::Result<Option<String>> {
        match entry.attr(attr) {
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
        match entry.attr_value(attr) {
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

        // A multi-dimensional array (`short m[2][3]`) has one subrange per
        // dimension; the element count is their product.
        let mut total: Option<usize> = None;
        while let Some(child) = child_iter.next()? {
            let child_entry = child.entry();
            if child_entry.tag() != gimli::DW_TAG_subrange_type {
                continue;
            }
            let lower = self
                .attr_usize(child_entry, gimli::DW_AT_lower_bound)?
                .unwrap_or(0);
            let dim = if let Some(count) = self.attr_usize(child_entry, gimli::DW_AT_count)? {
                count
            } else if let Some(upper) = self.attr_usize(child_entry, gimli::DW_AT_upper_bound)? {
                (upper + 1).saturating_sub(lower)
            } else {
                // Flexible / unknown bound (`char data[]`).
                0
            };
            total = Some(total.unwrap_or(1).saturating_mul(dim));
        }

        Ok(total.unwrap_or(0))
    }
}

/// A constant `DW_AT_data_member_location` in any data form (`DW_FORM_data1`…`data8`,
/// `udata`, `sdata`, `implicit_const`) — Go, for one, emits `data1`. `None` for a
/// location expression or a missing attribute.
fn member_location<R: Reader>(value: Option<gimli::AttributeValue<R>>) -> Option<usize> {
    match value? {
        gimli::AttributeValue::Sdata(n) if n >= 0 => Some(n as usize),
        v => v.udata_value().map(|n| n as usize),
    }
}
