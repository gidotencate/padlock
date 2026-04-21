// padlock-cli/src/commands/summary.rs
//
// Project health summary command — shows aggregate score, severity distribution,
// worst files, and worst structs across an entire codebase.

use std::path::PathBuf;

use padlock_core::findings::Report;

use crate::config::Config;
use crate::filter::FilterArgs;
use crate::paths::collect_layouts;

pub fn run(
    paths: &[PathBuf],
    top: usize,
    cache_line_size: Option<usize>,
    word_size: Option<usize>,
    target: Option<String>,
    filter: &FilterArgs,
) -> anyhow::Result<()> {
    // Load config by searching upward from the first supplied path.
    let cfg = Config::for_path(
        paths
            .first()
            .map(|p| p.as_path())
            .unwrap_or(std::path::Path::new(".")),
    );

    // Collect layouts from all paths (dirs expanded, binaries via DWARF).
    let (mut layouts, analyzed, skipped) = collect_layouts(paths)?;

    // Apply arch override: CLI --target takes precedence over config arch.override.
    let arch_name_override = target.as_deref().or(cfg.arch_override.as_deref());
    if let Some(arch_name) = arch_name_override {
        let arch = padlock_core::arch::arch_by_name(arch_name).unwrap_or_else(|| {
            eprintln!("padlock: warning: unknown target/arch '{arch_name}', ignoring override");
            padlock_dwarf::reader::detect_arch_from_host()
        });
        for layout in &mut layouts {
            layout.arch = arch;
        }
    }

    // Apply --cache-line-size / --word-size overrides.
    if cache_line_size.is_some() || word_size.is_some() {
        for layout in &mut layouts {
            layout.arch =
                padlock_core::arch::with_overrides(layout.arch, cache_line_size, word_size);
        }
    }

    // Merge config filter defaults; CLI flags take precedence.
    let mut filter = filter.clone();
    filter.apply_config_defaults(&cfg);

    // Apply config ignore list and path exclusions, then CLI pre-filters.
    layouts.retain(|l| {
        !cfg.is_ignored(&l.name)
            && !l
                .source_file
                .as_deref()
                .map(|f| cfg.is_path_excluded(f))
                .unwrap_or(false)
    });
    filter.apply_to_layouts(&mut layouts)?;

    // Run all analysis passes.
    let mut report = Report::from_layouts(&layouts);
    report.analyzed_paths = analyzed;
    report.skipped = skipped;

    // Apply config severity filter.
    for sr in &mut report.structs {
        sr.findings
            .retain(|f| cfg.should_report_for(&sr.struct_name, f.severity()));
    }

    // Apply post-analysis CLI filters and sort.
    filter.apply_to_report(&mut report);

    // Render and print.
    let out =
        padlock_output::render_project_summary(&padlock_output::project_summary::SummaryInput {
            report: &report,
            top,
        });
    print!("{out}");

    Ok(())
}
