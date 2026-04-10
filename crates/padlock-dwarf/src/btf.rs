// padlock-dwarf/src/btf.rs
//
// Minimal BTF (BPF Type Format) parser for extracting struct layouts.
// BTF is used by Linux eBPF programs and is embedded in the `.BTF` ELF section.
//
// Reference: https://www.kernel.org/doc/html/latest/bpf/btf.html

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};

// ── BTF constants ─────────────────────────────────────────────────────────────

const BTF_MAGIC: u16 = 0xeB9F;
const BTF_KIND_INT: u32 = 1;
const BTF_KIND_PTR: u32 = 2;
const BTF_KIND_ARRAY: u32 = 3;
const BTF_KIND_STRUCT: u32 = 4;
const BTF_KIND_UNION: u32 = 5;
const BTF_KIND_ENUM: u32 = 6;
const BTF_KIND_FWD: u32 = 7;
const BTF_KIND_TYPEDEF: u32 = 8;
const BTF_KIND_VOLATILE: u32 = 9;
const BTF_KIND_CONST: u32 = 10;
const BTF_KIND_RESTRICT: u32 = 11;
const BTF_KIND_FUNC: u32 = 12;
const BTF_KIND_FUNC_PROTO: u32 = 13;
const BTF_KIND_VAR: u32 = 14;
const BTF_KIND_DATASEC: u32 = 15;
const BTF_KIND_FLOAT: u32 = 16;
const BTF_KIND_DECL_TAG: u32 = 17;
const BTF_KIND_TYPE_TAG: u32 = 18;
const BTF_KIND_ENUM64: u32 = 19;

// ── BTF wire types (little-endian) ────────────────────────────────────────────

#[derive(Debug, Clone)]
struct BtfHeader {
    hdr_len: u32,
    type_off: u32,
    type_len: u32,
    str_off: u32,
    str_len: u32,
}

#[derive(Debug, Clone)]
struct RawBtfType {
    name_off: u32,
    info: u32,
    size_or_type: u32,
}

impl RawBtfType {
    fn kind(&self) -> u32 {
        (self.info >> 24) & 0x1f
    }
    fn vlen(&self) -> u32 {
        self.info & 0xffff
    }
    fn kind_flag(&self) -> bool {
        (self.info >> 31) & 1 == 1
    }
}

// ── parsed type table ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum BtfType {
    Int {
        name: String,
        size: u32,
    },
    Ptr,
    Array {
        elem_type: u32,
        nelems: u32,
    },
    Struct {
        name: String,
        size: u32,
        members: Vec<BtfMember>,
        is_union: bool,
    },
    Enum {
        size: u32,
    },
    Typedef {
        type_id: u32,
    },
    Qualifier {
        type_id: u32,
    }, // volatile, const, restrict
    Float {
        size: u32,
    },
    Unknown,
}

#[derive(Debug, Clone)]
struct BtfMember {
    name: String,
    type_id: u32,
    bit_offset: u32,
    bitfield_size: u32, // 0 = not a bitfield
}

// ── parser ────────────────────────────────────────────────────────────────────

pub struct BtfParser<'a> {
    data: &'a [u8],
    // types is 1-indexed: types[0] is unused (type_id 0 = void)
    types: Vec<BtfType>,
    arch: &'static ArchConfig,
}

impl<'a> BtfParser<'a> {
    pub fn new(data: &'a [u8], arch: &'static ArchConfig) -> anyhow::Result<Self> {
        let mut p = BtfParser {
            data,
            types: Vec::new(),
            arch,
        };
        p.parse()?;
        Ok(p)
    }

    fn parse(&mut self) -> anyhow::Result<()> {
        if self.data.len() < 24 {
            anyhow::bail!("BTF data too short");
        }

        let magic = u16::from_le_bytes([self.data[0], self.data[1]]);
        if magic != BTF_MAGIC {
            anyhow::bail!("invalid BTF magic: 0x{:04x}", magic);
        }

        let hdr = BtfHeader {
            hdr_len: u32::from_le_bytes(self.data[4..8].try_into()?),
            type_off: u32::from_le_bytes(self.data[8..12].try_into()?),
            type_len: u32::from_le_bytes(self.data[12..16].try_into()?),
            str_off: u32::from_le_bytes(self.data[16..20].try_into()?),
            str_len: u32::from_le_bytes(self.data[20..24].try_into()?),
        };

        let type_base = hdr.hdr_len as usize + hdr.type_off as usize;
        let type_end = type_base + hdr.type_len as usize;
        let str_base = hdr.hdr_len as usize + hdr.str_off as usize;
        let str_end = str_base + hdr.str_len as usize;

        if type_end > self.data.len() || str_end > self.data.len() {
            anyhow::bail!("BTF sections extend beyond data");
        }

        let type_data = &self.data[type_base..type_end];
        let str_data = &self.data[str_base..str_end];

        // type_id 0 is void — reserve slot 0
        self.types.push(BtfType::Unknown);

        let mut off = 0usize;
        while off + 12 <= type_data.len() {
            let name_off = u32::from_le_bytes(type_data[off..off + 4].try_into()?);
            let info = u32::from_le_bytes(type_data[off + 4..off + 8].try_into()?);
            let size_or_type = u32::from_le_bytes(type_data[off + 8..off + 12].try_into()?);
            off += 12;

            let raw = RawBtfType {
                name_off,
                info,
                size_or_type,
            };
            let name = read_btf_str(str_data, name_off as usize);
            let kind = raw.kind();
            let vlen = raw.vlen() as usize;

            let btf_type = match kind {
                BTF_KIND_INT => {
                    off += 4; // skip int encoding
                    BtfType::Int {
                        name,
                        size: size_or_type,
                    }
                }
                BTF_KIND_PTR => BtfType::Ptr,
                BTF_KIND_ARRAY => {
                    if off + 12 > type_data.len() {
                        break;
                    }
                    let elem_type = u32::from_le_bytes(type_data[off..off + 4].try_into()?);
                    // index_type: off+4..off+8 (skip)
                    let nelems = u32::from_le_bytes(type_data[off + 8..off + 12].try_into()?);
                    off += 12;
                    BtfType::Array { elem_type, nelems }
                }
                BTF_KIND_STRUCT | BTF_KIND_UNION => {
                    let mut members = Vec::with_capacity(vlen);
                    for _ in 0..vlen {
                        if off + 12 > type_data.len() {
                            break;
                        }
                        let m_name_off = u32::from_le_bytes(type_data[off..off + 4].try_into()?);
                        let m_type = u32::from_le_bytes(type_data[off + 4..off + 8].try_into()?);
                        let m_offset = u32::from_le_bytes(type_data[off + 8..off + 12].try_into()?);
                        off += 12;

                        let (bit_offset, bitfield_size) = if raw.kind_flag() {
                            (m_offset & 0xffffff, (m_offset >> 24) & 0xff)
                        } else {
                            (m_offset, 0)
                        };

                        members.push(BtfMember {
                            name: read_btf_str(str_data, m_name_off as usize),
                            type_id: m_type,
                            bit_offset,
                            bitfield_size,
                        });
                    }
                    BtfType::Struct {
                        name,
                        size: size_or_type,
                        members,
                        is_union: kind == BTF_KIND_UNION,
                    }
                }
                BTF_KIND_ENUM => {
                    off += vlen * 8; // each enum value = name_off(4) + val(4)
                    BtfType::Enum { size: size_or_type }
                }
                BTF_KIND_TYPEDEF => BtfType::Typedef {
                    type_id: size_or_type,
                },
                BTF_KIND_VOLATILE | BTF_KIND_CONST | BTF_KIND_RESTRICT => BtfType::Qualifier {
                    type_id: size_or_type,
                },
                BTF_KIND_FLOAT => BtfType::Float { size: size_or_type },
                BTF_KIND_ENUM64 => {
                    off += vlen * 12; // each enum64 value = name_off(4) + val_lo(4) + val_hi(4)
                    BtfType::Enum { size: size_or_type }
                }
                // Kinds with no extra bytes beyond the 12-byte header
                BTF_KIND_FWD | BTF_KIND_FUNC | BTF_KIND_TYPE_TAG => BtfType::Unknown,
                // FUNC_PROTO: vlen params, each 8 bytes (name_off + type_id)
                BTF_KIND_FUNC_PROTO => {
                    off += vlen * 8;
                    BtfType::Unknown
                }
                // VAR: 4 extra bytes (linkage u32)
                BTF_KIND_VAR => {
                    off += 4;
                    BtfType::Unknown
                }
                // DATASEC: vlen * 12 bytes (type + offset + size per btf_var_secinfo)
                BTF_KIND_DATASEC => {
                    off += vlen * 12;
                    BtfType::Unknown
                }
                // DECL_TAG: 4 extra bytes (component_idx i32)
                BTF_KIND_DECL_TAG => {
                    off += 4;
                    BtfType::Unknown
                }
                _ => {
                    // Truly unknown kind — stop parsing to avoid reading garbage
                    break;
                }
            };

            self.types.push(btf_type);
        }

        Ok(())
    }

    /// Resolve a type_id to its byte size.
    fn type_size(&self, type_id: u32) -> usize {
        if type_id == 0 {
            return 0; // void
        }
        let idx = type_id as usize;
        if idx >= self.types.len() {
            return self.arch.pointer_size;
        }
        match &self.types[idx] {
            BtfType::Int { size, .. } | BtfType::Float { size } | BtfType::Enum { size } => {
                *size as usize
            }
            BtfType::Ptr => self.arch.pointer_size,
            BtfType::Array { elem_type, nelems } => self.type_size(*elem_type) * (*nelems as usize),
            BtfType::Struct { size, .. } => *size as usize,
            BtfType::Typedef { type_id } | BtfType::Qualifier { type_id } => {
                self.type_size(*type_id)
            }
            BtfType::Unknown => self.arch.pointer_size,
        }
    }

    /// Infer alignment from a type_id (BTF doesn't store alignment explicitly).
    fn type_align(&self, type_id: u32) -> usize {
        if type_id == 0 {
            return 1;
        }
        let idx = type_id as usize;
        if idx >= self.types.len() {
            return self.arch.pointer_size;
        }
        match &self.types[idx] {
            BtfType::Int { size, .. } | BtfType::Float { size } | BtfType::Enum { size } => {
                (*size as usize).min(self.arch.max_align)
            }
            BtfType::Ptr => self.arch.pointer_size,
            BtfType::Array { elem_type, .. } => self.type_align(*elem_type),
            BtfType::Struct { members, .. } => members
                .iter()
                .map(|m| self.type_align(m.type_id))
                .max()
                .unwrap_or(1),
            BtfType::Typedef { type_id } | BtfType::Qualifier { type_id } => {
                self.type_align(*type_id)
            }
            BtfType::Unknown => self.arch.pointer_size,
        }
    }

    /// Resolve typedef/qualifier chains to a displayable type name.
    fn type_name(&self, type_id: u32) -> String {
        if type_id == 0 {
            return "void".to_string();
        }
        let idx = type_id as usize;
        if idx >= self.types.len() {
            return format!("type_{}", type_id);
        }
        match &self.types[idx] {
            BtfType::Int { name, .. } | BtfType::Struct { name, .. } => {
                if name.is_empty() {
                    format!("type_{}", type_id)
                } else {
                    name.clone()
                }
            }
            BtfType::Float { size } => format!("f{}", size * 8),
            BtfType::Ptr => "*".to_string(),
            BtfType::Array { elem_type, nelems } => {
                format!("[{}]{}", nelems, self.type_name(*elem_type))
            }
            BtfType::Enum { .. } => format!("enum_{}", type_id),
            BtfType::Typedef { type_id } => self.type_name(*type_id),
            BtfType::Qualifier { type_id } => self.type_name(*type_id),
            BtfType::Unknown => format!("unknown_{}", type_id),
        }
    }

    /// Extract all named structs and unions as `StructLayout` values.
    pub fn extract_structs(&self) -> Vec<StructLayout> {
        let mut layouts = Vec::new();

        for (idx, ty) in self.types.iter().enumerate() {
            if let BtfType::Struct {
                name,
                size,
                members,
                is_union,
            } = ty
            {
                // Skip anonymous structs (name is empty) — they're usually embedded
                if name.is_empty() {
                    continue;
                }

                let mut fields: Vec<Field> = Vec::new();
                // Track byte ranges already covered by synthetic bitfield-group fields
                // so we don't emit overlapping entries.
                let mut covered_until: usize = 0;

                for member in members {
                    let is_bitfield = member.bitfield_size != 0 || member.bit_offset % 8 != 0;

                    if is_bitfield {
                        // Represent the storage unit for this bitfield group.
                        // The storage unit starts at the byte-aligned base of the bit offset
                        // and has the size of the member's declared base type.
                        let storage_size = self.type_size(member.type_id).max(1);
                        // Align the bit offset down to the nearest storage-unit boundary.
                        let storage_bits = (storage_size * 8) as u32;
                        let unit_start_bit = (member.bit_offset / storage_bits) * storage_bits;
                        let unit_byte_offset = (unit_start_bit / 8) as usize;
                        let unit_end = unit_byte_offset + storage_size;

                        // Only emit a synthetic field if this storage unit isn't
                        // already covered by a previously emitted field.
                        if unit_byte_offset >= covered_until {
                            let fname = format!("{}__bits", member.name);
                            let falign = storage_size.min(self.arch.max_align);
                            fields.push(Field {
                                name: fname.clone(),
                                ty: TypeInfo::Primitive {
                                    name: format!("u{}", storage_size * 8),
                                    size: storage_size,
                                    align: falign,
                                },
                                offset: unit_byte_offset,
                                size: storage_size,
                                align: falign,
                                source_file: None,
                                source_line: None,
                                access: AccessPattern::Unknown,
                            });
                            covered_until = unit_end;
                        }
                        continue;
                    }

                    let byte_offset = (member.bit_offset / 8) as usize;
                    let fsize = self.type_size(member.type_id);
                    let falign = self.type_align(member.type_id);
                    let fname = if member.name.is_empty() {
                        format!("field_{}", fields.len())
                    } else {
                        member.name.clone()
                    };

                    covered_until = covered_until.max(byte_offset + fsize);
                    fields.push(Field {
                        name: fname.clone(),
                        ty: TypeInfo::Primitive {
                            name: self.type_name(member.type_id),
                            size: fsize,
                            align: falign,
                        },
                        offset: byte_offset,
                        size: fsize,
                        align: falign,
                        source_file: None,
                        source_line: None,
                        access: AccessPattern::Unknown,
                    });
                }

                if fields.is_empty() {
                    continue;
                }

                let max_align = fields.iter().map(|f| f.align).max().unwrap_or(1);

                // Detect packed structs: a struct is packed if its total_size is
                // smaller than what natural alignment would produce. This catches
                // __attribute__((packed)) structs emitted by the compiler.
                let natural_size = {
                    let mut off2 = 0usize;
                    for f in &fields {
                        if max_align > 0 {
                            off2 = off2.next_multiple_of(f.align.max(1));
                        }
                        off2 += f.size;
                    }
                    if max_align > 0 {
                        off2 = off2.next_multiple_of(max_align.max(1));
                    }
                    off2
                };
                let is_packed = !*is_union && (*size as usize) < natural_size;

                layouts.push(StructLayout {
                    name: name.clone(),
                    total_size: *size as usize,
                    align: max_align,
                    fields,
                    source_file: None,
                    source_line: None,
                    arch: self.arch,
                    is_packed,
                    is_union: *is_union,
                });

                let _ = idx;
            }
        }

        layouts
    }
}

fn read_btf_str(str_data: &[u8], off: usize) -> String {
    if off >= str_data.len() {
        return String::new();
    }
    let end = str_data[off..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| off + p)
        .unwrap_or(str_data.len());
    String::from_utf8_lossy(&str_data[off..end]).into_owned()
}

// ── public entry point ────────────────────────────────────────────────────────

/// Extract struct layouts from raw BTF data (the contents of a `.BTF` ELF section).
pub fn extract_from_btf(
    btf_data: &[u8],
    arch: &'static ArchConfig,
) -> anyhow::Result<Vec<StructLayout>> {
    let parser = BtfParser::new(btf_data, arch)?;
    Ok(parser.extract_structs())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    /// Build a minimal valid BTF blob containing one struct with two int fields.
    fn build_test_btf() -> Vec<u8> {
        // Strings: "\0point\0x\0y\0"
        let strings: &[u8] = b"\0point\0x\0y\0";
        let str_len = strings.len() as u32;

        // Strings layout: "\0point\0x\0y\0"
        //   offset 0: \0 (empty name)
        //   offset 1: "point" (6 bytes including null)
        //   offset 7: "x" (2 bytes including null)
        //   offset 9: "y" (2 bytes including null)
        // Types:
        // type_id 1: INT "x"  size=4
        // type_id 2: INT "y"  size=4
        // type_id 3: STRUCT "point"  size=8  vlen=2
        //   member 0: name="x"(off=7) type=1 offset=0
        //   member 1: name="y"(off=9) type=2 offset=32 (bit offset)

        let mut type_data: Vec<u8> = Vec::new();

        // INT "x": name_off=7, info=(1<<24|0), size=4; extra int_encoding=4bytes
        let x_name_off: u32 = 7;
        type_data.extend_from_slice(&x_name_off.to_le_bytes());
        type_data.extend_from_slice(&(BTF_KIND_INT << 24).to_le_bytes());
        type_data.extend_from_slice(&4u32.to_le_bytes());
        type_data.extend_from_slice(&0u32.to_le_bytes()); // int encoding

        // INT "y": name_off=9
        let y_name_off: u32 = 9;
        type_data.extend_from_slice(&y_name_off.to_le_bytes());
        type_data.extend_from_slice(&(BTF_KIND_INT << 24).to_le_bytes());
        type_data.extend_from_slice(&4u32.to_le_bytes());
        type_data.extend_from_slice(&0u32.to_le_bytes());

        // STRUCT "point": name_off=1, info=(4<<24|2), size=8
        let point_name_off: u32 = 1;
        type_data.extend_from_slice(&point_name_off.to_le_bytes());
        type_data.extend_from_slice(&((BTF_KIND_STRUCT << 24) | 2u32).to_le_bytes()); // vlen=2
        type_data.extend_from_slice(&8u32.to_le_bytes()); // size=8
        // member 0: x
        type_data.extend_from_slice(&x_name_off.to_le_bytes()); // name_off for "x"
        type_data.extend_from_slice(&1u32.to_le_bytes()); // type_id=1
        type_data.extend_from_slice(&0u32.to_le_bytes()); // bit_offset=0
        // member 1: y
        type_data.extend_from_slice(&y_name_off.to_le_bytes()); // name_off for "y"
        type_data.extend_from_slice(&2u32.to_le_bytes()); // type_id=2
        type_data.extend_from_slice(&32u32.to_le_bytes()); // bit_offset=32

        let type_len = type_data.len() as u32;

        // BTF header (24 bytes)
        let hdr_len: u32 = 24;
        let mut btf = Vec::new();
        btf.extend_from_slice(&BTF_MAGIC.to_le_bytes()); // magic
        btf.push(1); // version
        btf.push(0); // flags
        btf.extend_from_slice(&hdr_len.to_le_bytes()); // hdr_len
        btf.extend_from_slice(&0u32.to_le_bytes()); // type_off = 0
        btf.extend_from_slice(&type_len.to_le_bytes()); // type_len
        btf.extend_from_slice(&type_len.to_le_bytes()); // str_off = after types
        btf.extend_from_slice(&str_len.to_le_bytes()); // str_len
        btf.extend_from_slice(&type_data);
        btf.extend_from_slice(strings);
        btf
    }

    #[test]
    fn btf_parse_simple_struct() {
        let btf = build_test_btf();
        let layouts = extract_from_btf(&btf, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "point");
        assert_eq!(layouts[0].total_size, 8);
        assert_eq!(layouts[0].fields.len(), 2);
    }

    #[test]
    fn btf_field_offsets_correct() {
        let btf = build_test_btf();
        let layouts = extract_from_btf(&btf, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].name, "x");
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].name, "y");
        assert_eq!(l.fields[1].offset, 4);
    }

    #[test]
    fn btf_invalid_magic_errors() {
        let mut btf = build_test_btf();
        btf[0] = 0xff;
        btf[1] = 0xff;
        assert!(extract_from_btf(&btf, &X86_64_SYSV).is_err());
    }

    #[test]
    fn btf_bitfield_members_become_synthetic_storage_unit_fields() {
        // Struct with one bitfield member: `u32 flags : 3` at bit_offset = 0.
        // Expected: a synthetic "flags__bits" field of type u32 at offset 0.
        let strings: &[u8] = b"\0mystruct\0flags\0";
        // "mystruct" at 1, "flags" at 10
        let str_len = strings.len() as u32;
        let mut type_data: Vec<u8> = Vec::new();

        // INT type (u32): name_off=10 (flags), size=4
        let flags_name_off: u32 = 10;
        type_data.extend_from_slice(&flags_name_off.to_le_bytes());
        type_data.extend_from_slice(&(BTF_KIND_INT << 24).to_le_bytes());
        type_data.extend_from_slice(&4u32.to_le_bytes());
        type_data.extend_from_slice(&0u32.to_le_bytes()); // int encoding

        // STRUCT "mystruct": kind_flag=1 (bit 31 of info set), vlen=1, size=4
        let struct_name_off: u32 = 1;
        // info: kind_flag(1) | kind(4 << 24) | vlen(1)
        let struct_info: u32 = (1u32 << 31) | (BTF_KIND_STRUCT << 24) | 1u32;
        type_data.extend_from_slice(&struct_name_off.to_le_bytes());
        type_data.extend_from_slice(&struct_info.to_le_bytes());
        type_data.extend_from_slice(&4u32.to_le_bytes()); // size=4
        // member: flags, type=1, offset encoding = (bitfield_size=3 << 24) | bit_offset=0
        let m_offset: u32 = (3u32 << 24) | 0u32; // 3-bit bitfield at bit 0
        type_data.extend_from_slice(&flags_name_off.to_le_bytes());
        type_data.extend_from_slice(&1u32.to_le_bytes()); // type_id=1
        type_data.extend_from_slice(&m_offset.to_le_bytes());

        let type_len = type_data.len() as u32;
        let hdr_len: u32 = 24;
        let mut btf = Vec::new();
        btf.extend_from_slice(&BTF_MAGIC.to_le_bytes());
        btf.push(1);
        btf.push(0);
        btf.extend_from_slice(&hdr_len.to_le_bytes());
        btf.extend_from_slice(&0u32.to_le_bytes());
        btf.extend_from_slice(&type_len.to_le_bytes());
        btf.extend_from_slice(&type_len.to_le_bytes());
        btf.extend_from_slice(&str_len.to_le_bytes());
        btf.extend_from_slice(&type_data);
        btf.extend_from_slice(strings);

        let layouts = extract_from_btf(&btf, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.name, "mystruct");
        // Should have one synthetic field representing the 4-byte storage unit
        assert_eq!(l.fields.len(), 1);
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[0].size, 4); // storage unit = u32 = 4 bytes
        assert!(l.fields[0].name.ends_with("__bits"));
    }

    #[test]
    fn btf_skips_unknown_kinds_gracefully() {
        // Build a BTF blob that has a FUNC kind (12) before the struct.
        // The parser should skip it and still extract the struct.
        let strings: &[u8] = b"\0foo\0x\0myfunc\0";
        let str_len = strings.len() as u32;
        // "foo" at 1, "x" at 5, "myfunc" at 7
        let mut type_data: Vec<u8> = Vec::new();

        // INT "x": name_off=5
        type_data.extend_from_slice(&5u32.to_le_bytes());
        type_data.extend_from_slice(&(BTF_KIND_INT << 24).to_le_bytes());
        type_data.extend_from_slice(&4u32.to_le_bytes());
        type_data.extend_from_slice(&0u32.to_le_bytes());

        // FUNC "myfunc": name_off=7, no extra bytes
        type_data.extend_from_slice(&7u32.to_le_bytes());
        type_data.extend_from_slice(&(BTF_KIND_FUNC << 24).to_le_bytes());
        type_data.extend_from_slice(&1u32.to_le_bytes()); // points to INT

        // STRUCT "foo": name_off=1, vlen=1, size=4
        type_data.extend_from_slice(&1u32.to_le_bytes());
        type_data.extend_from_slice(&((BTF_KIND_STRUCT << 24) | 1u32).to_le_bytes());
        type_data.extend_from_slice(&4u32.to_le_bytes());
        // member: x at bit_offset=0, type=1 (INT)
        type_data.extend_from_slice(&5u32.to_le_bytes());
        type_data.extend_from_slice(&1u32.to_le_bytes());
        type_data.extend_from_slice(&0u32.to_le_bytes());

        let type_len = type_data.len() as u32;
        let hdr_len: u32 = 24;
        let mut btf = Vec::new();
        btf.extend_from_slice(&BTF_MAGIC.to_le_bytes());
        btf.push(1);
        btf.push(0);
        btf.extend_from_slice(&hdr_len.to_le_bytes());
        btf.extend_from_slice(&0u32.to_le_bytes());
        btf.extend_from_slice(&type_len.to_le_bytes());
        btf.extend_from_slice(&type_len.to_le_bytes());
        btf.extend_from_slice(&str_len.to_le_bytes());
        btf.extend_from_slice(&type_data);
        btf.extend_from_slice(strings);

        let layouts = extract_from_btf(&btf, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "foo");
        assert_eq!(layouts[0].fields[0].name, "x");
    }
}
