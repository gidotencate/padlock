// padlock-source/src/rust_modules.rs
//
// A crate's module tree, enough to tell which file a type name refers to when
// several files declare the same name. A file's module path follows its
// location under `src/` (`a/b.rs`, `a/b/mod.rs` → `a::b`; `lib.rs`/`main.rs`
// → the root; `src/bin/x.rs` is a root of its own) or a `#[path = "…"] mod x;`
// declaration. `use` items (including glob imports and `pub use` re-exports) and
// qualified type paths (`crate::a::Config`, `super::Config`, `a::Config`) are
// resolved against it. Renamed imports (`use a::Config as C`), extern crates
// and macro-generated modules are not: those names stay unknown.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use syn::visit::Visit;

/// What one file says about names: what it declares, imports and spells out.
#[derive(Default)]
struct FileInfo {
    decls: HashSet<String>,
    /// `use a::b::Name;` → (`Name`, [`a`, `b`, `Name`]).
    bindings: Vec<(String, Vec<String>)>,
    /// `use a::b::*;` → [`a`, `b`].
    globs: Vec<Vec<String>>,
    /// Path-qualified type uses, `a::Name<T>` → [`a`, `Name`].
    qualified: Vec<Vec<String>>,
    /// `#[path = "x.rs"] mod m;` → (`m`, `x.rs`).
    path_mods: Vec<(String, String)>,
}

struct Collector<'a>(&'a mut FileInfo);

fn flatten_use(tree: &syn::UseTree, prefix: &mut Vec<String>, info: &mut FileInfo) {
    match tree {
        syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            flatten_use(&p.tree, prefix, info);
            prefix.pop();
        }
        syn::UseTree::Name(n) => {
            let ident = n.ident.to_string();
            if ident == "self" {
                if let Some(last) = prefix.last() {
                    info.bindings.push((last.clone(), prefix.clone()));
                }
            } else {
                let mut path = prefix.clone();
                path.push(ident.clone());
                info.bindings.push((ident, path));
            }
        }
        // `use a::Name as Other`: the file spells the type `Other`.
        syn::UseTree::Rename(_) => {}
        syn::UseTree::Glob(_) => info.globs.push(prefix.clone()),
        syn::UseTree::Group(g) => {
            for t in &g.items {
                flatten_use(t, prefix, info);
            }
        }
    }
}

impl<'ast> Visit<'ast> for Collector<'_> {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        flatten_use(&node.tree, &mut Vec::new(), self.0);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if node.content.is_none() {
            for a in &node.attrs {
                if a.path().is_ident("path")
                    && let syn::Meta::NameValue(nv) = &a.meta
                    && let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) = &nv.value
                {
                    self.0.path_mods.push((node.ident.to_string(), s.value()));
                }
            }
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        if node.qself.is_none() && node.path.segments.len() >= 2 {
            self.0.qualified.push(
                node.path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect(),
            );
        }
        syn::visit::visit_type_path(self, node);
    }
}

fn info_of(file: &syn::File) -> FileInfo {
    let mut info = FileInfo {
        decls: crate::frontends::rust::declared_names(file),
        ..Default::default()
    };
    Collector(&mut info).visit_file(file);
    info
}

/// Lexically normalize `.` and `..` (the file may not exist).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Module path of `file` from its place under `src` (`None` outside it). The
/// first element is `""` for the library/main root and `bin:x` for a binary.
fn module_path_by_location(file: &Path, src: &Path) -> Option<Vec<String>> {
    let rel = file.strip_prefix(src).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let last = parts.pop()?;
    let stem = last.strip_suffix(".rs")?.to_string();
    let mut root = vec![String::new()];
    if parts.first().is_some_and(|p| p == "bin") {
        parts.remove(0);
        if parts.is_empty() {
            // src/bin/x.rs
            return Some(vec![format!("bin:{stem}")]);
        }
        root = vec![format!("bin:{}", parts.remove(0))];
    }
    let mut path = root;
    path.extend(parts);
    if !(matches!(stem.as_str(), "mod" | "lib" | "main") && path.len() == 1 || stem == "mod") {
        path.push(stem);
    }
    Some(path)
}

struct Graph<'a> {
    infos: &'a [FileInfo],
    /// Module path of each file (`None`: not part of this crate's tree).
    paths: Vec<Option<Vec<String>>>,
    by_path: HashMap<Vec<String>, Vec<usize>>,
    modules: HashSet<Vec<String>>,
}

impl Graph<'_> {
    fn module_exists(&self, p: &[String]) -> bool {
        self.modules.contains(p)
    }

    /// The module a path's leading segments name, seen from module `from`.
    fn resolve_module(&self, from: &[String], segs: &[String]) -> Option<Vec<String>> {
        let root = vec![from.first()?.clone()];
        let (mut cur, rest): (Vec<String>, &[String]) = match segs.first()?.as_str() {
            "crate" => (root, &segs[1..]),
            "self" => (from.to_vec(), &segs[1..]),
            "super" => {
                let mut cur = from.to_vec();
                let mut i = 0;
                while segs.get(i).is_some_and(|s| s == "super") {
                    if cur.len() <= 1 {
                        return None;
                    }
                    cur.pop();
                    i += 1;
                }
                (cur, &segs[i..])
            }
            first => {
                let mut child = from.to_vec();
                child.push(first.to_string());
                if self.module_exists(&child) {
                    (child, &segs[1..])
                } else {
                    // 2015-style paths start at the crate root.
                    let mut top = root.clone();
                    top.push(first.to_string());
                    if self.module_exists(&top) {
                        (top, &segs[1..])
                    } else {
                        return None;
                    }
                }
            }
        };
        for s in rest {
            cur.push(s.clone());
            if !self.module_exists(&cur) {
                // An inline `mod` inside a file: its items belong to the file.
                cur.pop();
                break;
            }
        }
        Some(cur)
    }

    /// Files declaring `name` in module `module`, following `use` re-exports.
    fn find_decl(&self, module: &[String], name: &str, depth: u8) -> Vec<usize> {
        if depth > 6 {
            return Vec::new();
        }
        let Some(files) = self.by_path.get(module) else {
            return Vec::new();
        };
        let direct: Vec<usize> = files
            .iter()
            .copied()
            .filter(|&i| self.infos[i].decls.contains(name))
            .collect();
        if !direct.is_empty() {
            return direct;
        }
        let mut found = Vec::new();
        for &i in files {
            let info = &self.infos[i];
            for (binding, path) in &info.bindings {
                if binding == name
                    && let Some((_, mods)) = path.split_last()
                    && let Some(m) = self.resolve_module(module, mods)
                    && m != module
                {
                    found.extend(self.find_decl(&m, name, depth + 1));
                }
            }
            for glob in &info.globs {
                if let Some(m) = self.resolve_module(module, glob)
                    && m != module
                {
                    found.extend(self.find_decl(&m, name, depth + 1));
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found
    }
}

/// For each file, the file declaring each of `ambiguous` names it uses but does
/// not declare itself — when `use` items and qualified paths agree on one.
/// `files[..local_count]` belong to the crate rooted at `src`; the rest are
/// dependencies and are not resolved.
pub(crate) fn resolve_bindings(
    files: &[(String, Option<&syn::File>)],
    local_count: usize,
    src: &Path,
    ambiguous: &HashSet<String>,
) -> Vec<HashMap<String, usize>> {
    let infos: Vec<FileInfo> = files
        .iter()
        .map(|(_, f)| f.map(info_of).unwrap_or_default())
        .collect();
    let canon = |p: &str| normalize(Path::new(p));
    let mut paths: Vec<Option<Vec<String>>> = files
        .iter()
        .enumerate()
        .map(|(i, (p, _))| {
            if i >= local_count {
                return None;
            }
            module_path_by_location(&canon(p), src)
        })
        .collect();
    // `#[path = "x.rs"] mod m;` puts x.rs at `<declaring module>::m`.
    let index: HashMap<PathBuf, usize> = files
        .iter()
        .enumerate()
        .take(local_count)
        .map(|(i, (p, _))| (canon(p), i))
        .collect();
    for (i, info) in infos.iter().enumerate().take(local_count) {
        let (Some(here), Some(dir)) = (
            paths[i].clone(),
            Path::new(&files[i].0).parent().map(Path::to_path_buf),
        ) else {
            continue;
        };
        for (name, rel) in &info.path_mods {
            if let Some(&t) = index.get(&normalize(&dir.join(rel))) {
                let mut p = here.clone();
                p.push(name.clone());
                paths[t] = Some(p);
            }
        }
    }
    let mut by_path: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
    let mut modules: HashSet<Vec<String>> = HashSet::new();
    for (i, p) in paths.iter().enumerate() {
        if let Some(p) = p {
            by_path.entry(p.clone()).or_default().push(i);
            for n in 1..=p.len() {
                modules.insert(p[..n].to_vec());
            }
        }
    }
    let graph = Graph {
        infos: &infos,
        paths,
        by_path,
        modules,
    };
    let mut out = vec![HashMap::new(); files.len()];
    for (i, info) in infos.iter().enumerate().take(local_count) {
        let Some(here) = graph.paths[i].clone() else {
            continue;
        };
        for name in ambiguous.iter().filter(|n| !info.decls.contains(*n)) {
            let mut targets: Vec<usize> = Vec::new();
            for (binding, path) in &info.bindings {
                if binding == name
                    && let Some((_, mods)) = path.split_last()
                    && let Some(m) = graph.resolve_module(&here, mods)
                {
                    targets.extend(graph.find_decl(&m, name, 0));
                }
            }
            for q in &info.qualified {
                if q.last() == Some(name)
                    && let Some((_, mods)) = q.split_last()
                    && let Some(m) = graph.resolve_module(&here, mods)
                {
                    targets.extend(graph.find_decl(&m, name, 0));
                }
            }
            if targets.is_empty() {
                for glob in &info.globs {
                    if let Some(m) = graph.resolve_module(&here, glob) {
                        targets.extend(graph.find_decl(&m, name, 0));
                    }
                }
            }
            targets.sort_unstable();
            targets.dedup();
            if let [only] = targets[..] {
                out[i].insert(name.clone(), only);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &[(&str, &str)], ambiguous: &[&str]) -> (Vec<String>, Vec<HashMap<String, usize>>) {
        let parsed: Vec<syn::File> = src
            .iter()
            .map(|(_, t)| syn::parse_str(t).unwrap())
            .collect();
        let files: Vec<(String, Option<&syn::File>)> = src
            .iter()
            .zip(&parsed)
            .map(|((p, _), f)| (format!("/c/src/{p}"), Some(f)))
            .collect();
        let amb: HashSet<String> = ambiguous.iter().map(|s| s.to_string()).collect();
        let out = resolve_bindings(&files, files.len(), Path::new("/c/src"), &amb);
        (files.into_iter().map(|(p, _)| p).collect(), out)
    }

    fn target(names: &[String], b: &HashMap<String, usize>, n: &str) -> Option<String> {
        b.get(n)
            .map(|&i| names[i].trim_start_matches("/c/src/").to_string())
    }

    #[test]
    fn qualified_path_and_use_pick_the_module() {
        let (names, out) = run(
            &[
                ("lib.rs", "mod a;\nmod b;\nmod c;\nmod d;\n"),
                ("a.rs", "pub struct Config { x: u8 }\n"),
                ("b/mod.rs", "pub struct Config { x: u16 }\n"),
                ("c.rs", "use crate::b::Config;\nstruct U { c: Config }\n"),
                ("d.rs", "struct V { c: super::a::Config }\n"),
            ],
            &["Config"],
        );
        assert_eq!(
            target(&names, &out[3], "Config").as_deref(),
            Some("b/mod.rs")
        );
        assert_eq!(target(&names, &out[4], "Config").as_deref(), Some("a.rs"));
    }

    #[test]
    fn both_candidates_in_one_file_stay_unknown() {
        let (_, out) = run(
            &[
                ("lib.rs", "mod a;\nmod b;\nmod c;\n"),
                ("a.rs", "pub struct Config { x: u8 }\n"),
                ("b.rs", "pub struct Config { x: u16 }\n"),
                (
                    "c.rs",
                    "struct U { p: crate::a::Config, q: crate::b::Config }\n",
                ),
            ],
            &["Config"],
        );
        assert!(out[3].is_empty());
    }

    #[test]
    fn reexports_and_globs_are_followed() {
        let (names, out) = run(
            &[
                ("lib.rs", "mod a;\nmod inner;\nmod user;\n"),
                ("inner.rs", "pub struct Config { x: u8 }\n"),
                ("a.rs", "pub use crate::inner::Config;\n"),
                ("user.rs", "use crate::a::*;\nstruct U { c: Config }\n"),
            ],
            &["Config", "Other"],
        );
        assert_eq!(
            target(&names, &out[3], "Config").as_deref(),
            Some("inner.rs")
        );
    }

    #[test]
    fn path_attribute_relocates_a_module() {
        let (names, out) = run(
            &[
                ("lib.rs", "#[path = \"elsewhere/x.rs\"] mod a;\nmod user;\n"),
                ("elsewhere/x.rs", "pub struct Config { x: u8 }\n"),
                ("user.rs", "struct U { c: crate::a::Config }\n"),
            ],
            &["Config"],
        );
        assert_eq!(
            target(&names, &out[2], "Config").as_deref(),
            Some("elsewhere/x.rs")
        );
    }
}
