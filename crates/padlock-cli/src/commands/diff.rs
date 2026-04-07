// padlock-cli/src/commands/diff.rs

use std::path::Path;

pub fn run(path: &Path) -> anyhow::Result<()> {
    if padlock_source::detect_language(path).is_none() {
        anyhow::bail!(
            "diff only works on source files (.c, .cpp, .rs, .go); got {}",
            path.display()
        );
    }

    let arch = padlock_dwarf::reader::detect_arch_from_host();
    let layouts = padlock_source::parse_source(path, arch)?;

    let mut any_diff = false;
    for layout in &layouts {
        let d = padlock_output::render_diff(layout);
        if d != "(no changes)\n" {
            println!("--- {} (current order)", layout.name);
            println!("+++ {} (optimal order)", layout.name);
            print!("{d}");
            println!();
            any_diff = true;
        }
    }
    if !any_diff {
        println!("All structs are already optimally ordered.");
    }
    Ok(())
}
