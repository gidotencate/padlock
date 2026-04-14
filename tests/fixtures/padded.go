package fixtures

// Padded has obvious padding waste — used by integration tests.
type Padded struct {
	A byte
	B float64
	C byte
}

// Clean is well-ordered — no padding, no findings expected.
type Clean struct {
	B float64
	A byte
	C byte
}
