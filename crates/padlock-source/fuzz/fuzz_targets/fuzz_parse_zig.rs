#![no_main]

use libfuzzer_sys::fuzz_target;
use padlock_core::arch::X86_64_SYSV;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = padlock_source::frontends::zig::parse_zig(&source, &X86_64_SYSV);
});
