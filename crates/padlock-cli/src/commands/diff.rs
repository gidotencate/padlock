// padlock-cli/src/commands/diff.rs

use std::path::PathBuf;

use crate::paths::walk_source_files;

pub fn run(paths: &[PathBuf], filter: Option<&str>) -> anyhow::Result<()> {
    // Compile the regex once if a pattern was supplied.
    let re = filter
        .map(|p| {
            regex::Regex::new(p).map_err(|_| anyhow::anyhow!("invalid --filter pattern: {p:?}"))
        })
        .transpose()?;

    let source_files = expand_to_source_files(paths)?;
    let arch = padlock_dwarf::reader::detect_arch_from_host();
    let mut any_diff = false;

    for file in &source_files {
        let layouts = match padlock_source::parse_source(file, arch) {
            Ok(output) => output.layouts,
            Err(e) => {
                eprintln!("padlock: warning: {}: {e}", file.display());
                continue;
            }
        };

        for layout in &layouts {
            if let Some(ref re) = re
                && !re.is_match(&layout.name)
            {
                continue;
            }
            let d = padlock_output::render_diff(layout);
            if d != "(no changes)\n" {
                println!("--- {} (current order)", layout.name);
                println!("+++ {} (optimal order)", layout.name);
                print!("{d}");
                println!();
                any_diff = true;
            }
        }
    }

    if !any_diff {
        println!("All structs are already optimally ordered.");
    }
    Ok(())
}

fn expand_to_source_files(paths: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            files.extend(walk_source_files(path));
        } else if padlock_source::detect_language(path).is_some() {
            files.push(path.clone());
        } else {
            anyhow::bail!(
                "diff only works on source files (.c, .cpp, .rs, .go, .zig) or directories; got {}",
                path.display()
            );
        }
    }
    Ok(files)
}
