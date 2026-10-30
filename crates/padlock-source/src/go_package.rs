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
    // `type Kind int16` lines of generated files too large to parse.
    let mut big_aliases: Vec<(String, String)> = Vec::new();
    for (path, _, len) in &signature {
        if path.to_string_lossy().ends_with("_test.go") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        // Generated tables (the CLI's own size limit) hold no struct types a
        // hand-written file could need, and dominate parse time. Their
        // one-line type declarations (an enum's `type Kind int16`) still count.
        if *len > 500_000 {
            if file_is_built(path, &text, goos, goarch)
                && go::package_clause(&text).as_deref() == Some(name)
            {
                big_aliases.extend(go::simple_type_lines(&text));
            }
            continue;
        }
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
    for (alias, target) in big_aliases {
        ctx.add_alias(alias, target);
    }
    ctx.pkg_dir = signature
        .first()
        .and_then(|(p, _, _)| p.parent())
        .map(Path::to_path_buf);
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
    // Both `Pair` and the full `Pair[int, string]`: a generic instantiation is a
    // layout of its own, named with its arguments.
    let named: HashSet<&str> = own
        .iter()
        .flat_map(|l| &l.fields)
        .flat_map(|f| match &f.ty {
            padlock_core::ir::TypeInfo::Primitive { name, .. }
            | padlock_core::ir::TypeInfo::Opaque { name, .. } => {
                vec![name.split('[').next().unwrap_or(name).trim(), name.trim()]
            }
            _ => Vec::new(),
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

// ── types of other packages ────────────────────────────────────────────────────

/// A type declared in an imported package, sized from that package's source.
pub(crate) struct ForeignType {
    pub size: usize,
    pub align: usize,
    /// The package's own layout of it has uncertain fields.
    pub uncertain: bool,
}

/// `(module path, directory)` of the modules around `dir`: the nearest `go.mod`
/// and every module a `go.work` above it lists. The standard library's own
/// tree (`module std` / `module cmd`) resolves bare import paths from its root.
fn module_roots(dir: &Path) -> Arc<Vec<(String, PathBuf)>> {
    type Roots = Mutex<HashMap<PathBuf, Arc<Vec<(String, PathBuf)>>>>;
    static ROOTS: LazyLock<Roots> = LazyLock::new(Default::default);
    if let Some(r) = ROOTS.lock().ok().and_then(|m| m.get(dir).cloned()) {
        return r;
    }
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    let module_of = |d: &Path| -> Option<String> {
        let text = std::fs::read_to_string(d.join("go.mod")).ok()?;
        text.lines().find_map(|l| {
            l.trim()
                .strip_prefix("module")
                .filter(|r| r.starts_with([' ', '\t']))
                .map(|r| r.trim().trim_matches('"').to_string())
        })
    };
    let mut seen_mod = false;
    for anc in dir.ancestors() {
        if !seen_mod && let Some(m) = module_of(anc) {
            roots.push((m, anc.to_path_buf()));
            seen_mod = true;
        }
        if let Ok(work) = std::fs::read_to_string(anc.join("go.work")) {
            let mut in_block = false;
            for line in work.lines() {
                let l = line.split("//").next().unwrap_or("").trim();
                let entry = if in_block {
                    if l.starts_with(')') {
                        in_block = false;
                        continue;
                    }
                    Some(l)
                } else if let Some(rest) = l.strip_prefix("use") {
                    let rest = rest.trim();
                    if rest.starts_with('(') {
                        in_block = true;
                        None
                    } else {
                        Some(rest)
                    }
                } else {
                    None
                };
                if let Some(e) = entry.map(|e| e.trim_matches('"')).filter(|e| !e.is_empty()) {
                    let d = anc.join(e);
                    if let Some(m) = module_of(&d)
                        && !roots.iter().any(|(_, r)| *r == d)
                    {
                        roots.push((m, d));
                    }
                }
            }
        }
    }
    let roots = Arc::new(roots);
    if let Ok(mut m) = ROOTS.lock() {
        m.insert(dir.to_path_buf(), roots.clone());
    }
    roots
}

/// `$GOROOT/src`, asked of the `go` tool once.
fn goroot_src() -> Option<&'static Path> {
    static SRC: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        let out = std::process::Command::new("go")
            .args(["env", "GOROOT"])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let root = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let src = root.join("src");
        src.is_dir().then_some(src)
    });
    SRC.as_deref()
}

/// Directory of the package imported as `import_path` from a package in
/// `from`: a package of the surrounding module(s), else the standard library.
pub(crate) fn resolve_import(from: &Path, import_path: &str) -> Option<PathBuf> {
    type Cache = Mutex<HashMap<(PathBuf, String), Option<PathBuf>>>;
    static RESOLVED: LazyLock<Cache> = LazyLock::new(Default::default);
    let key = (from.to_path_buf(), import_path.to_string());
    if let Some(r) = RESOLVED.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return r;
    }
    let found = (|| {
        let roots = module_roots(from);
        // The longest matching module path wins.
        let mut best: Option<(&str, &PathBuf)> = None;
        for (m, d) in roots.iter() {
            let hit = import_path == m
                || import_path
                    .strip_prefix(m.as_str())
                    .is_some_and(|r| r.starts_with('/'));
            if hit && best.is_none_or(|(bm, _)| m.len() > bm.len()) {
                best = Some((m, d));
            }
        }
        if let Some((m, d)) = best {
            let dir = d.join(import_path[m.len()..].trim_start_matches('/'));
            return dir.is_dir().then_some(dir);
        }
        // The standard library's own tree resolves its imports from its root.
        if let Some((_, d)) = roots.iter().find(|(m, _)| m == "std" || m == "cmd") {
            let dir = d.join(import_path);
            if dir.is_dir() {
                return Some(dir);
            }
        }
        // A bare first element (no dot) is the standard library.
        if !import_path.split('/').next().unwrap_or("").contains('.') {
            let dir = goroot_src()?.join(import_path);
            return dir.is_dir().then_some(dir);
        }
        None
    })();
    if let Ok(mut m) = RESOLVED.lock() {
        m.insert(key, found.clone());
    }
    found
}

/// The `package` name of the files of `dir` that build for `arch`.
fn package_name_in(dir: &Path, arch: &'static ArchConfig) -> Option<String> {
    type Names = Mutex<HashMap<(PathBuf, &'static str), Option<String>>>;
    static NAMES: LazyLock<Names> = LazyLock::new(Default::default);
    let key = (dir.to_path_buf(), arch.name);
    if let Some(n) = NAMES.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return n;
    }
    let (goos, goarch) = go_target(arch);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "go") && !p.to_string_lossy().ends_with("_test.go")
        })
        .collect();
    files.sort();
    let name = files.iter().find_map(|p| {
        let text = std::fs::read_to_string(p).ok()?;
        if !file_is_built(p, &text, goos, goarch) {
            return None;
        }
        go::package_clause(&text).filter(|n| n != "documentation")
    });
    if let Ok(mut m) = NAMES.lock() {
        m.insert(key, name.clone());
    }
    name
}

thread_local! {
    /// How many foreign packages are being built on this thread (a guard
    /// against pathological import chains).
    static FOREIGN_DEPTH: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Size of type `name` of the package imported as `import_path` from a package
/// in `from`; `None` when the package or the type cannot be found (a generic
/// type, a type declared in a function, a package outside the module).
pub(crate) fn foreign_type(
    from: &Path,
    import_path: &str,
    name: &str,
    arch: &'static ArchConfig,
) -> Option<ForeignType> {
    let dir = resolve_import(from, import_path)?;
    let pkg_name = package_name_in(&dir, arch)?;
    if FOREIGN_DEPTH.with(|d| d.get()) >= 24 {
        return None;
    }
    FOREIGN_DEPTH.with(|d| d.set(d.get() + 1));
    let package = package_for(&dir, &pkg_name, arch, true);
    FOREIGN_DEPTH.with(|d| d.set(d.get() - 1));
    let package = package?;
    let ps = arch.pointer_size;
    let mut name = name.to_string();
    // `type A B` chains within the package, ending at a struct, an interface
    // or a type the package's own sizer can evaluate.
    for _ in 0..8 {
        if let Some(l) = package.layouts.iter().find(|l| l.name == name) {
            return Some(ForeignType {
                size: l.total_size,
                align: l.align,
                uncertain: !l.uncertain_fields.is_empty(),
            });
        }
        if package.ctx.interfaces_contains(&name) {
            return Some(ForeignType {
                size: ps * 2,
                align: ps,
                uncertain: false,
            });
        }
        let target = package.ctx.alias_target(&name)?;
        let declared_here = |n: &str| {
            package.layouts.iter().any(|l| l.name == n)
                || package.ctx.interfaces_contains(n)
                || package.ctx.alias_target(n).is_some()
        };
        if target != name && declared_here(&target) {
            name = target;
            continue;
        }
        let (size, align, uncertain) = go::size_in_ctx(&target, &package.ctx);
        return Some(ForeignType {
            size,
            align,
            uncertain,
        });
    }
    None
}

/// Import paths named by the `import` declarations of Go source text.
fn import_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let l = line.trim();
        if in_block {
            if l.starts_with(')') {
                in_block = false;
            } else if let Some(p) = quoted(l) {
                out.push(p);
            }
        } else if let Some(rest) = l.strip_prefix("import") {
            let rest = rest.trim_start();
            if rest.starts_with('(') {
                in_block = !rest.contains(')');
                if let Some(p) = quoted(rest) {
                    out.push(p);
                }
            } else if let Some(p) = quoted(rest) {
                out.push(p);
            }
        } else if l.starts_with("func ") || l.starts_with("type ") {
            break;
        }
    }
    out
}

fn quoted(s: &str) -> Option<String> {
    let start = s.find('"')? + 1;
    let len = s[start..].find('"')?;
    Some(s[start..start + len].to_string())
}

/// The `.go` files whose content the layouts of `path` depend on: its package's
/// files and, transitively, those of the packages of the same module it
/// imports (the standard library is covered by the toolchain version).
pub(crate) fn dependencies(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent().map(Path::to_path_buf) else {
        return Vec::new();
    };
    let dir = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    };
    let goroot = goroot_src();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut queue = vec![dir.clone()];
    let mut files: Vec<PathBuf> = Vec::new();
    while let Some(d) = queue.pop() {
        if !seen.insert(d.clone()) || seen.len() > 400 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "go") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                for imp in import_paths(&text) {
                    if let Some(target) = resolve_import(&d, &imp)
                        && !goroot.is_some_and(|g| target.starts_with(g))
                    {
                        queue.push(target);
                    }
                }
            }
            if p != path {
                files.push(p);
            }
        }
    }
    files
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

    #[test]
    fn generic_declared_in_one_file_is_instantiated_for_uses_in_another() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("box.go"),
            "package p\n\ntype Box[T any] struct { s string; ok bool; v T }\n",
        )
        .unwrap();
        let path = dir.path().join("use.go");
        let src = "package p\n\ntype Local struct{ a, b, c int64 }\ntype Holder struct { e Box[Local]; f Box[string] }\n";
        std::fs::write(&path, src).unwrap();
        let layouts = parse_in_package(&path, src, &padlock_core::arch::X86_64_SYSV).unwrap();
        let holder = layouts.iter().find(|l| l.name == "Holder").unwrap();
        // Box[Local] = 16 + 8 + 24 = 48; Box[string] = 16 + 8 + 16 = 40.
        assert_eq!(holder.total_size, 88);
        assert!(holder.uncertain_fields.is_empty());
    }

    #[test]
    fn types_of_imported_packages_of_the_module_are_sized() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(root.join("go.mod"), "module example.com/m\n\ngo 1.24\n").unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(
            root.join("a/a.go"),
            "package a\n\ntype ID string\ntype Kind int16\ntype Pt struct{ X, Y int32 }\ntype Shape interface{ Area() float64 }\ntype Id2 ID\n",
        )
        .unwrap();
        let path = root.join("b/b.go");
        let src = "package b\n\nimport (\n\t\"example.com/m/a\"\n)\n\ntype S struct {\n\tk a.Kind\n\tid a.ID\n\tp a.Pt\n\ts a.Shape\n\tptr *a.Pt\n\tids []a.Id2\n\ti2 a.Id2\n}\n";
        std::fs::write(&path, src).unwrap();
        let layouts = parse_in_package(&path, src, &padlock_core::arch::X86_64_SYSV).unwrap();
        let s = layouts.iter().find(|l| l.name == "S").unwrap();
        // k 2 (+6) | id 16 | p 8 | s 16 | ptr 8 | ids 24 | i2 16 = 96
        assert_eq!(s.total_size, 96);
        assert!(s.uncertain_fields.is_empty(), "{:?}", s.uncertain_fields);
        // The imported package's files are dependencies for the parse cache.
        let deps = dependencies(&path);
        assert!(deps.iter().any(|d| d.ends_with("a/a.go")), "{deps:?}");
    }

    #[test]
    fn import_names_default_to_the_last_path_element() {
        use crate::frontends::go::default_package_name as n;
        assert_eq!(n("github.com/x/y"), "y");
        assert_eq!(n("github.com/x/y/v3"), "y");
        assert_eq!(n("gopkg.in/yaml.v3"), "yaml");
        assert_eq!(n("github.com/x/go-foo"), "foo");
    }

    #[test]
    fn a_package_named_like_a_std_one_is_not_given_the_std_size() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(root.join("go.mod"), "module example.com/m\n\ngo 1.24\n").unwrap();
        std::fs::create_dir_all(root.join("internal/atomic")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        // sync/atomic.Bool is 4 bytes; this one is 1.
        std::fs::write(
            root.join("internal/atomic/a.go"),
            "package atomic\n\ntype Bool struct{ u uint8 }\n",
        )
        .unwrap();
        let path = root.join("b/b.go");
        let src = "package b\n\nimport \"example.com/m/internal/atomic\"\n\ntype S struct {\n\ta atomic.Bool\n\tb bool\n}\n";
        std::fs::write(&path, src).unwrap();
        let layouts = parse_in_package(&path, src, &padlock_core::arch::X86_64_SYSV).unwrap();
        let s = layouts.iter().find(|l| l.name == "S").unwrap();
        assert_eq!(s.total_size, 2);
        assert!(s.uncertain_fields.is_empty());
    }
}
