// padlock-cli/src/commands/analyze.rs

use std::path::PathBuf;

use padlock_core::findings::Report;

use crate::config::Config;
use crate::filter::FilterArgs;
use crate::paths::collect_layouts;

pub fn run(paths: &[PathBuf], json: bool, sarif: bool, filter: &FilterArgs) -> anyhow::Result<()> {
    // Load config by searching upward from the first supplied path.
    let cfg = Config::for_path(
        paths
            .first()
            .map(|p| p.as_path())
            .unwrap_or(std::path::Path::new(".")),
    );

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

    // Check fail_below score threshold (respects per-struct overrides).
    let failed = report.structs.iter().any(|s| {
        let threshold = cfg.fail_below_for(&s.struct_name);
        threshold > 0 && s.score < threshold as f64
    });

    if sarif {
        println!("{}", padlock_output::to_sarif(&report)?);
    } else if json {
        println!("{}", padlock_output::to_json(&report)?);
    } else {
        print!("{}", padlock_output::render_report(&report));
    }

    if failed {
        std::process::exit(1);
    }

    Ok(())
}
