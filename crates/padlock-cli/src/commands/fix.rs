// padlock-cli/src/commands/fix.rs
//
// Applies optimal field reordering to source files in-place.
// A `.bak` copy of the original is written before any changes are made.

use std::path::{Path, PathBuf};

use padlock_core::findings::{Finding, Report};
use padlock_source::{SourceLanguage, fixgen};

use crate::paths::walk_source_files;

pub fn run(paths: &[PathBuf], dry_run: bool, filter: Option<&str>) -> anyhow::Result<()> {
    // Compile regex once if a filter was given.
    let re = filter
        .map(|p| {
            regex::Regex::new(p).map_err(|_| anyhow::anyhow!("invalid --filter pattern: {p:?}"))
        })
        .transpose()?;

    let source_files = expand_to_source_files(paths)?;

    for file in &source_files {
        if let Err(e) = fix_file(file, dry_run, re.as_ref()) {
            eprintln!("padlock: warning: {}: {e}", file.display());
        }
    }

    Ok(())
}

fn fix_file(path: &Path, dry_run: bool, re: Option<&regex::Regex>) -> anyhow::Result<()> {
    let lang = padlock_source::detect_language(path).ok_or_else(|| {
        anyhow::anyhow!(
            "fix only works on source files (.c, .cpp, .rs, .go); got {}",
            path.display()
        )
    })?;

    let arch = padlock_dwarf::reader::detect_arch_from_host();
    let source = std::fs::read_to_string(path)?;
    let layouts = padlock_source::parse_source(path, arch)?;
    let report = Report::from_layouts(&layouts);

    // Collect layouts that have a ReorderSuggestion and (optionally) match the filter.
    let mut layouts_to_fix: Vec<&padlock_core::ir::StructLayout> = Vec::new();
    for sr in &report.structs {
        if let Some(re) = re
            && !re.is_match(&sr.struct_name)
        {
            continue;
        }
        let has_reorder = sr
            .findings
            .iter()
            .any(|f| matches!(f, Finding::ReorderSuggestion { .. }));
        if !has_reorder {
            continue;
        }
        if let Some(layout) = layouts.iter().find(|l| l.name == sr.struct_name) {
            layouts_to_fix.push(layout);
        }
    }

    if layouts_to_fix.is_empty() {
        println!(
            "{}: nothing to fix — all structs are already optimally ordered.",
            path.display()
        );
        return Ok(());
    }

    // Show per-struct diff.
    for layout in &layouts_to_fix {
        let old_text =
            match lang {
                SourceLanguage::C | SourceLanguage::Cpp => {
                    fixgen::find_c_struct_span(&source, &layout.name).map(|r| source[r].to_string())
                }
                SourceLanguage::Rust => fixgen::find_rust_struct_span(&source, &layout.name)
                    .map(|r| source[r].to_string()),
                SourceLanguage::Go => fixgen::find_go_struct_span(&source, &layout.name)
                    .map(|r| source[r].to_string()),
            };

        let new_text = match lang {
            SourceLanguage::C | SourceLanguage::Cpp => fixgen::generate_c_fix(layout),
            SourceLanguage::Rust => fixgen::generate_rust_fix(layout),
            SourceLanguage::Go => fixgen::generate_go_fix(layout),
        };

        let diff_base = old_text.as_deref().unwrap_or(&source);
        let diff = fixgen::unified_diff(diff_base, &new_text, 3);
        println!("=== {} ===", layout.name);
        println!("{diff}");
    }

    if dry_run {
        println!("(dry-run: no files written)");
        return Ok(());
    }

    // Write .bak backup, then apply in-place rewrite.
    let bak = path.with_extension(format!(
        "{}.bak",
        path.extension().unwrap_or_default().to_string_lossy()
    ));
    std::fs::copy(path, &bak)?;

    let fixed_source = match lang {
        SourceLanguage::C | SourceLanguage::Cpp => fixgen::apply_fixes_c(&source, &layouts_to_fix),
        SourceLanguage::Rust => fixgen::apply_fixes_rust(&source, &layouts_to_fix),
        SourceLanguage::Go => fixgen::apply_fixes_go(&source, &layouts_to_fix),
    };

    std::fs::write(path, &fixed_source)?;
    println!(
        "Rewrote {} struct(s) in {}. Backup: {}",
        layouts_to_fix.len(),
        path.display(),
        bak.display()
    );

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
                "fix only works on source files (.c, .cpp, .rs, .go) or directories; got {}",
                path.display()
            );
        }
    }
    Ok(files)
}
