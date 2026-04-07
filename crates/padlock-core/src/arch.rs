#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Endianness {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArchConfig {
    pub name: &'static str,
    pub pointer_size: usize,
    pub cache_line_size: usize,
    pub max_align: usize,
    pub endianness: Endianness,
}

pub const X86_64_SYSV: ArchConfig = ArchConfig {
    name: "x86_64",
    pointer_size: 8,
    cache_line_size: 64,
    max_align: 16,
    endianness: Endianness::Little,
};

pub const AARCH64: ArchConfig = ArchConfig {
    name: "aarch64",
    pointer_size: 8,
    cache_line_size: 64,
    max_align: 16,
    endianness: Endianness::Little,
};

pub const AARCH64_APPLE: ArchConfig = ArchConfig {
    name: "aarch64-apple",
    pointer_size: 8,
    cache_line_size: 128,
    max_align: 16,
    endianness: Endianness::Little,
};

pub const WASM32: ArchConfig = ArchConfig {
    name: "wasm32",
    pointer_size: 4,
    cache_line_size: 64,
    max_align: 8,
    endianness: Endianness::Little,
};

pub const RISCV64: ArchConfig = ArchConfig {
    name: "riscv64",
    pointer_size: 8,
    cache_line_size: 64,
    max_align: 16,
    endianness: Endianness::Little,
};

/// Resolve an architecture name string to a static `ArchConfig` reference.
///
/// Accepted values: `x86_64`, `aarch64`, `aarch64_apple`, `wasm32`, `riscv64`.
pub fn arch_by_name(name: &str) -> Option<&'static ArchConfig> {
    match name {
        "x86_64" => Some(&X86_64_SYSV),
        "aarch64" => Some(&AARCH64),
        "aarch64_apple" => Some(&AARCH64_APPLE),
        "wasm32" => Some(&WASM32),
        "riscv64" => Some(&RISCV64),
        _ => None,
    }
}
