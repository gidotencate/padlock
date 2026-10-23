// Struct with obvious padding waste — used by integration tests.
const Padded = extern struct {
    a: u8,
    b: f64,
    c: u8,
};

// Struct that is already optimally ordered.
const Tight = extern struct {
    b: f64,
    a: u8,
    c: u8,
};
