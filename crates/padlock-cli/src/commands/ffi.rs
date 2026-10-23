// padlock-cli/src/commands/ffi.rs
//
// `padlock ffi <paths>…`
//
// For every Rust `#[repr(C)]` struct that has a C/C++ struct of the same name
// among the analyzed files, check that the two describe the same memory:
// same size, alignment and field count, and each field (by position) at the
// same offset with the same size. A mismatch is an FFI bug waiting to happen.

use std::path::PathBuf;

use comfy_table::Table;
use padlock_core::ir::StructLayout;

use crate::paths::collect_layouts;

fn is_rust(l: &StructLayout) -> bool {
    l.source_file.as_deref().is_some_and(|f| f.ends_with(".rs"))
}

#[derive(serde::Serialize)]
struct Mismatch {
    name: String,
    rust_file: Option<String>,
    c_file: Option<String>,
    problem: String,
}

fn compare(r: &StructLayout, c: &StructLayout) -> Option<String> {
    if r.total_size != c.total_size {
        return Some(format!(
            "size {} (Rust) vs {} (C)",
            r.total_size, c.total_size
        ));
    }
    if r.align != c.align {
        return Some(format!("alignment {} (Rust) vs {} (C)", r.align, c.align));
    }
    if r.fields.len() != c.fields.len() {
        return Some(format!(
            "{} fields (Rust) vs {} (C)",
            r.fields.len(),
            c.fields.len()
        ));
    }
    for (rf, cf) in r.fields.iter().zip(&c.fields) {
        if rf.offset != cf.offset || rf.size != cf.size {
            return Some(format!(
                "`{}` at {} ({}B) vs `{}` at {} ({}B)",
                rf.name, rf.offset, rf.size, cf.name, cf.offset, cf.size
            ));
        }
    }
    None
}

/// Returns `Ok(true)` when every matched pair agrees.
pub fn run(paths: &[PathBuf], json: bool) -> anyhow::Result<bool> {
    let (layouts, _, _) = collect_layouts(paths, false, true)?;
    let rust: Vec<&StructLayout> = layouts
        .iter()
        .filter(|l| is_rust(l) && !l.is_repr_rust)
        .collect();
    let c: Vec<&StructLayout> = layouts.iter().filter(|l| !is_rust(l)).collect();
    let mut checked = 0;
    let mut mismatches = Vec::new();
    for r in &rust {
        let Some(cl) = c.iter().find(|cl| cl.name == r.name) else {
            continue;
        };
        checked += 1;
        if let Some(problem) = compare(r, cl) {
            mismatches.push(Mismatch {
                name: r.name.clone(),
                rust_file: r.source_file.clone(),
                c_file: cl.source_file.clone(),
                problem,
            });
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "checked": checked,
                "mismatches": mismatches,
            }))?
        );
        return Ok(mismatches.is_empty());
    }
    if !mismatches.is_empty() {
        let mut table = Table::new();
        table.set_header(vec!["Struct", "Problem", "Rust", "C"]);
        for m in &mismatches {
            table.add_row(vec![
                m.name.clone(),
                m.problem.clone(),
                m.rust_file.clone().unwrap_or_default(),
                m.c_file.clone().unwrap_or_default(),
            ]);
        }
        println!("{table}");
    }
    println!(
        "{}/{checked} repr(C) structs match their C definition",
        checked - mismatches.len()
    );
    Ok(mismatches.is_empty())
}
