// padlock-cli/src/commands/list.rs

use std::path::PathBuf;

use comfy_table::{Cell, Table};
use padlock_core::findings::Report;

use crate::config::Config;
use crate::filter::FilterArgs;
use crate::paths::collect_layouts;

pub fn run(paths: &[PathBuf], filter: &FilterArgs) -> anyhow::Result<()> {
    let cfg = Config::for_path(
        paths
            .first()
            .map(|p| p.as_path())
            .unwrap_or(std::path::Path::new(".")),
    );
    let mut filter = filter.clone();
    filter.apply_config_defaults(&cfg);
    let (mut layouts, _, _skipped) = collect_layouts(paths, filter.include_generated)?;
    layouts.retain(|l| {
        !cfg.is_ignored(&l.name)
            && !l
                .source_file
                .as_deref()
                .map(|f| cfg.is_path_excluded(f))
                .unwrap_or(false)
    });
    filter.apply_to_layouts(&mut layouts)?;

    if layouts.is_empty() {
        println!("No structs found.");
        return Ok(());
    }

    // Run analysis to get scores and findings (needed for sort/packable filter).
    let mut report = Report::from_layouts(&layouts);
    filter.apply_to_report(&mut report);

    if report.structs.is_empty() {
        println!("No structs matched the given filters.");
        return Ok(());
    }

    let mut table = Table::new();
    table.set_header(vec![
        "Struct",
        "Size (B)",
        "Fields",
        "Holes",
        "Wasted (B)",
        "Score",
        "Location",
    ]);

    for sr in &report.structs {
        let location = match (&sr.source_file, sr.source_line) {
            (Some(f), Some(l)) => format!("{f}:{l}"),
            (Some(f), None) => f.clone(),
            _ => "-".to_string(),
        };

        table.add_row(vec![
            Cell::new(&sr.struct_name),
            Cell::new(sr.total_size),
            Cell::new(sr.num_fields),
            Cell::new(sr.num_holes),
            Cell::new(sr.wasted_bytes),
            Cell::new(format!("{:.0}", sr.score)),
            Cell::new(location),
        ]);
    }

    println!("{table}");
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::SortBy;
    use std::fs;
    use tempfile::TempDir;

    fn default_filter() -> FilterArgs {
        FilterArgs {
            filter: None,
            exclude: None,
            min_holes: None,
            min_size: None,
            packable: false,
            sort_by: SortBy::Score,
            hide_repr_rust: false,
            include_generated: false,
        }
    }

    #[test]
    fn list_runs_without_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("s.rs");
        fs::write(&file, "struct S { x: i32, y: i64, z: bool }").unwrap();
        assert!(run(&[file], &default_filter()).is_ok());
    }

    #[test]
    fn list_empty_dir_does_not_error() {
        let dir = TempDir::new().unwrap();
        assert!(run(&[dir.path().to_path_buf()], &default_filter()).is_ok());
    }
}
