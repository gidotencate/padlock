// padlock-cli/src/compile_commands.rs
//
// Exact C/C++ layouts from a `compile_commands.json`: every translation unit is
// compiled again with its own flags plus debug info, and the layouts the
// compiler produced are read from the object files' DWARF. This sees through
// macros, `#if` configuration, templates and platform ABI — everything the
// source frontends can only approximate.

use std::path::{Path, PathBuf};

use padlock_core::ir::StructLayout;
use rayon::prelude::*;

#[derive(serde::Deserialize)]
struct Entry {
    directory: PathBuf,
    file: PathBuf,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default)]
    command: Option<String>,
}

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("padlock-cc-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(ScratchDir(dir))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Split a shell command line into words (single/double quotes, backslashes).
fn shell_words(cmd: &str) -> Vec<String> {
    let (mut words, mut cur, mut in_word) = (Vec::new(), String::new(), false);
    let mut chars = cmd.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    in_word = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

/// The entry's compiler invocation rewritten to emit an object with complete
/// debug info into `out`: its own `-c`/`-o`, debug, dependency-file and
/// `-Werror` flags are dropped, then `-c -g` and the flags that make the
/// compiler describe every type are added.
fn debug_build_args(entry: &Entry, out: &Path) -> Option<Vec<String>> {
    let words = if entry.arguments.is_empty() {
        shell_words(entry.command.as_deref()?)
    } else {
        entry.arguments.clone()
    };
    let (compiler, rest) = words.split_first()?;
    let mut args = vec![compiler.clone()];
    let mut skip_next = false;
    for w in rest {
        if skip_next {
            skip_next = false;
            continue;
        }
        match w.as_str() {
            "-o" | "-MF" | "-MT" | "-MQ" => skip_next = true,
            "-c" | "-S" | "-E" | "-MD" | "-MMD" | "-M" | "-MM" | "-MP" => {}
            w if w.starts_with("-g") || w.starts_with("-Werror") || w.starts_with("-o") => {}
            w => args.push(w.to_string()),
        }
    }
    let is_clang = Path::new(compiler)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("clang"));
    args.extend([
        "-c".into(),
        "-g".into(),
        "-fno-eliminate-unused-debug-types".into(),
    ]);
    args.push(if is_clang {
        "-fstandalone-debug".into()
    } else {
        // Describe dynamic classes even where their key function is not defined.
        "-femit-class-debug-always".into()
    });
    args.extend(["-o".into(), out.to_string_lossy().into_owned()]);
    Some(args)
}

/// Longest common directory of `paths`.
fn common_dir(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter().filter_map(|p| p.parent());
    let mut common: PathBuf = iter.next()?.to_path_buf();
    for p in iter {
        while !p.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

/// Compile every entry and collect the layouts of types declared in the
/// project (under the common directory of its source files). Returns the
/// layouts and the source files that were compiled successfully.
pub fn collect(path: &Path) -> anyhow::Result<(Vec<StructLayout>, Vec<PathBuf>)> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    let entries: Vec<Entry> = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not a compile_commands.json: {e}", path.display()))?;
    // A private scratch directory for the objects, removed when done.
    let tmp = ScratchDir::new()?;
    let sources: Vec<PathBuf> = entries.iter().map(|e| e.directory.join(&e.file)).collect();
    let project = common_dir(&sources);

    let results: Vec<(PathBuf, anyhow::Result<Vec<StructLayout>>)> = entries
        .par_iter()
        .enumerate()
        .map(|(i, entry)| {
            let file = entry.directory.join(&entry.file);
            let obj = tmp.0.join(format!("tu{i}.o"));
            let result = (|| {
                let args =
                    debug_build_args(entry, &obj).ok_or_else(|| anyhow::anyhow!("no command"))?;
                let out = std::process::Command::new(&args[0])
                    .args(&args[1..])
                    .current_dir(&entry.directory)
                    .output()?;
                if !out.status.success() {
                    anyhow::bail!(
                        "compiler failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                            .lines()
                            .next()
                            .unwrap_or("")
                    );
                }
                let data = std::fs::read(&obj)?;
                let arch = padlock_dwarf::reader::detect_arch(&data)
                    .unwrap_or_else(|_| padlock_dwarf::reader::detect_arch_from_host());
                let dwarf = padlock_dwarf::reader::load(&data)?;
                padlock_dwarf::extractor::Extractor::new(&dwarf, arch).extract_all()
            })();
            (file, result)
        })
        .collect();

    let mut layouts: Vec<StructLayout> = Vec::new();
    let mut compiled = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (file, result) in results {
        match result {
            Ok(ls) => {
                compiled.push(file);
                for l in ls {
                    let in_project = match (&project, &l.source_file) {
                        (Some(root), Some(f)) => Path::new(f).starts_with(root),
                        _ => false,
                    };
                    // The same type appears in every TU that includes its header.
                    let key = (l.name.clone(), l.source_file.clone(), l.source_line);
                    if in_project && !l.name.starts_with('<') && seen.insert(key) {
                        layouts.push(l);
                    }
                }
            }
            Err(e) => eprintln!("padlock: warning: {}: {e}", file.display()),
        }
    }
    Ok((layouts, compiled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_handles_quotes() {
        assert_eq!(
            shell_words(r#"cc -DNAME="a b" -I'x y' -c f.c"#),
            ["cc", "-DNAME=a b", "-Ix y", "-c", "f.c"]
        );
    }

    #[test]
    fn debug_args_replace_output_and_debug_flags() {
        let e = Entry {
            directory: PathBuf::from("/b"),
            file: PathBuf::from("f.c"),
            arguments: vec![],
            command: Some("gcc -O2 -g0 -DX=1 -MD -MF f.d -c f.c -o f.o -Werror".into()),
        };
        let args = debug_build_args(&e, Path::new("/tmp/o.o")).unwrap();
        assert_eq!(args[0], "gcc");
        assert!(args.contains(&"-DX=1".to_string()));
        assert!(args.contains(&"-O2".to_string()));
        assert!(
            !args
                .iter()
                .any(|a| a == "-g0" || a == "f.o" || a == "-Werror" || a == "f.d")
        );
        assert_eq!(args.last().unwrap(), "/tmp/o.o");
    }
}
