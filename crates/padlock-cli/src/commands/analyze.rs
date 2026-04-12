// padlock-cli/src/commands/analyze.rs

use std::path::PathBuf;

use padlock_core::findings::Report;

use crate::config::Config;
use crate::filter::{FailSeverity, FilterArgs};
use crate::paths::collect_layouts;

/// Output and arch options for the `analyze` subcommand.
pub struct AnalyzeOpts {
    pub json: bool,
    pub sarif: bool,
    pub markdown: bool,
    pub cache_line_size: Option<usize>,
    pub word_size: Option<usize>,
    pub fail_on_severity: Option<FailSeverity>,
}

pub fn run(paths: &[PathBuf], opts: AnalyzeOpts, filter: &FilterArgs) -> anyhow::Result<()> {
    let AnalyzeOpts {
        json,
        sarif,
        markdown,
        cache_line_size,
        word_size,
        fail_on_severity,
    } = opts;
    // Load config by searching upward from the first supplied path.
    let cfg = Config::for_path(
        paths
            .first()
            .map(|p| p.as_path())
            .unwrap_or(std::path::Path::new(".")),
    );

    // Merge config filter defaults into a local copy; CLI flags take precedence.
    let mut filter = filter.clone();
    filter.apply_config_defaults(&cfg);

    // Collect layouts from all paths (dirs expanded, binaries via DWARF).
    let (mut layouts, analyzed) = collect_layouts(paths)?;

    // Apply arch override from config.
    if let Some(ref arch_name) = cfg.arch_override {
        let arch = padlock_core::arch::arch_by_name(arch_name).unwrap_or_else(|| {
            eprintln!("padlock: warning: unknown arch '{arch_name}', ignoring override");
            padlock_dwarf::reader::detect_arch_from_host()
        });
        for layout in &mut layouts {
            layout.arch = arch;
        }
    }

    // Apply --cache-line-size / --word-size overrides per layout.
    if cache_line_size.is_some() || word_size.is_some() {
        for layout in &mut layouts {
            layout.arch =
                padlock_core::arch::with_overrides(layout.arch, cache_line_size, word_size);
        }
    }

    // Apply config ignore list, then CLI pre-filters (name, size, holes).
    layouts.retain(|l| !cfg.is_ignored(&l.name));
    filter.apply_to_layouts(&mut layouts)?;

    // Run all analysis passes.
    let mut report = Report::from_layouts(&layouts);
    report.analyzed_paths = analyzed;

    // Apply config severity filter (respects per-struct overrides).
    for sr in &mut report.structs {
        sr.findings
            .retain(|f| cfg.should_report_for(&sr.struct_name, f.severity()));
    }

    // Apply post-analysis CLI filters (packable) and sort.
    filter.apply_to_report(&mut report);

    // Check fail_below score threshold (config-based, per-struct overrides).
    let score_failed = report.structs.iter().any(|s| {
        let threshold = cfg.fail_below_for(&s.struct_name);
        threshold > 0 && s.score < threshold as f64
    });

    // Check --fail-on-severity threshold (CLI flag takes precedence; config fallback).
    let effective_fail_sev: Option<FailSeverity> = fail_on_severity.or_else(|| {
        cfg.fail_on_severity.as_ref().map(|s| match s {
            padlock_core::findings::Severity::High => FailSeverity::High,
            padlock_core::findings::Severity::Medium => FailSeverity::Medium,
            padlock_core::findings::Severity::Low => FailSeverity::Low,
        })
    });
    let severity_failed = if let Some(ref threshold) = effective_fail_sev {
        report
            .structs
            .iter()
            .flat_map(|s| &s.findings)
            .any(|f| threshold.matches(f.severity()))
    } else {
        false
    };

    let failed = score_failed || severity_failed;

    if sarif {
        println!("{}", padlock_output::to_sarif(&report)?);
    } else if json {
        println!("{}", padlock_output::to_json(&report)?);
    } else if markdown {
        print!("{}", padlock_output::to_markdown(&report));
    } else {
        print!("{}", padlock_output::render_report(&report));
    }

    if failed {
        std::process::exit(1);
    }

    Ok(())
}
