#![no_main]

use libfuzzer_sys::fuzz_target;
use padlock_core::arch::X86_64_SYSV;

// See fuzz_parse_c.rs — same goal, C++ frontend (templates, inheritance,
// bitfields, alignas, pragma pack all add parsing paths the C frontend
// doesn't have).
fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = padlock_source::frontends::c_cpp::parse_cpp(&source, &X86_64_SYSV);
});
