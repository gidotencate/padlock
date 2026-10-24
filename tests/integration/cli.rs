use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
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

// ── analyze: Rust — Arc<T>/Rc<T> false-sharing (regression, 0.24.0) ───────────
//
// Library-level tests (padlock-source's lib.rs/concurrency.rs/rust.rs) cover
// this in-process via parse_source_str. These two exercise the same fix
// through the actual `padlock` binary end-to-end — a different layer than
// any existing test for this fix touches.

#[test]
fn analyze_rust_plain_arc_fields_not_flagged_false_sharing() {
    // --filter scopes to just this struct — the fixture file's other struct
    // (SharedMutexConfig) legitimately does have a FalseSharing finding, so
    // asserting its absence requires excluding that struct from this run.
    padlock()
        .args([
            "analyze",
            &fixture("arc_fields.rs"),
            "--json",
            "--filter",
            "^PlainArcConfig$",
        ])
        .assert()
        .success()
        .stdout(contains("PlainArcConfig").and(contains("FalseSharing").not()));
}

#[test]
fn analyze_rust_arc_mutex_fields_still_flagged_false_sharing() {
    padlock()
        .args([
            "analyze",
            &fixture("arc_fields.rs"),
            "--json",
            "--filter",
            "^SharedMutexConfig$",
        ])
        .assert()
        .success()
        .stdout(contains("SharedMutexConfig").and(contains("FalseSharing")));
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
    write!(
        f,
        "#[repr(C)]\nstruct S {{\n    a: u8,\n    b: u64,\n    c: u8,\n}}\n"
    )
    .unwrap();
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
        "const S = extern struct {{\n    a: u8,\n    b: f64,\n    c: u8,\n}};\n"
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

// ── diff --against <git-ref> ───────────────────────────────────────────────────

/// Set up a throwaway git repo with two commits: `s.rs` growing from
/// `struct Foo { a: u8, b: u64 }` to also having a `c: u8` field and a new
/// `Bar` struct. Returns the repo dir and the HEAD~1 ref string.
fn diff_against_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test"]);

    std::fs::write(
        dir.path().join("s.rs"),
        "#[repr(C)] struct Foo { a: u8, b: u64 }\n",
    )
    .unwrap();
    git(&["add", "s.rs"]);
    git(&["commit", "-q", "-m", "initial"]);

    std::fs::write(
        dir.path().join("s.rs"),
        "#[repr(C)] struct Foo { a: u8, b: u64, c: u8 }\nstruct Bar { x: u32 }\n",
    )
    .unwrap();
    git(&["add", "s.rs"]);
    git(&["commit", "-q", "-m", "add field c and Bar"]);

    dir
}

#[test]
fn diff_against_detects_size_change() {
    let dir = diff_against_fixture();
    padlock()
        .args(["diff", "s.rs", "--against", "HEAD~1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(contains("Foo"))
        .stdout(contains("16B -> 24B"));
}

#[test]
fn diff_against_detects_new_struct() {
    let dir = diff_against_fixture();
    padlock()
        .args(["diff", "s.rs", "--against", "HEAD~1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(contains("+ Bar"))
        .stdout(contains("new"));
}

#[test]
fn diff_against_detects_removed_struct() {
    let dir = diff_against_fixture();
    // Working tree (HEAD) has Bar; simulate removing it before the next commit.
    std::fs::write(
        dir.path().join("s.rs"),
        "struct Foo { a: u8, b: u64, c: u8 }\n",
    )
    .unwrap();
    padlock()
        .args(["diff", "s.rs", "--against", "HEAD"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(contains("- Bar"))
        .stdout(contains("removed"));
}

#[test]
fn diff_against_same_ref_reports_no_changes() {
    let dir = diff_against_fixture();
    padlock()
        .args(["diff", "s.rs", "--against", "HEAD"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(contains("No layout changes"));
}

#[test]
fn diff_against_filter_narrows_to_matching_struct() {
    let dir = diff_against_fixture();
    padlock()
        .args(["diff", "s.rs", "--against", "HEAD~1", "--filter", "^Bar$"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(contains("Bar"))
        .stdout(contains("Foo").not());
}

#[test]
fn diff_against_resolves_repo_root_from_target_path_not_process_cwd() {
    // Regression test: git_repo_root() must anchor on the analyzed file's
    // own directory, not wherever the padlock process happens to be
    // invoked from. Using an absolute path to the target file while cwd is
    // set to a *different*, unrelated directory reproduces the real-world
    // case (e.g. an editor running padlock with its own cwd).
    let dir = diff_against_fixture();
    let outside_dir = tempfile::tempdir().unwrap();
    let abs_file = dir.path().join("s.rs").canonicalize().unwrap();

    padlock()
        .args(["diff", abs_file.to_str().unwrap(), "--against", "HEAD~1"])
        .current_dir(outside_dir.path())
        .assert()
        .success()
        .stdout(contains("16B -> 24B"))
        .stdout(contains("+ Foo").not());
}

// ── settings reach the parallel parser ────────────────────────────────────────

fn total_sizes(stdout: &[u8]) -> Vec<u64> {
    let v: serde_json::Value = serde_json::from_slice(stdout).unwrap();
    let mut sizes: Vec<u64> = v["structs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["total_size"].as_u64().unwrap())
        .collect();
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

#[test]
fn stdlib_flag_applies_to_every_file_of_a_directory_scan() {
    // Parsing runs on rayon worker threads; a thread-local setting made on the
    // main thread was silently ignored for them.
    let dir = tempfile::TempDir::new().unwrap();
    for i in 0..8 {
        std::fs::write(
            dir.path().join(format!("f{i}.cpp")),
            format!("struct S{i} {{ std::string s; char c; }};"),
        )
        .unwrap();
    }
    let out = padlock()
        .args(["analyze", "--json", "--stdlib", "libc++"])
        .arg(dir.path())
        .output()
        .unwrap();
    // libc++ std::string is 24 bytes → 24 + 1 → 32.
    assert_eq!(total_sizes(&out.stdout), vec![32]);
    // Switching the setting must not be answered from the parse cache.
    let out = padlock()
        .args(["analyze", "--json", "--stdlib", "libstdc++"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(total_sizes(&out.stdout), vec![40]);
}

#[test]
fn define_flag_selects_ifdef_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("d.c");
    std::fs::write(
        &file,
        "struct S {\n char a;\n#ifdef WIDE\n long w;\n#endif\n};\n",
    )
    .unwrap();
    let out = padlock()
        .args(["analyze", "--json"])
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(total_sizes(&out.stdout), vec![1]);
    let out = padlock()
        .args(["analyze", "--json", "-D", "WIDE"])
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(total_sizes(&out.stdout), vec![16]);
}

#[test]
fn repr_rust_struct_is_shown_in_compiler_order_unless_declared_order() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("s.rs");
    std::fs::write(&file, "struct S { a: u8, b: u64, c: u8 }\n").unwrap();
    let out = padlock()
        .args(["analyze", "--json"])
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(total_sizes(&out.stdout), vec![16]);
    let out = padlock()
        .args(["analyze", "--json", "--declared-order"])
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(total_sizes(&out.stdout), vec![24]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("ReorderSuggestion"));
}

#[test]
fn template_instantiated_by_another_file_is_analyzed() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("buf.hpp"),
        "template<typename T, int N> struct Buf { char head; T data[N]; };\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("user.cpp"),
        "struct User { Buf<long, 2> b; Buf<char, 3> c; };\n",
    )
    .unwrap();
    let out = padlock()
        .args(["analyze", "--json", "--no-cache"])
        .arg(dir.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let size_of = |name: &str| {
        v["structs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["struct_name"] == name)
            .map(|s| s["total_size"].as_u64().unwrap())
    };
    // g++: sizeof(Buf<long, 2>) == 24, sizeof(Buf<char, 3>) == 4
    assert_eq!(size_of("Buf<long, 2>"), Some(24));
    assert_eq!(size_of("Buf<char, 3>"), Some(4));
}

#[test]
fn template_instantiation_from_config() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".padlock.toml"),
        "[instantiate]\nPair = [\"char, double\"]\n",
    )
    .unwrap();
    let file = dir.path().join("p.hpp");
    std::fs::write(
        &file,
        "template<class A, class B> struct Pair { A a; B b; };\n",
    )
    .unwrap();
    let out = padlock()
        .args(["analyze", "--json", "--no-cache"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("Pair<char, double>"));
    assert_eq!(total_sizes(&out.stdout), vec![16]);
}

#[test]
fn rust_go_zig_generics_are_instantiated_from_uses() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("a.rs"),
        "#[repr(C)] struct Pair<A, B> { a: A, b: B }\n#[repr(C)] struct U { p: Pair<u8, u64> }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.go"),
        "package p\ntype Box[T any] struct { ok bool; v T }\ntype U struct { b Box[int64] }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("c.zig"),
        "fn List(comptime T: type) type {\n    return struct { len: usize, first: T };\n}\nconst L = List(u8);\n",
    )
    .unwrap();
    let out = padlock()
        .args(["analyze", "--json", "--no-cache"])
        .arg(dir.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for name in ["Pair<u8, u64>", "Box[int64]", "List(u8)"] {
        assert!(text.contains(name), "{name} must be instantiated: {text}");
    }
}

// ── DWARF input variants (need gcc; skipped when it is missing) ──────────────
// Linux only: they expect ELF output and LP64 `long` (MinGW is LLP64 and
// writes `t.exe`; macOS `gcc` is clang and keeps DWARF in the object files).

#[cfg(target_os = "linux")]
fn gcc_build(dir: &Path, flags: &[&str]) -> Option<std::path::PathBuf> {
    let src = dir.join("t.c");
    std::fs::write(
        &src,
        "struct Inner { char c; long l; };\nstruct Outer { char a; struct Inner in; union { char x[13]; long y; } u; };\nstruct Outer g;\nint main(void) { return 0; }\n",
    )
    .unwrap();
    let out = dir.join("t");
    let ok = std::process::Command::new("gcc")
        .args(flags)
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .current_dir(dir)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

fn struct_size(json: &[u8], name: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    v["structs"]
        .as_array()?
        .iter()
        .find(|s| s["struct_name"] == name)
        .and_then(|s| s["total_size"].as_u64())
}

#[test]
#[cfg(target_os = "linux")]
fn dwarf_type_units_split_dwarf_and_compression_are_read() {
    for flags in [
        &["-g"][..],
        &["-gdwarf-4", "-fdebug-types-section"],
        &["-gdwarf-5", "-fdebug-types-section"],
        &["-g", "-gsplit-dwarf"],
        &["-g", "-gz"],
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let Some(bin) = gcc_build(dir.path(), flags) else {
            eprintln!("gcc not available; skipping");
            return;
        };
        let out = padlock()
            .args(["analyze", "--json", "--no-cache"])
            .arg(&bin)
            .output()
            .unwrap();
        // gcc: sizeof(struct Outer) == 40 (a, Inner at 8, union at 24 aligned to 8).
        assert_eq!(
            struct_size(&out.stdout, "Outer"),
            Some(40),
            "flags {flags:?}"
        );
        assert_eq!(
            struct_size(&out.stdout, "Inner"),
            Some(16),
            "flags {flags:?}"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn verify_agrees_with_gcc_and_reports_mismatches() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("v.c");
    std::fs::write(
        &src,
        "struct M { char a; short m[2][3]; long l; };\nstruct M g;\nint main(void) { return 0; }\n",
    )
    .unwrap();
    let bin = dir.path().join("v");
    let built = std::process::Command::new("gcc")
        .args(["-g", "-O0"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !built {
        eprintln!("gcc not available; skipping");
        return;
    }
    // Source and binary agree (including the 2-D array's 12-byte size).
    padlock()
        .args(["verify", "--binary"])
        .arg(&bin)
        .arg(&src)
        .assert()
        .success()
        .stdout(contains("1/1 structs match"));
    // A source that no longer matches the binary is reported and fails.
    std::fs::write(&src, "struct M { char a; long l; short m[2][3]; };\n").unwrap();
    padlock()
        .args(["verify", "--binary"])
        .arg(&bin)
        .arg(&src)
        .assert()
        .failure()
        .stdout(contains("size-mismatch"));
}

#[test]
fn target_flag_lays_sources_out_for_that_target() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("t.c");
    std::fs::write(&file, "struct T { char a; void *p; long l; double d; };\n").unwrap();
    let size_for = |target: Option<&str>| {
        let mut cmd = padlock();
        cmd.args(["analyze", "--json", "--no-cache"]);
        if let Some(t) = target {
            cmd.args(["--target", t]);
        }
        let out = cmd.arg(&file).output().unwrap();
        total_sizes(&out.stdout)
    };
    // x86-64: 1+7, 8, 8, 8 = 32. i386: 1+3, 4, 4, 8 (4-aligned) = 20.
    // Cortex-M: 1+3, 4, 4, 8 (8-aligned) = 24.
    assert_eq!(size_for(None), vec![32]);
    assert_eq!(size_for(Some("x86")), vec![20]);
    assert_eq!(size_for(Some("cortex_m")), vec![24]);
    // `explain` on a target without a cache must not panic.
    padlock()
        .args(["explain", "--no-cache", "--target", "avr"])
        .arg(&file)
        .assert()
        .success();
}

#[test]
#[cfg(target_os = "linux")]
fn compile_commands_gives_compiler_exact_layouts() {
    if std::process::Command::new("g++")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("g++ not available; skipping");
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    // A macro-generated field and a -D-selected branch: invisible to source parsing.
    std::fs::write(
        src.join("conn.h"),
        "#define FIELD(t, n) t n;\ntemplate<class T> struct Box { char tag; T value; };\nstruct Conn {\n  char state;\n  FIELD(long, id)\n#ifdef WIDE\n  long extra[4];\n#endif\n  Box<double> b;\n};\n",
    )
    .unwrap();
    std::fs::write(src.join("conn.cpp"), "#include \"conn.h\"\nConn g;\n").unwrap();
    let db = dir.path().join("compile_commands.json");
    std::fs::write(
        &db,
        serde_json::json!([{
            "directory": src,
            "file": "conn.cpp",
            "command": "g++ -std=c++17 -DWIDE -O2 -c conn.cpp -o conn.o"
        }])
        .to_string(),
    )
    .unwrap();
    let out = padlock()
        .args(["analyze", "--json", "--no-cache", "--compile-commands"])
        .arg(&db)
        .arg(&src)
        .output()
        .unwrap();
    // g++ -DWIDE: char(1)+7, long 8, long[4] 32, Box<double> 16 = 64.
    assert_eq!(
        struct_size(&out.stdout, "Conn"),
        Some(64),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(struct_size(&out.stdout, "Box<double>"), Some(16));
}

#[test]
fn go_exact_uses_the_go_toolchain_layout() {
    if std::process::Command::new("go")
        .arg("version")
        .output()
        .is_err()
    {
        eprintln!("go not available; skipping");
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/p\n\ngo 1.21\n",
    )
    .unwrap();
    // `url.URL` is a struct from another package: padlock cannot size it from
    // source and reports it as uncertain; the toolchain knows it exactly.
    std::fs::write(
        dir.path().join("p.go"),
        "package p\n\nimport \"net/url\"\n\ntype S struct {\n\tok bool\n\tu  url.URL\n\tn  int32\n}\n",
    )
    .unwrap();
    let before_files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    let out = padlock()
        .args(["analyze", "--json", "--no-cache", "--go-exact"])
        .arg(dir.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let s = v["structs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["struct_name"] == "S")
        .unwrap();
    // go 1.21+: unsafe.Sizeof(url.URL{}) == 144 on amd64 → S = 8 + 144 + 4 → 160.
    let size = s["total_size"].as_u64().unwrap();
    assert!(
        size > 100,
        "url.URL must be sized by the toolchain, got {size}"
    );
    assert!(s.get("uncertain_fields").is_none());
    // The probe is virtual: nothing was added to the package.
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        before_files.len()
    );
}

#[test]
fn rust_exact_uses_rustc_field_order() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"lib.rs\"\n",
    )
    .unwrap();
    // Option<Vec<u8>> (niche), a tuple and a char: rustc decides.
    std::fs::write(
        dir.path().join("lib.rs"),
        "pub struct S { pub a: u8, pub v: Option<Vec<u8>>, pub t: (u8, u32), pub c: char }\n\
         pub fn make() -> S { S { a: 1, v: None, t: (2, 3), c: 'x' } }\n",
    )
    .unwrap();
    let out = padlock()
        .args(["analyze", "--json", "--no-cache", "--rust-exact"])
        .arg(dir.path().join("lib.rs"))
        .output()
        .unwrap();
    // rustc: Option<Vec<u8>> 24 + (u8, u32) 8 + char 4 + u8 1 → 40 (align 8).
    assert_eq!(
        struct_size(&out.stdout, "S"),
        Some(40),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // rustc really ran (into its own target directory).
    assert!(
        dir.path().join("target/padlock-rust-exact").is_dir(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn ffi_flags_rust_repr_c_that_disagrees_with_the_c_header() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("hdr.h"),
        "struct Packet { unsigned char kind; unsigned int len; long ts; };\n\
         struct Ok { int a; int b; };\n",
    )
    .unwrap();
    // Rust uses u64 where C has a 4-byte `unsigned int`.
    std::fs::write(
        dir.path().join("ffi.rs"),
        "#[repr(C)] pub struct Packet { pub kind: u8, pub len: u64, pub ts: i64 }\n\
         #[repr(C)] pub struct Ok { pub a: i32, pub b: i32 }\n",
    )
    .unwrap();
    padlock()
        .args(["ffi"])
        .arg(dir.path())
        .assert()
        .failure()
        .stdout(contains("Packet"))
        .stdout(contains("1/2 repr(C) structs match"));
}

#[test]
fn perf_data_types_mark_measured_hot_fields() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("c.c");
    std::fs::write(
        &file,
        "struct conn { long hits; char name[120]; long misses; };\n",
    )
    .unwrap();
    let perf = dir.path().join("types.txt");
    std::fs::write(
        &perf,
        "# Overhead  Data Type  Data Type Offset\n\
         \x20   40.00%  struct conn  struct conn +0 (hits)\n\
         \x20   35.00%  struct conn  struct conn +128 (misses)\n",
    )
    .unwrap();
    let plain = padlock()
        .args(["analyze", "--json", "--no-cache"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&plain.stdout).contains("LocalityIssue"));
    let measured = padlock()
        .args(["analyze", "--json", "--no-cache", "--perf-types"])
        .arg(&perf)
        .arg(&file)
        .output()
        .unwrap();
    // hits and misses are hot, the name between them is not: interleaving.
    assert!(
        String::from_utf8_lossy(&measured.stdout).contains("LocalityIssue"),
        "{}",
        String::from_utf8_lossy(&measured.stdout)
    );
}
