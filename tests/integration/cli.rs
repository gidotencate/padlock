use assert_cmd::Command;
use predicates::str::contains;
use std::io::Write;
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

// ── analyze: Zig ─────────────────────────────────────────────────────────────

#[test]
fn analyze_zig_padded_finds_struct() {
    padlock()
        .args(["analyze", &fixture("padded.zig"), "--json"])
        .assert()
        .success()
        .stdout(contains("Padded"));
}

#[test]
fn analyze_zig_padded_suggests_reorder() {
    padlock()
        .args(["analyze", &fixture("padded.zig"), "--json"])
        .assert()
        .success()
        .stdout(contains("ReorderSuggestion"));
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

// ── fix: dry-run exit code ────────────────────────────────────────────────────

/// `fix --dry-run` must exit 1 when there are pending reorderings.
#[test]
fn fix_dry_run_exits_nonzero_when_fixable() {
    let mut f = tempfile::Builder::new().suffix(".c").tempfile().unwrap();
    write!(
        f,
        "struct S {{\n    char a;\n    double b;\n    char c;\n}};\n"
    )
    .unwrap();
    padlock()
        .args(["fix", "--dry-run", f.path().to_str().unwrap()])
        .assert()
        .failure(); // exit 1 — pending changes
}

/// `fix --dry-run` must exit 0 when every struct is already optimal.
#[test]
fn fix_dry_run_exits_zero_when_already_optimal() {
    let mut f = tempfile::Builder::new().suffix(".c").tempfile().unwrap();
    // double first → no reorder needed
    write!(
        f,
        "struct S {{\n    double b;\n    char a;\n    char c;\n}};\n"
    )
    .unwrap();
    padlock()
        .args(["fix", "--dry-run", f.path().to_str().unwrap()])
        .assert()
        .success(); // exit 0 — nothing to do
}

// ── fix: in-place rewrite ─────────────────────────────────────────────────────

/// `fix` rewrites the file in-place and produces the correct field order.
#[test]
fn fix_rewrites_c_struct_in_place() {
    let mut f = tempfile::Builder::new().suffix(".c").tempfile().unwrap();
    write!(
        f,
        "struct S {{\n    char a;\n    double b;\n    char c;\n}};\n"
    )
    .unwrap();
    let path = f.path().to_str().unwrap().to_string();

    padlock().args(["fix", &path]).assert().success();

    let contents = std::fs::read_to_string(&path).unwrap();
    let b_pos = contents.find("double b").unwrap();
    let a_pos = contents.find("char a").unwrap();
    assert!(b_pos < a_pos, "double must appear before char after fix");
    // No blank line after the opening brace
    let brace = contents.find('{').unwrap();
    assert!(
        !contents[brace + 1..].starts_with("\n\n"),
        "fix must not insert a blank line after '{{'"
    );
}

/// `fix` rewrites a Rust file in-place.
/// Three fields are used so the reorder has a genuine byte savings (2-field
/// structs of different alignments often have the same total size either way).
#[test]
fn fix_rewrites_rust_struct_in_place() {
    let mut f = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
    // a:u8, b:u64, c:u8 → optimal b,a,c saves 8 bytes (24→16)
    write!(f, "struct S {{\n    a: u8,\n    b: u64,\n    c: u8,\n}}\n").unwrap();
    let path = f.path().to_str().unwrap().to_string();

    padlock().args(["fix", &path]).assert().success();

    let contents = std::fs::read_to_string(&path).unwrap();
    let b_pos = contents.find("b: u64").unwrap();
    let a_pos = contents.find("a: u8").unwrap();
    assert!(b_pos < a_pos, "u64 field must appear before u8 after fix");
    let brace = contents.find('{').unwrap();
    assert!(
        !contents[brace + 1..].starts_with("\n\n"),
        "fix must not insert a blank line after '{{'"
    );
}

/// `fix` rewrites a Zig file in-place.
/// Three fields ensure a genuine byte savings so `ReorderSuggestion` is generated.
#[test]
fn fix_rewrites_zig_struct_in_place() {
    let mut f = tempfile::Builder::new().suffix(".zig").tempfile().unwrap();
    // a:u8, b:f64, c:u8 → optimal b,a,c saves 8 bytes (24→16)
    write!(
        f,
        "const S = struct {{\n    a: u8,\n    b: f64,\n    c: u8,\n}};\n"
    )
    .unwrap();
    let path = f.path().to_str().unwrap().to_string();

    padlock().args(["fix", &path]).assert().success();

    let contents = std::fs::read_to_string(&path).unwrap();
    let b_pos = contents.find("b: f64").unwrap();
    let a_pos = contents.find("a: u8").unwrap();
    assert!(b_pos < a_pos, "f64 field must appear before u8 after fix");
    let brace = contents.find('{').unwrap();
    assert!(
        !contents[brace + 1..].starts_with("\n\n"),
        "fix must not insert a blank line after '{{'"
    );
}

/// `fix --filter` only rewrites structs matching the pattern.
/// Three-field structs ensure there is a genuine byte savings and a ReorderSuggestion.
#[test]
fn fix_filter_only_rewrites_matching_struct() {
    let mut f = tempfile::Builder::new().suffix(".c").tempfile().unwrap();
    // Both structs need fixing, but we only fix 'Target'.
    write!(
        f,
        "struct Target {{\n    char a;\n    double b;\n    char c;\n}};\nstruct Other {{\n    char x;\n    double y;\n    char z;\n}};\n"
    )
    .unwrap();
    let path = f.path().to_str().unwrap().to_string();

    padlock()
        .args(["fix", "--filter", "^Target$", &path])
        .assert()
        .success();

    let contents = std::fs::read_to_string(&path).unwrap();
    // Target should be fixed (double before char)
    let target_section = &contents[..contents.find("struct Other").unwrap()];
    assert!(
        target_section.find("double b").unwrap() < target_section.find("char a").unwrap(),
        "Target must be fixed"
    );
    // Other should be untouched (char still before double)
    let other_section = &contents[contents.find("struct Other").unwrap()..];
    assert!(
        other_section.find("char x").unwrap() < other_section.find("double y").unwrap(),
        "Other must be untouched"
    );
}
