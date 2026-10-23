// padlock-cli/src/commands/explain.rs
//
// `padlock explain [--filter PATTERN] <paths>…`
//
// Prints a visual field-by-field memory layout table for each struct found.
// One struct per panel; padding gaps shown inline.

use std::path::PathBuf;

use crate::paths::collect_layouts;

pub fn run(paths: &[PathBuf], filter: Option<&str>, no_cache: bool) -> anyhow::Result<()> {
    let re = filter
        .map(regex::Regex::new)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid --filter pattern: {e}"))?;

    if let Some(first) = paths.first() {
        let cfg = padlock_core::config::Config::for_path(first);
        padlock_source::set_preprocessor_defines(&cfg.defines);
        padlock_source::set_declared_order(cfg.declared_order);
    }
    let (layouts, _, _skipped) = collect_layouts(paths, false, no_cache)?;

    let filtered: Vec<_> = layouts
        .iter()
        .filter(|l| re.as_ref().is_none_or(|r| r.is_match(&l.name)))
        .collect();

    if filtered.is_empty() {
        eprintln!("padlock: no structs found");
        return Ok(());
    }

    for layout in filtered {
        println!("{}", padlock_output::render_explain(layout));
    }

    Ok(())
}
