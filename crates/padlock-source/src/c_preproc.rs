// padlock-source/src/c_preproc.rs
//
// Just enough of the C preprocessor for layout: which macros are defined (in
// the branches a compiler for the target would take), and their expansion in
// declarations. Headers lean on macros for types and members — glibc's
// `__STD_TYPE __TIME_T_TYPE __time_t;` is `typedef long int __time_t;`, its
// `__SOCKADDR_COMMON (sa_);` is `sa_family_t sa_family;` — and on the
// compiler's predefined macros (`__x86_64__`, `__WORDSIZE`) to choose branches.
//
// `expand` rewrites every line that is not a directive, never adding or
// removing a newline, so line numbers stay those of the original source. The
// directives themselves are left for the C frontend's own `#if` handling.

use std::collections::{HashMap, HashSet};

use padlock_core::arch::ArchConfig;

use crate::frontends::consteval::{self, Env};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Macro {
    /// `None` for an object-like macro.
    pub(crate) params: Option<Vec<String>>,
    pub(crate) variadic: bool,
    pub(crate) body: String,
}

pub(crate) type Macros = HashMap<String, Macro>;

/// The macros in effect while a file is processed: a shared base (predefined,
/// `-D`, included headers) plus the file's own `#define`/`#undef`s on top, so
/// the base is never copied per file.
pub(crate) struct Scope<'a> {
    base: &'a Macros,
    /// `Some` = defined by the file, `None` = `#undef`ed by it.
    own: HashMap<String, Option<Macro>>,
    /// Names brought in by a header included part-way through the file.
    imported: HashSet<String>,
}

impl<'a> Scope<'a> {
    pub(crate) fn new(base: &'a Macros) -> Self {
        Scope {
            base,
            own: HashMap::new(),
            imported: HashSet::new(),
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Macro> {
        match self.own.get(name) {
            Some(m) => m.as_ref(),
            None => self.base.get(name),
        }
    }

    fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn is_empty(&self) -> bool {
        self.base.is_empty() && self.own.is_empty()
    }

    fn define(&mut self, name: String, m: Macro) {
        self.imported.remove(&name);
        self.own.insert(name, Some(m));
    }

    /// A macro from a header included at this point of the file.
    fn import(&mut self, name: String, m: Macro) {
        self.imported.insert(name.clone());
        self.own.insert(name, Some(m));
    }

    fn undef(&mut self, name: &str) {
        self.own.insert(name.to_string(), None);
    }

    /// The macros the file itself left defined.
    pub(crate) fn into_own(self) -> Macros {
        let imported = self.imported;
        self.own
            .into_iter()
            .filter(|(k, _)| !imported.contains(k))
            .filter_map(|(k, m)| m.map(|m| (k, m)))
            .collect()
    }
}

/// Names the C frontend understands itself (or that must stay visible to it):
/// never expanded.
const KEEP: &[&str] = &[
    "alignas",
    "_Alignas",
    "alignof",
    "_Alignof",
    "bool",
    "true",
    "false",
    "static_assert",
    "_Static_assert",
    "GUARDED_BY",
    "PT_GUARDED_BY",
    "_Atomic",
    "offsetof",
    "NULL",
];

fn object(body: &str) -> Macro {
    Macro {
        params: None,
        variadic: false,
        body: body.to_string(),
    }
}

/// The macros a GCC-compatible compiler predefines for the target.
pub(crate) fn predefined(arch: &ArchConfig) -> Macros {
    let ps = arch.pointer_size;
    let long = match arch.name {
        "x86_64-msvc" => 4,
        "avr" => 4,
        _ => ps,
    };
    let mut m: Macros = HashMap::new();
    let mut def = |k: &str, v: String| {
        m.insert(k.to_string(), object(&v));
    };
    def("__STDC__", "1".into());
    def("__STDC_VERSION__", "201710L".into());
    def("__STDC_HOSTED__", "1".into());
    def("__GNUC__", "13".into());
    def("__GNUC_MINOR__", "2".into());
    def("__GNUC_PATCHLEVEL__", "0".into());
    def("__CHAR_BIT__", "8".into());
    def("__SIZEOF_POINTER__", ps.to_string());
    def("__SIZEOF_LONG__", long.to_string());
    def("__SIZEOF_INT__", "4".into());
    def("__SIZEOF_SHORT__", "2".into());
    def("__SIZEOF_LONG_LONG__", "8".into());
    def("__SIZEOF_FLOAT__", "4".into());
    def("__SIZEOF_DOUBLE__", "8".into());
    def("__SIZEOF_SIZE_T__", ps.to_string());
    def("__ORDER_LITTLE_ENDIAN__", "1234".into());
    def("__ORDER_BIG_ENDIAN__", "4321".into());
    def("__BYTE_ORDER__", "__ORDER_LITTLE_ENDIAN__".into());
    let size_type = if ps == 8 && long == 8 {
        "long unsigned int"
    } else if ps == 8 {
        "long long unsigned int"
    } else {
        "unsigned int"
    };
    def("__SIZE_TYPE__", size_type.into());
    def("__PTRDIFF_TYPE__", size_type.replace("unsigned ", ""));
    def("__INTPTR_TYPE__", size_type.replace("unsigned ", ""));
    def("__UINTPTR_TYPE__", size_type.into());
    def("__WCHAR_TYPE__", "int".into());
    def("__INT8_TYPE__", "signed char".into());
    def("__UINT8_TYPE__", "unsigned char".into());
    def("__INT16_TYPE__", "short int".into());
    def("__UINT16_TYPE__", "short unsigned int".into());
    def("__INT32_TYPE__", "int".into());
    def("__UINT32_TYPE__", "unsigned int".into());
    let (i64t, u64t) = if long == 8 {
        ("long int", "long unsigned int")
    } else {
        ("long long int", "long long unsigned int")
    };
    def("__INT64_TYPE__", i64t.into());
    def("__UINT64_TYPE__", u64t.into());
    let mut flags: Vec<&str> = Vec::new();
    match arch.name {
        "x86_64" => {
            for k in [
                "__x86_64__",
                "__x86_64",
                "__amd64__",
                "__amd64",
                "__LP64__",
                "_LP64",
            ] {
                flags.push(k);
            }
        }
        "x86_64-msvc" => {
            for k in ["_WIN32", "_WIN64", "__x86_64__", "_M_X64"] {
                flags.push(k);
            }
        }
        "x86" => {
            for k in ["__i386__", "__i386", "i386", "__ILP32__"] {
                flags.push(k);
            }
        }
        "aarch64" | "aarch64-apple" => {
            for k in ["__aarch64__", "__LP64__", "_LP64"] {
                flags.push(k);
            }
        }
        "riscv64" => {
            // Linux on RISC-V uses the lp64d ABI.
            for k in ["__riscv", "__LP64__", "_LP64", "__riscv_float_abi_double"] {
                flags.push(k);
            }
        }
        "cortex_m" | "cortex_m4" => {
            for k in ["__arm__", "__thumb__", "__ARM_EABI__"] {
                flags.push(k);
            }
        }
        "avr" => {
            for k in ["__AVR__", "__AVR_ARCH__"] {
                flags.push(k);
            }
        }
        "wasm32" => {
            for k in ["__wasm__", "__wasm32__", "__ILP32__"] {
                flags.push(k);
            }
        }
        _ => {}
    }
    match arch.name {
        "aarch64-apple" => {
            for k in ["__APPLE__", "__MACH__"] {
                flags.push(k);
            }
        }
        "x86_64" | "x86" | "aarch64" | "riscv64" => {
            for k in [
                "__linux__",
                "__linux",
                "__unix__",
                "__unix",
                "__ELF__",
                "__gnu_linux__",
            ] {
                flags.push(k);
            }
        }
        _ => {}
    }
    for k in flags {
        m.insert(k.to_string(), object("1"));
    }
    if arch.name == "riscv64" {
        m.insert("__riscv_xlen".into(), object("64"));
        m.insert("__riscv_flen".into(), object("64"));
    }
    m
}

/// Physical lines with `\`-continuations joined; `swallowed` counts the extra
/// newlines folded into each logical line.
fn logical_lines(source: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut swallowed = 0;
    for line in source.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(head) = line.strip_suffix('\\') {
            cur.push_str(head);
            cur.push(' ');
            swallowed += 1;
            continue;
        }
        cur.push_str(line);
        out.push((std::mem::take(&mut cur), swallowed));
        swallowed = 0;
    }
    if !cur.is_empty() {
        out.push((cur, swallowed));
    }
    out
}

/// A directive's `(name, rest)`: `#  define X 1` → `("define", "X 1")`.
fn directive(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    Some((&rest[..end], rest[end..].trim()))
}

/// Strip `/* … */` and `// …` comments from one logical line (block comments
/// spanning lines are handled by the caller's `in_comment` state).
fn strip_comments(line: &str, in_comment: &mut bool) -> String {
    let mut out = String::new();
    let b = line.as_bytes();
    let mut i = 0;
    let mut in_str: Option<u8> = None;
    while i < b.len() {
        if *in_comment {
            if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                *in_comment = false;
                i += 2;
                out.push(' ');
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(q) = in_str {
            out.push(b[i] as char);
            if b[i] == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if b[i] == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b[i] {
            b'"' | b'\'' => {
                in_str = Some(b[i]);
                out.push(b[i] as char);
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                *in_comment = true;
                i += 2;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => break,
            _ => {
                // Keep multi-byte UTF-8 intact.
                let ch = line[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

fn parse_define(rest: &str) -> Option<(String, Macro)> {
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() {
        return None;
    }
    let after = &rest[end..];
    if let Some(p) = after.strip_prefix('(') {
        let close = p.find(')')?;
        let mut params = Vec::new();
        let mut variadic = false;
        for a in p[..close].split(',') {
            let a = a.trim();
            if a == "..." {
                variadic = true;
            } else if let Some(n) = a.strip_suffix("...") {
                variadic = true;
                params.push(n.trim().to_string());
            } else if !a.is_empty() {
                params.push(a.to_string());
            }
        }
        let body = p[close + 1..].trim().to_string();
        return Some((
            name.to_string(),
            Macro {
                params: Some(params),
                variadic,
                body,
            },
        ));
    }
    Some((name.to_string(), object(after.trim())))
}

/// Evaluate an `#if`/`#elif` expression as the preprocessor would (undefined
/// identifiers are 0; anything unparsable is false).
pub(crate) fn eval_condition(expr: &str, macros: &Scope<'_>) -> bool {
    // `defined X` / `defined(X)` first, so the name is not expanded.
    let mut text = String::new();
    let mut rest = expr;
    while let Some(pos) = find_word(rest, "defined") {
        text.push_str(&rest[..pos]);
        let after = rest[pos + "defined".len()..].trim_start();
        let (name, consumed) = if let Some(p) = after.strip_prefix('(') {
            match p.find(')') {
                Some(c) => (p[..c].trim(), rest.len() - p.len() + c + 1),
                None => ("", rest.len()),
            }
        } else {
            let end = after
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], rest.len() - after.len() + end)
        };
        text.push_str(if macros.contains(name) { " 1 " } else { " 0 " });
        rest = &rest[consumed..];
    }
    text.push_str(rest);
    // `__has_include(<x>)` and friends: assume absent.
    for f in [
        "__has_include_next",
        "__has_include",
        "__has_attribute",
        "__has_builtin",
        "__has_feature",
        "__has_extension",
        "__has_cpp_attribute",
    ] {
        while let Some(pos) = find_word(&text, f) {
            let after = &text[pos + f.len()..];
            let close = after.find(')').map(|c| c + 1).unwrap_or(after.len());
            text.replace_range(pos..pos + f.len() + close, " 0 ");
        }
    }
    let expanded = expand_text(&text, macros, &mut HashSet::new(), 0);
    let lookup = |_: &str| Some("0".to_string());
    let sizeof = |_: &str| None;
    let env = Env {
        lookup: &lookup,
        sizeof: &sizeof,
    };
    consteval::eval(expanded.trim(), &env, 0).is_some_and(|v| v != 0)
}

fn find_word(text: &str, word: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = text[from..].find(word) {
        let at = from + i;
        let before = text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = text[at + word.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !before && !after {
            return Some(at);
        }
        from = at + word.len();
    }
    None
}

/// Whether every identifier in a condition is known: defined, or reserved
/// (`__x`, `_X` — the compiler's and C library's, whose definitions are
/// modelled). A condition on a project macro (`CONFIG_X`, `NDEBUG`) is not:
/// the build may define it, so both branches stay for the C frontend to flag.
fn decidable(cond: &str, macros: &Scope<'_>) -> bool {
    // An include guard (`#ifndef X_H` / `#define X_H`), recorded as `guard:`.
    if cond.starts_with("guard:") {
        return true;
    }
    let mut words = cond
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty() && !w.starts_with(|c: char| c.is_ascii_digit()));
    words.all(|w| {
        w == "defined"
            || macros.contains(w)
            || w.starts_with("__")
            || (w.starts_with('_') && w[1..].starts_with(|c: char| c.is_ascii_uppercase()))
    })
}

struct Group {
    /// The current branch is taken.
    active: bool,
    /// Some branch of the group was taken.
    taken: bool,
    /// The enclosing branch is taken.
    parent_active: bool,
    /// The enclosing lines are visible in the output.
    parent_visible: bool,
    /// Every condition of the group is decidable (see [`decidable`]): its
    /// directives and untaken branches are blanked, as a compiler would.
    decided: bool,
}

/// Walk `source` like the preprocessor: add (`#define`) and remove (`#undef`)
/// macros in the branches that are taken, and expand every line with the
/// macros in effect at that line, keeping the line structure. Conditional
/// groups decided for the target are resolved (their directives and untaken
/// branches become blank lines); the others are kept for the C frontend.
/// Returns the expanded text; the file's own macros stay in `macros`.
/// The text with every conditional resolved (undefined names are 0) and macros
/// expanded, includes not followed.
pub(crate) fn process_text(source: &str, macros: &mut Scope<'_>) -> String {
    process_impl(source, macros, &mut |_, _| IncludeEffect::default(), true)
}

#[cfg(test)]
pub(crate) fn process(source: &str, macros: &mut Scope<'_>) -> String {
    process_with(source, macros, &mut |_, _| IncludeEffect::default())
}

/// What a followed `#include` contributes to the including file.
#[derive(Default)]
pub(crate) struct IncludeEffect {
    /// The macros the included header defines.
    pub(crate) macros: Vec<(String, Macro)>,
    /// A header that only switches struct packing (`pshpack1.h`: one
    /// `#pragma pack`) is spliced in as that pragma, since the pragma is
    /// scoped to the include's position in the including file.
    pub(crate) pragma: Option<String>,
}

/// Called at a followed `#include` with the output so far and the directive's
/// text.
pub(crate) type IncludeHook<'a> = dyn FnMut(&str, &str) -> IncludeEffect + 'a;

/// [`process`], calling `on_include(output_so_far, include_text)` at every
/// `#include` the preprocessor would follow (in a taken branch); the macros it
/// returns (those the included header defines) are in effect for the lines
/// after it.
pub(crate) fn process_with(
    source: &str,
    macros: &mut Scope<'_>,
    on_include: &mut IncludeHook<'_>,
) -> String {
    process_impl(source, macros, on_include, false)
}

/// [`process_with`]; `decide_all` resolves every conditional group as a
/// compiler would (undefined names are 0), not only those whose names are known.
fn process_impl(
    source: &str,
    macros: &mut Scope<'_>,
    on_include: &mut IncludeHook<'_>,
    decide_all: bool,
) -> String {
    let lines = logical_lines(source);
    let mut in_comment = false;
    let codes: Vec<(String, bool, bool)> = lines
        .iter()
        .map(|(line, _)| {
            let was = in_comment;
            let code = strip_comments(line, &mut in_comment);
            (code, was, in_comment)
        })
        .collect();
    // Every condition of each group, keyed by the line of its `#if`.
    let mut group_conds: HashMap<usize, Vec<String>> = HashMap::new();
    let mut open: Vec<usize> = Vec::new();
    for (i, (code, _, _)) in codes.iter().enumerate() {
        match directive(code) {
            Some(("if", rest)) => {
                open.push(i);
                group_conds.insert(i, vec![rest.to_string()]);
            }
            Some((kind @ ("ifdef" | "ifndef"), rest)) => {
                open.push(i);
                let name = first_word(rest).to_string();
                // `#ifndef X` whose first directive is `#define X`: a guard.
                let guard = kind == "ifndef"
                    && codes[i + 1..]
                        .iter()
                        .find_map(|(c, _, _)| directive(c))
                        .is_some_and(|(d, r)| d == "define" && first_word(r) == name);
                group_conds.insert(i, vec![if guard { format!("guard:{name}") } else { name }]);
            }
            Some(("elif" | "elifdef" | "elifndef", rest)) => {
                if let Some(g) = open.last().and_then(|g| group_conds.get_mut(g)) {
                    g.push(rest.to_string());
                }
            }
            Some(("endif", _)) => {
                open.pop();
            }
            _ => {}
        }
    }

    let mut out = String::with_capacity(source.len() + source.len() / 8);
    let mut stack: Vec<Group> = Vec::new();
    let active = |stack: &[Group]| stack.last().is_none_or(|g| g.active);
    let visible = |stack: &[Group]| stack.iter().all(|g| !g.decided || g.active);
    let n = lines.len();
    for (idx, ((line, swallowed), (code, was_in_comment, ends_in_comment))) in
        lines.iter().zip(&codes).enumerate()
    {
        match directive(code) {
            Some((name, rest)) => {
                let mut replacement: Option<String> = None;
                let shown = match name {
                    "if" | "ifdef" | "ifndef" => {
                        let parent_active = active(&stack);
                        let parent_visible = visible(&stack);
                        let decided = decide_all
                            || group_conds
                                .get(&idx)
                                .is_some_and(|c| c.iter().all(|c| decidable(c, macros)));
                        let cond = parent_active
                            && match name {
                                "ifdef" => macros.contains(first_word(rest)),
                                "ifndef" => !macros.contains(first_word(rest)),
                                _ => eval_condition(rest, macros),
                            };
                        stack.push(Group {
                            active: cond,
                            taken: cond,
                            parent_active,
                            parent_visible,
                            decided,
                        });
                        parent_visible && !decided
                    }
                    "elif" | "elifdef" | "elifndef" | "else" | "endif" => {
                        let shown = stack.last().is_some_and(|g| g.parent_visible && !g.decided);
                        if name == "endif" {
                            stack.pop();
                        } else if let Some(g) = stack.last_mut() {
                            let cond = g.parent_active
                                && !g.taken
                                && match name {
                                    "else" => true,
                                    "elifdef" => macros.contains(first_word(rest)),
                                    "elifndef" => !macros.contains(first_word(rest)),
                                    _ => eval_condition(rest, macros),
                                };
                            g.active = cond;
                            g.taken |= cond;
                        }
                        // An unmatched directive is left as written.
                        shown || stack.is_empty() && name != "endif"
                    }
                    _ => {
                        if active(&stack) {
                            match name {
                                "define" => {
                                    if let Some((n, m)) = parse_define(rest) {
                                        macros.define(n, m);
                                    }
                                }
                                "undef" => macros.undef(first_word(rest)),
                                "include" => {
                                    let effect = on_include(&out, rest);
                                    for (n, m) in effect.macros {
                                        macros.import(n, m);
                                    }
                                    replacement = effect.pragma;
                                }
                                _ => {}
                            }
                        }
                        visible(&stack)
                    }
                };
                if shown {
                    match replacement {
                        // (Only when the include is visible: `shown` covers it.)
                        Some(pragma) => out.push_str(&pragma),
                        None => out.push_str(line),
                    }
                }
            }
            None if visible(&stack) => {
                let expanded = expand_text(code, macros, &mut HashSet::new(), 0);
                // Keep the original line (with its comments) only when nothing
                // changed and no comment starts, ends or continues on it.
                if expanded == *code && !was_in_comment && !ends_in_comment && !line.contains("/*")
                {
                    out.push_str(line);
                } else {
                    out.push_str(&expanded);
                }
            }
            None => {}
        }
        for _ in 0..*swallowed {
            out.push('\n');
        }
        if idx + 1 < n {
            out.push('\n');
        }
    }
    out
}

fn first_word(s: &str) -> &str {
    let s = s.trim();
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    &s[..end]
}

/// Split `(a, (b, c), d)` arguments starting just after the `(`; returns the
/// arguments and the byte length consumed including the closing `)`.
fn split_args(text: &str) -> Option<(Vec<String>, usize)> {
    let mut depth = 0;
    let mut args = Vec::new();
    let mut cur = String::new();
    for (i, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(ch);
            }
            ')' if depth == 0 => {
                args.push(cur.trim().to_string());
                return Some((args, i + 1));
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => args.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(ch),
        }
    }
    None
}

/// Substitute parameters (with `#` stringizing and `##` pasting) in a body.
fn substitute(m: &Macro, args: &[String]) -> String {
    let params = m.params.as_deref().unwrap_or(&[]);
    let arg_of = |name: &str| -> Option<String> {
        if let Some(i) = params.iter().position(|p| p == name) {
            return Some(args.get(i).cloned().unwrap_or_default());
        }
        if m.variadic && name == "__VA_ARGS__" {
            return Some(
                args.get(params.len()..)
                    .map(|a| a.join(", "))
                    .unwrap_or_default(),
            );
        }
        None
    };
    let mut out = String::new();
    let body = &m.body;
    let mut chars = body.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch.is_alphabetic() || ch == '_' {
            let mut end = i + ch.len_utf8();
            while let Some(&(j, c)) = chars.peek() {
                if c.is_alphanumeric() || c == '_' {
                    end = j + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let word = &body[i..end];
            out.push_str(&arg_of(word).unwrap_or_else(|| word.to_string()));
        } else if ch == '#' {
            if chars.peek().is_some_and(|&(_, c)| c == '#') {
                chars.next();
                // Token pasting: drop surrounding whitespace.
                while out.ends_with(' ') {
                    out.pop();
                }
                while chars.peek().is_some_and(|&(_, c)| c == ' ') {
                    chars.next();
                }
            } else {
                // Stringizing `#param`.
                while chars.peek().is_some_and(|&(_, c)| c == ' ') {
                    chars.next();
                }
                let start = chars.peek().map(|&(j, _)| j).unwrap_or(body.len());
                let mut end = start;
                while let Some(&(j, c)) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' {
                        end = j + c.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                let a = arg_of(&body[start..end]).unwrap_or_default();
                out.push('"');
                out.push_str(&a.replace('"', "\\\""));
                out.push('"');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Expand macros in one stretch of text (no newlines added).
fn expand_text(text: &str, macros: &Scope<'_>, active: &mut HashSet<String>, depth: u8) -> String {
    if depth > 32 || macros.is_empty() {
        return text.to_string();
    }
    let mut out = String::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            // Copy literals untouched.
            let start = i;
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push_str(&text[start..i]);
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            let Some(m) = macros
                .get(word)
                .filter(|_| !active.contains(word) && !KEEP.contains(&word))
            else {
                out.push_str(word);
                continue;
            };
            let replacement = match &m.params {
                None => m.body.clone(),
                Some(_) => {
                    let after = &text[i..];
                    let trimmed = after.trim_start();
                    let Some(inner) = trimmed.strip_prefix('(') else {
                        // A function-like macro name without arguments is left alone.
                        out.push_str(word);
                        continue;
                    };
                    let Some((args, used)) = split_args(inner) else {
                        out.push_str(word);
                        continue;
                    };
                    i += after.len() - trimmed.len() + 1 + used;
                    let args: Vec<String> = args
                        .iter()
                        .map(|a| expand_text(a, macros, active, depth + 1))
                        .collect();
                    substitute(m, &args)
                }
            };
            active.insert(word.to_string());
            let expanded = expand_text(&replacement, macros, active, depth + 1);
            active.remove(word);
            out.push(' ');
            out.push_str(&expanded);
            out.push(' ');
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn glibc_style_type_macros_expand() {
        let base = predefined(&X86_64_SYSV);
        let mut m = Scope::new(&base);
        let hdr = "#if defined __x86_64__ && !defined __ILP32__\n# define __WORDSIZE 64\n#else\n# define __WORDSIZE 32\n#endif\n#if __WORDSIZE == 32\n# define __STD_TYPE __extension__ typedef\n#elif __WORDSIZE == 64\n# define __STD_TYPE typedef\n#endif\n#define __SLONGWORD_TYPE long int\n#define __TIME_T_TYPE __SLONGWORD_TYPE\n";
        process(hdr, &mut m);
        assert_eq!(m.get("__WORDSIZE").unwrap().body, "64");
        let src = "__STD_TYPE __TIME_T_TYPE __time_t;\n";
        let out = process(src, &mut m);
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>().join(" "),
            "typedef long int __time_t;"
        );
    }

    #[test]
    fn function_like_macros_and_pasting() {
        let base = Macros::new();
        let mut m = Scope::new(&base);
        process(
            "#define __SOCKADDR_COMMON(sa_prefix) sa_family_t sa_prefix##family\n#define ElfW(type) Elf64_##type\n",
            &mut m,
        );
        let out = process(
            "struct s {\n  __SOCKADDR_COMMON (sa_);\n  ElfW(Addr) a; /* c */\n};\n",
            &mut m,
        );
        assert!(out.contains("sa_family_t sa_family"), "{out}");
        assert!(out.contains("Elf64_Addr"), "{out}");
        assert_eq!(out.lines().count(), 4);
    }

    #[test]
    fn line_count_survives_continuations() {
        let base = predefined(&X86_64_SYSV);
        let mut m = Scope::new(&base);
        let src = "#define A \\\n  1\nstruct s { int x; };\n";
        assert_eq!(
            process(src, &mut m).matches('\n').count(),
            src.matches('\n').count()
        );
    }

    #[test]
    fn expansion_uses_the_macros_in_effect_at_each_line() {
        let base = Macros::new();
        let mut m = Scope::new(&base);
        let out = process("#define F(x) x\nint F(a);\n#undef F\nint F(b);\n", &mut m);
        assert!(out.contains("int  a "), "{out}");
        assert!(out.contains("int F(b);"), "{out}");
    }

    #[test]
    fn decided_branches_are_resolved_and_project_ones_kept() {
        let base = predefined(&X86_64_SYSV);
        let mut m = Scope::new(&base);
        let src = "#ifdef __cplusplus\nextern \"C\" {\n#endif\nstruct s {\n#ifdef CONFIG_X\n  int x;\n#endif\n#if __SIZEOF_POINTER__ == 8\n  long w;\n#else\n  int w;\n#endif\n};\n#ifdef __cplusplus\n}\n#endif\n";
        let out = process(src, &mut m);
        assert!(!out.contains("extern"), "{out}");
        assert!(out.contains("#ifdef CONFIG_X"), "{out}");
        assert!(out.contains("long w;") && !out.contains("int w;"), "{out}");
        assert_eq!(out.matches('\n').count(), src.matches('\n').count());
    }
}
