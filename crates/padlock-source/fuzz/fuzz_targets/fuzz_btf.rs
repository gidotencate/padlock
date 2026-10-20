#![no_main]

use libfuzzer_sys::fuzz_target;
use padlock_core::arch::X86_64_SYSV;

// Fuzzes the BTF binary parser directly against raw bytes (no UTF-8
// coercion needed — BTF is a binary format). The 4 adversarial tests added
// in 0.20.0 (truncated header, overflowing type_off/type_len, a lying vlen,
// a name offset past the string table) were hand-constructed; this harness
// explores inputs none of them anticipated.
fuzz_target!(|data: &[u8]| {
    let _ = padlock_dwarf::btf::extract_from_btf(data, &X86_64_SYSV);
});
