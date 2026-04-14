// Struct with obvious padding waste — used by integration tests.
#[repr(C)]
pub struct Padded {
    pub a: u8,
    pub b: u64,
    pub c: u8,
}

// Well-ordered struct — no padding, no findings expected.
#[repr(C)]
pub struct Clean {
    pub b: u64,
    pub a: u8,
    pub c: u8,
}
