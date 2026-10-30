// padlock-cli/src/commands/verify.rs
//
// `padlock verify --binary <BIN> <sources>…`
//
// Cross-checks padlock's source-level layouts against the compiler's own layout,
// read from the debug info of a binary built from those sources. Reports every
// struct whose size or field offsets differ, and exits non-zero when any does —
// a CI guard that the source analysis can be trusted for a code base.

use std::collections::HashMap;
use std::path::PathBuf;

use comfy_table::Table;
use padlock_core::ir::StructLayout;

use crate::paths::collect_layouts;

/// A struct name without module/namespace qualifiers and spaces, so `main.Conn`,
/// `ns::Conn` and `Conn` — or `Buf<char,8>` and `Buf<char, 8>` — match.
fn match_key(name: &str) -> String {
    let mut s = name;
    loop {
        // Strip one leading `ident.` / `ident::` (but not inside template arguments).
        let head_end = s.find(['<', '[', '(']).unwrap_or(s.len());
        let head = &s[..head_end];
        let cut = head
            .rfind("::")
            .map(|i| i + 2)
            .or_else(|| head.rfind('.').map(|i| i + 1));
        match cut {
            Some(c) if c < s.len() => s = &s[c..],
            _ => break,
        }
    }
    s.replace(' ', "")
}

#[derive(serde::Serialize)]
struct FieldMismatch {
    field: String,
    source_offset: usize,
    binary_offset: usize,
    source_size: usize,
    binary_size: usize,
}

#[derive(serde::Serialize)]
struct Verdict {
    name: String,
    source_file: Option<String>,
    source_size: usize,
    binary_size: Option<usize>,
    /// `ok`, `size-mismatch`, `offset-mismatch`, `offsets-differ-estimate`
    /// (compiler-ordered Rust/Zig struct: order is not guaranteed) or
    /// `not-in-binary` (unused types are often absent from debug info).
    status: &'static str,
    field_mismatches: Vec<FieldMismatch>,
}

fn compare(src: &StructLayout, bin: &StructLayout) -> Verdict {
    let bin_fields: HashMap<&str, (usize, usize)> = bin
        .fields
        .iter()
        .map(|f| (f.name.as_str(), (f.offset, f.size)))
        .collect();
    let mut field_mismatches = Vec::new();
    for f in &src.fields {
        // Synthetic fields (`[a:3|b:5]`, `__vptr`, `__base_X`) have no DWARF twin,
        // and a zero-size member's offset is unobservable.
        // …and blank `_` fields (padding in Go): several share the name, so a
        // lookup by name would compare each with the first.
        if f.name.starts_with('[') || f.name.starts_with("__") || f.name == "_" || f.size == 0 {
            continue;
        }
        if let Some(&(bo, bs)) = bin_fields.get(f.name.as_str())
            && (bo != f.offset || (bs != f.size && bs != 0))
        {
            field_mismatches.push(FieldMismatch {
                field: f.name.clone(),
                source_offset: f.offset,
                binary_offset: bo,
                source_size: f.size,
                binary_size: bs,
            });
        }
    }
    let status = if src.total_size != bin.total_size {
        "size-mismatch"
    } else if field_mismatches.is_empty() {
        "ok"
    } else if src.is_repr_rust {
        "offsets-differ-estimate"
    } else {
        "offset-mismatch"
    };
    Verdict {
        name: src.name.clone(),
        source_file: src.source_file.clone(),
        source_size: src.total_size,
        binary_size: Some(bin.total_size),
        status,
        field_mismatches,
    }
}

/// Returns `Ok(true)` when every struct found in both agrees.
pub fn run(
    sources: &[PathBuf],
    binaries: &[PathBuf],
    json: bool,
    filter: Option<&str>,
    defines: Vec<String>,
    include_dirs: Vec<PathBuf>,
) -> anyhow::Result<bool> {
    // The source side must see the same macros and headers the compiler did:
    // `.padlock.toml` (searched upward from the first source) plus -D / -I.
    let cfg = padlock_core::config::Config::for_path(
        sources
            .first()
            .map(|p| p.as_path())
            .unwrap_or(std::path::Path::new(".")),
    );
    let mut all_defines = cfg.defines.clone();
    all_defines.extend(defines);
    padlock_source::set_preprocessor_defines(&all_defines);
    let mut all_dirs: Vec<PathBuf> = cfg.include_dirs.iter().map(PathBuf::from).collect();
    all_dirs.extend(include_dirs);
    padlock_source::set_include_dirs(all_dirs);
    let re = filter
        .map(regex::Regex::new)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid --filter pattern: {e}"))?;
    let (src_layouts, _, _) = collect_layouts(sources, false, true)?;
    let (bin_layouts, _, _) = collect_layouts(binaries, false, true)?;

    let mut by_key: HashMap<String, Vec<&StructLayout>> = HashMap::new();
    for l in &bin_layouts {
        by_key.entry(match_key(&l.name)).or_default().push(l);
    }
    // Among binary types with the same short name (`main.Pool` and `sync.Pool`)
    // prefer the exact name, then the program's own package/crate, then a
    // unique candidate. Ambiguous matches are reported as not found.
    let pick = |src: &StructLayout| -> Option<&StructLayout> {
        let cands = by_key.get(&match_key(&src.name))?;
        // Go names types by import path (`regexp/syntax.Error`): only a type of
        // the source file's own package is the same type.
        if let Some(f) = src.source_file.as_deref()
            && f.ends_with(".go")
        {
            let pkg = std::path::Path::new(f)
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|d| d.to_str())
                .unwrap_or("");
            let want = format!("{pkg}.{}", src.name);
            let main = format!("main.{}", src.name);
            return cands
                .iter()
                .find(|c| c.name == want || c.name.ends_with(&format!("/{want}")) || c.name == main)
                .copied();
        }
        if let Some(exact) = cands.iter().find(|c| c.name == src.name) {
            return Some(exact);
        }
        let stem = src
            .source_file
            .as_deref()
            .and_then(|f| std::path::Path::new(f).file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let own: Vec<&&StructLayout> = cands
            .iter()
            .filter(|c| {
                ["main.", "crate::", "main::"]
                    .iter()
                    .any(|p| c.name.starts_with(p))
                    || (!stem.is_empty()
                        && (c.name.starts_with(&format!("{stem}::"))
                            || c.name.starts_with(&format!("{stem}."))))
            })
            .collect();
        match (own.as_slice(), cands.as_slice()) {
            ([one], _) => Some(one),
            (_, [only]) => Some(only),
            _ => {
                // Several identical descriptions of the same type are fine.
                let first = cands[0];
                cands
                    .iter()
                    .all(|c| {
                        c.total_size == first.total_size && c.fields.len() == first.fields.len()
                    })
                    .then_some(first)
            }
        }
    };

    let mut verdicts: Vec<Verdict> = src_layouts
        .iter()
        .filter(|l| re.as_ref().is_none_or(|r| r.is_match(&l.name)))
        // Anonymous types cannot be matched by name; test-only types are not in
        // the binary.
        .filter(|l| !l.name.starts_with('<'))
        .filter(|l| {
            !l.source_file
                .as_deref()
                .is_some_and(|f| f.ends_with("_test.go"))
        })
        .map(|src| match pick(src) {
            Some(bin) => compare(src, bin),
            None => Verdict {
                name: src.name.clone(),
                source_file: src.source_file.clone(),
                source_size: src.total_size,
                binary_size: None,
                status: "not-in-binary",
                field_mismatches: Vec::new(),
            },
        })
        .collect();
    verdicts.sort_by(|a, b| a.status.cmp(b.status).then(a.name.cmp(&b.name)));
    let failed = verdicts
        .iter()
        .any(|v| matches!(v.status, "size-mismatch" | "offset-mismatch"));

    if json {
        println!("{}", serde_json::to_string_pretty(&verdicts)?);
        return Ok(!failed);
    }

    let checked = verdicts
        .iter()
        .filter(|v| v.status != "not-in-binary")
        .count();
    let ok = verdicts.iter().filter(|v| v.status == "ok").count();
    let mut table = Table::new();
    table.set_header(vec![
        "Struct",
        "Source",
        "Binary",
        "Status",
        "First difference",
    ]);
    for v in verdicts
        .iter()
        .filter(|v| !matches!(v.status, "ok" | "not-in-binary"))
    {
        let first = v.field_mismatches.first().map(|m| {
            format!(
                "{}: offset {} vs {}, size {} vs {}",
                m.field, m.source_offset, m.binary_offset, m.source_size, m.binary_size
            )
        });
        table.add_row(vec![
            v.name.clone(),
            format!("{}B", v.source_size),
            v.binary_size.map(|b| format!("{b}B")).unwrap_or_default(),
            v.status.to_string(),
            first.unwrap_or_default(),
        ]);
    }
    if checked > ok {
        println!("{table}");
    }
    let missing = verdicts.len() - checked;
    println!(
        "{ok}/{checked} structs match the binary's debug info{}",
        if missing > 0 {
            format!(" ({missing} not described in the binary)")
        } else {
            String::new()
        }
    );
    Ok(!failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_key_strips_qualifiers_and_spaces() {
        assert_eq!(match_key("main.Conn"), "Conn");
        assert_eq!(match_key("ns::inner::Conn"), "Conn");
        assert_eq!(match_key("layout::Buf<u8, 4>"), "Buf<u8,4>");
        assert_eq!(match_key("main.Pair[int, string]"), "Pair[int,string]");
        assert_eq!(match_key("Buf<std::string>"), "Buf<std::string>");
    }
}
