// padlock-cli/src/commands/analyze.rs

use std::path::Path;

use padlock_core::findings::Report;

pub fn run(path: &Path, json: bool, sarif: bool) -> anyhow::Result<()> {
    // Decide whether this is a source file or a binary.
    let layouts = if is_source_file(path) {
        let arch = padlock_dwarf::reader::detect_arch_from_host();
        padlock_source::parse_source(path, arch)?
    } else {
        let data = std::fs::read(path)?;
        let dwarf = padlock_dwarf::reader::load(&data)?;
        let arch  = padlock_dwarf::reader::detect_arch(&data)?;
        padlock_dwarf::extractor::Extractor::new(&dwarf, arch).extract_all()?
    };

    let report = Report::from_layouts(&layouts);

    if sarif {
        println!("{}", padlock_output::to_sarif(&report)?);
    } else if json {
        println!("{}", padlock_output::to_json(&report)?);
    } else {
        print!("{}", padlock_output::render_report(&report));
    }

    Ok(())
}

fn is_source_file(path: &Path) -> bool {
    padlock_source::detect_language(path).is_some()
}
