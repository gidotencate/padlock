// padlock-cli/src/commands/diff.rs

use std::path::{Path, PathBuf};

use padlock_core::findings::Report;

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

/// Compare struct layouts between `git_ref` and the current working tree —
/// not declared-vs-optimal order (that's the default `diff` mode), but the
/// same struct's layout at two points in history. Catches ABI-relevant
/// changes (size, padding, score) a plain source diff wouldn't call out.
pub fn run_against(paths: &[PathBuf], git_ref: &str, filter: Option<&str>) -> anyhow::Result<()> {
    let re = filter
        .map(|p| {
            regex::Regex::new(p).map_err(|_| anyhow::anyhow!("invalid --filter pattern: {p:?}"))
        })
        .transpose()?;

    let repo_root = git_repo_root()?;
    let source_files = expand_to_source_files(paths)?;
    let arch = padlock_dwarf::reader::detect_arch_from_host();

    let mut old_layouts = Vec::new();
    let mut new_layouts = Vec::new();

    for file in &source_files {
        let Some(lang) = padlock_source::detect_language(file) else {
            continue;
        };

        match padlock_source::parse_source(file, arch) {
            Ok(output) => new_layouts.extend(output.layouts),
            Err(e) => {
                eprintln!("padlock: warning: {}: {e}", file.display());
                continue;
            }
        }

        // A file that doesn't exist at `git_ref` (new file, or the ref is
        // simply older than its introduction) has no old content — every
        // struct in it will correctly show up as added, below.
        if let Some(old_source) = git_show(&repo_root, git_ref, file)?
            && let Ok(layouts) = padlock_source::parse_source_str(&old_source, &lang, arch)
        {
            old_layouts.extend(layouts);
        }
    }

    let old_report = Report::from_layouts(&old_layouts);
    let new_report = Report::from_layouts(&new_layouts);

    let matches_filter = |name: &str| re.as_ref().is_none_or(|r| r.is_match(name));

    let old_by_name: std::collections::HashMap<&str, &padlock_core::findings::StructReport> =
        old_report
            .structs
            .iter()
            .map(|s| (s.struct_name.as_str(), s))
            .collect();
    let new_by_name: std::collections::HashMap<&str, &padlock_core::findings::StructReport> =
        new_report
            .structs
            .iter()
            .map(|s| (s.struct_name.as_str(), s))
            .collect();

    let mut any_change = false;

    for new in new_report
        .structs
        .iter()
        .filter(|s| matches_filter(&s.struct_name))
    {
        match old_by_name.get(new.struct_name.as_str()) {
            None => {
                println!(
                    "+ {}  (new — {}B, score {:.0})",
                    new.struct_name, new.total_size, new.score
                );
                any_change = true;
            }
            Some(old)
                if old.total_size != new.total_size
                    || old.wasted_bytes != new.wasted_bytes
                    || (old.score - new.score).abs() > f64::EPSILON =>
            {
                println!(
                    "~ {}  {}B -> {}B  (waste {}B -> {}B, score {:.0} -> {:.0})",
                    new.struct_name,
                    old.total_size,
                    new.total_size,
                    old.wasted_bytes,
                    new.wasted_bytes,
                    old.score,
                    new.score
                );
                any_change = true;
            }
            Some(_) => {}
        }
    }

    for old in old_report
        .structs
        .iter()
        .filter(|s| matches_filter(&s.struct_name))
    {
        if !new_by_name.contains_key(old.struct_name.as_str()) {
            println!(
                "- {}  (removed — was {}B, score {:.0})",
                old.struct_name, old.total_size, old.score
            );
            any_change = true;
        }
    }

    if !any_change {
        println!("No layout changes between {git_ref} and the working tree.");
    }
    Ok(())
}

fn git_repo_root() -> anyhow::Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("not inside a git repository (git rev-parse --show-toplevel failed)");
    }
    let root = String::from_utf8(output.stdout)
        .map_err(|_| anyhow::anyhow!("git repo root path is not valid UTF-8"))?;
    Ok(PathBuf::from(root.trim()))
}

/// Return `path`'s content as it existed at `git_ref`, or `None` if the file
/// didn't exist at that ref (new file, or ref predates it).
fn git_show(repo_root: &Path, git_ref: &str, path: &Path) -> anyhow::Result<Option<String>> {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let rel = abs.strip_prefix(repo_root).unwrap_or(path);
    // git pathspecs always use forward slashes, regardless of host OS.
    let rel_slash = rel.to_string_lossy().replace('\\', "/");

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("show")
        .arg(format!("{git_ref}:{rel_slash}"))
        .output()?;

    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}
