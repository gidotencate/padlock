pub mod diff;
pub mod explain;
pub mod json;
pub mod sarif;
pub mod summary;

pub use diff::render_diff;
pub use explain::render_explain;
pub use json::to_json;
pub use sarif::to_sarif;
pub use summary::render_report;
