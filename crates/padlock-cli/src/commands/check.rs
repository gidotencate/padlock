// padlock-cli/src/commands/check.rs
//
// `padlock check [--baseline FILE] [--save-baseline] <paths>…`
//
// Ratchet / baseline mode: compare current findings against a saved snapshot.
// Only fails when things get *worse* relative to the baseline — existing issues
// that were already present do not block CI. Teams can adopt padlock without
// having to fix everything upfront.
//
// Workflow:
//   1. First run:  padlock check src/ --save-baseline --baseline .padlock-baseline.json
//   2. Every CI run: padlock check src/ --baseline .padlock-baseline.json
//      → exits 0 if no new regressions, 1 if any struct got worse
//
// A struct is "worse" if:
//   - Its worst finding severity increased (Low→Medium, Medium→High)
//   - Its score decreased by more than 1 point
//   - It is new (not in baseline) and has at least one High finding

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use padlock_core::findings::{Report, Severity};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::filter::FilterArgs;
use crate::paths::collect_layouts;

/// Per-struct snapshot stored in the baseline file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub struct_name: String,
    pub source_file: Option<String>,
    pub score: f64,
    /// Worst severity found ("none", "low", "medium", "high")
    pub worst_severity: String,
    pub wasted_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub padlock_version: String,
    pub structs: Vec<BaselineEntry>,
}

/// Result of comparing one struct against its baseline entry.
#[derive(Debug, Serialize)]
pub struct RegressionEntry {
    pub struct_name: String,
    pub source_file: Option<String>,
    pub reason: String,
    pub baseline_score: Option<f64>,
    pub current_score: f64,
}

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub regressions: Vec<RegressionEntry>,
    pub new_improvements: usize,
    pub unchanged: usize,
    pub passed: bool,
}

pub fn run(
    paths: &[PathBuf],
    baseline_path: Option<&Path>,
    save_baseline: bool,
    json: bool,
    filter: &FilterArgs,
) -> anyhow::Result<()> {
    let cfg = Config::for_path(
        paths
            .first()
            .map(|p| p.as_path())
            .unwrap_or(Path::new(".")),
    );

    let (mut layouts, analyzed) = collect_layouts(paths)?;
    layouts.retain(|l| !cfg.is_ignored(&l.name));
    filter.apply_to_layouts(&mut layouts)?;

    let mut report = Report::from_layouts(&layouts);
    report.analyzed_paths = analyzed;
    filter.apply_to_report(&mut report);

    // ── save-baseline mode ────────────────────────────────────────────────────
    if save_baseline {
        let path = baseline_path
            .unwrap_or(Path::new(".padlock-baseline.json"));
        let entries: Vec<BaselineEntry> = report
            .structs
            .iter()
            .map(|sr| BaselineEntry {
                struct_name: sr.struct_name.clone(),
                source_file: sr.source_file.clone(),
                score: sr.score,
                worst_severity: worst_severity_str(&sr.findings),
                wasted_bytes: sr.wasted_bytes,
            })
            .collect();
        let baseline = Baseline {
            padlock_version: env!("CARGO_PKG_VERSION").to_string(),
            structs: entries,
        };
        let serialized = serde_json::to_string_pretty(&baseline)?;
        std::fs::write(path, serialized)?;
        if !json {
            println!(
                "padlock: baseline saved to {} ({} structs)",
                path.display(),
                baseline.structs.len()
            );
        }
        return Ok(());
    }

    // ── comparison mode ───────────────────────────────────────────────────────
    let baseline = match baseline_path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|_| anyhow::anyhow!("baseline file not found: {}", p.display()))?;
            let b: Baseline = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("failed to parse baseline: {e}"))?;
            b
        }
        None => {
            // No baseline — just run as a normal analysis with exit-code logic.
            let has_high = report
                .structs
                .iter()
                .any(|s| worst_severity_str(&s.findings) == "high");
            if json {
                let result = CheckResult {
                    regressions: vec![],
                    new_improvements: 0,
                    unchanged: report.structs.len(),
                    passed: !has_high,
                };
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print!("{}", padlock_output::render_report(&report));
                if has_high {
                    eprintln!("\npadlock: check failed — High severity findings present (use --save-baseline to establish a baseline)");
                }
            }
            if has_high {
                std::process::exit(1);
            }
            return Ok(());
        }
    };

    // Build lookup: (struct_name, source_file) → BaselineEntry
    let baseline_map: HashMap<(String, Option<String>), &BaselineEntry> = baseline
        .structs
        .iter()
        .map(|e| ((e.struct_name.clone(), e.source_file.clone()), e))
        .collect();

    let mut regressions: Vec<RegressionEntry> = Vec::new();
    let mut improvements = 0usize;
    let mut unchanged = 0usize;

    for sr in &report.structs {
        let key = (sr.struct_name.clone(), sr.source_file.clone());
        let current_worst = worst_severity_str(&sr.findings);

        match baseline_map.get(&key) {
            Some(base) => {
                let sev_regressed =
                    severity_rank(&current_worst) > severity_rank(&base.worst_severity);
                // Score decrease of more than 1 point is a regression.
                let score_regressed = sr.score < base.score - 1.0;

                if sev_regressed || score_regressed {
                    let reason = if sev_regressed {
                        format!(
                            "severity increased: {} → {}",
                            base.worst_severity, current_worst
                        )
                    } else {
                        format!(
                            "score dropped: {:.0} → {:.0}",
                            base.score, sr.score
                        )
                    };
                    regressions.push(RegressionEntry {
                        struct_name: sr.struct_name.clone(),
                        source_file: sr.source_file.clone(),
                        reason,
                        baseline_score: Some(base.score),
                        current_score: sr.score,
                    });
                } else if sr.score > base.score + 1.0 {
                    improvements += 1;
                } else {
                    unchanged += 1;
                }
            }
            None => {
                // New struct not in baseline — flag if it has High findings.
                if current_worst == "high" {
                    regressions.push(RegressionEntry {
                        struct_name: sr.struct_name.clone(),
                        source_file: sr.source_file.clone(),
                        reason: "new struct with High severity finding".to_string(),
                        baseline_score: None,
                        current_score: sr.score,
                    });
                } else {
                    unchanged += 1;
                }
            }
        }
    }

    let passed = regressions.is_empty();
    let result = CheckResult {
        regressions,
        new_improvements: improvements,
        unchanged,
        passed,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        render_check_result(&result);
    }

    if !passed {
        std::process::exit(1);
    }

    Ok(())
}

fn render_check_result(result: &CheckResult) {
    if result.passed {
        println!(
            "padlock check passed — no regressions ({} unchanged, {} improved)",
            result.unchanged, result.new_improvements
        );
    } else {
        eprintln!(
            "padlock check FAILED — {} regression(s):\n",
            result.regressions.len()
        );
        for r in &result.regressions {
            let loc = r
                .source_file
                .as_deref()
                .map(|f| format!(" ({})", f))
                .unwrap_or_default();
            eprintln!("  [REGRESSION] {}{}", r.struct_name, loc);
            eprintln!("    {}", r.reason);
            if let Some(base) = r.baseline_score {
                eprintln!("    score: {:.0} → {:.0}", base, r.current_score);
            }
        }
        eprintln!(
            "\n{} unchanged, {} improved",
            result.unchanged, result.new_improvements
        );
    }
}

fn worst_severity_str(findings: &[padlock_core::findings::Finding]) -> String {
    let mut worst = 0u8;
    for f in findings {
        let rank = match f.severity() {
            Severity::High => 3,
            Severity::Medium => 2,
            Severity::Low => 1,
        };
        worst = worst.max(rank);
    }
    match worst {
        3 => "high".to_string(),
        2 => "medium".to_string(),
        1 => "low".to_string(),
        _ => "none".to_string(),
    }
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_severity_empty_is_none() {
        assert_eq!(worst_severity_str(&[]), "none");
    }

    #[test]
    fn severity_rank_ordering() {
        assert!(severity_rank("high") > severity_rank("medium"));
        assert!(severity_rank("medium") > severity_rank("low"));
        assert!(severity_rank("low") > severity_rank("none"));
    }

    #[test]
    fn baseline_round_trips_json() {
        let b = Baseline {
            padlock_version: "0.4.0".into(),
            structs: vec![BaselineEntry {
                struct_name: "Foo".into(),
                source_file: Some("foo.rs".into()),
                score: 90.0,
                worst_severity: "low".into(),
                wasted_bytes: 2,
            }],
        };
        let json = serde_json::to_string(&b).unwrap();
        let b2: Baseline = serde_json::from_str(&json).unwrap();
        assert_eq!(b2.structs[0].struct_name, "Foo");
        assert_eq!(b2.structs[0].score, 90.0);
    }
}
