// padlock-cli/src/rust_exact.rs
//
// `--rust-exact`: ask rustc for the real layout of Rust structs. Each Cargo
// package that owns an analyzed `.rs` file is built (into a separate target
// directory) with `-Zprint-type-sizes` — enabled on a stable toolchain through
// `RUSTC_BOOTSTRAP=1` — and the sizes, alignments and field order rustc chose
// replace padlock's estimates. Exact for `repr(Rust)` reordering, niches and std
// types; only types rustc actually generated code for are reported.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use padlock_core::ir::StructLayout;

#[derive(Debug, Default)]
struct RustcType {
    size: usize,
    align: usize,
    /// (field name, offset, size) in memory order.
    fields: Vec<(String, usize, usize)>,
}

/// Parse `-Zprint-type-sizes` output: `type: \`m::T\`: 12 bytes, alignment: 4 bytes`
/// followed by `field \`.y\`: 8 bytes` / `padding: 2 bytes` lines for a struct.
fn parse_type_sizes(text: &str) -> HashMap<String, RustcType> {
    let mut out: HashMap<String, RustcType> = HashMap::new();
    let mut cur: Option<(String, RustcType, usize, bool)> = None; // (name, ty, offset, is_enum)
    let mut flush = |cur: &mut Option<(String, RustcType, usize, bool)>| {
        if let Some((name, ty, _, is_enum)) = cur.take()
            && !is_enum
        {
            out.entry(name).or_insert(ty);
        }
    };
    let num = |s: &str| -> Option<usize> { s.split_whitespace().next()?.parse().ok() };
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("print-type-size ") else {
            continue;
        };
        if let Some(t) = rest.strip_prefix("type: `") {
            flush(&mut cur);
            let Some((name, tail)) = t.split_once("`: ") else {
                continue;
            };
            let size = num(tail).unwrap_or(0);
            let align = tail.split("alignment: ").nth(1).and_then(num).unwrap_or(1);
            cur = Some((
                name.to_string(),
                RustcType {
                    size,
                    align,
                    fields: Vec::new(),
                },
                0,
                false,
            ));
            continue;
        }
        let Some((_, ty, offset, is_enum)) = cur.as_mut() else {
            continue;
        };
        // Direct members are indented 4 spaces; deeper lines belong to enum variants.
        let depth = rest.len() - rest.trim_start().len();
        let item = rest.trim_start();
        if item.starts_with("discriminant:") || item.starts_with("variant `") {
            *is_enum = true;
            continue;
        }
        if depth != 4 {
            continue;
        }
        if let Some(f) = item.strip_prefix("field `.") {
            let Some((fname, tail)) = f.split_once("`: ") else {
                continue;
            };
            let size = num(tail).unwrap_or(0);
            ty.fields.push((fname.to_string(), *offset, size));
            *offset += size;
        } else if let Some(p) = item.strip_prefix("padding: ") {
            *offset += num(p).unwrap_or(0);
        }
    }
    flush(&mut cur);
    out
}

/// Nearest directory at or above `file` holding a `Cargo.toml`.
fn cargo_root(file: &Path) -> Option<PathBuf> {
    let file = std::fs::canonicalize(file).ok()?;
    let mut dir = file.parent()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

fn build_with_type_sizes(root: &Path) -> anyhow::Result<String> {
    let target_dir = root.join("target").join("padlock-rust-exact");
    // A fresh `--cfg` changes the crate's fingerprint, so cargo recompiles it
    // (and rustc prints the sizes again) even when nothing changed; its
    // dependencies stay cached.
    let nonce = format!(
        "padlock_exact_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let run = |selector: &[&str]| -> anyhow::Result<std::process::Output> {
        Ok(std::process::Command::new("cargo")
            .arg("rustc")
            .arg("--quiet")
            .args(selector)
            .args(["--", "-Awarnings", "-Zprint-type-sizes", "--cfg", &nonce])
            .env("RUSTC_BOOTSTRAP", "1")
            .env("CARGO_TARGET_DIR", &target_dir)
            .current_dir(root)
            .output()?)
    };
    let mut out = run(&["--lib"])?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    // A binary-only package has no `--lib` target: build each binary (extra
    // rustc arguments apply to one target at a time).
    if !out.status.success() {
        text.clear();
        for bin in binary_names(root) {
            out = run(&["--bin", &bin])?;
            if !out.status.success() {
                break;
            }
            text.push_str(&String::from_utf8_lossy(&out.stdout));
        }
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "cargo build failed: {}",
                err.lines().find(|l| l.contains("error")).unwrap_or("")
            );
        }
    }
    if !text.contains("print-type-size") {
        anyhow::bail!("rustc printed no type sizes");
    }
    Ok(text)
}

/// The `[[bin]]` targets of the package at `root`.
fn binary_names(root: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let manifest = root.join("Cargo.toml");
    let manifest = std::fs::canonicalize(&manifest).unwrap_or(manifest);
    meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| {
            p["manifest_path"]
                .as_str()
                .map(|m| std::fs::canonicalize(m).unwrap_or_else(|_| m.into()) == manifest)
                .unwrap_or(false)
        })
        .flat_map(|p| p["targets"].as_array().cloned().unwrap_or_default())
        .filter(|t| {
            t["kind"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k == "bin"))
        })
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect()
}

/// Replace Rust struct layouts with the ones rustc reports.
pub fn refine(layouts: &mut [StructLayout]) {
    let mut by_root: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, l) in layouts.iter().enumerate() {
        if let Some(f) = l.source_file.as_deref()
            && f.ends_with(".rs")
            && let Some(root) = cargo_root(Path::new(f))
        {
            by_root.entry(root).or_default().push(i);
        }
    }
    for (root, idxs) in by_root {
        let types = match build_with_type_sizes(&root) {
            Ok(t) => parse_type_sizes(&t),
            Err(e) => {
                eprintln!(
                    "padlock: note: --rust-exact skipped {}: {e}",
                    root.display()
                );
                continue;
            }
        };
        // rustc names types by crate-relative path (`m::T`); padlock by ident.
        let mut by_short: HashMap<&str, Vec<&RustcType>> = HashMap::new();
        for (name, t) in &types {
            let short = name.rsplit("::").next().unwrap_or(name);
            by_short.entry(short).or_default().push(t);
        }
        for i in idxs {
            let l = &mut layouts[i];
            let Some(t) = types.get(&l.name).or_else(|| {
                match by_short.get(l.name.as_str()).map(Vec::as_slice) {
                    Some([only]) => Some(*only),
                    _ => None,
                }
            }) else {
                continue;
            };
            if t.fields.len() != l.fields.len() {
                continue;
            }
            let mut fields = std::mem::take(&mut l.fields);
            let mut ordered = Vec::with_capacity(fields.len());
            for (fname, off, size) in &t.fields {
                let Some(pos) = fields.iter().position(|f| f.name == *fname) else {
                    break;
                };
                let mut f = fields.remove(pos);
                f.offset = *off;
                if *size > 0 {
                    f.size = *size;
                }
                ordered.push(f);
            }
            if !fields.is_empty() {
                // Names did not line up; keep padlock's layout.
                ordered.append(&mut fields);
                ordered.sort_by_key(|f| f.offset);
                l.fields = ordered;
                continue;
            }
            l.fields = ordered;
            l.total_size = t.size;
            l.align = t.align;
            l.uncertain_fields.clear();
            // This is rustc's actual layout, not an estimate of it.
            l.is_repr_rust = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_struct_field_order_and_padding() {
        let text = "print-type-size type: `m::T`: 12 bytes, alignment: 4 bytes
print-type-size     field `.y`: 8 bytes
print-type-size     field `.x`: 2 bytes
print-type-size     end padding: 2 bytes
print-type-size type: `E`: 8 bytes, alignment: 4 bytes
print-type-size     discriminant: 1 bytes
print-type-size     variant `A`: 7 bytes
print-type-size         padding: 3 bytes
print-type-size         field `.0`: 4 bytes, alignment: 4 bytes
print-type-size type: `P`: 16 bytes, alignment: 8 bytes
print-type-size     field `.a`: 1 bytes
print-type-size     padding: 7 bytes
print-type-size     field `.b`: 8 bytes, alignment: 8 bytes
";
        let t = parse_type_sizes(text);
        let mt = &t["m::T"];
        assert_eq!((mt.size, mt.align), (12, 4));
        assert_eq!(mt.fields, vec![("y".into(), 0, 8), ("x".into(), 8, 2)]);
        assert!(!t.contains_key("E"), "enums are left to padlock");
        assert_eq!(t["P"].fields[1], ("b".into(), 8, 8));
    }
}
