// cargo-padlock — cargo subcommand that builds the current Cargo project and
// analyses the resulting binary for struct layout issues.
//
// Cargo invokes this binary when the user runs `cargo padlock [args]`.
// Cargo passes "padlock" as argv[1], so we skip it.
//
// Usage (after `cargo install padlock-cli` or PATH contains ./target/debug/):
//
//   cargo padlock                      # analyze default binary
//   cargo padlock --package mylib      # specific package
//   cargo padlock --bin mybin          # specific binary
//   cargo padlock --json               # JSON output
//   cargo padlock --sarif              # SARIF output (CI)
//   cargo padlock --release            # build with --release profile

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, bail};
use clap::Parser;

// Re-use config from the library part of the crate.
#[path = "../config.rs"]
mod config;
use config::Config;

#[derive(Parser)]
#[command(
    name = "cargo-padlock",
    bin_name = "cargo padlock",
    about = "Analyse struct memory layout of a Cargo project binary"
)]
struct Args {
    /// The subcommand token inserted by cargo (always "padlock"); ignored.
    #[arg(hide = true, default_value = "padlock")]
    _cargo_subcommand: String,

    /// Package to build (--package / -p forwarded to cargo build)
    #[arg(long, short = 'p')]
    package: Option<String>,

    /// Binary target to build and analyse
    #[arg(long)]
    bin: Option<String>,

    /// Build with --release (uses release binary; debug info must still be present)
    #[arg(long)]
    release: bool,

    /// Output results as JSON
    #[arg(long)]
    json: bool,

    /// Output results as SARIF (for CI / GitHub code-scanning)
    #[arg(long)]
    sarif: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // ── 1. Read Cargo.toml to discover default binary name ────────────────────
    let cargo_toml_path = PathBuf::from("Cargo.toml");
    if !cargo_toml_path.exists() {
        bail!(
            "Cargo.toml not found in the current directory.\n\
             Run `cargo padlock` from the root of a Cargo project."
        );
    }
    let toml_src =
        std::fs::read_to_string(&cargo_toml_path).context("failed to read Cargo.toml")?;
    let manifest: toml::Value = toml::from_str(&toml_src).context("failed to parse Cargo.toml")?;

    let package_name = manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_owned);

    // ── 2. Determine the binary name to analyse ───────────────────────────────
    let bin_name: String = if let Some(b) = &args.bin {
        b.clone()
    } else if let Some(name) = package_name {
        // Default binary has the same name as the package
        name
    } else {
        bail!("Could not determine binary name from Cargo.toml. Use --bin <name>.");
    };

    // ── 3. cargo build ────────────────────────────────────────────────────────
    eprintln!("padlock: building `{bin_name}`…");
    let mut build_cmd = Command::new("cargo");
    build_cmd.arg("build").arg("--bin").arg(&bin_name);
    if let Some(pkg) = &args.package {
        build_cmd.args(["--package", pkg]);
    }
    if args.release {
        build_cmd.arg("--release");
    }

    let status = build_cmd
        .status()
        .context("failed to invoke `cargo build`")?;
    if !status.success() {
        bail!("`cargo build` failed — fix build errors before running padlock.");
    }

    // ── 4. Locate the built binary ────────────────────────────────────────────
    let profile = if args.release { "release" } else { "debug" };
    let binary_path = PathBuf::from("target").join(profile).join(&bin_name);
    if !binary_path.exists() {
        bail!(
            "Expected binary at `{}` but it was not found after build.",
            binary_path.display()
        );
    }

    // ── 5. Run padlock analysis via library calls ─────────────────────────────
    eprintln!("padlock: analysing `{}`…", binary_path.display());

    let data = std::fs::read(&binary_path)
        .with_context(|| format!("failed to read `{}`", binary_path.display()))?;
    let dwarf = padlock_dwarf::reader::load(&data)
        .with_context(|| format!("failed to load DWARF from `{}`", binary_path.display()))?;
    let arch =
        padlock_dwarf::reader::detect_arch(&data).unwrap_or(&padlock_core::arch::X86_64_SYSV);
    let layouts = padlock_dwarf::extractor::Extractor::new(&dwarf, arch)
        .extract_all()
        .context("DWARF extraction failed")?;

    if layouts.is_empty() {
        eprintln!("padlock: no struct layouts found — is the binary built with debug info?");
        eprintln!(
            "         Tip: add `[profile.release] debug = true` to Cargo.toml when using --release."
        );
        return Ok(());
    }

    // Load config from current directory (project root where cargo padlock is run).
    let cfg = Config::load_from(&std::env::current_dir().unwrap_or_default());

    // Filter ignored structs and excluded paths
    let layouts: Vec<_> = layouts
        .into_iter()
        .filter(|l| {
            !cfg.is_ignored(&l.name)
                && !l
                    .source_file
                    .as_deref()
                    .map(|f| cfg.is_path_excluded(f))
                    .unwrap_or(false)
        })
        .collect();

    let report = padlock_core::findings::Report::from_layouts(&layouts);

    // Apply severity filter
    let mut report = report;
    for sr in &mut report.structs {
        sr.findings.retain(|f| cfg.should_report(f.severity()));
    }

    if args.sarif {
        let sarif = padlock_output::to_sarif(&report).context("SARIF serialisation failed")?;
        println!("{sarif}");
    } else if args.json {
        let json = padlock_output::to_json(&report).context("JSON serialisation failed")?;
        println!("{json}");
    } else {
        print!("{}", padlock_output::render_report(&report));
    }

    // Exit non-zero when fail_below threshold is breached or any high-severity finding exists.
    let failed_score = cfg.fail_below > 0
        && report
            .structs
            .iter()
            .any(|s| s.score < cfg.fail_below as f64);
    let has_high = report.structs.iter().any(|sr| {
        sr.findings
            .iter()
            .any(|f| *f.severity() == padlock_core::findings::Severity::High)
    });
    if failed_score || has_high {
        std::process::exit(1);
    }

    Ok(())
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// Cargo.toml package name extraction mirrors what the binary does.
    #[test]
    fn parse_package_name_from_toml() {
        let src = r#"
[package]
name = "my-crate"
version = "0.1.0"
"#;
        let manifest: toml::Value = toml::from_str(src).unwrap();
        let name = manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .map(str::to_owned);
        assert_eq!(name.as_deref(), Some("my-crate"));
    }

    #[test]
    fn binary_path_construction_debug() {
        let bin = "mypkg";
        let profile = "debug";
        let path = PathBuf::from("target").join(profile).join(bin);
        assert_eq!(path, PathBuf::from("target/debug/mypkg"));
    }

    #[test]
    fn binary_path_construction_release() {
        let bin = "mypkg";
        let profile = "release";
        let path = PathBuf::from("target").join(profile).join(bin);
        assert_eq!(path, PathBuf::from("target/release/mypkg"));
    }

    #[test]
    fn missing_cargo_toml_is_detectable() {
        let path = PathBuf::from("/nonexistent/path/Cargo.toml");
        assert!(!path.exists());
    }
}
