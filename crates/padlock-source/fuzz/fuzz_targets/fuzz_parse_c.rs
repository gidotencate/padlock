#![no_main]

use libfuzzer_sys::fuzz_target;
use padlock_core::arch::X86_64_SYSV;

// Fuzzes the C frontend against arbitrary byte input coerced to valid UTF-8.
// Goal: must never panic, regardless of what garbage/binary data lands in a
// .c file — a parse error (Err) is an acceptable, expected outcome; a panic
// is not. This is exactly the bug class fixed in 0.19.0 (a fixed-size
// byte-offset lookback window panicking on a multi-byte comment character).
fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = padlock_source::frontends::c_cpp::parse_c(&source, &X86_64_SYSV);
});
