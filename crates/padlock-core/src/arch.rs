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

/// ARM Cortex-M0 / M0+ / M3 — no cache.
///
/// cache_line_size = 0 means "no cache": false-sharing analysis is suppressed
/// entirely for this architecture (division by zero is guarded in the analysis
/// passes). Padding waste and reorder findings still apply.
pub const CORTEX_M: ArchConfig = ArchConfig {
    name: "cortex_m",
    pointer_size: 4,
    cache_line_size: 0,
    max_align: 8,
    endianness: Endianness::Little,
};

/// ARM Cortex-M4 / M4F / M7 / M33 — 32-byte cache lines.
///
/// The M4 and M7 have optional L1 data caches (32-byte lines). M33 is similar.
/// False-sharing analysis is active for these targets.
pub const CORTEX_M4: ArchConfig = ArchConfig {
    name: "cortex_m4",
    pointer_size: 4,
    cache_line_size: 32,
    max_align: 8,
    endianness: Endianness::Little,
};

/// AVR 8-bit (ATmega328P, ATmega2560, etc.) — no cache, 2-byte pointers.
///
/// Every type is 1-byte aligned on AVR so padding waste findings will be rare.
/// False-sharing analysis is suppressed (cache_line_size = 0).
pub const AVR: ArchConfig = ArchConfig {
    name: "avr",
    pointer_size: 2,
    cache_line_size: 0,
    max_align: 1,
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

/// Resolve an architecture name string or Rust target triple to a static `ArchConfig`.
///
/// Short names: `x86_64`, `aarch64`, `aarch64_apple`, `wasm32`, `riscv64`.
///
/// Common Rust target triples are also accepted, for example:
/// - `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`
/// - `aarch64-unknown-linux-gnu`, `aarch64-linux-android`
/// - `aarch64-apple-darwin`, `aarch64-apple-ios`
/// - `wasm32-unknown-unknown`, `wasm32-wasi`
/// - `riscv64gc-unknown-linux-gnu`
pub fn arch_by_name(name: &str) -> Option<&'static ArchConfig> {
    match name {
        // Short names (used in config files and existing code).
        "x86_64" => Some(&X86_64_SYSV),
        "aarch64" => Some(&AARCH64),
        "aarch64_apple" => Some(&AARCH64_APPLE),
        "wasm32" => Some(&WASM32),
        "riscv64" => Some(&RISCV64),
        "cortex_m" => Some(&CORTEX_M),
        "cortex_m4" => Some(&CORTEX_M4),
        "avr" => Some(&AVR),
        // Rust target triples — matched by prefix for flexibility.
        _ => arch_by_triple(name),
    }
}

/// Map a Rust target triple to an `ArchConfig`.
pub fn arch_by_triple(triple: &str) -> Option<&'static ArchConfig> {
    if triple.starts_with("x86_64-") {
        Some(&X86_64_SYSV)
    } else if triple.starts_with("aarch64-apple-") {
        // Apple Silicon has a 128-byte cache line.
        Some(&AARCH64_APPLE)
    } else if triple.starts_with("aarch64-") {
        Some(&AARCH64)
    } else if triple.starts_with("wasm32-") {
        Some(&WASM32)
    } else if triple.starts_with("riscv64") {
        Some(&RISCV64)
    // ARM Cortex-M — no cache variants (M0, M0+, M3, M23)
    } else if triple.starts_with("thumbv6m-")
        || triple.starts_with("thumbv7m-")
        || triple.starts_with("thumbv8m.base-")
    {
        Some(&CORTEX_M)
    // ARM Cortex-M — cached variants (M4, M7, M33)
    } else if triple.starts_with("thumbv7em-") || triple.starts_with("thumbv8m.main-") {
        Some(&CORTEX_M4)
    // AVR
    } else if triple.starts_with("avr-") {
        Some(&AVR)
    } else {
        None
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_resolve() {
        assert_eq!(arch_by_name("x86_64"), Some(&X86_64_SYSV));
        assert_eq!(arch_by_name("aarch64"), Some(&AARCH64));
        assert_eq!(arch_by_name("aarch64_apple"), Some(&AARCH64_APPLE));
        assert_eq!(arch_by_name("wasm32"), Some(&WASM32));
        assert_eq!(arch_by_name("riscv64"), Some(&RISCV64));
    }

    #[test]
    fn target_triples_resolve() {
        assert_eq!(arch_by_name("x86_64-unknown-linux-gnu"), Some(&X86_64_SYSV));
        assert_eq!(arch_by_name("x86_64-pc-windows-msvc"), Some(&X86_64_SYSV));
        assert_eq!(arch_by_name("aarch64-unknown-linux-gnu"), Some(&AARCH64));
        assert_eq!(arch_by_name("aarch64-linux-android"), Some(&AARCH64));
        // Apple targets get the 128-byte cache line config.
        assert_eq!(arch_by_name("aarch64-apple-darwin"), Some(&AARCH64_APPLE));
        assert_eq!(arch_by_name("aarch64-apple-ios"), Some(&AARCH64_APPLE));
        assert_eq!(arch_by_name("wasm32-unknown-unknown"), Some(&WASM32));
        assert_eq!(arch_by_name("riscv64gc-unknown-linux-gnu"), Some(&RISCV64));
    }

    #[test]
    fn unknown_triple_returns_none() {
        assert_eq!(arch_by_name("mips-unknown-linux-gnu"), None);
        assert_eq!(arch_by_name("totally-bogus"), None);
    }

    #[test]
    fn apple_aarch64_has_128_byte_cache_line() {
        let cfg = arch_by_name("aarch64-apple-darwin").unwrap();
        assert_eq!(cfg.cache_line_size, 128);
    }

    #[test]
    fn cortex_m_short_names() {
        assert_eq!(arch_by_name("cortex_m"), Some(&CORTEX_M));
        assert_eq!(arch_by_name("cortex_m4"), Some(&CORTEX_M4));
        assert_eq!(arch_by_name("avr"), Some(&AVR));
    }

    #[test]
    fn cortex_m_no_cache_variants() {
        // M0/M0+ → no cache
        assert_eq!(arch_by_name("thumbv6m-none-eabi"), Some(&CORTEX_M));
        assert_eq!(
            arch_by_name("thumbv6m-none-eabi").unwrap().cache_line_size,
            0
        );
        // M3 → no cache
        assert_eq!(arch_by_name("thumbv7m-none-eabi"), Some(&CORTEX_M));
        // M23 → no cache
        assert_eq!(arch_by_name("thumbv8m.base-none-eabi"), Some(&CORTEX_M));
    }

    #[test]
    fn cortex_m4_cached_variants() {
        // M4/M4F → 32-byte cache lines
        assert_eq!(arch_by_name("thumbv7em-none-eabi"), Some(&CORTEX_M4));
        assert_eq!(arch_by_name("thumbv7em-none-eabihf"), Some(&CORTEX_M4));
        assert_eq!(
            arch_by_name("thumbv7em-none-eabi").unwrap().cache_line_size,
            32
        );
        // M33 → 32-byte cache lines
        assert_eq!(arch_by_name("thumbv8m.main-none-eabi"), Some(&CORTEX_M4));
        assert_eq!(arch_by_name("thumbv8m.main-none-eabihf"), Some(&CORTEX_M4));
    }

    #[test]
    fn avr_triple() {
        assert_eq!(arch_by_name("avr-unknown-gnu-atmega328p"), Some(&AVR));
        assert_eq!(arch_by_name("avr").unwrap().pointer_size, 2);
        assert_eq!(arch_by_name("avr").unwrap().cache_line_size, 0);
    }

    #[test]
    fn cortex_m_has_4_byte_pointers() {
        assert_eq!(CORTEX_M.pointer_size, 4);
        assert_eq!(CORTEX_M4.pointer_size, 4);
    }
}
