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

    gimli::Dwarf::load(load_section)
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
    #[cfg(target_arch = "x86_64")]
    {
        &X86_64_SYSV
    }
    #[cfg(target_arch = "aarch64")]
    {
        &AARCH64
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        &X86_64_SYSV
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── synthetic binary header helpers ──────────────────────────────────────

    /// Build a minimal 64-bit little-endian ELF header for the given machine
    /// code. The header is valid enough for `object::File::parse` to accept it
    /// (magic, class, data, version, e_ehsize, zero section count).
    fn minimal_elf64(machine: u16) -> Vec<u8> {
        let mut h = vec![0u8; 64];
        // ELF identification
        h[0..4].copy_from_slice(b"\x7fELF");
        h[4] = 2; // ELFCLASS64
        h[5] = 1; // ELFDATA2LSB (little-endian)
        h[6] = 1; // EV_CURRENT
        h[7] = 0; // ELFOSABI_NONE
                  // e_type = ET_REL (1)
        h[16] = 1;
        h[17] = 0;
        // e_machine
        h[18] = (machine & 0xff) as u8;
        h[19] = (machine >> 8) as u8;
        // e_version = 1
        h[20] = 1;
        // e_ehsize = 64
        h[52] = 64;
        // e_shentsize = 64 (even with 0 sections object crate expects this)
        h[58] = 64;
        h
    }

    /// Build a minimal 64-bit little-endian Mach-O header for AArch64.
    /// 32 bytes total (MH_MAGIC_64 header, zero load commands).
    fn minimal_macho_arm64() -> Vec<u8> {
        let mut h = vec![0u8; 32];
        // MH_MAGIC_64 = 0xFEEDFACF → little-endian bytes
        h[0..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        // CPU_TYPE_ARM64 = 0x0100000C
        h[4..8].copy_from_slice(&0x0100_000Cu32.to_le_bytes());
        // cpusubtype = CPU_SUBTYPE_ARM64_ALL = 0
        // filetype = MH_OBJECT = 1
        h[12..16].copy_from_slice(&1u32.to_le_bytes());
        // ncmds = 0, sizeofcmds = 0, flags = 0, reserved = 0
        h
    }

    // ELF machine codes
    const EM_X86_64: u16 = 0x3e;
    const EM_AARCH64: u16 = 0xb7;
    const EM_RISCV: u16 = 0xf3;

    // ── detect_arch tests ─────────────────────────────────────────────────────

    #[test]
    fn detect_arch_x86_64_elf() {
        let elf = minimal_elf64(EM_X86_64);
        let arch = detect_arch(&elf).unwrap();
        assert_eq!(arch.name, "x86_64");
        assert_eq!(arch.pointer_size, 8);
        assert_eq!(arch.cache_line_size, 64);
    }

    #[test]
    fn detect_arch_aarch64_elf() {
        let elf = minimal_elf64(EM_AARCH64);
        let arch = detect_arch(&elf).unwrap();
        assert_eq!(arch.name, "aarch64");
        assert_eq!(arch.pointer_size, 8);
        assert_eq!(arch.cache_line_size, 64);
    }

    #[test]
    fn detect_arch_aarch64_macho_returns_apple_variant() {
        let macho = minimal_macho_arm64();
        let arch = detect_arch(&macho).unwrap();
        assert_eq!(arch.name, "aarch64-apple");
        assert_eq!(arch.cache_line_size, 128); // Apple Silicon: 128-byte cache lines
    }

    #[test]
    fn detect_arch_riscv64_elf() {
        let elf = minimal_elf64(EM_RISCV);
        let arch = detect_arch(&elf).unwrap();
        assert_eq!(arch.name, "riscv64");
    }

    #[test]
    fn detect_arch_rejects_garbage() {
        let garbage = vec![0u8; 64];
        assert!(detect_arch(&garbage).is_err());
    }

    #[test]
    fn detect_arch_from_host_returns_valid_config() {
        let arch = detect_arch_from_host();
        // Must be one of the known configs
        assert!(matches!(arch.name, "x86_64" | "aarch64" | "aarch64-apple"));
        assert!(arch.pointer_size == 4 || arch.pointer_size == 8);
        assert!(arch.cache_line_size > 0);
    }
}
