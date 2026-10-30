// padlock-cli/src/go_exact.rs
//
// `--go-exact`: ask the Go toolchain for the real layout of every Go struct
// padlock found. A probe test is added to each package *virtually* (`go test
// -overlay`, nothing is written to the source tree); it prints `reflect`'s size,
// alignment and per-field offset/size for each type, which then replace the
// source-level estimates — exact even for types from other packages
// (`driver.Connector`, `sync.Pool`, …) that padlock cannot size from source.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use padlock_core::ir::StructLayout;

/// `package name` of a Go source file.
fn package_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("package ")
            .map(|p| p.split_whitespace().next().unwrap_or("").to_string())
    })
}

struct Probe {
    size: usize,
    align: usize,
    /// (offset, size, align) per field, in declaration order.
    fields: Vec<(usize, usize, usize)>,
}

fn run_probe(
    dir: &Path,
    package: &str,
    types: &[String],
) -> anyhow::Result<HashMap<String, Probe>> {
    // `-overlay` needs absolute paths, spelled the way `go` spells them.
    let dir = &plain_absolute(&std::fs::canonicalize(dir)?);
    let scratch = std::env::temp_dir().join(format!(
        "padlock-go-{}-{}",
        std::process::id(),
        dir.to_string_lossy().len()
    ));
    std::fs::create_dir_all(&scratch)?;
    let mut src = format!(
        "package {package}\n\nimport (\n\t\"fmt\"\n\t\"reflect\"\n\t\"testing\"\n)\n\n\
         func padlockDump(name string, t reflect.Type) {{\n\
         \tfmt.Printf(\"PADLOCK\\t%s\\t%d\\t%d\\n\", name, t.Size(), t.Align())\n\
         \tfor i := 0; i < t.NumField(); i++ {{\n\
         \t\tf := t.Field(i)\n\
         \t\tfmt.Printf(\"PADLOCKF\\t%s\\t%d\\t%d\\t%d\\t%d\\n\", name, i, f.Offset, f.Type.Size(), f.Type.Align())\n\
         \t}}\n}}\n\nfunc TestPadlockLayoutProbe(t *testing.T) {{\n"
    );
    for ty in types {
        src.push_str(&format!(
            "\tpadlockDump({ty:?}, reflect.TypeOf((*{ty})(nil)).Elem())\n"
        ));
    }
    src.push_str("}\n");
    let probe = scratch.join("probe_test.go");
    std::fs::write(&probe, src)?;
    let virt = dir.join("zz_padlock_layout_probe_test.go");
    let overlay = scratch.join("overlay.json");
    std::fs::write(
        &overlay,
        serde_json::json!({ "Replace": { virt.to_string_lossy(): probe.to_string_lossy() } })
            .to_string(),
    )?;
    let out = std::process::Command::new("go")
        .args([
            "test",
            "-count=1",
            "-vet=off",
            "-run",
            "^TestPadlockLayoutProbe$",
            "-v",
        ])
        .arg(format!("-overlay={}", overlay.display()))
        .arg(".")
        .current_dir(dir)
        .output();
    let _ = std::fs::remove_dir_all(&scratch);
    let out = out?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() && !stdout.contains("PADLOCK\t") {
        let err = String::from_utf8_lossy(&out.stderr);
        // The first real diagnostic, not the `# package` header line.
        let mut diagnostics = err
            .lines()
            .chain(stdout.lines())
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
        let first = diagnostics.next();
        // Every `undefined: X` the compiler printed (it reports several at
        // once), so the caller can drop those types and retry.
        let undefined: Vec<&str> = err
            .lines()
            .chain(stdout.lines())
            .filter(|l| l.contains("undefined: "))
            .collect();
        anyhow::bail!(
            "go test failed: {}{}",
            first.unwrap_or(""),
            if undefined.len() > 1 {
                format!(" [{}]", undefined.join(" | "))
            } else {
                String::new()
            }
        );
    }
    let mut probes: HashMap<String, Probe> = HashMap::new();
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.as_slice() {
            ["PADLOCK", name, size, align] => {
                probes.insert(
                    name.to_string(),
                    Probe {
                        size: size.parse()?,
                        align: align.parse()?,
                        fields: Vec::new(),
                    },
                );
            }
            ["PADLOCKF", name, _i, off, size, align] => {
                if let Some(p) = probes.get_mut(*name) {
                    p.fields.push((off.parse()?, size.parse()?, align.parse()?));
                }
            }
            _ => {}
        }
    }
    if probes.is_empty() {
        anyhow::bail!("the layout probe did not run (overlay not applied?)");
    }
    Ok(probes)
}

/// Whether `text` contains `ident` as a whole identifier (`Pair[tspath.Path]`
/// mentions `tspath` and `Path`, not `ts`).
fn mentions_ident(text: &str, ident: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == ident)
}

/// Names the Go compiler reported as `undefined: Name` in `text`.
fn undefined_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("undefined: ") {
        let after = &rest[i + "undefined: ".len()..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            names.push(name);
        }
        rest = after;
    }
    names
}

/// `canonicalize` on Windows returns a verbatim path (`\\?\C:\...`), which
/// `go` does not match against its own spelling of the package directory, so
/// an overlay keyed on it is silently ignored. Drop the prefix for ordinary
/// drive paths.
fn plain_absolute(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Replace the layouts of Go structs with what the Go toolchain reports.
/// Packages whose probe cannot be built keep their source-level layouts.
pub fn refine(layouts: &mut [StructLayout]) {
    // Group Go layouts by package directory.
    let mut by_dir: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, l) in layouts.iter().enumerate() {
        let Some(f) = l.source_file.as_deref() else {
            continue;
        };
        if !f.ends_with(".go") || f.ends_with("_test.go") {
            continue;
        }
        if let Some(dir) = Path::new(f).parent() {
            by_dir.entry(dir.to_path_buf()).or_default().push(i);
        }
    }
    for (dir, idxs) in by_dir {
        let Some(pkg) = layouts[idxs[0]]
            .source_file
            .as_deref()
            .and_then(|f| package_name(Path::new(f)))
        else {
            continue;
        };
        let mut types: Vec<String> = idxs.iter().map(|&i| layouts[i].name.clone()).collect();
        // A type declared inside a function is not nameable from the probe and
        // fails the whole package build ("undefined: X"): drop the names the
        // compiler reports and probe the rest.
        let mut probed = run_probe(&dir, &pkg, &types);
        for _ in 0..8 {
            let Err(e) = &probed else { break };
            let undefined = undefined_names(&e.to_string());
            let before = types.len();
            types.retain(|t| !undefined.iter().any(|u| mentions_ident(t, u)));
            if types.len() == before || types.is_empty() {
                break;
            }
            probed = run_probe(&dir, &pkg, &types);
        }
        let probes = match probed {
            Ok(p) => p,
            Err(e) => {
                eprintln!("padlock: note: --go-exact skipped {}: {e}", dir.display());
                continue;
            }
        };
        for i in idxs {
            let l = &mut layouts[i];
            let Some(p) = probes.get(&l.name) else {
                continue;
            };
            if p.fields.len() != l.fields.len() {
                continue;
            }
            for (f, (off, size, align)) in l.fields.iter_mut().zip(&p.fields) {
                f.offset = *off;
                f.size = *size;
                f.align = *align;
            }
            l.total_size = p.size;
            l.align = p.align;
            l.uncertain_fields.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_type_arguments_mention_their_package() {
        assert!(mentions_ident("Pair[tspath.Path]", "tspath"));
        assert!(!mentions_ident("Pair[tspath.Path]", "ts"));
    }

    #[test]
    fn undefined_names_are_collected() {
        let text = "x_test.go:3:4: undefined: fileEvents | y_test.go:4:5: undefined: declKey";
        assert_eq!(undefined_names(text), vec!["fileEvents", "declKey"]);
    }

    #[test]
    fn verbatim_drive_paths_lose_their_prefix() {
        assert_eq!(
            plain_absolute(Path::new(r"\\?\D:\a\pkg")),
            PathBuf::from(r"D:\a\pkg")
        );
        // UNC verbatim paths and ordinary paths are left alone.
        assert_eq!(
            plain_absolute(Path::new(r"\\?\UNC\srv\share")),
            PathBuf::from(r"\\?\UNC\srv\share")
        );
        assert_eq!(
            plain_absolute(Path::new("/tmp/pkg")),
            PathBuf::from("/tmp/pkg")
        );
    }
}
