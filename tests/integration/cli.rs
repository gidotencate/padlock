use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn padlock() -> Command {
    Command::cargo_bin("padlock").unwrap()
}

/// Resolve a path relative to the workspace root from any crate's test binary.
/// CARGO_MANIFEST_DIR is `crates/padlock-cli`; workspace root is two levels up.
fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

// ── version ───────────────────────────────────────────────────────────────────

#[test]
fn version_flag() {
    padlock()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("padlock"));
}

// ── analyze: C ────────────────────────────────────────────────────────────────

#[test]
fn analyze_c_padded_exits_zero() {
    padlock()
        .args(["analyze", &fixture("padded.c")])
        .assert()
        .success();
}

#[test]
fn analyze_c_padded_finds_struct() {
    padlock()
        .args(["analyze", &fixture("padded.c"), "--json"])
        .assert()
        .success()
        .stdout(contains("Padded"));
}

#[test]
fn analyze_c_padded_reports_findings() {
    padlock()
        .args(["analyze", &fixture("padded.c"), "--json"])
        .assert()
        .success()
        .stdout(contains("PaddingWaste"));
}

#[test]
fn analyze_c_fail_on_severity_exits_nonzero() {
    padlock()
        .args([
            "analyze",
            &fixture("padded.c"),
            "--fail-on-severity",
            "high",
        ])
        .assert()
        .failure();
}

// ── analyze: Rust ─────────────────────────────────────────────────────────────

#[test]
fn analyze_rust_padded_finds_struct() {
    padlock()
        .args(["analyze", &fixture("padded.rs"), "--json"])
        .assert()
        .success()
        .stdout(contains("Padded"));
}

#[test]
fn analyze_rust_padded_suggests_reorder() {
    padlock()
        .args(["analyze", &fixture("padded.rs"), "--json"])
        .assert()
        .success()
        .stdout(contains("ReorderSuggestion"));
}

// ── analyze: Go ───────────────────────────────────────────────────────────────

#[test]
fn analyze_go_padded_finds_struct() {
    padlock()
        .args(["analyze", &fixture("padded.go"), "--json"])
        .assert()
        .success()
        .stdout(contains("Padded"));
}

// ── output formats ────────────────────────────────────────────────────────────

#[test]
fn analyze_sarif_output_is_valid_json() {
    let output = padlock()
        .args(["analyze", &fixture("padded.c"), "--sarif"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value =
        serde_json::from_slice(&output).expect("SARIF output should be valid JSON");
    assert_eq!(parsed["version"], "2.1.0");
}
