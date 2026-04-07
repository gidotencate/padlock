// padlock-cli/src/commands/fix.rs
//
// Applies optimal field reordering to a source file in-place.
// A `.bak` copy of the original is written before any changes are made.

use std::path::Path;

use padlock_core::findings::{Finding, Report};
use padlock_source::{fixgen, SourceLanguage};

pub fn run(path: &Path, dry_run: bool) -> anyhow::Result<()> {
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

    // Collect layouts that have a ReorderSuggestion finding
    let mut layouts_to_fix: Vec<&padlock_core::ir::StructLayout> = Vec::new();
    for sr in &report.structs {
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
        println!("Nothing to fix — all structs are already optimally ordered.");
        return Ok(());
    }

    // Show per-struct diff (old struct text → new struct text)
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

    // Write .bak backup then apply in-place rewrite
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
