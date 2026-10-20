// Regression fixture for the Arc<T>/Rc<T> false-sharing false-positive fix
// (0.24.0). Two plain Arc<T> fields sharing a cache line is one of the most
// common patterns in real Rust code and must not be flagged as a
// concurrency hazard; Arc<Mutex<T>> genuinely wraps a lock and must still
// be flagged.

pub struct PlainArcConfig {
    pub name: std::sync::Arc<String>,
    pub logger: std::sync::Arc<Logger>,
}

pub struct Logger;

pub struct SharedMutexConfig {
    pub a: std::sync::Arc<std::sync::Mutex<u64>>,
    pub b: std::sync::Arc<std::sync::Mutex<u64>>,
}
