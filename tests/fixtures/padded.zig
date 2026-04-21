// Struct with obvious padding waste — used by integration tests.
const Padded = struct {
    a: u8,
    b: f64,
    c: u8,
};

// Struct that is already optimally ordered.
const Tight = struct {
    b: f64,
    a: u8,
    c: u8,
};
