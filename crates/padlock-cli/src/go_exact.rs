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
    // `-overlay` needs absolute paths.
    let dir = &std::fs::canonicalize(dir)?;
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
        let first = err
            .lines()
            .chain(stdout.lines())
            .find(|l| !l.trim().is_empty() && !l.starts_with('#'));
        anyhow::bail!("go test failed: {}", first.unwrap_or(""));
    }
    let mut probes: HashMap<String, Probe> = HashMap::new();
    for line in stdout.lines() {
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
    Ok(probes)
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
        let types: Vec<String> = idxs.iter().map(|&i| layouts[i].name.clone()).collect();
        let probes = match run_probe(&dir, &pkg, &types) {
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
