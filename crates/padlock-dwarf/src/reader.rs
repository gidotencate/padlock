use std::rc::Rc;

use gimli::read::EndianRcSlice;
use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
use padlock_core::arch::{ArchConfig, AARCH64, AARCH64_APPLE, RISCV64, WASM32, X86_64_SYSV};

pub type DwarfRc = gimli::Dwarf<EndianRcSlice<gimli::RunTimeEndian>>;

/// Parse DWARF debug info from raw binary data.
///
/// For unlinked object files, ELF relocations in DWARF sections are applied
/// so that string offsets and cross-section references resolve correctly.
pub fn load(binary_data: &[u8]) -> anyhow::Result<DwarfRc> {
    let file = object::File::parse(binary_data)?;
    let endian = if file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    let load_section =
        |id: gimli::SectionId| -> anyhow::Result<EndianRcSlice<gimli::RunTimeEndian>> {
            let data: Vec<u8> = match file.section_by_name(id.name()) {
                Some(s) => load_section_with_relocations(&file, endian, s)?,
                None => Vec::new(),
            };
            Ok(EndianRcSlice::new(Rc::from(data.as_slice()), endian))
        };

    Ok(gimli::Dwarf::load(load_section)?)
}

/// Load a section's bytes and apply any ELF RELA relocations targeting it.
///
/// In unlinked `.o` files, DWARF cross-section references (e.g. `DW_FORM_strp`
/// entries in `.debug_info` pointing into `.debug_str`) are stored as zero with
/// an associated relocation whose addend is the real offset.  Applying the
/// relocations here gives gimli the fully-resolved bytes it expects.
fn load_section_with_relocations(
    file: &object::File<'_>,
    endian: gimli::RunTimeEndian,
    section: object::Section<'_, '_>,
) -> anyhow::Result<Vec<u8>> {
    let mut data = section.uncompressed_data()?.into_owned();
    let is_little = matches!(endian, gimli::RunTimeEndian::Little);

    for (offset, reloc) in section.relocations() {
        // Compute the target value: section/symbol base address + addend.
        // In relocatable `.o` files, section addresses are 0, so the addend
        // alone gives the correct intra-section offset.
        let target: u64 = match reloc.target() {
            RelocationTarget::Section(idx) => {
                let sec = file.section_by_index(idx)?;
                (sec.address() as i64 + reloc.addend()) as u64
            }
            RelocationTarget::Symbol(sym_idx) => {
                let sym = file.symbol_by_index(sym_idx)?;
                (sym.address() as i64 + reloc.addend()) as u64
            }
            _ => continue,
        };

        let off = offset as usize;
        match reloc.size() {
            32 => {
                let bytes = if is_little {
                    (target as u32).to_le_bytes()
                } else {
                    (target as u32).to_be_bytes()
                };
                if off + 4 <= data.len() {
                    data[off..off + 4].copy_from_slice(&bytes);
                }
            }
            64 => {
                let bytes = if is_little {
                    target.to_le_bytes()
                } else {
                    target.to_be_bytes()
                };
                if off + 8 <= data.len() {
                    data[off..off + 8].copy_from_slice(&bytes);
                }
            }
            _ => {}
        }
    }

    Ok(data)
}

/// Detect the target architecture from a binary.
pub fn detect_arch(binary_data: &[u8]) -> anyhow::Result<&'static ArchConfig> {
    let file = object::File::parse(binary_data)?;
    match file.architecture() {
        object::Architecture::X86_64 => Ok(&X86_64_SYSV),
        object::Architecture::Aarch64 => {
            if is_apple_binary(&file) {
                Ok(&AARCH64_APPLE)
            } else {
                Ok(&AARCH64)
            }
        }
        object::Architecture::Wasm32 => Ok(&WASM32),
        object::Architecture::Riscv64 => Ok(&RISCV64),
        other => Err(anyhow::anyhow!("unsupported architecture: {:?}", other)),
    }
}

fn is_apple_binary(file: &object::File<'_>) -> bool {
    matches!(file.format(), object::BinaryFormat::MachO)
}

/// Return the architecture of the machine running padlock.
/// Used when analysing source files (no binary available to inspect).
pub fn detect_arch_from_host() -> &'static ArchConfig {
    #[cfg(target_arch = "x86_64")]  { &X86_64_SYSV }
    #[cfg(target_arch = "aarch64")] { &AARCH64 }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))] { &X86_64_SYSV }
}
