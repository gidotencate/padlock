// padlock-output/src/json.rs

use padlock_core::findings::Report;

/// Serialize the full report to a pretty-printed JSON string.
pub fn to_json(report: &Report) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(report)?)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::findings::Report;
    use padlock_core::ir::test_fixtures::connection_layout;

    #[test]
    fn json_is_valid() {
        let report = Report::from_layouts(&[connection_layout()]);
        let json = to_json(&report).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).expect("invalid JSON");
        assert!(val.is_object());
    }

    #[test]
    fn json_contains_struct_name() {
        let report = Report::from_layouts(&[connection_layout()]);
        let json = to_json(&report).unwrap();
        assert!(json.contains("Connection"));
    }

    #[test]
    fn json_has_structs_array() {
        let report = Report::from_layouts(&[connection_layout()]);
        let json = to_json(&report).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(val["structs"].is_array());
    }
}
