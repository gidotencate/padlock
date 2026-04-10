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

/// Create a custom `ArchConfig` by overriding specific fields of a base arch.
///
/// Useful for `--cache-line-size` and `--word-size` CLI overrides.
/// The returned reference is intentionally leaked — CLI binaries are short-lived.
pub fn with_overrides(
    base: &ArchConfig,
    cache_line_size: Option<usize>,
    word_size: Option<usize>,
) -> &'static ArchConfig {
    let ptr = word_size.unwrap_or(base.pointer_size);
    let max_align = if word_size.is_some() {
        // 32-bit targets typically cap natural alignment at 8; 64-bit at 16
        if ptr <= 4 { 8 } else { base.max_align }
    } else {
        base.max_align
    };
    Box::leak(Box::new(ArchConfig {
        name: "custom",
        pointer_size: ptr,
        cache_line_size: cache_line_size.unwrap_or(base.cache_line_size),
        max_align,
        endianness: base.endianness,
    }))
}

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
