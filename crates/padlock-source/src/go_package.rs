// padlock-source/src/go_package.rs
//
// A Go package is all the `.go` files of one directory that share a `package`
// clause, so a field may use a type, alias or constant declared in a sibling
// file. Parsing a file on its own would size such a field as one word. Here the
// package is parsed once per directory (cached), with every file seeing the
// declarations of the others, and struct sizes are resolved package-wide.
// Files excluded for the target by build constraints (`_windows.go`,
// `//go:build !linux`) are left out, as `go build` would.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use padlock_core::arch::ArchConfig;
use padlock_core::findings::SkippedStruct;
use padlock_core::ir::StructLayout;

use crate::frontends::go::{self, GoCtx};

const KNOWN_OS: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "js",
    "linux",
    "nacl",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
    "zos",
];

const KNOWN_ARCH: &[&str] = &[
    "386",
    "amd64",
    "amd64p32",
    "arm",
    "armbe",
    "arm64",
    "arm64be",
    "loong64",
    "mips",
    "mipsle",
    "mips64",
    "mips64le",
    "mips64p32",
    "mips64p32le",
    "ppc",
    "ppc64",
    "ppc64le",
    "riscv",
    "riscv64",
    "s390",
    "s390x",
    "sparc",
    "sparc64",
    "wasm",
];

const UNIX_OS: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "linux",
    "netbsd",
    "openbsd",
    "solaris",
];

/// `(GOOS, GOARCH)` for a padlock target.
fn go_target(arch: &ArchConfig) -> (&'static str, &'static str) {
    match arch.name {
        "x86_64-msvc" => ("windows", "amd64"),
        "aarch64-apple" => ("darwin", "arm64"),
        "aarch64" => ("linux", "arm64"),
        "x86" => ("linux", "386"),
        "riscv64" => ("linux", "riscv64"),
        "wasm32" => ("wasip1", "wasm"),
        "cortex_m" | "cortex_m4" => ("linux", "arm"),
        _ => ("linux", "amd64"),
    }
}

/// Whether a build tag holds for the target. Release tags (`go1.21`) and `gc`
/// / `cgo` hold; custom tags (`purego`, `race`, experiments) do not.
/// The toolchain's own build tags for the target — the `GOEXPERIMENT`s on by
/// default (`goexperiment.exectracer2`, …) and the arch level (`amd64.v1`) —
/// asked once from `go list`; without a Go toolchain, those of Go 1.22–1.23.
fn tool_tags(goos: &str, goarch: &str) -> Arc<HashSet<String>> {
    type Tags = Mutex<HashMap<(String, String), Arc<HashSet<String>>>>;
    static TAGS: LazyLock<Tags> = LazyLock::new(Default::default);
    let key = (goos.to_string(), goarch.to_string());
    if let Some(t) = TAGS.lock().ok().and_then(|t| t.get(&key).cloned()) {
        return t;
    }
    let asked = std::process::Command::new("go")
        .args(["list", "-f", "{{context.ToolTags}}", "runtime"])
        .env("GOOS", goos)
        .env("GOARCH", goarch)
        .env("CGO_ENABLED", "0")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split_whitespace()
                .map(str::to_string)
                .collect::<HashSet<String>>()
        });
    let tags = Arc::new(asked.unwrap_or_else(|| {
        let mut t: HashSet<String> = [
            "goexperiment.regabiwrappers",
            "goexperiment.regabiargs",
            "goexperiment.coverageredesign",
            "goexperiment.allocheaders",
            "goexperiment.exectracer2",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        if goarch == "amd64" {
            t.insert("amd64.v1".to_string());
        }
        t
    }));
    if let Ok(mut t) = TAGS.lock() {
        t.insert(key, tags.clone());
    }
    tags
}

fn tag_holds(tag: &str, goos: &str, goarch: &str) -> bool {
    tag == goos
        || tool_tags(goos, goarch).contains(tag)
        || tag == goarch
        || (tag == "unix" && UNIX_OS.contains(&goos))
        || (tag == "linux" && goos == "android")
        || (tag == "darwin" && goos == "ios")
        || tag == "gc"
        || tag == "cgo"
        || tag
            .strip_prefix("go1.")
            .is_some_and(|v| v.parse::<u32>().is_ok())
}

/// Evaluate a `//go:build` expression (`linux && (amd64 || arm64) && !purego`).
fn eval_build_expr(expr: &str, goos: &str, goarch: &str) -> bool {
    let mut tokens: Vec<String> = Vec::new();
    let mut chars = expr.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '(' | ')' | '!' => {
                tokens.push(c.to_string());
                chars.next();
            }
            '&' | '|' => {
                chars.next();
                chars.next();
                tokens.push(if c == '&' { "&&" } else { "||" }.to_string());
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' || c == '.' {
                        word.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if word.is_empty() {
                    chars.next();
                } else {
                    tokens.push(word);
                }
            }
        }
    }
    let mut pos = 0;
    // An unparsable constraint keeps the file rather than silently dropping it.
    parse_or(&tokens, &mut pos, goos, goarch).unwrap_or(true)
}

fn parse_or(t: &[String], pos: &mut usize, goos: &str, goarch: &str) -> Option<bool> {
    let mut v = parse_and(t, pos, goos, goarch)?;
    while t.get(*pos).is_some_and(|x| x == "||") {
        *pos += 1;
        v |= parse_and(t, pos, goos, goarch)?;
    }
    Some(v)
}

fn parse_and(t: &[String], pos: &mut usize, goos: &str, goarch: &str) -> Option<bool> {
    let mut v = parse_not(t, pos, goos, goarch)?;
    while t.get(*pos).is_some_and(|x| x == "&&") {
        *pos += 1;
        v &= parse_not(t, pos, goos, goarch)?;
    }
    Some(v)
}

fn parse_not(t: &[String], pos: &mut usize, goos: &str, goarch: &str) -> Option<bool> {
    let tok = t.get(*pos)?;
    *pos += 1;
    match tok.as_str() {
        "!" => parse_not(t, pos, goos, goarch).map(|v| !v),
        "(" => {
            let v = parse_or(t, pos, goos, goarch)?;
            (t.get(*pos)? == ")").then(|| *pos += 1)?;
            Some(v)
        }
        "&&" | "||" | ")" => None,
        tag => Some(tag_holds(tag, goos, goarch)),
    }
}

/// Whether `go build` for the target would compile this file (file-name
/// suffixes and `//go:build` lines; the legacy `// +build` form is ignored,
/// since Go 1.17+ files carry both).
fn file_is_built(path: &Path, source: &str, goos: &str, goarch: &str) -> bool {
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".go"))
        .unwrap_or("");
    let stem = stem.strip_suffix("_test").unwrap_or(stem);
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() >= 2 {
        let last = parts[parts.len() - 1];
        let prev = parts[parts.len() - 2];
        if KNOWN_ARCH.contains(&last) {
            if last != goarch {
                return false;
            }
            if parts.len() >= 3 && KNOWN_OS.contains(&prev) && !tag_holds(prev, goos, goarch) {
                return false;
            }
        } else if KNOWN_OS.contains(&last) && !tag_holds(last, goos, goarch) {
            return false;
        }
    }
    for line in source.lines() {
        let line = line.trim();
        if let Some(expr) = line.strip_prefix("//go:build ") {
            return eval_build_expr(expr, goos, goarch);
        }
        if line.starts_with("package ") {
            break;
        }
    }
    true
}

/// Whether `go build` for `arch` would compile the Go file at `path` (its
/// `_GOOS`/`_GOARCH` name suffix and `//go:build` line).
pub(crate) fn built_for_target(path: &Path, arch: &ArchConfig) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return true;
    };
    let (goos, goarch) = go_target(arch);
    file_is_built(path, &text, goos, goarch)
}

/// One package (directory + `package` name), parsed and resolved together.
struct Package {
    /// (path, mtime, size) of every `.go` file in the directory, to detect edits.
    signature: Vec<(PathBuf, Option<SystemTime>, u64)>,
    /// The files that belong to the package for the target.
    members: HashSet<PathBuf>,
    /// The merged declarations of all member files.
    ctx: GoCtx,
    /// Resolved layouts of all member files, `source_file` set.
    layouts: Vec<StructLayout>,
    /// Types each member file skipped (generics with no use, …).
    skipped: HashMap<PathBuf, Vec<SkippedStruct>>,
}

/// One cache slot per package. Its lock is held while the package is checked
/// or built, so parallel workers parsing files of the same package wait for a
/// single build instead of each doing it; different packages build in parallel.
type Slot = Mutex<Option<(Arc<Package>, Instant)>>;
type Cache = Mutex<HashMap<(PathBuf, String, &'static str, u64), Arc<Slot>>>;

/// How long a package is trusted before its files' timestamps are checked again.
const RECHECK_AFTER: Duration = Duration::from_secs(2);

static PACKAGES: LazyLock<Cache> = LazyLock::new(Default::default);

fn dir_signature(dir: &Path) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
    let mut sig: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "go"))
        .map(|p| {
            let md = std::fs::metadata(&p).ok();
            let mtime = md.as_ref().and_then(|m| m.modified().ok());
            let len = md.map(|m| m.len()).unwrap_or(0);
            (p, mtime, len)
        })
        .collect();
    sig.sort();
    sig
}

fn build_package(
    name: &str,
    arch: &'static ArchConfig,
    signature: Vec<(PathBuf, Option<SystemTime>, u64)>,
    parallel: bool,
) -> Package {
    let (goos, goarch) = go_target(arch);
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    for (path, _, len) in &signature {
        // Generated tables (the CLI's own size limit) hold no struct types a
        // hand-written file could need, and dominate parse time.
        if *len > 500_000 || path.to_string_lossy().ends_with("_test.go") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if !file_is_built(path, &text, goos, goarch)
            || go::package_clause(&text).as_deref() != Some(name)
        {
            continue;
        }
        files.push((path.clone(), text));
    }
    let decls = in_parallel(parallel, &files, |_, (_, text)| {
        go::file_declarations(text, arch)
    });
    let mut ctx = match decls.iter().flatten().next() {
        Some((first, _)) => first.clone(),
        None => {
            go::file_declarations("package p\n", arch)
                .expect("trivial Go parses")
                .0
        }
    };
    for (d, _) in decls.iter().flatten().skip(1) {
        ctx.absorb(d);
    }
    let parsed = in_parallel(parallel, &files, |i, (path, text)| {
        let outer = crate::take_skipped();
        let own = go::parse_go_in_package(text, arch, &ctx, i + 1, decls[i].as_ref());
        let skipped = crate::take_skipped();
        crate::restore_skipped(outer);
        let file = path.to_string_lossy().into_owned();
        let own = own.map(|mut v| {
            for l in &mut v {
                l.source_file = Some(file.clone());
            }
            v
        });
        (own, skipped)
    });
    let mut layouts = Vec::new();
    let mut skipped = HashMap::new();
    for ((path, _), (own, notes)) in files.iter().zip(parsed) {
        skipped.insert(path.clone(), notes);
        if let Ok(own) = own {
            layouts.extend(own);
        }
    }
    crate::resolve_nested_structs(&mut layouts);
    Package {
        signature,
        members: files.into_iter().map(|(p, _)| p).collect(),
        ctx,
        layouts,
        skipped,
    }
}

/// Map `f` over `items` on scoped OS threads, keeping order. Plain threads,
/// not rayon: the caller may be a rayon worker that holds this package's slot
/// lock, and work stealing could make it pick up a sibling file and deadlock.
fn in_parallel<T: Sync, R: Send>(
    parallel: bool,
    items: &[T],
    f: impl Fn(usize, &T) -> R + Sync,
) -> Vec<R> {
    if !parallel {
        return items.iter().enumerate().map(|(i, t)| f(i, t)).collect();
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(items.len().div_ceil(8).max(1));
    if threads <= 1 {
        return items.iter().enumerate().map(|(i, t)| f(i, t)).collect();
    }
    let chunk = items.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .enumerate()
            .map(|(c, part)| {
                let f = &f;
                scope.spawn(move || {
                    part.iter()
                        .enumerate()
                        .map(|(j, t)| f(c * chunk + j, t))
                        .collect::<Vec<R>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("Go package worker panicked"))
            .collect()
    })
}

/// The cached package `name` in `dir`, built (or rebuilt after an edit or a
/// settings change) as needed.
fn package_for(
    dir: &Path,
    name: &str,
    arch: &'static ArchConfig,
    parallel: bool,
) -> Option<Arc<Package>> {
    let key = (
        dir.to_path_buf(),
        name.to_string(),
        arch.name,
        crate::settings_generation(),
    );
    let slot = PACKAGES.lock().ok()?.entry(key).or_default().clone();
    let mut state = slot.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = match state.as_mut() {
        Some((p, checked)) if checked.elapsed() < RECHECK_AFTER => Some(p.clone()),
        Some((p, checked)) => {
            let sig = dir_signature(dir);
            (p.signature == sig).then(|| {
                *checked = Instant::now();
                p.clone()
            })
        }
        None => None,
    };
    Some(match fresh {
        Some(p) => p,
        None => {
            let p = Arc::new(build_package(name, arch, dir_signature(dir), parallel));
            *state = Some((p.clone(), Instant::now()));
            p
        }
    })
}

/// Build every package of `dir` (one per `package` name among its non-test
/// files) into the cache, on the calling thread.
pub(crate) fn prepare(dir: &Path, arch: &'static ArchConfig) {
    let names: HashSet<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "go") && !p.to_string_lossy().ends_with("_test.go")
        })
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| go::package_clause(&t))
        .collect();
    for name in names {
        package_for(dir, &name, arch, true);
    }
}

/// Layouts of `path` (with text `source`) resolved against its whole package,
/// or `None` when the file has no package clause or directory.
pub(crate) fn parse_in_package(
    path: &Path,
    source: &str,
    arch: &'static ArchConfig,
) -> Option<Vec<StructLayout>> {
    let name = go::package_clause(source)?;
    let dir = path.parent()?.to_path_buf();
    let dir = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    };
    let package = package_for(&dir, &name, arch, true)?;
    let file = path.to_string_lossy().into_owned();
    // A member file was already parsed and resolved with its package.
    let own_path = dir.join(path.file_name()?);
    if package.members.contains(&own_path) {
        crate::restore_skipped(package.skipped.get(&own_path).cloned().unwrap_or_default());
        return Some(
            package
                .layouts
                .iter()
                .filter(|l| l.source_file.as_deref() == Some(file.as_str()))
                .cloned()
                .collect(),
        );
    }
    // A test file, or one excluded for the target: parse it against the package.
    against_package(&package, &file, source, arch)
}

/// Directories with more `.go` files than this are not scanned for editor
/// buffers (building the package would stall the editor); the buffer is
/// analyzed on its own then.
const MAX_BUFFER_PACKAGE_FILES: usize = 400;

/// Layouts of an editor buffer (`source` is the live text of `path`, possibly
/// unsaved) resolved against the package on disk: the buffer's own
/// declarations win over the saved version of the same file.
pub(crate) fn parse_buffer_in_package(
    path: &Path,
    source: &str,
    arch: &'static ArchConfig,
) -> Option<Vec<StructLayout>> {
    let name = go::package_clause(source)?;
    let dir = path.parent()?.to_path_buf();
    let dir = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    };
    if dir_signature(&dir).len() > MAX_BUFFER_PACKAGE_FILES {
        return None;
    }
    let package = package_for(&dir, &name, arch, true)?;
    against_package(&package, &path.to_string_lossy(), source, arch)
}

/// Parse `source` with `package`'s declarations in scope and resolve its
/// structs against the package's layouts.
fn against_package(
    package: &Package,
    file: &str,
    source: &str,
    arch: &'static ArchConfig,
) -> Option<Vec<StructLayout>> {
    let mut own = go::parse_go_in_package(source, arch, &package.ctx, usize::MAX, None).ok()?;
    for l in &mut own {
        l.source_file = Some(file.to_string());
    }
    let n = own.len();
    // The package's layouts are final; only those this file names are needed —
    // and not the saved versions of types the file itself (re)defines.
    let defined: HashSet<String> = own.iter().map(|l| l.name.clone()).collect();
    let named: HashSet<&str> = own
        .iter()
        .flat_map(|l| &l.fields)
        .filter_map(|f| match &f.ty {
            padlock_core::ir::TypeInfo::Primitive { name, .. }
            | padlock_core::ir::TypeInfo::Opaque { name, .. } => {
                Some(name.split('[').next().unwrap_or(name).trim())
            }
            _ => None,
        })
        .collect();
    let context: Vec<StructLayout> = package
        .layouts
        .iter()
        .filter(|l| named.contains(l.name.as_str()) && !defined.contains(&l.name))
        .cloned()
        .collect();
    own.extend(context);
    crate::resolve_nested_structs(&mut own);
    own.truncate(n);
    Some(own)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_expressions() {
        assert!(eval_build_expr("linux && amd64", "linux", "amd64"));
        assert!(!eval_build_expr("linux && !amd64", "linux", "amd64"));
        assert!(eval_build_expr(
            "(darwin || linux) && !purego",
            "linux",
            "amd64"
        ));
        assert!(eval_build_expr("unix", "linux", "amd64"));
        assert!(!eval_build_expr("windows", "linux", "amd64"));
        assert!(eval_build_expr("go1.21", "linux", "amd64"));
    }

    #[test]
    fn file_name_constraints() {
        let built = |n: &str| file_is_built(Path::new(n), "package p\n", "linux", "amd64");
        assert!(built("a.go"));
        assert!(built("a_linux.go"));
        assert!(built("a_linux_amd64.go"));
        assert!(built("a_amd64.go"));
        assert!(!built("a_windows.go"));
        assert!(!built("a_linux_arm64.go"));
        assert!(!built("a_arm64.go"));
        // An underscore part that is not a known OS/arch is just a name.
        assert!(built("decode_state.go"));
        assert!(!file_is_built(
            Path::new("x.go"),
            "//go:build windows\n\npackage p\n",
            "linux",
            "amd64"
        ));
    }

    #[test]
    fn types_from_sibling_files_are_resolved() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("geom.go"),
            "package image\n\ntype Point struct{ X, Y int }\ntype Rectangle struct{ Min, Max Point }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("format.go"),
            "package image\n\nconst blockSize = 512\ntype block [blockSize]byte\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("other_windows.go"),
            "package image\n\ntype block [1]byte\n",
        )
        .unwrap();
        let path = dir.path().join("image.go");
        let src = "package image\n\ntype RGBA struct {\n\tPix []uint8\n\tStride int\n\tRect Rectangle\n\tb block\n}\n";
        std::fs::write(&path, src).unwrap();
        let layouts = parse_in_package(&path, src, &padlock_core::arch::X86_64_SYSV).unwrap();
        let rgba = layouts.iter().find(|l| l.name == "RGBA").unwrap();
        // 24 + 8 + 32 + 512 (the windows-only `block` is excluded).
        assert_eq!(rgba.total_size, 576);
        assert!(
            rgba.uncertain_fields.is_empty(),
            "{:?}",
            rgba.uncertain_fields
        );
        assert!(layouts.iter().all(|l| l.name != "Point"));
    }
}
