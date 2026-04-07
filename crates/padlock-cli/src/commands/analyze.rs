// padlock-cli/src/commands/analyze.rs

use std::path::Path;

use padlock_core::findings::Report;

use crate::config::Config;

pub fn run(path: &Path, json: bool, sarif: bool) -> anyhow::Result<()> {
    let cfg = Config::for_path(path);

    // Decide whether this is a source file or a binary.
    let mut layouts = if is_source_file(path) {
        let arch = padlock_dwarf::reader::detect_arch_from_host();
        padlock_source::parse_source(path, arch)?
    } else {
        let data = std::fs::read(path)?;
        let dwarf = padlock_dwarf::reader::load(&data)?;
        let arch = padlock_dwarf::reader::detect_arch(&data)?;
        padlock_dwarf::extractor::Extractor::new(&dwarf, arch).extract_all()?
    };

    // Apply arch override if set in config
    if let Some(ref arch_name) = cfg.arch_override {
        let arch = padlock_core::arch::arch_by_name(arch_name).unwrap_or_else(|| {
            eprintln!("padlock: warning: unknown arch '{arch_name}', ignoring override");
            padlock_dwarf::reader::detect_arch_from_host()
        });
        for layout in &mut layouts {
            layout.arch = arch;
        }
    }

    // Filter ignored structs
    layouts.retain(|l| !cfg.is_ignored(&l.name));

    let report = Report::from_layouts(&layouts);

    // Apply severity filter
    let report = filter_report(report, &cfg);

    // Check fail_below threshold
    let failed = cfg.fail_below > 0
        && report
            .structs
            .iter()
            .any(|s| s.score < cfg.fail_below as f64);

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

fn filter_report(mut report: Report, cfg: &Config) -> Report {
    for sr in &mut report.structs {
        sr.findings.retain(|f| cfg.should_report(f.severity()));
    }
    report
}

fn is_source_file(path: &Path) -> bool {
    padlock_source::detect_language(path).is_some()
}
