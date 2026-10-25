// padlock-source/src/c_includes.rs
//
// A C/C++ file's types come from the headers it includes as much as from the
// file itself: `struct timespec { __time_t tv_sec; … }` needs `bits/types.h`,
// `sem_t` needs `__SIZEOF_SEM_T` from `bits/semaphore.h`. Parsing the file on
// its own leaves such fields at a guessed pointer size. Here a file is laid out
// with the typedefs, integer constants, defined macros and records of every
// header it includes (transitively, in preprocessing order, first definition
// wins), found next to the including file, in `-I` directories
// (`set_include_dirs`) and, for Linux targets, the system include paths.
//
// Each header is processed once per run (cached) in the context of its own
// includes. That is an approximation of the preprocessor — a header is not
// re-expanded per including file, function-like macros are not expanded and
// `#include`s inside inactive `#if` branches are followed too — but it turns
// most cross-header guesses into real sizes; what stays unresolved keeps its
// `uncertain` flag.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{StructLayout, TypeInfo};

use crate::SourceLanguage;
use crate::c_preproc::{self, Macros};
use crate::frontends::c_cpp::{self, IncludeContext};

static INCLUDE_DIRS: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());

/// Extra directories searched for `#include`s (like a compiler's `-I`).
pub fn set_include_dirs(dirs: Vec<PathBuf>) {
    crate::bump_settings_generation();
    if let Ok(mut d) = INCLUDE_DIRS.write() {
        *d = dirs;
    }
}

/// The configured `-I` directories, for cache fingerprints.
pub fn include_dirs() -> Vec<PathBuf> {
    INCLUDE_DIRS.read().map(|d| d.clone()).unwrap_or_default()
}

/// System include directories for a target: only when the host's headers are
/// the target's (a Linux target whose multiarch directory exists).
fn system_dirs(arch: &ArchConfig) -> Vec<PathBuf> {
    let triple = match arch.name {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        "riscv64" => "riscv64-linux-gnu",
        "x86" => "i386-linux-gnu",
        _ => return Vec::new(),
    };
    let multiarch = Path::new("/usr/include").join(triple);
    if !multiarch.is_dir() {
        return Vec::new();
    }
    let mut dirs = vec![PathBuf::from("/usr/local/include"), multiarch];
    // The compiler's own headers (stddef.h, stdarg.h): the newest gcc version.
    if let Ok(entries) = std::fs::read_dir(Path::new("/usr/lib/gcc").join(triple)) {
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("include"))
            .filter(|p| p.is_dir())
            .collect();
        versions.sort();
        if let Some(newest) = versions.pop() {
            dirs.push(newest);
        }
    }
    dirs.push(PathBuf::from("/usr/include"));
    dirs
}

fn resolve_include(name: &str, angle: bool, from_dir: &Path, arch: &ArchConfig) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if !angle {
        candidates.push(from_dir.to_path_buf());
    }
    candidates.extend(include_dirs());
    candidates.extend(system_dirs(arch));
    candidates
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// A header's own contribution, processed in the context of its includes.
struct Header {
    includes: Vec<PathBuf>,
    /// Alias-map entries the header itself declares.
    exports: HashMap<String, String>,
    /// Macros the header itself defines (in the branches taken).
    macros: Macros,
    /// Its records, resolved and with anonymous members folded in.
    layouts: Vec<StructLayout>,
    /// Types it skipped (templates with no use, …), for when it is analyzed.
    skipped: Vec<padlock_core::findings::SkippedStruct>,
}

type Key = (PathBuf, &'static str, u64, bool);

enum Slot {
    /// Being built by some thread since this instant.
    Building(Instant),
    Done(Arc<Header>),
}

static HEADERS: LazyLock<(Mutex<HashMap<Key, Slot>>, Condvar)> = LazyLock::new(Default::default);

/// How long a thread waits for a header another thread is building before
/// building it itself. Waiting without a bound could deadlock on headers that
/// include each other being built from both ends.
const WAIT_FOR_OTHER: Duration = Duration::from_millis(200);

fn language_of(path: &Path, source: &str, cpp_hint: bool) -> SourceLanguage {
    match crate::detect_language_from_source(path, source) {
        Some(SourceLanguage::Cpp) => SourceLanguage::Cpp,
        // A C++ file's headers are parsed as C++ (a `.h` may hold either).
        _ if cpp_hint => SourceLanguage::Cpp,
        _ => SourceLanguage::C,
    }
}

fn includes_of(source: &str, dir: &Path, arch: &ArchConfig) -> Vec<PathBuf> {
    c_cpp::include_directives(source)
        .into_iter()
        .filter_map(|(name, angle)| resolve_include(&name, angle, dir, arch))
        .collect()
}

/// What a file sees at its start: everything its translation unit included
/// and defined before it.
struct Context {
    include: Arc<IncludeContext>,
    /// The headers in preprocessing order (their layouts resolve the file's
    /// members).
    headers: Vec<Arc<Header>>,
}

enum Claim {
    Done(Arc<Header>),
    /// This thread now builds the header.
    Build,
    /// Not available (a failed build).
    None,
}

/// Look `key` up, waiting a little for another thread's build, or claim it.
fn claim(key: &Key) -> Claim {
    let (lock, ready) = &*HEADERS;
    let Ok(mut map) = lock.lock() else {
        return Claim::None;
    };
    loop {
        match map.get(key) {
            Some(Slot::Done(h)) => return Claim::Done(h.clone()),
            Some(Slot::Building(since)) if since.elapsed() < WAIT_FOR_OTHER => {
                let left = WAIT_FOR_OTHER.saturating_sub(since.elapsed());
                match ready.wait_timeout(map, left) {
                    Ok((m, _)) => map = m,
                    Err(_) => return Claim::None,
                }
            }
            _ => {
                map.insert(key.clone(), Slot::Building(Instant::now()));
                return Claim::Build;
            }
        }
    }
}

/// A finished header, without waiting or claiming.
fn peek(key: &Key) -> Option<Arc<Header>> {
    match HEADERS.0.lock().ok()?.get(key)? {
        Slot::Done(h) => Some(h.clone()),
        Slot::Building(_) => None,
    }
}

fn publish(key: Key, built: Option<&Arc<Header>>) {
    let (lock, ready) = &*HEADERS;
    if let Ok(mut map) = lock.lock() {
        match built {
            Some(h) => {
                map.insert(key, Slot::Done(h.clone()));
            }
            None => {
                map.remove(&key);
            }
        }
    }
    ready.notify_all();
}

/// One translation unit being preprocessed: the headers included so far, in
/// order, and what they declared. A header's content can depend on what came
/// before it (umbrella headers such as `Python.h` define macros the headers
/// they include rely on), so a header is processed in the unit that reaches
/// it first, with that unit's state at the `#include`, and the result is
/// reused by every later unit.
struct Unit {
    arch: &'static ArchConfig,
    cpp: bool,
    include: IncludeContext,
    headers: Vec<Arc<Header>>,
    seen: HashSet<PathBuf>,
}

impl Unit {
    fn new(arch: &'static ArchConfig, cpp: bool) -> Self {
        let mut include = IncludeContext {
            macros: c_preproc::predefined(arch),
            ..Default::default()
        };
        for (name, value) in crate::preprocessor_defines() {
            include.macros.insert(
                name.clone(),
                c_preproc::Macro {
                    params: None,
                    variadic: false,
                    body: value,
                },
            );
        }
        Unit {
            arch,
            cpp,
            include,
            headers: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn snapshot(&self) -> Context {
        Context {
            include: Arc::new(self.include.clone()),
            headers: self.headers.clone(),
        }
    }

    fn absorb(&mut self, h: &Arc<Header>) {
        for (k, m) in &h.macros {
            self.include.macros.insert(k.clone(), m.clone());
        }
        for (k, v) in &h.exports {
            if !k.starts_with('$') && c_cpp::is_builtin_c_type(k, self.arch) {
                continue;
            }
            self.include
                .aliases
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for l in &h.layouts {
            self.include
                .records
                .entry(l.name.clone())
                .or_insert((l.total_size, l.align));
        }
        self.headers.push(h.clone());
    }

    /// Process `#include <path>` at this point of the unit; returns the
    /// header when it was not already included.
    fn include(&mut self, path: &Path) -> Option<Arc<Header>> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        // Include guards: a header is included once per unit (and an include
        // cycle stops here).
        if !self.seen.insert(path.clone()) {
            return None;
        }
        let key = (
            path.clone(),
            self.arch.name,
            crate::settings_generation(),
            self.cpp,
        );
        let h = match claim(&key) {
            Claim::Done(h) => {
                // Built in another unit: still bring in what it includes.
                for inc in &h.includes {
                    self.include(inc);
                }
                h
            }
            Claim::Build => {
                let built = self.build(&path).map(Arc::new);
                publish(key, built.as_ref());
                built?
            }
            Claim::None => return None,
        };
        self.absorb(&h);
        Some(h)
    }

    fn build(&mut self, path: &Path) -> Option<Header> {
        let source = std::fs::read_to_string(path).ok()?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let includes = includes_of(&source, dir, self.arch);
        for inc in &includes {
            self.include(inc);
        }
        let ctx = self.snapshot();
        let lang = language_of(path, &source, self.cpp);
        let mut scope = c_preproc::Scope::new(&ctx.include.macros);
        let expanded = c_preproc::process(&source, &mut scope);
        let own_macros = scope.into_own();
        // The header's own skip notes belong to whoever analyzes it directly.
        let outer = crate::take_skipped();
        c_cpp::set_include_context(Some(ctx.include.clone()));
        c_cpp::capture_aliases(true);
        let parsed = match lang {
            SourceLanguage::Cpp => c_cpp::parse_cpp(&expanded, self.arch),
            _ => c_cpp::parse_c(&expanded, self.arch),
        };
        c_cpp::capture_aliases(false);
        c_cpp::set_include_context(None);
        let mut exports = c_cpp::take_exported_aliases();
        // Only what the header itself added: the rest is already in the unit.
        exports.retain(|k, v| ctx.include.aliases.get(k) != Some(v));
        let skipped = crate::take_skipped();
        crate::restore_skipped(outer);
        let mut layouts = resolve_with(parsed.ok()?, &ctx);
        crate::hide_anonymous_aggregates(&mut layouts);
        for l in &layouts {
            exports.insert(format!("$rec:{}", l.name), String::new());
        }
        Some(Header {
            includes,
            exports,
            macros: own_macros,
            layouts,
            skipped,
        })
    }
}

/// Resolve `own` against the context's already-final layouts (only the ones
/// it names are needed), returning `own`.
fn resolve_with(mut own: Vec<StructLayout>, ctx: &Context) -> Vec<StructLayout> {
    let named: HashSet<String> = own
        .iter()
        .flat_map(|l| &l.fields)
        .filter_map(|f| match &f.ty {
            TypeInfo::Primitive { name, .. } | TypeInfo::Opaque { name, .. } => {
                Some(crate::split_c_array_suffix(name).0.to_string())
            }
            _ => None,
        })
        .collect();
    let mut taken = HashSet::new();
    let extra: Vec<StructLayout> = ctx
        .headers
        .iter()
        .flat_map(|h| &h.layouts)
        .filter(|l| named.contains(&l.name) && taken.insert(l.name.clone()))
        .cloned()
        .collect();
    let n = own.len();
    own.extend(extra);
    crate::resolve_nested_structs(&mut own);
    own.truncate(n);
    own
}

/// The headers `path` includes directly, resolved and canonical.
pub(crate) fn direct_includes(path: &Path, arch: &ArchConfig) -> Vec<PathBuf> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    includes_of(&source, dir, arch)
        .into_iter()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect()
}

/// The headers `path` includes, transitively (for cache invalidation).
pub(crate) fn dependencies(path: &Path, arch: &'static ArchConfig, cpp: bool) -> Vec<PathBuf> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut unit = Unit::new(arch, cpp);
    if let Ok(own) = std::fs::canonicalize(path) {
        unit.seen.insert(own);
    }
    for inc in includes_of(&source, dir, arch) {
        unit.include(&inc);
    }
    if let Ok(own) = std::fs::canonicalize(path) {
        unit.seen.remove(&own);
    }
    unit.seen.into_iter().collect()
}

/// Lay out a C/C++ file with what its `#include`s declare and its macros
/// expanded.
pub(crate) fn parse_with_includes(
    path: &Path,
    source: &str,
    lang: &SourceLanguage,
    arch: &'static ArchConfig,
) -> Option<anyhow::Result<Vec<StructLayout>>> {
    let cpp = matches!(lang, SourceLanguage::Cpp);
    let mut unit = Unit::new(arch, cpp);
    // A file on disk is processed exactly like a header (and reuses that work
    // when another file already included it).
    if std::fs::read_to_string(path).is_ok_and(|disk| disk == source) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let key = (canonical, arch.name, crate::settings_generation(), cpp);
        // A header already processed in a unit of the other language (a C++
        // file including a C header, or a `.h` read as C++): the including
        // unit's language is the one its compiler uses, so reuse that.
        let other = (key.0.clone(), key.1, key.2, !cpp);
        let h = match peek(&key).or_else(|| peek(&other)) {
            Some(h) => Some(h),
            None => match claim(&key) {
                Claim::Done(h) => Some(h),
                Claim::Build => {
                    unit.seen.insert(key.0.clone());
                    let built = unit.build(path).map(Arc::new);
                    publish(key, built.as_ref());
                    built
                }
                Claim::None => None,
            },
        };
        if let Some(h) = h {
            crate::restore_skipped(h.skipped.clone());
            return Some(Ok(h.layouts.clone()));
        }
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    for inc in includes_of(source, dir, arch) {
        unit.include(&inc);
    }
    let ctx = unit.snapshot();
    let mut scope = c_preproc::Scope::new(&ctx.include.macros);
    let expanded = c_preproc::process(source, &mut scope);
    c_cpp::set_include_context(Some(ctx.include.clone()));
    let parsed = match lang {
        SourceLanguage::Cpp => c_cpp::parse_cpp(&expanded, arch),
        _ => c_cpp::parse_c(&expanded, arch),
    };
    c_cpp::set_include_context(None);
    Some(parsed.map(|own| resolve_with(own, &ctx)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn typedefs_constants_and_records_come_from_included_headers() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("types.h"),
            "typedef long my_time_t;\n#define NSLOTS 4\nstruct ts { my_time_t s; long ns; };\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("mid.h"),
            "#include \"types.h\"\n#define WIDE 1\n",
        )
        .unwrap();
        let path = dir.path().join("main.c");
        let src = "#include \"mid.h\"\nstruct S {\n  char c;\n  my_time_t t;\n  struct ts when;\n  int slots[NSLOTS];\n#ifdef WIDE\n  long extra;\n#endif\n};\n";
        let layouts = parse_with_includes(&path, src, &SourceLanguage::C, &X86_64_SYSV)
            .unwrap()
            .unwrap();
        let s = layouts.iter().find(|l| l.name == "S").unwrap();
        // c(1)+7, t 8, when 16, slots 16, extra 8 = 56.
        assert_eq!(s.total_size, 56);
        assert!(s.uncertain_fields.is_empty(), "{:?}", s.uncertain_fields);
        // Records of the headers are context only, not part of this file.
        assert!(layouts.iter().all(|l| l.name != "ts"));
    }

    #[test]
    fn include_cycles_terminate() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.h"), "#include \"b.h\"\ntypedef int A;\n").unwrap();
        std::fs::write(
            dir.path().join("b.h"),
            "#include \"a.h\"\ntypedef long B;\n",
        )
        .unwrap();
        let path = dir.path().join("m.c");
        let src = "#include \"a.h\"\nstruct M { A a; B b; };\n";
        let layouts = parse_with_includes(&path, src, &SourceLanguage::C, &X86_64_SYSV)
            .unwrap()
            .unwrap();
        assert_eq!(layouts[0].total_size, 16);
    }
}
