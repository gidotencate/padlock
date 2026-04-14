// padlock-output/src/sarif.rs
//
// Produces SARIF 2.1.0 — https://docs.oasis-open.org/sarif/sarif/v2.1.0/

use padlock_core::findings::{Finding, Report};
use serde::Serialize;

// ── SARIF schema types ────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SarifRoot {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<SarifRun>,
}

#[derive(Serialize)]
struct SarifRun {
    tool: SarifTool,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(Serialize)]
struct SarifDriver {
    name: &'static str,
    version: &'static str,
    rules: Vec<SarifRule>,
}

#[derive(Serialize)]
struct SarifRule {
    id: String,
    #[serde(rename = "shortDescription")]
    short_description: SarifMessage,
}

#[derive(Serialize)]
struct SarifResult {
    #[serde(rename = "ruleId")]
    rule_id: String,
    level: &'static str,
    message: SarifMessage,
    locations: Vec<SarifLocation>,
}

#[derive(Serialize)]
struct SarifMessage {
    text: String,
}

#[derive(Serialize)]
struct SarifLocation {
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysicalLocation,
}

#[derive(Serialize)]
struct SarifPhysicalLocation {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifactLocation,
    region: SarifRegion,
}

#[derive(Serialize)]
struct SarifArtifactLocation {
    uri: String,
}

#[derive(Serialize)]
struct SarifRegion {
    #[serde(rename = "startLine")]
    start_line: u32,
}

// ── rule catalogue ────────────────────────────────────────────────────────────

fn rules() -> Vec<SarifRule> {
    vec![
        SarifRule {
            id: "PAD001".into(),
            short_description: SarifMessage {
                text: "Struct padding waste".into(),
            },
        },
        SarifRule {
            id: "PAD002".into(),
            short_description: SarifMessage {
                text: "False sharing risk".into(),
            },
        },
        SarifRule {
            id: "PAD003".into(),
            short_description: SarifMessage {
                text: "Field reorder suggestion".into(),
            },
        },
        SarifRule {
            id: "PAD004".into(),
            short_description: SarifMessage {
                text: "Cache locality issue".into(),
            },
        },
    ]
}

fn level_for(finding: &Finding) -> &'static str {
    use padlock_core::findings::Severity;
    match finding.severity() {
        Severity::High => "error",
        Severity::Medium => "warning",
        Severity::Low => "note",
    }
}

fn rule_id_for(finding: &Finding) -> &'static str {
    match finding {
        Finding::PaddingWaste { .. } => "PAD001",
        Finding::FalseSharing { .. } => "PAD002",
        Finding::ReorderSuggestion { .. } => "PAD003",
        Finding::LocalityIssue { .. } => "PAD004",
    }
}

fn message_for(finding: &Finding) -> String {
    match finding {
        Finding::PaddingWaste {
            wasted_bytes,
            waste_pct,
            struct_name,
            gaps,
            ..
        } => {
            let gap_detail: Vec<String> = gaps
                .iter()
                .take(3)
                .map(|g| {
                    format!(
                        "{}B after `{}` (offset {})",
                        g.bytes, g.after_field, g.at_offset
                    )
                })
                .collect();
            let detail = if gaps.len() > 3 {
                format!("{} and {} more gaps", gap_detail.join(", "), gaps.len() - 3)
            } else {
                gap_detail.join(", ")
            };
            format!("{struct_name}: {wasted_bytes}B wasted ({waste_pct:.0}% of struct) — {detail}")
        }
        Finding::FalseSharing {
            struct_name,
            conflicts,
            is_inferred,
            ..
        } => {
            let field_lists: Vec<String> = conflicts
                .iter()
                .map(|c| format!("cache line {}: [{}]", c.cache_line, c.fields.join(", ")))
                .collect();
            let inferred = if *is_inferred {
                " (inferred from type names)"
            } else {
                ""
            };
            format!(
                "{struct_name}: false sharing — {}{}",
                field_lists.join("; "),
                inferred
            )
        }
        Finding::ReorderSuggestion {
            struct_name,
            savings,
            original_size,
            optimized_size,
            suggested_order,
            ..
        } => format!(
            "{struct_name}: reordering fields saves {savings}B ({original_size}B → {optimized_size}B): {}",
            suggested_order.join(", ")
        ),
        Finding::LocalityIssue {
            struct_name,
            hot_fields,
            cold_fields,
            is_inferred,
            ..
        } => {
            let inferred = if *is_inferred {
                " (inferred from type names)"
            } else {
                ""
            };
            format!(
                "{struct_name}: hot fields [{}] interleaved with cold [{}]{}",
                hot_fields.join(", "),
                cold_fields.join(", "),
                inferred
            )
        }
    }
}

/// Serialize findings to a SARIF 2.1.0 JSON string.
pub fn to_sarif(report: &Report) -> anyhow::Result<String> {
    let mut results = Vec::new();

    for sr in &report.structs {
        for finding in &sr.findings {
            let uri = sr.source_file.clone().unwrap_or_else(|| "unknown".into());
            let line = sr.source_line.unwrap_or(1);

            results.push(SarifResult {
                rule_id: rule_id_for(finding).into(),
                level: level_for(finding),
                message: SarifMessage {
                    text: message_for(finding),
                },
                locations: vec![SarifLocation {
                    physical_location: SarifPhysicalLocation {
                        artifact_location: SarifArtifactLocation { uri },
                        region: SarifRegion { start_line: line },
                    },
                }],
            });
        }
    }

    let root = SarifRoot {
        schema: "https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0-rtm.5.json",
        version: "2.1.0",
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "padlock",
                    version: env!("CARGO_PKG_VERSION"),
                    rules: rules(),
                },
            },
            results,
        }],
    };

    Ok(serde_json::to_string_pretty(&root)?)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::findings::Report;
    use padlock_core::ir::test_fixtures::connection_layout;

    #[test]
    fn sarif_is_valid_json() {
        let report = Report::from_layouts(&[connection_layout()]);
        let sarif = to_sarif(&report).unwrap();
        let val: serde_json::Value = serde_json::from_str(&sarif).expect("invalid JSON");
        assert!(val.is_object());
    }

    #[test]
    fn sarif_version_is_2_1_0() {
        let report = Report::from_layouts(&[connection_layout()]);
        let sarif = to_sarif(&report).unwrap();
        let val: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        assert_eq!(val["version"], "2.1.0");
    }

    #[test]
    fn sarif_has_runs_array() {
        let report = Report::from_layouts(&[connection_layout()]);
        let sarif = to_sarif(&report).unwrap();
        let val: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        assert!(val["runs"].is_array());
        assert!(!val["runs"].as_array().unwrap().is_empty());
    }

    #[test]
    fn sarif_results_contain_pad001() {
        let report = Report::from_layouts(&[connection_layout()]);
        let sarif = to_sarif(&report).unwrap();
        assert!(sarif.contains("PAD001"));
    }
}
