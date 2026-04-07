// Integration tests for padlock-macros.
//
// These tests verify that `#[assert_no_padding]` and `#[assert_size]` compile
// successfully for well-formed structs. The negative cases (padding present,
// wrong size) are best validated via `trybuild` or manual inspection; testing
// compile failures requires a build harness. The tests here ensure the macros
// expand without panicking and produce correct runtime size relations.

#![allow(dead_code)]
use padlock_macros::{assert_no_padding, assert_size};

// ── assert_no_padding ─────────────────────────────────────────────────────────

// A well-ordered struct: u64(8) + u32(4) + u32(4) = 16 = size_of. No padding.
#[assert_no_padding]
struct WellOrdered {
    a: u64,
    b: u32,
    c: u32,
}

// Single-field struct: trivially no padding.
#[assert_no_padding]
struct Single {
    x: u64,
}

// Unit struct: no fields, assertion is trivially satisfied.
#[assert_no_padding]
struct UnitStruct;

// All u8 fields: no alignment gaps possible.
#[assert_no_padding]
struct BytePacked {
    a: u8,
    b: u8,
    c: u8,
    d: u8,
}

// repr(C) with descending alignment order — guaranteed no padding.
#[assert_no_padding]
#[repr(C)]
struct ReprCNoPadding {
    big: u64,
    mid: u32,
    small: u16,
    tiny: u8,
    _pad: u8, // explicit pad field to fill the last byte
}

// ── assert_size ───────────────────────────────────────────────────────────────

#[assert_size(8)]
struct ExactlyEightBytes {
    x: u64,
}

#[assert_size(4)]
struct ExactlyFourBytes {
    x: u32,
}

// ── runtime sanity checks ─────────────────────────────────────────────────────

#[test]
fn well_ordered_size_is_correct() {
    assert_eq!(std::mem::size_of::<WellOrdered>(), 16);
}

#[test]
fn single_size_is_correct() {
    assert_eq!(std::mem::size_of::<Single>(), 8);
}

#[test]
fn byte_packed_size_is_correct() {
    assert_eq!(std::mem::size_of::<BytePacked>(), 4);
}

#[test]
fn exactly_eight_bytes_size() {
    assert_eq!(std::mem::size_of::<ExactlyEightBytes>(), 8);
}

#[test]
fn exactly_four_bytes_size() {
    assert_eq!(std::mem::size_of::<ExactlyFourBytes>(), 4);
}
