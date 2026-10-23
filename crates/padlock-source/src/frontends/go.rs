// padlock-source/src/frontends/go.rs
//
// Extracts struct layouts from Go source using tree-sitter-go.
// Sizes use Go's platform-native alignment rules (same as C on the target arch).

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Parser};

// One parser per rayon thread — avoids re-allocating and re-validating the
// grammar on every file.  Parser::parse with old_tree=None always does a full
// fresh parse, so no reset is needed between files.
thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("Go grammar is always valid");
        p
    });
}

// ── type resolution ───────────────────────────────────────────────────────────

fn go_type_size_align(ty: &str, arch: &'static ArchConfig) -> (usize, usize) {
    match ty.trim() {
        "bool" => (1, 1),
        "int8" | "uint8" | "byte" => (1, 1),
        "int16" | "uint16" => (2, 2),
        "int32" | "uint32" | "rune" | "float32" => (4, 4),
        "int64" | "uint64" | "float64" => (8, 8),
        // complex64 is two float32 (align 4); complex128 is two float64 (align 8).
        "complex64" => (8, 4),
        "complex128" => (16, 8),
        "int" | "uint" => (arch.pointer_size, arch.pointer_size),
        "uintptr" => (arch.pointer_size, arch.pointer_size),
        "string" => (arch.pointer_size * 2, arch.pointer_size), // ptr + len
        ty if ty.starts_with("[]") => (arch.pointer_size * 3, arch.pointer_size), // ptr+len+cap
        ty if ty.starts_with("map[") || ty.starts_with("chan ") => {
            (arch.pointer_size, arch.pointer_size)
        }
        ty if ty.starts_with('*') => (arch.pointer_size, arch.pointer_size),
        // Interface types: two-word fat pointer (type pointer + data pointer).
        // `error` and `any` are the two universally-known interface names; inline
        // anonymous interface bodies (`interface{ Method() }`) are caught by the
        // `starts_with("interface")` arm.
        //
        // Locally-declared named interfaces (e.g. `type Reader interface { … }`) are
        // resolved to 16B by `parse_struct_type` using the phase-1 interface name set
        // collected by `collect_go_interface_names` — they do not reach this function.
        //
        // Qualified names from external packages (e.g. `io.Reader`, `driver.Connector`)
        // fall through to the `_` arm (pointer_size) and are flagged as `uncertain_fields`
        // by `parse_struct_type` so the output layer can warn the user.
        "error" | "any" => (arch.pointer_size * 2, arch.pointer_size),
        ty if ty.starts_with("interface") => (arch.pointer_size * 2, arch.pointer_size),
        // ── Well-known stdlib concrete types ─────────────────────────────────
        // These are qualified names that would otherwise fall to pointer-size +
        // uncertain_fields.  Sizes are verified against Go 1.19+ on amd64/arm64
        // and are stable across Go versions (Go 1 compatibility guarantee covers
        // struct size).  `is_known_external_go_type` must stay in sync with this
        // list so these types are not also added to uncertain_fields.
        //
        // sync.Mutex: { state int32; sema uint32 } = 8B
        "sync.Mutex" => (8, 4),
        // sync.RWMutex: { w Mutex; writerSem, readerSem uint32; readerCount,
        //   readerWait atomic.Int32 } = 24B, align 4 (all members are 4-byte aligned).
        "sync.RWMutex" => (24, 4),
        // sync.Once: { done uint32; m Mutex } = 12B
        "sync.Once" => (12, 4),
        // atomic.Bool / atomic.Int32 / atomic.Uint32: { _ noCopy; v uint32 } = 4B
        "atomic.Bool" | "atomic.Int32" | "atomic.Uint32" => (4, 4),
        // atomic.Int64 / atomic.Uint64: { _ noCopy; _ align64; v int64 } = 8B, align 8
        "atomic.Int64" | "atomic.Uint64" => (8, 8),
        // atomic.Uintptr: { _ noCopy; _ align64; v uintptr } = arch word size, align word
        "atomic.Uintptr" => (arch.pointer_size, arch.pointer_size),
        // atomic.Value: { v any } — `any` is two words (type ptr + data ptr)
        "atomic.Value" => (arch.pointer_size * 2, arch.pointer_size),
        // sync.WaitGroup: { noCopy (0B); state atomic.Uint64 (8B, align 8);
        //   sema uint32 (4B) } = 12B of fields, padded to 16B by its 8-byte alignment.
        "sync.WaitGroup" => (16, 8),
        // sync.Pool: { noCopy; local unsafe.Pointer; localSize uintptr;
        //   victim unsafe.Pointer; victimSize uintptr; New func() } = 5 words.
        "sync.Pool" => (arch.pointer_size * 5, arch.pointer_size),
        // sync.Cond: { noCopy; L Locker (2 words); notify notifyList (4 words:
        //   wait, notify uint32, lock uintptr, head, tail pointers); checker copyChecker (1 word) }
        "sync.Cond" => (arch.pointer_size * 7, arch.pointer_size),
        // atomic.Pointer[T]: { _ [0]*T; _ noCopy; v unsafe.Pointer } — one pointer.
        "atomic.Pointer" => (arch.pointer_size, arch.pointer_size),
        // Plain integer-backed stdlib types.
        "time.Duration" | "time.Month" | "time.Weekday" => (8, 8),
        "os.FileMode" | "fs.FileMode" => (4, 4),
        // []byte-backed stdlib types.
        "net.IP" | "json.RawMessage" | "net.HardwareAddr" => {
            (arch.pointer_size * 3, arch.pointer_size)
        }
        // Widely used stdlib interfaces: two words (type/itab pointer + data pointer).
        "io.Reader"
        | "io.Writer"
        | "io.Closer"
        | "io.Seeker"
        | "io.ReadWriter"
        | "io.ReadCloser"
        | "io.WriteCloser"
        | "io.ReadWriteCloser"
        | "io.ReaderAt"
        | "io.WriterAt"
        | "io.ByteReader"
        | "io.ByteWriter"
        | "io.RuneReader"
        | "io.StringWriter"
        | "fmt.Stringer"
        | "sync.Locker"
        | "context.Context"
        | "net.Conn"
        | "net.Listener"
        | "net.Addr"
        | "http.Handler"
        | "http.ResponseWriter"
        | "http.RoundTripper"
        | "sort.Interface"
        | "reflect.Type"
        | "driver.Connector"
        | "driver.Conn"
        | "driver.Driver"
        | "driver.Stmt"
        | "driver.Tx"
        | "driver.Rows"
        | "driver.Result"
        | "slog.Handler" => (arch.pointer_size * 2, arch.pointer_size),
        // sync.Map (Go 1.19+): { mu Mutex(8); read atomic.Pointer(8);
        //   dirty map(8); misses int(8) } = 4 words. Stable layout since 1.19.
        "sync.Map" => (arch.pointer_size * 4, arch.pointer_size),
        // time.Time: { wall uint64; ext int64; loc *Location } = 24B, align 8.
        // Stable across all Go versions (Go 1 compatibility guarantee).
        "time.Time" => (24, 8),
        // strings.Builder: { addr *Builder(1 word); buf []byte(3 words) } = 4 words.
        "strings.Builder" => (arch.pointer_size * 4, arch.pointer_size),
        // bytes.Buffer: { buf []byte(3 words); off int(1 word); lastRead int8(1B+pad) }
        //   = 5 words on 64-bit (40B), 5 words on 32-bit (20B).
        "bytes.Buffer" => (arch.pointer_size * 5, arch.pointer_size),
        _ => (arch.pointer_size, arch.pointer_size),
    }
}

/// Returns `true` for qualified external-package type names whose sizes are
/// known and hardcoded in `go_type_size_align`.  These must NOT be added to
/// `uncertain_fields` because their sizes are accurate, not approximated.
fn is_known_external_go_type(ty: &str) -> bool {
    matches!(
        ty,
        "sync.Mutex"
            | "sync.RWMutex"
            | "sync.Once"
            | "sync.WaitGroup"
            | "atomic.Bool"
            | "atomic.Int32"
            | "atomic.Uint32"
            | "atomic.Int64"
            | "atomic.Uint64"
            | "atomic.Uintptr"
            | "atomic.Value"
            | "atomic.Pointer"
            | "sync.Pool"
            | "sync.Cond"
            | "time.Duration"
            | "time.Month"
            | "time.Weekday"
            | "os.FileMode"
            | "fs.FileMode"
            | "net.IP"
            | "json.RawMessage"
            | "net.HardwareAddr"
            | "io.Reader"
            | "io.Writer"
            | "io.Closer"
            | "io.Seeker"
            | "io.ReadWriter"
            | "io.ReadCloser"
            | "io.WriteCloser"
            | "io.ReadWriteCloser"
            | "io.ReaderAt"
            | "io.WriterAt"
            | "io.ByteReader"
            | "io.ByteWriter"
            | "io.RuneReader"
            | "io.StringWriter"
            | "fmt.Stringer"
            | "sync.Locker"
            | "context.Context"
            | "net.Conn"
            | "net.Listener"
            | "net.Addr"
            | "http.Handler"
            | "http.ResponseWriter"
            | "http.RoundTripper"
            | "sort.Interface"
            | "reflect.Type"
            | "driver.Connector"
            | "driver.Conn"
            | "driver.Driver"
            | "driver.Stmt"
            | "driver.Tx"
            | "driver.Rows"
            | "driver.Result"
            | "slog.Handler"
            | "time.Time"
            | "sync.Map"
            | "strings.Builder"
            | "bytes.Buffer"
    )
}

// ── phase-1: declaration pre-scan ─────────────────────────────────────────────

/// What a single walk over the file learns about its own declarations, so field
/// types can be sized without a type checker.
#[derive(Clone)]
struct GoCtx {
    arch: &'static ArchConfig,
    /// `type X interface { … }` — a two-word fat pointer.
    interfaces: HashSet<String>,
    /// `type X struct { … }` — sized by the nested-struct resolver in `lib.rs`.
    structs: HashSet<String>,
    /// `type X int16`, `type X = Y`, `type X [4]int32`: name → underlying type text.
    aliases: HashMap<String, String>,
    /// `const N = 8` (and `iota` groups): name → defining expression text.
    consts: HashMap<String, String>,
    /// Generic structs (`type Pair[K comparable, V any] struct {…}`) → parameter names.
    generics: HashMap<String, Vec<String>>,
    /// Concrete uses `Name[args]` anywhere in the file.
    uses: Vec<(String, Vec<String>)>,
}

/// Result of sizing one field type.
struct Sized {
    size: usize,
    align: usize,
    /// Size is a best guess (unresolvable external type or array length).
    uncertain: bool,
    /// Array of a local struct: `(StructName, total element count)`. The element
    /// size is unknown here; the nested-struct resolver scales it by name.
    named_elem: Option<(String, usize)>,
}

fn collect_go_declarations(source: &str, root: Node<'_>, arch: &'static ArchConfig) -> GoCtx {
    let mut ctx = GoCtx {
        arch,
        interfaces: HashSet::new(),
        structs: HashSet::new(),
        aliases: HashMap::new(),
        consts: HashMap::new(),
        generics: HashMap::new(),
        uses: Vec::new(),
    };
    let mut all_params: Vec<String> = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
        match node.kind() {
            "type_spec" | "type_alias" => {
                let (Some(name), Some(ty)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("type"),
                ) else {
                    continue;
                };
                if let Some(tp) = node.child_by_field_name("type_parameters") {
                    let mut params = Vec::new();
                    for i in 0..tp.named_child_count() {
                        let Some(decl) = tp.named_child(i as u32) else {
                            continue;
                        };
                        let mut cursor = decl.walk();
                        for n in decl.children_by_field_name("name", &mut cursor) {
                            params.push(source[n.byte_range()].to_string());
                        }
                    }
                    all_params.extend(params.iter().cloned());
                    if ty.kind() == "struct_type" {
                        ctx.generics
                            .insert(source[name.byte_range()].to_string(), params);
                    }
                    continue;
                }
                let name = source[name.byte_range()].to_string();
                match ty.kind() {
                    "interface_type" => {
                        ctx.interfaces.insert(name);
                    }
                    "struct_type" => {
                        ctx.structs.insert(name);
                    }
                    _ => {
                        ctx.aliases
                            .insert(name, source[ty.byte_range()].trim().to_string());
                    }
                }
            }
            "const_declaration" => collect_consts(source, node, &mut ctx.consts),
            "generic_type" => {
                if let (Some(t), Some(args)) = (
                    node.child_by_field_name("type"),
                    node.child_by_field_name("type_arguments"),
                ) {
                    let args: Vec<String> = (0..args.named_child_count())
                        .filter_map(|i| args.named_child(i as u32))
                        .map(|a| super::c_cpp::normalize_type_text(&source[a.byte_range()]))
                        .collect();
                    let entry = (source[t.byte_range()].to_string(), args);
                    if !ctx.uses.contains(&entry) {
                        ctx.uses.push(entry);
                    }
                }
            }
            _ => {}
        }
    }
    // A use whose arguments mention a type parameter is not concrete.
    ctx.uses.retain(|(_, args)| {
        !args.iter().any(|a| {
            a.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|w| all_params.iter().any(|p| p == w))
        })
    });
    ctx
}

/// Concrete uses of generic types (`Pair[int, string]`) in a Go source text.
pub fn generic_uses(source: &str) -> Vec<(String, Vec<String>)> {
    PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .map(|t| {
            collect_go_declarations(source, t.root_node(), &padlock_core::arch::X86_64_SYSV).uses
        })
        .unwrap_or_default()
}

/// Record the constants of one `const (…)` / `const x = …` declaration,
/// expanding `iota` and Go's implicit repetition of the previous expression.
fn collect_consts(source: &str, decl: Node<'_>, consts: &mut HashMap<String, String>) {
    let mut iota = 0usize;
    let mut last_values: Vec<String> = Vec::new();
    for i in 0..decl.named_child_count() {
        let Some(spec) = decl.named_child(i as u32) else {
            continue;
        };
        if spec.kind() != "const_spec" {
            continue;
        }
        let mut names = Vec::new();
        let mut cursor = spec.walk();
        for n in spec.children_by_field_name("name", &mut cursor) {
            names.push(source[n.byte_range()].to_string());
        }
        if let Some(list) = spec.child_by_field_name("value") {
            last_values = (0..list.named_child_count())
                .filter_map(|j| list.named_child(j as u32))
                .map(|e| source[e.byte_range()].trim().to_string())
                .collect();
        }
        for (name, value) in names.iter().zip(last_values.iter()) {
            let expanded = replace_word(value, "iota", &iota.to_string());
            consts.insert(name.clone(), expanded);
        }
        iota += 1;
    }
}

/// Replace whole-word occurrences of `word` in `text`.
fn replace_word(text: &str, word: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(word) {
        let before_ok = !rest[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = &rest[pos + word.len()..];
        let after_ok = !after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..pos]);
        out.push_str(if before_ok && after_ok { with } else { word });
        rest = after;
    }
    out.push_str(rest);
    out
}

// ── field sizing ──────────────────────────────────────────────────────────────

/// Split `[N]T` into (`N`, `T`). `[]T` (slice) and non-arrays give `None`.
fn split_go_array(ty: &str) -> Option<(&str, &str)> {
    let rest = ty.strip_prefix('[')?;
    if rest.starts_with(']') {
        return None;
    }
    let mut depth = 1;
    for (i, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&rest[..i], rest[i + 1..].trim()));
                }
            }
            _ => {}
        }
    }
    None
}

fn eval_go_const(expr: &str, ctx: &GoCtx, depth: u8) -> Option<i64> {
    let lookup = |id: &str| ctx.consts.get(id).cloned();
    let sizeof = |operand: &str| {
        // `unsafe.Sizeof(atomic.Int64{})`: size the composite literal's type.
        let ty = operand.trim().strip_suffix("{}")?;
        i64::try_from(go_field_size(ty, ctx, depth + 1).size).ok()
    };
    super::consteval::eval(
        expr,
        &super::consteval::Env {
            lookup: &lookup,
            sizeof: &sizeof,
        },
        0,
    )
}

/// Size and alignment of a Go field type, resolving local aliases, constant
/// array lengths, anonymous structs and local interfaces.
fn go_field_size(ty: &str, ctx: &GoCtx, depth: u8) -> Sized {
    let ps = ctx.arch.pointer_size;
    let ty = ty.trim();
    let plain = |size, align| Sized {
        size,
        align,
        uncertain: false,
        named_elem: None,
    };
    if depth > 8 {
        return Sized {
            uncertain: true,
            ..plain(ps, ps)
        };
    }
    if let Some(inner) = ty.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
        return go_field_size(inner, ctx, depth + 1);
    }
    if let Some((len_expr, elem)) = split_go_array(ty) {
        let e = go_field_size(elem, ctx, depth + 1);
        let (count, len_known) = match eval_go_const(len_expr, ctx, depth).filter(|n| *n >= 0) {
            Some(n) => (n as usize, true),
            None => (1, false),
        };
        // An array of a local struct: the element size is only known after
        // the nested-struct resolver runs, so hand it the element name.
        let elem_name = resolve_alias_chain(elem, ctx);
        let named_elem = match e.named_elem {
            Some((name, n)) => Some((name, n.saturating_mul(count))),
            None if ctx.structs.contains(&elem_name) => Some((elem_name, count)),
            None => None,
        };
        return Sized {
            size: e.size.saturating_mul(count),
            align: e.align,
            uncertain: e.uncertain || !len_known,
            named_elem,
        };
    }
    if let Some(inner) = ty
        .strip_prefix("__anon_struct<")
        .and_then(|t| t.strip_suffix('>'))
        && let Some((sz, al)) = inner.split_once(',')
        && let (Ok(sz), Ok(al)) = (sz.trim().parse(), al.trim().parse())
    {
        return plain(sz, al);
    }
    if ctx.structs.contains(ty) {
        return plain(ps, ps);
    }
    if let Some(target) = ctx.aliases.get(ty) {
        return go_field_size(target, ctx, depth + 1);
    }
    if ctx.interfaces.contains(ty) {
        return plain(ps * 2, ps);
    }
    // `func(...)` values are a single pointer.
    if ty == "func" || ty.starts_with("func(") {
        return plain(ps, ps);
    }
    // Instantiated generic stdlib types such as `atomic.Pointer[T]`.
    let base = match ty.find('[') {
        Some(i) if i > 0 && ty.ends_with(']') => ty[..i].trim_end(),
        _ => ty,
    };
    let (size, align) = go_type_size_align(base, ctx.arch);
    // Qualified types (e.g. `driver.Connector`, `io.Reader`) come from external
    // packages. Without type information we cannot determine whether they are
    // interfaces (16B fat pointer) or structs (arbitrary size). Flag them as
    // uncertain so the output layer can warn the user.
    // Exception: types in `is_known_external_go_type` have hardcoded accurate
    // sizes in `go_type_size_align` and must not be marked uncertain.
    let uncertain =
        !base.starts_with('*') && base.contains('.') && !is_known_external_go_type(base);
    Sized {
        uncertain,
        ..plain(size, align)
    }
}

/// Follow `type A B` aliases to the final bare type name.
fn resolve_alias_chain(ty: &str, ctx: &GoCtx) -> String {
    let mut cur = ty.trim().to_string();
    for _ in 0..8 {
        match ctx.aliases.get(&cur) {
            Some(next) => cur = next.trim().to_string(),
            None => break,
        }
    }
    cur
}

// ── tree-sitter walker ────────────────────────────────────────────────────────

fn extract_structs(source: &str, root: Node<'_>, arch: &'static ArchConfig) -> Vec<StructLayout> {
    // Phase 1: learn the file's own interfaces, structs, aliases and constants.
    let ctx = collect_go_declarations(source, root, arch);

    let mut layouts = Vec::new();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }

        // type_declaration → type_spec(s) → struct_type. A grouped
        // `type ( A struct{…}; B struct{…} )` holds several specs.
        if node.kind() == "type_declaration" {
            layouts.extend(parse_type_declaration(source, node, &ctx));
        }
    }
    layouts
}

fn parse_type_declaration(source: &str, node: Node<'_>, ctx: &GoCtx) -> Vec<StructLayout> {
    let decl_start_byte = node.start_byte();
    let mut out = Vec::new();
    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        if child.kind() == "type_spec" {
            // A grouped spec's line is its own, not the `type (` line.
            let source_line = if node.named_child_count() > 1 {
                child.start_position().row as u32 + 1
            } else {
                node.start_position().row as u32 + 1
            };
            let start = if node.named_child_count() > 1 {
                child.start_byte()
            } else {
                decl_start_byte
            };
            out.extend(parse_type_spec(source, child, ctx, source_line, start));
        }
    }
    out
}

fn parse_type_spec(
    source: &str,
    node: Node<'_>,
    ctx: &GoCtx,
    source_line: u32,
    decl_start_byte: usize,
) -> Vec<StructLayout> {
    let mut name: Option<String> = None;
    let mut struct_node: Option<Node> = None;
    let mut is_generic = false;

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            "type_identifier" => name = Some(source[child.byte_range()].to_string()),
            "struct_type" => struct_node = Some(child),
            "type_parameter_list" => is_generic = true,
            _ => {}
        }
    }

    let Some(name) = name else {
        return Vec::new();
    };

    let Some(struct_node) = struct_node else {
        return Vec::new();
    };
    if is_generic {
        return instantiate_generic(source, struct_node, name, ctx, source_line, decl_start_byte);
    }
    parse_struct_type(source, struct_node, name, ctx, source_line, decl_start_byte)
        .into_iter()
        .collect()
}

/// Lay out a generic struct once per concrete instantiation (`Pair[int, string]`)
/// found in this file, configured, or found in other files; skip it when there is
/// none.
fn instantiate_generic(
    source: &str,
    struct_node: Node<'_>,
    name: String,
    ctx: &GoCtx,
    source_line: u32,
    decl_start_byte: usize,
) -> Vec<StructLayout> {
    let params = ctx.generics.get(&name).cloned().unwrap_or_default();
    let mut arg_lists = crate::extra_instantiations(&name);
    arg_lists.extend(
        ctx.uses
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, a)| a.clone()),
    );
    let mut out: Vec<StructLayout> = Vec::new();
    for args in arg_lists {
        let args: Vec<String> = args
            .iter()
            .map(|a| super::c_cpp::normalize_type_text(a))
            .collect();
        if args.len() != params.len() {
            continue;
        }
        let inst_name = format!("{name}[{}]", args.join(", "));
        if out.iter().any(|l| l.name == inst_name) {
            continue;
        }
        let mut inst = ctx.clone();
        for (p, a) in params.iter().zip(&args) {
            inst.aliases.insert(p.clone(), a.clone());
        }
        if let Some(layout) = parse_struct_type(
            source,
            struct_node,
            inst_name,
            &inst,
            source_line,
            decl_start_byte,
        ) {
            out.push(layout);
        }
    }
    if out.is_empty() {
        crate::record_skipped(
            &name,
            &format!(
                "generic struct — {} in the analyzed files; add an `[instantiate]` \
                 entry in .padlock.toml or use binary analysis",
                crate::INSTANTIATION_HINT
            ),
        );
    }
    out
}

type RawGoField = (String, String, Option<String>, u32);

/// Raw `(name, type text, guard, line)` entries for every field of a struct body.
fn raw_struct_fields(source: &str, node: Node<'_>, ctx: &GoCtx) -> Vec<RawGoField> {
    let mut raw_fields = Vec::new();
    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        if child.kind() == "field_declaration_list" {
            for j in 0..child.child_count() {
                let Some(field_node) = child.child(j) else {
                    continue;
                };
                if field_node.kind() == "field_declaration" {
                    collect_field_declarations(source, field_node, ctx, &mut raw_fields);
                }
            }
        }
    }
    raw_fields
}

/// Lay out fields in declaration order. Returns the fields, total size,
/// alignment and the names of fields whose size is a best guess.
fn simulate_go_layout(
    raw_fields: Vec<RawGoField>,
    ctx: &GoCtx,
) -> (Vec<Field>, usize, usize, Vec<String>) {
    let mut offset = 0usize;
    let mut struct_align = 1usize;
    let mut fields: Vec<Field> = Vec::new();
    let mut uncertain_fields: Vec<String> = Vec::new();

    for (fname, ty_name, guard, field_line) in raw_fields {
        let sized = go_field_size(&ty_name, ctx, 0);
        let (size, align) = (sized.size, sized.align);
        if sized.uncertain {
            uncertain_fields.push(fname.clone());
        }
        // An array of a local struct is renamed `Name[count]` so the nested-struct
        // resolver can substitute the real element size; a generic instantiation
        // `Pair[int, string]` is spelled canonically so it matches its layout.
        let ty_name = match &sized.named_elem {
            Some((elem, count)) => format!("{elem}[{count}]"),
            None if ty_name
                .find('[')
                .is_some_and(|i| i > 0 && ctx.generics.contains_key(&ty_name[..i])) =>
            {
                super::c_cpp::normalize_type_text(&ty_name)
            }
            None => ty_name,
        };

        if align > 0 {
            offset = offset.next_multiple_of(align);
        }
        struct_align = struct_align.max(align);
        let access = if let Some(g) = guard {
            AccessPattern::Concurrent {
                guard: Some(g),
                is_atomic: false,
                is_annotated: true,
            }
        } else {
            AccessPattern::Unknown
        };
        fields.push(Field {
            name: fname,
            ty: TypeInfo::Primitive {
                name: ty_name,
                size,
                align,
            },
            offset,
            size,
            align,
            source_file: None,
            source_line: Some(field_line),
            access,
        });
        offset += size;
    }
    // Go pads a struct that ends in a zero-size field so a pointer to that
    // field cannot point past the object.
    if offset > 0 && fields.last().is_some_and(|f| f.size == 0) {
        offset += 1;
    }
    if struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }
    (fields, offset, struct_align, uncertain_fields)
}

fn parse_struct_type(
    source: &str,
    node: Node<'_>,
    name: String,
    ctx: &GoCtx,
    source_line: u32,
    decl_start_byte: usize,
) -> Option<StructLayout> {
    let raw_fields = raw_struct_fields(source, node, ctx);
    if raw_fields.is_empty() {
        return None;
    }
    let (fields, total_size, align, uncertain_fields) = simulate_go_layout(raw_fields, ctx);

    Some(StructLayout {
        name,
        total_size,
        align,
        fields,
        source_file: None,
        source_line: Some(source_line),
        arch: ctx.arch,
        is_packed: false,
        is_union: false,
        is_repr_rust: false,
        suppressed_findings: super::suppress::suppressed_from_preceding_source(
            source,
            decl_start_byte,
        ),
        uncertain_fields,
        forced_align: None,
        pack_limit: None,
        cpp: None,
    })
}

/// Extract a guard name from a Go field's trailing line comment.
///
/// Recognised forms (must appear after the field type on the same line):
/// - `// padlock:guard=mu`
/// - `// guarded_by: mu`
/// - `// +checklocksprotects:mu` (gVisor-style)
pub fn extract_guard_from_go_comment(comment: &str) -> Option<String> {
    let c = comment.trim();
    // Strip leading `//` and optional whitespace
    let body = c.strip_prefix("//").map(str::trim)?;

    // padlock:guard=mu
    if let Some(rest) = body.strip_prefix("padlock:guard=") {
        let guard = rest.trim();
        if !guard.is_empty() {
            return Some(guard.to_string());
        }
    }
    // guarded_by: mu
    if let Some(rest) = body
        .strip_prefix("guarded_by:")
        .or_else(|| body.strip_prefix("guarded_by ="))
    {
        let guard = rest.trim();
        if !guard.is_empty() {
            return Some(guard.to_string());
        }
    }
    // +checklocksprotects:mu (gVisor)
    if let Some(rest) = body.strip_prefix("+checklocksprotects:") {
        let guard = rest.trim();
        if !guard.is_empty() {
            return Some(guard.to_string());
        }
    }
    None
}

/// Find the trailing line comment on the same source line as `node`.
fn trailing_comment_on_line(source: &str, node: Node<'_>) -> Option<String> {
    // The node's end byte is just past the last token on the field line.
    // Read the rest of that line from the source.
    let end = node.end_byte();
    if end >= source.len() {
        return None;
    }
    let rest = &source[end..];
    // Take only up to the next newline
    let line = rest.lines().next().unwrap_or("");
    // Look for `//` in that remainder
    line.find("//").map(|pos| line[pos..].to_string())
}

fn collect_field_declarations(
    source: &str,
    node: Node<'_>,
    ctx: &GoCtx,
    out: &mut Vec<RawGoField>,
) {
    // field_declaration: field_identifier+ type [tag] [comment]
    // OR embedded type (anonymous field): [*]TypeName [tag] [comment]
    let mut field_names: Vec<String> = Vec::new();
    let mut is_pointer_embed = false;
    let field_line = node.start_position().row as u32 + 1;

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "field_identifier" => field_names.push(source[child.byte_range()].to_string()),
                // `*Base` embeds: the `*` is a bare token, not a pointer_type node.
                "*" => is_pointer_embed = true,
                _ => {}
            }
        }
    }

    let ty_text = node.child_by_field_name("type").map(|t| {
        if t.kind() == "struct_type" {
            // `inner struct{ … }`: lay it out and encode its size/alignment.
            let raw = raw_struct_fields(source, t, ctx);
            let (_, size, align, _) = simulate_go_layout(raw, ctx);
            format!("__anon_struct<{size},{align}>")
        } else {
            source[t.byte_range()].trim().to_string()
        }
    });

    let guard =
        trailing_comment_on_line(source, node).and_then(|c| extract_guard_from_go_comment(&c));

    if !field_names.is_empty() {
        if let Some(ty) = ty_text {
            // Normal named fields
            for name in field_names {
                out.push((name, ty.clone(), guard.clone(), field_line));
            }
        }
    } else if let Some(ty) = ty_text {
        // Embedded (anonymous) field: `sync.Mutex`, `Base`, or `*Base`.
        // Go field name is the unqualified type name WITHOUT the pointer `*`
        // (Go spec: for `*T`, the implicit field name is `T`, not `*T`).
        // The type string keeps `*` so sizing sees a pointer.
        let ty = if is_pointer_embed && !ty.starts_with('*') {
            format!("*{ty}")
        } else {
            ty
        };
        let simple_name = ty
            .trim_start_matches('*')
            .split('.')
            .next_back()
            .unwrap_or(&ty)
            .to_string();
        out.push((simple_name, ty, guard, field_line));
    }
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_go(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let tree = PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| anyhow::anyhow!("tree-sitter-go failed to parse source"))?;
    Ok(extract_structs(source, tree.root_node(), arch))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn parse_simple_go_struct() {
        let src = r#"
package main
type Point struct {
    X int32
    Y int32
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
        assert_eq!(layouts[0].fields.len(), 2);
    }

    #[test]
    fn go_layout_with_padding() {
        let src = "package p\ntype T struct { A bool; B int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 8); // bool (1) + 7 pad → 8
    }

    #[test]
    fn go_string_is_two_words() {
        let src = "package p\ntype S struct { Name string }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // ptr + len
    }

    // ── Go guard comment extraction ────────────────────────────────────────────

    #[test]
    fn extract_guard_padlock_form() {
        assert_eq!(
            extract_guard_from_go_comment("// padlock:guard=mu"),
            Some("mu".to_string())
        );
    }

    #[test]
    fn extract_guard_guarded_by_form() {
        assert_eq!(
            extract_guard_from_go_comment("// guarded_by: counter_lock"),
            Some("counter_lock".to_string())
        );
    }

    #[test]
    fn extract_guard_checklocksprotects_form() {
        assert_eq!(
            extract_guard_from_go_comment("// +checklocksprotects:mu"),
            Some("mu".to_string())
        );
    }

    #[test]
    fn extract_guard_no_match_returns_none() {
        assert!(extract_guard_from_go_comment("// just a comment").is_none());
        assert!(extract_guard_from_go_comment("// TODO: fix this").is_none());
    }

    #[test]
    fn go_struct_padlock_guard_annotation_sets_concurrent() {
        let src = r#"package p
type Cache struct {
    Readers int64 // padlock:guard=mu
    Writers int64 // padlock:guard=other_mu
    Mu      sync.Mutex
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        // Readers and Writers should be Concurrent with different guards
        if let AccessPattern::Concurrent { guard, .. } = &l.fields[0].access {
            assert_eq!(guard.as_deref(), Some("mu"));
        } else {
            panic!(
                "expected Concurrent for Readers, got {:?}",
                l.fields[0].access
            );
        }
        if let AccessPattern::Concurrent { guard, .. } = &l.fields[1].access {
            assert_eq!(guard.as_deref(), Some("other_mu"));
        } else {
            panic!(
                "expected Concurrent for Writers, got {:?}",
                l.fields[1].access
            );
        }
    }

    #[test]
    fn go_struct_different_guards_same_cache_line_is_false_sharing() {
        let src = r#"package p
type HotPath struct {
    Readers int64 // padlock:guard=lock_a
    Writers int64 // padlock:guard=lock_b
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(padlock_core::analysis::false_sharing::has_false_sharing(
            &layouts[0]
        ));
    }

    #[test]
    fn go_struct_same_guard_is_not_false_sharing() {
        let src = r#"package p
type Safe struct {
    A int64 // padlock:guard=mu
    B int64 // padlock:guard=mu
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(!padlock_core::analysis::false_sharing::has_false_sharing(
            &layouts[0]
        ));
    }

    // ── interface{} / any sizing ───────────────────────────────────────────────

    #[test]
    fn interface_field_is_two_words() {
        // interface{} is a fat pointer: (type pointer, data pointer) = 2×pointer
        let src = "package p\ntype S struct { V interface{} }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // 2 × 8B on x86-64
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn any_field_is_two_words() {
        // `any` is an alias for `interface{}` since Go 1.18
        let src = "package p\ntype S struct { V any }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // 2 × 8B on x86-64
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn interface_field_same_size_as_error() {
        // `error` was already two-word; interface{} must match
        let src_iface = "package p\ntype S struct { V interface{} }";
        let src_err = "package p\ntype S struct { V error }";
        let iface = parse_go(src_iface, &X86_64_SYSV).unwrap();
        let err = parse_go(src_err, &X86_64_SYSV).unwrap();
        assert_eq!(iface[0].fields[0].size, err[0].fields[0].size);
    }

    #[test]
    fn struct_with_mixed_interface_and_ints_has_correct_layout() {
        // interface{} at offset 0 (size 16, align 8) then int64 at offset 16
        let src = "package p\ntype S struct { V interface{}; N int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[0].size, 16);
        assert_eq!(l.fields[1].offset, 16);
        assert_eq!(l.total_size, 24);
    }

    #[test]
    fn inline_interface_with_methods_is_two_words() {
        // An anonymous interface with methods (e.g. `interface{ Close() error }`) is a
        // two-word fat pointer — same as `interface{}`.  The tree-sitter node kind is
        // `interface_type` in both cases so the `ty.starts_with("interface")` match handles
        // all inline interface bodies.
        let src = "package p\ntype S struct { Conn interface{ Close() error } }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16);
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn named_cross_package_interface_falls_back_to_pointer_size() {
        // Named interfaces from other packages (driver.Connector, io.ReadCloser, …)
        // appear in the AST as `qualified_type` nodes with text like "driver.Connector".
        // Without go/types resolution we cannot distinguish an interface from a concrete
        // struct, so they fall back to pointer_size (8B on x86-64) — the same as an
        // opaque pointer.  This is a known source-analysis limitation; binary (DWARF)
        // analysis always returns the correct compiler layout.
        let src = "package p\ntype DB struct { connector foo.Connector }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        // Known limitation: reports 8B, not the actual 16B.
        assert_eq!(
            layouts[0].fields[0].size, 8,
            "named cross-package interface falls back to pointer_size (known limitation)"
        );
        // The field must be flagged as uncertain so the output layer can warn the user.
        assert!(
            layouts[0]
                .uncertain_fields
                .contains(&"connector".to_string()),
            "qualified-type field should be in uncertain_fields"
        );
    }

    // ── local interface type resolution ───────────────────────────────────────

    #[test]
    fn local_interface_field_is_fat_pointer() {
        // A named interface declared in the same file must be sized as a two-word
        // fat pointer (16B on 64-bit), not as a single pointer (8B).
        let src = r#"package p
type Reader interface {
    Read(p []byte) (n int, err error)
}
type Buf struct {
    R Reader
    N int32
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Buf").expect("Buf");
        let r = l.fields.iter().find(|f| f.name == "R").expect("R field");
        assert_eq!(
            r.size, 16,
            "local interface must be sized as 16B fat pointer"
        );
        assert_eq!(r.align, 8);
    }

    #[test]
    fn local_interface_field_not_marked_uncertain() {
        // A locally-declared interface is resolved; it must NOT appear in uncertain_fields.
        let src = r#"package p
type Closer interface { Close() error }
type File struct { C Closer }
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "File").expect("File");
        assert!(
            !l.uncertain_fields.contains(&"C".to_string()),
            "local interface field must not be uncertain"
        );
    }

    #[test]
    fn qualified_type_field_marked_uncertain() {
        // A qualified type (e.g. `io.Reader`) from an external package cannot be
        // resolved without go/types; the field must appear in uncertain_fields.
        let src = "package p\ntype S struct { R foo.Reader; N int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert!(
            l.uncertain_fields.contains(&"R".to_string()),
            "qualified-type field must be in uncertain_fields"
        );
        // Non-qualified field must not be uncertain
        assert!(
            !l.uncertain_fields.contains(&"N".to_string()),
            "plain int32 field must not be uncertain"
        );
    }

    #[test]
    fn pointer_to_qualified_type_not_uncertain() {
        // `*pkg.Type` is an explicit pointer — size is always pointer_size (8B).
        // No need to flag it as uncertain since the pointer indirection makes the
        // type's internal layout irrelevant for padding analysis.
        let src = "package p\ntype S struct { P *io.Reader }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert!(
            !l.uncertain_fields.contains(&"P".to_string()),
            "*qualified.Type pointer must not be uncertain"
        );
    }

    // ── embedded struct support ───────────────────────────────────────────────

    #[test]
    fn embedded_struct_field_uses_type_name_as_field_name() {
        // `Base` is an embedded field — Go uses the type name as the field name.
        let src = r#"package p
type Base struct { X int32 }
type Derived struct {
    Base
    Y int32
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let derived = layouts
            .iter()
            .find(|l| l.name == "Derived")
            .expect("Derived");
        // Must have a field named "Base"
        assert!(
            derived.fields.iter().any(|f| f.name == "Base"),
            "embedded field should be named 'Base'"
        );
    }

    #[test]
    fn embedded_qualified_type_uses_unqualified_name() {
        // `sync.Mutex` embedded — field name should be "Mutex"
        let src = r#"package p
type Safe struct {
    sync.Mutex
    Value int64
}
"#;
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Safe").expect("Safe");
        assert!(
            l.fields.iter().any(|f| f.name == "Mutex"),
            "embedded sync.Mutex should produce field named 'Mutex'"
        );
    }

    #[test]
    fn embedded_field_has_non_zero_size_from_resolution() {
        // After lib.rs nested-struct resolution, Base's size should be filled in.
        // We test via parse_source_str which triggers resolution.
        let src = r#"package p
type Inner struct { A int64; B int64 }
type Outer struct {
    Inner
    C int32
}
"#;
        use crate::{SourceLanguage, parse_source_str};
        let layouts = parse_source_str(src, &SourceLanguage::Go, &X86_64_SYSV).unwrap();
        let outer = layouts.iter().find(|l| l.name == "Outer").expect("Outer");
        let inner_field = outer
            .fields
            .iter()
            .find(|f| f.name == "Inner")
            .expect("Inner field");
        // Inner struct is 16 bytes (two int64s)
        assert_eq!(
            inner_field.size, 16,
            "embedded Inner field should be resolved to 16 bytes"
        );
    }

    #[test]
    fn struct_with_no_embedded_fields_unaffected() {
        let src = "package p\ntype S struct { A int32; B int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields.len(), 2);
        assert_eq!(l.fields[0].name, "A");
        assert_eq!(l.fields[1].name, "B");
    }

    // ── Go generics ───────────────────────────────────────────────────────────

    #[test]
    fn go_generic_struct_is_skipped() {
        // Generic structs cannot be sized without type instantiation.
        let src = "package p\ntype Pair[T any] struct { First T; Second T }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Pair"),
            "generic struct must be skipped"
        );
    }

    #[test]
    fn go_concrete_struct_alongside_generic_is_parsed() {
        // The generic is skipped but a concrete sibling struct is still parsed.
        let src = "package p\ntype Pair[T any] struct { First T }\ntype Point struct { X int32; Y int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Pair"),
            "Pair must be skipped"
        );
        assert!(
            layouts.iter().any(|l| l.name == "Point"),
            "Point must be parsed"
        );
    }

    // ── bad weather: embedded fields ──────────────────────────────────────────

    #[test]
    fn embedded_ptr_field_name_is_type_without_star() {
        // Go spec: for `*T`, the implicit field name is `T`, not `*T`.
        let src = "package p\ntype A struct { *B }\ntype B struct { X uint8 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let a = layouts.iter().find(|l| l.name == "A").expect("A");
        assert_eq!(
            a.fields[0].name, "B",
            "embedded pointer field name must be 'B' not '*B'"
        );
        // Size must be pointer-size (8), NOT B's struct size (1).
        assert_eq!(
            a.fields[0].size, 8,
            "embedded pointer is always pointer-sized"
        );
    }

    #[test]
    fn embedded_ptr_struct_does_not_hang() {
        // Regression: tree-sitter-go <=0.23 had a quadratic parse bug on structs
        // containing embedded pointer fields paired with non-builtin field types.
        let src = "package p\ntype A struct { *B }\ntype B struct { X uint8 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert!(layouts.iter().any(|l| l.name == "B"));
    }

    #[test]
    fn embedded_ptr_mutual_recursion_does_not_hang() {
        // Regression: parse-error trees from tree-sitter-go can strip the `*` from
        // embedded pointer types, making Rec1/Rec2 appear to embed each other
        // non-transitively. resolve_nested_structs must not loop forever.
        use crate::{SourceLanguage, parse_source_str};
        let src = "package p\ntype Rec1 struct { *Rec2 }\ntype Rec2 struct { F string; *Rec1 }";
        let _ = parse_source_str(src, &SourceLanguage::Go, &X86_64_SYSV).unwrap();
    }

    #[test]
    fn embedded_ptr_combined_patterns_does_not_hang() {
        use crate::{SourceLanguage, parse_source_str};
        let src = "package main\n\n\
                   type Rec1 struct { *Rec2 }\n\
                   type Rec2 struct { F string; *Rec1 }\n\n\
                   type A struct { *B }\n\
                   type B struct { X uint8 }\n";
        let _ = parse_source_str(src, &SourceLanguage::Go, &X86_64_SYSV).unwrap();
    }

    #[test]
    fn embedded_unknown_type_falls_back_to_pointer_size() {
        // If the embedded type is not defined in the file, size = pointer_size
        let src = "package p\ntype S struct { external.Type\nX int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let emb = l
            .fields
            .iter()
            .find(|f| f.name == "Type")
            .expect("Type field");
        // Falls back to pointer size (8 on x86_64) since type is unknown
        assert_eq!(emb.size, 8);
    }

    // ── well-known external Go stdlib types ───────────────────────────────────

    #[test]
    fn sync_mutex_sized_correctly() {
        let src = "package p\nimport \"sync\"\ntype S struct { mu sync.Mutex; x int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let mu = l.fields.iter().find(|f| f.name == "mu").expect("mu");
        assert_eq!(mu.size, 8, "sync.Mutex must be 8B");
        assert_eq!(mu.align, 4);
        // mu is not uncertain — its size is known
        assert!(
            !l.uncertain_fields.contains(&"mu".to_string()),
            "sync.Mutex must not be in uncertain_fields"
        );
    }

    #[test]
    fn sync_rwmutex_sized_correctly() {
        let src = "package p\nimport \"sync\"\ntype S struct { rw sync.RWMutex }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let rw = l.fields.iter().find(|f| f.name == "rw").expect("rw");
        assert_eq!(rw.size, 24, "sync.RWMutex must be 24B");
        assert!(!l.uncertain_fields.contains(&"rw".to_string()));
    }

    #[test]
    fn sync_once_sized_correctly() {
        let src = "package p\nimport \"sync\"\ntype S struct { o sync.Once }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let o = l.fields.iter().find(|f| f.name == "o").expect("o");
        assert_eq!(o.size, 12, "sync.Once must be 12B");
        assert!(!l.uncertain_fields.contains(&"o".to_string()));
    }

    #[test]
    fn atomic_value_sized_correctly() {
        let src = "package p\nimport \"sync/atomic\"\ntype S struct { v atomic.Value }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let v = l.fields.iter().find(|f| f.name == "v").expect("v");
        assert_eq!(v.size, 16, "atomic.Value must be 16B (any = 2 words)");
        assert!(!l.uncertain_fields.contains(&"v".to_string()));
    }

    #[test]
    fn atomic_int64_sized_correctly() {
        let src = "package p\nimport \"sync/atomic\"\ntype S struct { n atomic.Int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let n = l.fields.iter().find(|f| f.name == "n").expect("n");
        assert_eq!(n.size, 8, "atomic.Int64 must be 8B");
        assert_eq!(n.align, 8);
        assert!(!l.uncertain_fields.contains(&"n".to_string()));
    }

    #[test]
    fn atomic_int32_sized_correctly() {
        let src = "package p\nimport \"sync/atomic\"\ntype S struct { n atomic.Int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let n = l.fields.iter().find(|f| f.name == "n").expect("n");
        assert_eq!(n.size, 4, "atomic.Int32 must be 4B");
        assert!(!l.uncertain_fields.contains(&"n".to_string()));
    }

    #[test]
    fn atomic_bool_sized_correctly() {
        let src = "package p\nimport \"sync/atomic\"\ntype S struct { b atomic.Bool }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let b = l.fields.iter().find(|f| f.name == "b").expect("b");
        assert_eq!(b.size, 4, "atomic.Bool must be 4B");
        assert!(!l.uncertain_fields.contains(&"b".to_string()));
    }

    #[test]
    fn struct_with_mutex_and_data_has_correct_total_size() {
        // Realistic server pattern: mu sync.Mutex (8B) + count int64 (8B) = 16B
        let src = "package p\nimport \"sync\"\ntype Counter struct { mu sync.Mutex; count int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts
            .iter()
            .find(|l| l.name == "Counter")
            .expect("Counter");
        assert_eq!(l.total_size, 16, "Counter must be 16B");
    }

    #[test]
    fn sync_waitgroup_sized_correctly() {
        // sync.WaitGroup: { noCopy (0B); state atomic.Uint64 (8B); sema uint32 (4B) }
        // = 12B of fields, padded to 16B by its 8-byte alignment (verified with
        // unsafe.Sizeof on go1.22). Common in concurrent Go code.
        let src = "package p\nimport \"sync\"\ntype S struct { wg sync.WaitGroup; x int32 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let wg = l.fields.iter().find(|f| f.name == "wg").expect("wg");
        assert_eq!(wg.size, 16, "sync.WaitGroup must be 16B");
        assert_eq!(wg.align, 8);
        assert!(
            !l.uncertain_fields.contains(&"wg".to_string()),
            "sync.WaitGroup must not be in uncertain_fields"
        );
    }

    #[test]
    fn time_time_sized_correctly() {
        // time.Time: { wall uint64; ext int64; loc *Location } = 24B, align 8.
        let src = "package p\nimport \"time\"\ntype Event struct { ts time.Time; id int64 }";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Event").expect("Event");
        let ts = l.fields.iter().find(|f| f.name == "ts").expect("ts");
        assert_eq!(ts.size, 24, "time.Time must be 24B");
        assert_eq!(ts.align, 8);
        assert!(
            !l.uncertain_fields.contains(&"ts".to_string()),
            "time.Time must not be in uncertain_fields"
        );
    }

    // ── field shapes (verified against `go run` with unsafe.Sizeof/Offsetof) ───

    fn go_layout(src: &str, name: &str) -> StructLayout {
        crate::parse_source_str(src, &crate::SourceLanguage::Go, &X86_64_SYSV)
            .unwrap()
            .into_iter()
            .find(|l| l.name == name)
            .unwrap_or_else(|| panic!("{name} not parsed"))
    }

    fn go_offset(l: &StructLayout, field: &str) -> usize {
        l.fields.iter().find(|f| f.name == field).unwrap().offset
    }

    #[test]
    fn arrays_are_sized_by_length_and_element() {
        let l = go_layout(
            "package p\nconst N = 4\ntype S struct { a byte; b [48]byte; c int64; d [N * 2]int16; e [2][3]int16; f byte }",
            "S",
        );
        assert_eq!(go_offset(&l, "c"), 56);
        assert_eq!(go_offset(&l, "d"), 64);
        assert_eq!(go_offset(&l, "e"), 80);
        assert_eq!(go_offset(&l, "f"), 92);
    }

    #[test]
    fn array_length_may_use_unsafe_sizeof() {
        let src = "package p\nimport (\"sync/atomic\"; \"unsafe\")\ntype S struct { a atomic.Int64; b atomic.Uint64; _ [64 - unsafe.Sizeof(atomic.Int64{}) - unsafe.Sizeof(atomic.Uint64{})]byte; mu int64 }";
        let l = go_layout(src, "S");
        assert_eq!(go_offset(&l, "mu"), 64);
        assert!(l.uncertain_fields.is_empty());
    }

    #[test]
    fn iota_constants_feed_array_lengths() {
        let src =
            "package p\nconst (\n A = iota\n B\n C\n)\ntype S struct { x [C + 1]byte; y int32 }";
        let l = go_layout(src, "S");
        assert_eq!(go_offset(&l, "y"), 4);
    }

    #[test]
    fn unevaluable_array_length_is_uncertain() {
        let l = go_layout("package p\ntype S struct { x [cpu.Pad]byte; y int64 }", "S");
        assert_eq!(l.uncertain_fields, vec!["x".to_string()]);
    }

    #[test]
    fn local_type_aliases_are_resolved() {
        let l = go_layout(
            "package p\ntype Alias int16\ntype Vec [4]int32\ntype S struct { a bool; x Alias; b bool; v Vec; c bool }",
            "S",
        );
        assert_eq!(go_offset(&l, "x"), 2);
        assert_eq!(go_offset(&l, "b"), 4);
        assert_eq!(go_offset(&l, "v"), 8);
        assert_eq!(go_offset(&l, "c"), 24);
    }

    #[test]
    fn array_of_local_structs_is_scaled_by_the_resolver() {
        let src = "package p\ntype In struct { x bool; y int32 }\ntype S struct { a bool; b [2]In; c bool }";
        let l = go_layout(src, "S");
        assert_eq!(go_offset(&l, "c"), 20);
        assert_eq!(l.total_size, 24);
    }

    #[test]
    fn inline_struct_func_and_generic_fields_are_kept() {
        let src = "package p\nimport \"sync/atomic\"\ntype S struct { a bool; in struct{ x bool; y int64 }; f func(int); p atomic.Pointer[int]; z bool }";
        let l = go_layout(src, "S");
        assert_eq!(go_offset(&l, "in"), 8);
        assert_eq!(go_offset(&l, "f"), 24);
        assert_eq!(go_offset(&l, "p"), 32);
        assert_eq!(go_offset(&l, "z"), 40);
    }

    #[test]
    fn embedded_pointer_is_one_word() {
        let src =
            "package p\ntype Big struct { a [64]byte }\ntype S struct { a bool; *Big; b bool }";
        let l = go_layout(src, "S");
        assert_eq!(l.total_size, 24);
    }

    #[test]
    fn grouped_type_declarations_yield_every_struct() {
        let src = "package p\ntype (\n A struct { x int32 }\n B struct { y int64 }\n)";
        let layouts = parse_go(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 2);
    }

    #[test]
    fn complex_and_sync_types_have_their_real_alignment() {
        let l = go_layout(
            "package p\ntype S struct { a bool; c complex64; b bool }",
            "S",
        );
        assert_eq!(go_offset(&l, "c"), 4);
        let l = go_layout(
            "package p\ntype S struct { a bool; r sync.RWMutex; b bool }",
            "S",
        );
        assert_eq!(go_offset(&l, "r"), 4);
        assert_eq!(go_offset(&l, "b"), 28);
    }

    #[test]
    fn known_stdlib_interfaces_are_two_words_and_certain() {
        let l = go_layout(
            "package p\ntype DB struct { c driver.Connector; r io.Reader }",
            "DB",
        );
        assert_eq!(l.total_size, 32);
        assert!(l.uncertain_fields.is_empty());
    }

    #[test]
    fn trailing_zero_size_field_adds_a_padding_byte() {
        let l = go_layout("package p\ntype S struct { a int64; z struct{} }", "S");
        assert_eq!(l.total_size, 16);
    }
}
