// padlock-cli/src/commands/list.rs

use std::path::Path;

use comfy_table::{Cell, Table};

pub fn run(path: &Path) -> anyhow::Result<()> {
    let layouts = if padlock_source::detect_language(path).is_some() {
        let arch = padlock_dwarf::reader::detect_arch_from_host();
        padlock_source::parse_source(path, arch)?
    } else {
        let data = std::fs::read(path)?;
        let dwarf = padlock_dwarf::reader::load(&data)?;
        let arch  = padlock_dwarf::reader::detect_arch(&data)?;
        padlock_dwarf::extractor::Extractor::new(&dwarf, arch).extract_all()?
    };

    if layouts.is_empty() {
        println!("No structs found in {}", path.display());
        return Ok(());
    }

    let mut table = Table::new();
    table.set_header(vec!["Struct", "Size (B)", "Fields", "Wasted (B)", "Score", "Location"]);

    for layout in &layouts {
        let gaps = padlock_core::ir::find_padding(layout);
        let wasted: usize = gaps.iter().map(|g| g.bytes).sum();
        let score = padlock_core::analysis::scorer::score(layout);

        let location = match (&layout.source_file, layout.source_line) {
            (Some(f), Some(l)) => format!("{}:{}", f, l),
            (Some(f), None)    => f.clone(),
            _                  => "-".to_string(),
        };

        table.add_row(vec![
            Cell::new(&layout.name),
            Cell::new(layout.total_size),
            Cell::new(layout.fields.len()),
            Cell::new(wasted),
            Cell::new(format!("{score:.0}")),
            Cell::new(location),
        ]);
    }

    println!("{table}");
    Ok(())
}
