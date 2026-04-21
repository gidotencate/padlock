// padlock-cli/src/commands/explain.rs
//
// `padlock explain [--filter PATTERN] <paths>…`
//
// Prints a visual field-by-field memory layout table for each struct found.
// One struct per panel; padding gaps shown inline.

use std::path::PathBuf;

use crate::paths::collect_layouts;

pub fn run(paths: &[PathBuf], filter: Option<&str>) -> anyhow::Result<()> {
    let re = filter
        .map(regex::Regex::new)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid --filter pattern: {e}"))?;

    let (layouts, _, _skipped) = collect_layouts(paths)?;

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
