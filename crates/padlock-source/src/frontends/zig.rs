// padlock-source/src/frontends/zig.rs
//
// Extracts struct layouts from Zig source using tree-sitter-zig.
// Handles regular, extern, and packed struct variants.
// Sizes use Zig's platform-native alignment rules (same as C on the target arch).

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{Field, StructLayout, TypeInfo};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Parser};

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_zig::LANGUAGE.into())
            .expect("Zig grammar is always valid");
        p
    });
}

// ── type resolution ───────────────────────────────────────────────────────────

/// Returns `true` for Zig types that are compile-time-only and cannot be given
/// a runtime size. Fields with these types are sized as pointer-sized in the
/// layout simulation, and their names are added to `uncertain_fields` so the
/// user knows the sizing is a placeholder.
fn is_zig_comptime_type(ty: &str) -> bool {
    matches!(
        ty.trim(),
        "type" | "anytype" | "comptime_int" | "comptime_float"
    )
}

/// ABI `(size, align)` of a `bits`-wide integer: `u1..u8` → 1, `u9..u16` → 2,
/// `u17..u32` → 4, `u33..u64` → 8, `u65..u128` → 16, wider → multiples of 16.
fn int_abi_size_align(bits: usize) -> (usize, usize) {
    if bits == 0 {
        return (0, 1);
    }
    let bytes = bits.div_ceil(8);
    if bytes <= 16 {
        let size = bytes.next_power_of_two();
        (size, size)
    } else {
        (bytes.next_multiple_of(16), 16)
    }
}

/// Size/alignment of a Zig type name; the alignment of scalars up to 8 bytes is
/// capped at the target's maximum scalar alignment (`u64` is 4-aligned on x86, as
/// in its C ABI). 128-bit integers keep their 16-byte alignment (LLVM ≥ 18).
fn zig_type_size_align(ty: &str, arch: &'static ArchConfig) -> (usize, usize) {
    let (size, align) = zig_type_size_align_uncapped(ty, arch);
    if size > 8 {
        (size, align)
    } else {
        (size, align.min(arch.max_align.max(1)))
    }
}

fn zig_type_size_align_uncapped(ty: &str, arch: &'static ArchConfig) -> (usize, usize) {
    let ty = ty.trim();
    match ty {
        "bool" => (1, 1),
        "u8" | "i8" => (1, 1),
        "u16" | "i16" | "f16" => (2, 2),
        "u32" | "i32" | "f32" => (4, 4),
        "u64" | "i64" | "f64" => (8, 8),
        "u128" | "i128" | "f128" => (16, 16),
        // f80 is the x87 80-bit float: 16 bytes / align 16 on x86-64, 12 / 4 on x86.
        "f80" if arch.pointer_size == 4 => (12, 4),
        "f80" => (16, 16),
        "usize" | "isize" => (arch.pointer_size, arch.pointer_size),
        "void" | "anyopaque" => (0, 1),
        // comptime-only or type-erased — treat as pointer-sized
        "type" | "anytype" | "comptime_int" | "comptime_float" => {
            (arch.pointer_size, arch.pointer_size)
        }

        // Zig C-interop types (std.c / @cImport equivalents)
        "c_char" | "c_uchar" | "c_schar" => (1, 1),
        "c_short" | "c_ushort" => (2, 2),
        "c_int" | "c_uint" => (4, 4),
        "c_long" | "c_ulong" => (arch.pointer_size, arch.pointer_size), // 8B on LP64, 4B on LLP64
        "c_longlong" | "c_ulonglong" => (8, 8),
        "c_float" => (4, 4),
        "c_double" => (8, 8),
        "c_longdouble" if arch.pointer_size == 4 => (12, 4),
        "c_longdouble" => (16, 16),

        // Arbitrary-width integers: uN / iN where N is a decimal number. A field
        // of such a type occupies the ABI size of the integer: its byte count
        // rounded up to a power of two (up to 16 bytes), aligned to that size;
        // wider integers are aligned to 16 and padded to a multiple of 16.
        // (Inside a `packed struct` the bits are packed instead — see
        // `zig_type_bit_width`.)
        ty if (ty.starts_with('u') || ty.starts_with('i'))
            && ty[1..].bytes().all(|b| b.is_ascii_digit())
            && !ty[1..].is_empty() =>
        {
            match ty[1..].parse::<usize>() {
                Ok(bits) => int_abi_size_align(bits),
                Err(_) => (arch.pointer_size, arch.pointer_size),
            }
        }

        _ => (arch.pointer_size, arch.pointer_size),
    }
}

/// Return the exact bit width of a Zig type inside a `packed struct`.
///
/// In a packed struct every field is bit-packed without inter-field padding.
/// Arbitrary-width integers (`u3`, `i11`, …) occupy exactly N bits; all other
/// types occupy their natural byte size × 8 bits.
fn zig_type_bit_width(ty: &str, arch: &'static ArchConfig) -> usize {
    let ty = ty.trim();
    // Arbitrary-width integers: exact bit count
    if (ty.starts_with('u') || ty.starts_with('i'))
        && !ty[1..].is_empty()
        && ty[1..].bytes().all(|b| b.is_ascii_digit())
        && let Ok(bits) = ty[1..].parse::<usize>()
    {
        return bits;
    }
    match ty {
        "bool" => 1,
        "f16" => 16,
        "f32" => 32,
        "f64" => 64,
        "f80" => 80,
        "f128" => 128,
        _ => {
            // Fall back to byte size × 8
            let (bytes, _) = zig_type_size_align(ty, arch);
            bytes * 8
        }
    }
}

// ── file-level context ────────────────────────────────────────────────────────

/// Start and end byte of a node.
type ByteRange = (usize, usize);
/// A type-returning function's comptime parameters `(name, is_type)` and the
/// byte range of the struct it returns.
type GenericFn = (Vec<(String, bool)>, ByteRange);

/// What a pre-pass learns about the file's own declarations.
#[derive(Clone)]
struct ZigCtx {
    arch: &'static ArchConfig,
    /// `const N = 8;` → name → expression text.
    consts: HashMap<String, String>,
    /// `const E = enum(u8) {…}` → name → backing integer type (`u8`).
    enums: HashMap<String, String>,
    /// Local `struct` / `union` names, sized later by the nested-struct resolver.
    containers: HashSet<String>,
    /// Local `packed struct`s → their field type texts, so a packed struct nested
    /// in another packed struct contributes its exact bit width.
    packed: HashMap<String, Vec<String>>,
    /// Type-returning functions `fn Pair(comptime A: type, comptime n: usize) type
    /// { return struct {…}; }` → (parameters as `(name, is_type)`, byte range of
    /// the returned `struct_declaration`).
    generic_fns: HashMap<String, GenericFn>,
    /// Identifiers that stand for a type text: bound comptime parameters, and
    /// `const P = Pair(u8, 4);` aliases.
    type_aliases: HashMap<String, String>,
    /// Concrete calls `Pair(u8, 4)` of type-returning functions.
    uses: Vec<(String, Vec<String>)>,
}

/// Bit width of a type inside a `packed struct`: arbitrary-width integers are
/// exact, enums use their backing integer, nested packed structs the sum of
/// their fields; everything else its byte size × 8.
fn zig_bits(ty: &str, ctx: &ZigCtx, depth: u8) -> usize {
    let ty = ty.trim();
    if depth > 8 {
        return ctx.arch.pointer_size * 8;
    }
    if let Some(backing) = ctx.enums.get(ty) {
        return zig_bits(backing, ctx, depth + 1);
    }
    if let Some(fields) = ctx.packed.get(ty) {
        return fields.iter().map(|f| zig_bits(f, ctx, depth + 1)).sum();
    }
    zig_type_bit_width(ty, ctx.arch)
}

/// Size/alignment of a type node plus what the caller needs to finish the job.
struct ZSized {
    size: usize,
    align: usize,
    /// `[N]LocalStruct`: `(LocalStruct, total element count)` for the resolver.
    named: Option<(String, usize)>,
    /// A best guess (array length that could not be evaluated).
    uncertain: bool,
}

impl ZSized {
    fn plain((size, align): (usize, usize)) -> Self {
        ZSized {
            size,
            align,
            named: None,
            uncertain: false,
        }
    }
}

fn node_text<'a>(source: &'a str, node: Node<'_>) -> &'a str {
    source[node.byte_range()].trim()
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .filter_map(|i| node.named_child(i as u32))
        .collect()
}

/// Backing integer of a Zig `enum`: the explicit `enum(uN)` tag type, or the
/// smallest unsigned integer that holds every variant (`u0` for one variant).
fn enum_backing_type(source: &str, node: Node<'_>) -> String {
    let explicit = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|c| matches!(c.kind(), "builtin_type" | "identifier"));
    if let Some(t) = explicit {
        return node_text(source, t).to_string();
    }
    let variants = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .filter(|c| c.kind() == "container_field")
        .count();
    let bits = match variants {
        0 | 1 => 0,
        n => usize::BITS as usize - (n - 1).leading_zeros() as usize,
    };
    format!("u{bits}")
}

fn collect_zig_decls(source: &str, root: Node<'_>, arch: &'static ArchConfig) -> ZigCtx {
    let mut ctx = ZigCtx {
        arch,
        consts: HashMap::new(),
        enums: HashMap::new(),
        containers: HashSet::new(),
        packed: HashMap::new(),
        generic_fns: HashMap::new(),
        type_aliases: HashMap::new(),
        uses: Vec::new(),
    };
    let mut all_params: Vec<String> = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }
        if node.kind() == "function_declaration" {
            if let Some((name, generic)) = generic_fn_signature(source, node) {
                all_params.extend(generic.0.iter().map(|(p, _)| p.clone()));
                ctx.generic_fns.insert(name, generic);
            }
            continue;
        }
        if node.kind() == "call_expression"
            && let Some((name, args)) = call_parts(source, node)
        {
            let entry = (name, args);
            if !ctx.uses.contains(&entry) {
                ctx.uses.push(entry);
            }
            continue;
        }
        if node.kind() != "variable_declaration" {
            continue;
        }
        let kids = named_children(node);
        let Some(name) = kids
            .iter()
            .find(|c| c.kind() == "identifier")
            .map(|c| node_text(source, *c).to_string())
        else {
            continue;
        };
        // The initialiser is the last named child.
        let Some(init) = kids.last().filter(|c| c.kind() != "identifier") else {
            continue;
        };
        match init.kind() {
            "struct_declaration" | "union_declaration" => {
                let is_packed = (0..init.child_count())
                    .filter_map(|i| init.child(i))
                    .any(|c| c.kind() == "packed");
                if is_packed {
                    let types = named_children(*init)
                        .into_iter()
                        .filter(|c| c.kind() == "container_field")
                        .filter_map(|f| f.child_by_field_name("type"))
                        .map(|t| node_text(source, t).to_string())
                        .collect();
                    ctx.packed.insert(name.clone(), types);
                }
                ctx.containers.insert(name);
            }
            "enum_declaration" => {
                ctx.enums.insert(name, enum_backing_type(source, *init));
            }
            "integer" | "binary_expression" | "builtin_function" | "parenthesized_expression" => {
                ctx.consts
                    .insert(name, node_text(source, *init).to_string());
            }
            // `const P = Pair(u8, 4);` names an instantiation.
            "call_expression" => {
                if let Some((f, args)) = call_parts(source, *init) {
                    ctx.type_aliases
                        .insert(name, format!("{f}({})", args.join(", ")));
                }
            }
            _ => {}
        }
    }
    // Calls inside a generic function that mention its parameters are not concrete;
    // and only calls of type-returning functions are uses.
    let generic_fns = ctx.generic_fns.clone();
    ctx.uses.retain(|(f, args)| {
        generic_fns.contains_key(f)
            && !args.iter().any(|a| {
                a.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|w| all_params.iter().any(|p| p == w))
            })
    });
    ctx
}

/// `fn Name(comptime A: type, …) type { return struct {…}; }` → its name,
/// comptime parameters `(name, is_type)` and the returned struct's byte range.
fn generic_fn_signature(source: &str, node: Node<'_>) -> Option<(String, GenericFn)> {
    let name = node_text(source, node.child_by_field_name("name")?).to_string();
    if node_text(source, node.child_by_field_name("type")?) != "type" {
        return None;
    }
    let params_node = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|c| c.kind() == "parameters")?;
    let mut params = Vec::new();
    for p in named_children(params_node) {
        let comptime = (0..p.child_count())
            .filter_map(|i| p.child(i))
            .any(|c| c.kind() == "comptime");
        let pname = node_text(source, p.child_by_field_name("name")?).to_string();
        let ptype = p.child_by_field_name("type").map(|t| node_text(source, t));
        if !comptime {
            return None;
        }
        params.push((pname, ptype == Some("type")));
    }
    // The returned container: `return struct {…};` anywhere in the body.
    let body = node.child_by_field_name("body")?;
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "return_expression"
            && let Some(sd) = named_children(n)
                .into_iter()
                .find(|c| c.kind() == "struct_declaration")
        {
            return Some((name, (params, (sd.start_byte(), sd.end_byte()))));
        }
        for i in (0..n.child_count()).rev() {
            if let Some(c) = n.child(i) {
                stack.push(c);
            }
        }
    }
    None
}

/// `Pair(u8, 4)` → (`Pair`, [`u8`, `4`]).
fn call_parts(source: &str, node: Node<'_>) -> Option<(String, Vec<String>)> {
    let f = node.child_by_field_name("function")?;
    if f.kind() != "identifier" {
        return None;
    }
    let args = named_children(node)
        .into_iter()
        .filter(|c| c.id() != f.id())
        .map(|c| super::c_cpp::normalize_type_text(node_text(source, c)))
        .collect();
    Some((node_text(source, f).to_string(), args))
}

/// Concrete calls of type-returning functions (`Pair(u8, 4)`) in a Zig source text.
pub fn generic_uses(source: &str) -> Vec<(String, Vec<String>)> {
    PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .map(|t| collect_zig_decls(source, t.root_node(), &padlock_core::arch::X86_64_SYSV).uses)
        .unwrap_or_default()
}

thread_local! {
    static TEXT_DEPTH: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Size a type given as text (a bound comptime parameter), by parsing it as the
/// type of a one-field probe struct.
fn size_zig_type_text(text: &str, ctx: &ZigCtx) -> ZSized {
    let ps = ctx.arch.pointer_size;
    let depth = TEXT_DEPTH.with(|d| d.get());
    if depth > 8 {
        return ZSized {
            uncertain: true,
            ..ZSized::plain((ps, ps))
        };
    }
    let probe = format!("const __padlock_probe = struct {{ x: {text} }};");
    let Some(tree) = PARSER.with(|p| p.borrow_mut().parse(&probe, None)) else {
        return ZSized::plain((ps, ps));
    };
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "container_field"
            && let Some(t) = n.child_by_field_name("type")
        {
            TEXT_DEPTH.with(|d| d.set(depth + 1));
            let r = type_node_size_align(&probe, t, ctx);
            TEXT_DEPTH.with(|d| d.set(depth));
            return r;
        }
        for i in (0..n.child_count()).rev() {
            if let Some(c) = n.child(i) {
                stack.push(c);
            }
        }
    }
    ZSized::plain((ps, ps))
}

fn eval_zig_len(expr: &str, ctx: &ZigCtx) -> Option<usize> {
    let lookup = |id: &str| ctx.consts.get(id).cloned();
    let sizeof = |ty: &str| {
        let ty = ty.trim();
        let ty = ctx.enums.get(ty).map(String::as_str).unwrap_or(ty);
        i64::try_from(zig_type_size_align(ty, ctx.arch).0).ok()
    };
    let n = super::consteval::eval(
        expr,
        &super::consteval::Env {
            lookup: &lookup,
            sizeof: &sizeof,
        },
        0,
    )?;
    usize::try_from(n).ok()
}

/// Determine size/align of a type node, dispatching by node kind.
fn type_node_size_align(source: &str, node: Node<'_>, ctx: &ZigCtx) -> ZSized {
    let arch = ctx.arch;
    let ps = arch.pointer_size;
    match node.kind() {
        "builtin_type" => ZSized::plain(zig_type_size_align(node_text(source, node), arch)),
        "identifier" => {
            let text = node_text(source, node);
            if let Some(bound) = ctx.type_aliases.get(text) {
                return size_zig_type_text(bound, ctx);
            }
            if let Some(backing) = ctx.enums.get(text) {
                ZSized::plain(zig_type_size_align(backing, arch))
            } else if ctx.containers.contains(text) {
                ZSized {
                    named: Some((text.to_string(), 1)),
                    ..ZSized::plain((ps, ps))
                }
            } else {
                ZSized::plain(zig_type_size_align(text, arch))
            }
        }
        // *T — single pointer
        "pointer_type" => ZSized::plain((ps, ps)),
        // ?T — optional. A pointer or slice uses null as "none" (no extra space);
        // anything else is T plus a one-byte tag, rounded up to T's alignment.
        "nullable_type" => {
            if find_child_by_kinds(node, &["pointer_type"]).is_some() {
                ZSized::plain((ps, ps))
            } else if find_child_by_kinds(node, &["slice_type"]).is_some() {
                ZSized::plain((ps * 2, ps))
            } else if let Some(inner) = find_first_type_child(source, node) {
                let e = type_node_size_align(source, inner, ctx);
                let align = e.align.max(1);
                ZSized {
                    size: (e.size + 1).next_multiple_of(align),
                    align,
                    ..e
                }
            } else {
                ZSized::plain((ps, ps))
            }
        }
        // []T — slice = (ptr, len)
        "slice_type" => ZSized::plain((ps * 2, ps)),
        // [N]T — length is the first child expression, element type the last.
        "array_type" => {
            let kids = named_children(node);
            let (Some(len), Some(elem)) = (kids.first(), kids.last()) else {
                return ZSized {
                    uncertain: true,
                    ..ZSized::plain((ps, ps))
                };
            };
            let e = type_node_size_align(source, *elem, ctx);
            let (count, known) = match eval_zig_len(node_text(source, *len), ctx) {
                Some(n) => (n, true),
                None => (1, false),
            };
            ZSized {
                size: e.size.saturating_mul(count),
                align: e.align,
                named: e.named.map(|(n, c)| (n, c.saturating_mul(count))),
                uncertain: e.uncertain || !known,
            }
        }
        // E!T — the payload plus a 16-bit error code, larger alignment first.
        "error_union_type" | "error_union" => {
            let payload = node
                .child_by_field_name("ok")
                .or_else(|| named_children(node).last().copied())
                .map(|c| type_node_size_align(source, c, ctx));
            match payload {
                Some(p) if p.size > 0 => {
                    let align = p.align.max(2);
                    ZSized {
                        size: (p.size.next_multiple_of(2) + 2).next_multiple_of(align),
                        align,
                        ..p
                    }
                }
                _ => ZSized::plain((2, 2)),
            }
        }
        // @Vector(N, T): SIMD vector type.
        // Size = N × sizeof(T); alignment = size (SIMD vectors are naturally
        // aligned to their own size, capped at 64 bytes for current hardware).
        "builtin_function" => {
            if let Some((count, elem_sz, elem_al)) = parse_vector_builtin(source, node, arch) {
                let size = count * elem_sz;
                let align = size.next_power_of_two().min(64).max(elem_al);
                return ZSized::plain((size, align));
            }
            ZSized::plain((ps, ps))
        }
        // `Pair(u8, 4)`: an instantiation, sized by the nested-struct resolver.
        "call_expression" => match call_parts(source, node) {
            Some((f, args)) if ctx.generic_fns.contains_key(&f) => ZSized {
                named: Some((format!("{f}({})", args.join(", ")), 1)),
                ..ZSized::plain((ps, ps))
            },
            _ => ZSized::plain((ps, ps)),
        },
        _ => ZSized::plain((ps, ps)),
    }
}

/// For `@Vector(N, T)` nodes, return `Some((count, elem_size, elem_align))`.
fn parse_vector_builtin(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
) -> Option<(usize, usize, usize)> {
    // Verify the builtin name is @Vector
    let name_node = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|c| c.kind() == "builtin_identifier")?;
    if source[name_node.byte_range()].trim() != "@Vector" {
        return None;
    }
    let args_node = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|c| c.kind() == "arguments")?;

    let mut count: Option<usize> = None;
    let mut elem: Option<(usize, usize)> = None;

    for i in 0..args_node.child_count() {
        let Some(child) = args_node.child(i) else {
            continue;
        };
        match child.kind() {
            "integer" | "integer_literal" => {
                if count.is_none() {
                    count = source[child.byte_range()].trim().parse().ok();
                }
            }
            "builtin_type" | "identifier" if elem.is_none() => {
                let text = source[child.byte_range()].trim();
                elem = Some(zig_type_size_align(text, arch));
            }
            _ => {}
        }
    }
    let (elem_sz, elem_al) = elem?;
    Some((count?, elem_sz, elem_al))
}

fn find_child_by_kinds<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    for i in 0..node.child_count() {
        if let Some(c) = node.child(i)
            && kinds.contains(&c.kind())
        {
            return Some(c);
        }
    }
    None
}

fn find_first_type_child<'a>(source: &str, node: Node<'a>) -> Option<Node<'a>> {
    let _ = source;
    for i in 0..node.child_count() {
        if let Some(c) = node.child(i) {
            match c.kind() {
                "builtin_type" | "identifier" | "pointer_type" | "slice_type" | "array_type"
                | "nullable_type" | "error_union" | "error_union_type" => return Some(c),
                _ => {}
            }
        }
    }
    None
}

// ── tree-sitter walker ────────────────────────────────────────────────────────

fn extract_structs(source: &str, root: Node<'_>, arch: &'static ArchConfig) -> Vec<StructLayout> {
    let ctx = collect_zig_decls(source, root, arch);
    let mut layouts = Vec::new();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }

        if node.kind() == "variable_declaration"
            && let Some(layout) = parse_variable_declaration(source, node, &ctx)
        {
            layouts.push(layout);
        } else if node.kind() == "function_declaration" {
            // `fn Foo(comptime T: type) type { return struct {…}; }` — a
            // comptime-generic function returning a struct type. Lay it out once
            // per concrete call `Foo(u32)`; note it as skipped when there is none.
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(source, n).to_string());
            match name
                .as_ref()
                .and_then(|n| ctx.generic_fns.get(n).map(|g| (n, g)))
            {
                Some((name, (params, range))) => {
                    let found = instantiate_generic_fn(source, root, name, params, *range, &ctx);
                    if found.is_empty() {
                        note_comptime_generic_fn(source, node);
                    }
                    layouts.extend(found);
                }
                None => note_comptime_generic_fn(source, node),
            }
        }
    }
    layouts
}

fn instantiate_generic_fn(
    source: &str,
    root: Node<'_>,
    name: &str,
    params: &[(String, bool)],
    (start, end): (usize, usize),
    ctx: &ZigCtx,
) -> Vec<StructLayout> {
    let Some(struct_node) = root
        .descendant_for_byte_range(start, end)
        .filter(|n| n.kind() == "struct_declaration")
    else {
        return Vec::new();
    };
    let mut arg_lists = crate::extra_instantiations(name);
    arg_lists.extend(
        ctx.uses
            .iter()
            .filter(|(n, _)| n == name)
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
        let inst_name = format!("{name}({})", args.join(", "));
        if out.iter().any(|l| l.name == inst_name) {
            continue;
        }
        let mut inst = ctx.clone();
        for ((p, is_type), a) in params.iter().zip(&args) {
            if *is_type {
                inst.type_aliases.insert(p.clone(), a.clone());
            } else {
                inst.consts.insert(p.clone(), a.clone());
            }
        }
        let line = struct_node.start_position().row as u32 + 1;
        if let Some(layout) = parse_struct_declaration(source, struct_node, inst_name, &inst, line)
        {
            out.push(layout);
        }
    }
    out
}

/// Emit a `record_skipped` note if `node` is a function that:
///   - has at least one `comptime` parameter, AND
///   - returns the built-in `type` identifier.
fn note_comptime_generic_fn(source: &str, node: Node<'_>) {
    // Extract function name (first identifier child).
    let fn_name = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|c| c.kind() == "identifier")
        .map(|c| source[c.byte_range()].to_string());

    let Some(fn_name) = fn_name else {
        return;
    };

    // Look for a parameter list containing a comptime parameter.
    let has_comptime_param = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .filter(|c| c.kind() == "parameters")
        .any(|params| {
            (0..params.child_count())
                .filter_map(|j| params.child(j))
                .any(|param| {
                    // A comptime parameter has a child with kind "comptime"
                    (0..param.child_count())
                        .filter_map(|k| param.child(k))
                        .any(|tok| tok.kind() == "comptime")
                })
        });

    if !has_comptime_param {
        return;
    }

    // Check that the return type is the `type` keyword/identifier.
    let returns_type = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .any(|c| {
            // The return type node typically has kind "identifier" or a keyword node.
            // We look for any child whose source text is exactly "type".
            source[c.byte_range()].trim() == "type"
                && matches!(c.kind(), "identifier" | "primitive_type" | "builtin_type")
        });

    if returns_type {
        crate::record_skipped(
            &fn_name,
            "comptime-generic function — returns a struct type; \
             layout depends on type arguments",
        );
    }
}

fn parse_variable_declaration(source: &str, node: Node<'_>, ctx: &ZigCtx) -> Option<StructLayout> {
    let source_line = node.start_position().row as u32 + 1;
    let decl_start_byte = node.start_byte();
    let mut name: Option<String> = None;
    let mut struct_node: Option<Node> = None;
    let mut union_node: Option<Node> = None;

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            "identifier" if name.is_none() => {
                // The first identifier after `const`/`var` is the name
                name = Some(source[child.byte_range()].to_string());
            }
            "struct_declaration" => struct_node = Some(child),
            "union_declaration" => union_node = Some(child),
            _ => {}
        }
    }

    let name = name?;
    let mut layout = if let Some(sn) = struct_node {
        parse_struct_declaration(source, sn, name, ctx, source_line)?
    } else {
        let un = union_node?;
        parse_union_declaration(source, un, name, ctx, source_line)?
    };
    layout.suppressed_findings =
        super::suppress::suppressed_from_preceding_source(source, decl_start_byte);
    Some(layout)
}

/// Parse a Zig `union { ... }` or `union(enum) { ... }` declaration.
///
/// Layout rules:
/// - All fields share the same storage (offset 0), total = max(field sizes).
/// - Tagged unions add a synthetic `__tag` discriminant field; its size is
///   the smallest integer that covers the variant count.
/// - The struct is emitted with `is_union = true`.
fn parse_union_declaration(
    source: &str,
    node: Node<'_>,
    name: String,
    ctx: &ZigCtx,
    source_line: u32,
) -> Option<StructLayout> {
    let arch = ctx.arch;
    let mut is_tagged = false;
    let mut raw_fields: Vec<(String, String, usize, usize, u32)> = Vec::new();
    let mut uncertain_fields: Vec<String> = Vec::new();

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            // `union(enum)` — `enum` keyword is a direct child
            "enum" => is_tagged = true,
            // `union(SomeEnum)` — identifier naming the explicit tag type
            // We detect this by seeing an identifier inside the `(...)` group.
            // Mark it as tagged regardless of the tag type.
            "container_field" => {
                if let Some(f) = parse_container_field(source, child, ctx, &mut uncertain_fields) {
                    raw_fields.push(f);
                }
            }
            _ => {}
        }
    }

    if raw_fields.is_empty() {
        return None;
    }

    // Union layout: all fields at offset 0; total = max field size rounded to alignment.
    let max_size = raw_fields
        .iter()
        .map(|(_, _, sz, _, _)| *sz)
        .max()
        .unwrap_or(0);
    let max_align = raw_fields
        .iter()
        .map(|(_, _, _, al, _)| *al)
        .max()
        .unwrap_or(1);
    let total_size = if max_align > 0 {
        max_size.next_multiple_of(max_align)
    } else {
        max_size
    };

    let mut fields: Vec<Field> = raw_fields
        .into_iter()
        .map(|(fname, type_text, size, align, field_line)| Field {
            name: fname,
            ty: TypeInfo::Primitive {
                name: type_text,
                size,
                align,
            },
            offset: 0,
            size,
            align,
            source_file: None,
            source_line: Some(field_line),
            access: padlock_core::ir::AccessPattern::Unknown,
        })
        .collect();

    // Tagged union: add a synthetic `__tag` discriminant field.
    // Its size is the smallest integer type that holds all variant indices.
    if is_tagged {
        let n = fields.len();
        let tag_size: usize = if n <= 256 {
            1
        } else if n <= 65536 {
            2
        } else {
            4
        };
        fields.push(Field {
            name: "__tag".to_string(),
            ty: TypeInfo::Primitive {
                name: format!("u{}", tag_size * 8),
                size: tag_size,
                align: tag_size,
            },
            offset: total_size, // tag lives after the union payload
            size: tag_size,
            align: tag_size,
            source_file: None,
            source_line: None,
            access: padlock_core::ir::AccessPattern::Unknown,
        });
    }

    let struct_align = max_align; // tag alignment is usually smaller than payload

    let final_size = if is_tagged {
        let tag_size = fields.last().map(|f| f.size).unwrap_or(0);
        (total_size + tag_size).next_multiple_of(struct_align.max(1))
    } else {
        total_size
    };

    Some(StructLayout {
        name,
        total_size: final_size,
        align: struct_align,
        fields,
        source_file: None,
        source_line: Some(source_line),
        arch,
        is_packed: false,
        is_union: true,
        is_repr_rust: false,
        suppressed_findings: Vec::new(), // set by parse_variable_declaration
        uncertain_fields,
        forced_align: None,
        pack_limit: None,
        cpp: None,
    })
}

fn parse_struct_declaration(
    source: &str,
    node: Node<'_>,
    name: String,
    ctx: &ZigCtx,
    source_line: u32,
) -> Option<StructLayout> {
    let arch = ctx.arch;
    let mut is_packed = false;
    let mut is_extern = false;
    // (field_name, type_text, size, align, source_line)
    let mut raw_fields: Vec<(String, String, usize, usize, u32)> = Vec::new();
    let mut uncertain_fields: Vec<String> = Vec::new();

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            "packed" => is_packed = true,
            "extern" => is_extern = true,
            "container_field" => {
                if let Some(f) = parse_container_field(source, child, ctx, &mut uncertain_fields) {
                    if is_zig_comptime_type(&f.1) {
                        uncertain_fields.push(f.0.clone());
                    }
                    raw_fields.push(f);
                }
            }
            _ => {}
        }
    }

    if raw_fields.is_empty() {
        return None;
    }

    // Regular Zig structs have implementation-defined layout (reordering allowed).
    // Only extern and packed structs have stable C-compatible / bit-exact layout.
    // For analysis purposes we simulate the declared order for all variants,
    // since that is what the developer sees and intends to reason about.
    let mut offset = 0usize;
    let mut struct_align = 1usize;
    let mut fields: Vec<Field> = Vec::new();

    if is_packed {
        // Packed structs: fields are bit-packed with no inter-field padding.
        // Track the running bit offset; convert to byte offset for Field.offset.
        // Total struct size = ceil(total_bits / 8).
        let mut bit_offset = 0usize;
        for (fname, type_text, _byte_size, _byte_align, field_line) in &raw_fields {
            let bit_width = zig_bits(type_text, ctx, 0);
            let byte_offset = bit_offset / 8;
            let byte_size = bit_width
                .div_ceil(8)
                .max(if bit_width == 0 { 0 } else { 1 });
            // A named type's bits are already counted here; label it so the
            // nested-struct resolver does not re-size it in bytes.
            let display = if ctx.packed.contains_key(type_text.as_str())
                || ctx.enums.contains_key(type_text.as_str())
            {
                format!("{type_text} ({bit_width} bits)")
            } else {
                type_text.clone()
            };
            fields.push(Field {
                name: fname.clone(),
                ty: TypeInfo::Primitive {
                    name: display,
                    size: byte_size,
                    align: 1,
                },
                offset: byte_offset,
                size: byte_size,
                align: 1,
                source_file: None,
                source_line: Some(*field_line),
                access: padlock_core::ir::AccessPattern::Unknown,
            });
            bit_offset += bit_width;
        }
        // A packed struct is stored as one integer of `bit_offset` bits, so its
        // size and alignment are that integer's ABI size (`packed struct { a: u8,
        // b: u32 }` is a `u40`, which occupies 8 bytes).
        (offset, struct_align) = int_abi_size_align(bit_offset);
    } else {
        for (fname, type_text, size, align, field_line) in raw_fields {
            if align > 0 {
                offset = offset.next_multiple_of(align);
            }
            struct_align = struct_align.max(align);
            fields.push(Field {
                name: fname,
                ty: TypeInfo::Primitive {
                    name: type_text,
                    size,
                    align,
                },
                offset,
                size,
                align,
                source_file: None,
                source_line: Some(field_line),
                access: padlock_core::ir::AccessPattern::Unknown,
            });
            offset += size;
        }
        if struct_align > 0 {
            offset = offset.next_multiple_of(struct_align);
        }
    }

    let mut layout = StructLayout {
        name,
        total_size: offset,
        align: struct_align,
        fields,
        source_file: None,
        source_line: Some(source_line),
        arch,
        is_packed,
        is_union: false,
        // Regular Zig structs have implementation-defined field order (the
        // compiler may reorder for optimal packing). Only `extern struct` and
        // `packed struct` guarantee a stable, C-compatible layout. Flagging
        // regular structs mirrors what `is_repr_rust` does for plain Rust
        // structs: findings are downgraded one severity level so they remain
        // visible without over-alarming on padding the compiler already removes.
        is_repr_rust: !is_extern && !is_packed,
        suppressed_findings: Vec::new(), // set by parse_variable_declaration
        uncertain_fields,
        // A packed struct's size is its backing integer's ABI size.
        forced_align: is_packed.then_some(struct_align),
        pack_limit: None,
        cpp: None,
    };
    crate::apply_compiler_order(&mut layout);
    Some(layout)
}

/// Parse a `container_field` node and return `(name, type_text, size, align, source_line)`.
/// Fields whose size is a best guess are appended to `uncertain`.
fn parse_container_field(
    source: &str,
    node: Node<'_>,
    ctx: &ZigCtx,
    uncertain: &mut Vec<String>,
) -> Option<(String, String, usize, usize, u32)> {
    let arch = ctx.arch;
    let mut field_name: Option<String> = None;
    let mut type_text: Option<String> = None;
    let mut sized: Option<ZSized> = None;
    let mut align_override: Option<usize> = None;

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            "identifier" if field_name.is_none() => {
                field_name = Some(source[child.byte_range()].to_string());
            }
            "builtin_type" | "pointer_type" | "nullable_type" | "slice_type" | "array_type"
            | "error_union" | "error_union_type" | "builtin_function" | "identifier"
            | "call_expression" => {
                // A second identifier is a type name (e.g. a named struct or enum).
                let text = node_text(source, child);
                let sz = type_node_size_align(source, child, ctx);
                // A bound parameter or `const P = Pair(…)` alias is shown (and
                // resolved) as what it stands for.
                type_text = Some(match (&sz.named, ctx.type_aliases.get(text)) {
                    (Some((n, 1)), _) => n.clone(),
                    (_, Some(bound)) => bound.clone(),
                    _ => text.to_string(),
                });
                sized = Some(sz);
            }
            // `align(N)` on a field overrides its natural alignment.
            "byte_alignment" => {
                align_override = named_children(child)
                    .first()
                    .and_then(|n| node_text(source, *n).parse().ok());
            }
            _ => {}
        }
    }

    // Discard fields with empty names — tree-sitter-zig emits a zero-length
    // identifier node for `union {}` (empty union body), which is not a real field.
    let name = field_name.filter(|n| !n.is_empty())?;
    let mut ty = type_text.unwrap_or_else(|| "anyopaque".to_string());
    let sized = sized.unwrap_or_else(|| ZSized::plain((arch.pointer_size, arch.pointer_size)));
    if sized.uncertain {
        uncertain.push(name.clone());
    }
    // An array of a local struct is renamed `Name[count]` so the nested-struct
    // resolver can substitute the real element size.
    if let Some((elem, count)) = &sized.named
        && ty.starts_with('[')
    {
        ty = format!("{elem}[{count}]");
    }
    let field_line = node.start_position().row as u32 + 1;

    Some((
        name,
        ty,
        sized.size,
        align_override.unwrap_or(sized.align),
        field_line,
    ))
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_zig(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let tree = PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| anyhow::anyhow!("tree-sitter-zig parse failed"))?;
    Ok(extract_structs(source, tree.root_node(), arch))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn parse_simple_zig_struct() {
        let src = "const Point = struct { x: u32, y: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
        assert_eq!(layouts[0].fields.len(), 2);
        assert_eq!(layouts[0].total_size, 8);
    }

    #[test]
    fn zig_layout_with_padding() {
        let src = "const T = struct { a: bool, b: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0); // bool at 0
        assert_eq!(l.fields[1].offset, 8); // u64 at 8 (7 bytes padding)
        assert_eq!(l.total_size, 16);
    }

    #[test]
    fn zig_packed_struct_no_padding() {
        let src = "const Packed = packed struct { a: u8, b: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert!(l.is_packed);
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 1); // immediately after u8, no padding
        // 40 bits are stored as a u40, whose ABI size is 8 (verified with @sizeOf).
        assert_eq!(l.total_size, 8);
        assert_eq!(l.align, 8);
    }

    #[test]
    fn zig_extern_struct_detected() {
        let src = "const Extern = extern struct { x: i32, y: f64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        // extern struct has C layout: x at 0 (4B), 4B pad, y at 8 (8B)
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 8);
        assert_eq!(l.total_size, 16);
    }

    #[test]
    fn zig_pointer_field_is_pointer_sized() {
        let src = "const S = struct { ptr: *u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn zig_optional_pointer_is_pointer_sized() {
        let src = "const S = struct { opt: ?*u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    #[test]
    fn zig_slice_is_two_words() {
        let src = "const S = struct { buf: []u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // ptr + len
    }

    #[test]
    fn zig_usize_follows_arch() {
        let src = "const S = struct { n: usize };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    #[test]
    fn zig_multiple_structs_parsed() {
        let src = "const A = struct { x: u8 };\nconst B = struct { y: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 2);
        assert!(layouts.iter().any(|l| l.name == "A"));
        assert!(layouts.iter().any(|l| l.name == "B"));
    }

    #[test]
    fn zig_array_field_size() {
        let src = "const S = struct { buf: [4]u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16); // 4 * 4
    }

    // ── union / tagged union ──────────────────────────────────────────────────

    #[test]
    fn zig_bare_union_parsed_as_union() {
        let src = "const U = union { a: u8, b: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.name, "U");
        assert!(l.is_union, "union should have is_union=true");
    }

    #[test]
    fn zig_bare_union_total_size_is_max_field() {
        // a: u8 (1B), b: u32 (4B) → max = 4B, aligned to 4
        let src = "const U = union { a: u8, b: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.total_size, 4);
    }

    #[test]
    fn zig_union_all_fields_at_offset_zero() {
        let src = "const U = union { a: u8, b: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        for field in &l.fields {
            assert_eq!(
                field.offset, 0,
                "union field '{}' should be at offset 0",
                field.name
            );
        }
    }

    #[test]
    fn zig_tagged_union_has_tag_field() {
        let src = "const T = union(enum) { ok: u32, err: void };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert!(
            l.fields.iter().any(|f| f.name == "__tag"),
            "tagged union should have a synthetic __tag field"
        );
    }

    #[test]
    fn zig_tagged_union_size_includes_tag() {
        // ok: u32 (4B), err: void (0B) → payload = 4B, tag = 1B (2 variants ≤ 256)
        // total = (4 + 1).next_multiple_of(4) = 8B
        let src = "const T = union(enum) { ok: u32, err: void };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        // payload (4B) + tag (1B) → 5B → rounded to align 4 = 8B
        assert_eq!(l.total_size, 8);
    }

    #[test]
    fn zig_union_with_largest_field_u64() {
        // a: u8 (1B), b: u64 (8B), c: u32 (4B) → max = 8B, align = 8
        let src = "const U = union { a: u8, b: u64, c: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.total_size, 8);
        assert_eq!(l.align, 8);
    }

    #[test]
    fn zig_struct_and_union_in_same_file() {
        let src = "const S = struct { x: u32 };\nconst U = union { a: u8, b: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 2);
        assert!(layouts.iter().any(|l| l.name == "S" && !l.is_union));
        assert!(layouts.iter().any(|l| l.name == "U" && l.is_union));
    }

    // ── bad weather: unions ───────────────────────────────────────────────────

    #[test]
    fn zig_empty_union_returns_none() {
        // Empty union body → no layout produced
        let src = "const E = union {};";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert!(layouts.is_empty(), "empty union should produce no layout");
    }

    #[test]
    fn zig_union_no_padding_finding() {
        // Unions should never report inter-field padding (all fields at offset 0)
        let src = "const U = union { a: u8, b: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let gaps = padlock_core::ir::find_padding(&layouts[0]);
        assert!(
            gaps.is_empty(),
            "unions should have no padding gaps: {:?}",
            gaps
        );
    }

    // ── type-table tests ──────────────────────────────────────────────────────

    #[test]
    fn zig_c_interop_types_correct_size() {
        assert_eq!(zig_type_size_align("c_char", &X86_64_SYSV), (1, 1));
        assert_eq!(zig_type_size_align("c_short", &X86_64_SYSV), (2, 2));
        assert_eq!(zig_type_size_align("c_ushort", &X86_64_SYSV), (2, 2));
        assert_eq!(zig_type_size_align("c_int", &X86_64_SYSV), (4, 4));
        assert_eq!(zig_type_size_align("c_uint", &X86_64_SYSV), (4, 4));
        // c_long is pointer-sized on LP64 (Linux/macOS x86-64)
        assert_eq!(zig_type_size_align("c_long", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_ulong", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_longlong", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_ulonglong", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_float", &X86_64_SYSV), (4, 4));
        assert_eq!(zig_type_size_align("c_double", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("c_longdouble", &X86_64_SYSV), (16, 16));
    }

    #[test]
    fn zig_arbitrary_width_integers() {
        // u1 → 1B (ceil(1/8)=1), aligned to 1
        assert_eq!(zig_type_size_align("u1", &X86_64_SYSV), (1, 1));
        // u3 → 1B (ceil(3/8)=1)
        assert_eq!(zig_type_size_align("u3", &X86_64_SYSV), (1, 1));
        // u9 → 2B (ceil(9/8)=2), aligned to 2
        assert_eq!(zig_type_size_align("u9", &X86_64_SYSV), (2, 2));
        // The ABI size is the byte count rounded up to a power of two (verified
        // with @sizeOf/@alignOf on zig 0.15): u24 → 4B, u48 → 8B.
        assert_eq!(zig_type_size_align("u24", &X86_64_SYSV), (4, 4));
        assert_eq!(zig_type_size_align("u48", &X86_64_SYSV), (8, 8));
        assert_eq!(zig_type_size_align("u65", &X86_64_SYSV), (16, 16));
        assert_eq!(zig_type_size_align("u129", &X86_64_SYSV), (32, 16));
        // i7 → 1B, aligned to 1
        assert_eq!(zig_type_size_align("i7", &X86_64_SYSV), (1, 1));
    }

    #[test]
    fn zig_struct_with_c_interop_types() {
        // A Zig extern struct using C interop types
        let src = "const Header = extern struct { version: c_uint, length: c_ushort, flags: u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.fields[0].size, 4); // c_uint
        assert_eq!(l.fields[1].size, 2); // c_ushort
        assert_eq!(l.fields[2].size, 1); // u8
    }

    // ── comptime fields ───────────────────────────────────────────────────────

    #[test]
    fn zig_comptime_type_field_is_flagged_uncertain() {
        // `type` fields are compile-time-only; runtime size is unknown → uncertain.
        let src = "const Meta = struct { tag: type, value: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Meta").expect("Meta");
        assert!(
            l.uncertain_fields.contains(&"tag".to_string()),
            "comptime `type` field must be flagged as uncertain"
        );
        assert!(
            !l.uncertain_fields.contains(&"value".to_string()),
            "normal u64 field must not be flagged"
        );
    }

    #[test]
    fn zig_comptime_int_field_is_flagged_uncertain() {
        let src = "const S = struct { n: comptime_int, x: u32 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        assert!(l.uncertain_fields.contains(&"n".to_string()));
        assert!(!l.uncertain_fields.contains(&"x".to_string()));
    }

    #[test]
    fn zig_comptime_generic_fn_is_detected() {
        crate::SKIPPED_COLLECTOR.with(|cell| cell.borrow_mut().clear());
        let src =
            "pub fn ArrayList(comptime T: type) type { return struct { items: []T, len: usize }; }";
        let _layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        let skipped = crate::take_skipped();
        assert!(
            skipped.iter().any(|s| s.name == "ArrayList"),
            "comptime-generic function must be recorded as skipped; got: {skipped:?}"
        );
    }

    // ── is_repr_rust flag for layout stability ────────────────────────────────

    #[test]
    fn regular_zig_struct_is_repr_rust() {
        // Regular Zig structs have implementation-defined field order — the
        // compiler may reorder fields. Flagged with is_repr_rust so findings
        // are downgraded one severity level.
        let src = "const S = struct { a: u8, b: u64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts[0].is_repr_rust,
            "regular Zig struct must set is_repr_rust (compiler may reorder)"
        );
    }

    #[test]
    fn extern_zig_struct_is_not_repr_rust() {
        // extern struct guarantees C-compatible layout — no reordering allowed.
        let src = "const E = extern struct { x: i32, y: f64 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert!(
            !layouts[0].is_repr_rust,
            "extern struct has stable layout, must not set is_repr_rust"
        );
    }

    #[test]
    fn packed_zig_struct_is_not_repr_rust() {
        // packed struct guarantees bit-exact layout — no reordering allowed.
        let src = "const P = packed struct { a: u8, b: u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert!(
            !layouts[0].is_repr_rust,
            "packed struct has stable layout, must not set is_repr_rust"
        );
    }

    // ── @Vector sizing ────────────────────────────────────────────────────────

    #[test]
    fn zig_vector_f32x4_size() {
        // @Vector(4, f32) → 4 × 4B = 16B, aligned to 16
        let src = "const S = extern struct { v: @Vector(4, f32) };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let f = &layouts[0].fields[0];
        assert_eq!(f.size, 16);
        assert_eq!(f.align, 16);
    }

    #[test]
    fn zig_vector_i32x8_size() {
        // @Vector(8, i32) → 8 × 4B = 32B, aligned to 32
        let src = "const S = extern struct { v: @Vector(8, i32) };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let f = &layouts[0].fields[0];
        assert_eq!(f.size, 32);
        assert_eq!(f.align, 32);
    }

    #[test]
    fn zig_struct_with_vector_and_scalar() {
        // Layout: @Vector(4, f32)(16B, align 16) + padding + u8 field
        let src = "const S = extern struct { v: @Vector(4, f32), x: u8 };";
        let layouts = parse_zig(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].fields[0].size, 16);
        // u8 follows the vector at offset 16 (no padding needed after aligned vector)
        assert_eq!(layouts[0].fields[1].offset, 16);
    }

    // ── field shapes (verified against `zig run` @sizeOf/@offsetOf) ───────────

    fn zig(src: &str, name: &str) -> StructLayout {
        crate::parse_source_str(src, &crate::SourceLanguage::Zig, &X86_64_SYSV)
            .unwrap()
            .into_iter()
            .find(|l| l.name == name)
            .unwrap_or_else(|| panic!("{name} not parsed"))
    }

    fn zoff(l: &StructLayout, field: &str) -> usize {
        l.fields.iter().find(|f| f.name == field).unwrap().offset
    }

    #[test]
    fn array_length_may_be_a_const_expression() {
        let src =
            "const N = 8; const S = extern struct { a: u8, b: [N * 2 + @sizeOf(u64)]u8, c: u64 };";
        let l = zig(src, "S");
        assert_eq!(zoff(&l, "c"), 32);
        assert!(l.uncertain_fields.is_empty());
    }

    #[test]
    fn unevaluable_array_length_is_uncertain() {
        let l = zig(
            "const S = extern struct { a: u8, b: [unknown_len]u8, c: u8 };",
            "S",
        );
        assert_eq!(l.uncertain_fields, vec!["b".to_string()]);
    }

    #[test]
    fn array_of_local_struct_is_scaled() {
        let src = "const In = extern struct { x: u8, y: u32 }; const S = extern struct { a: u8, b: [2]In, c: u8 };";
        let l = zig(src, "S");
        assert_eq!(zoff(&l, "c"), 20);
        assert_eq!(l.total_size, 24);
    }

    #[test]
    fn enum_fields_use_their_backing_integer() {
        let src = "const E = enum(u32) { a, b }; const F = enum { a, b, c };
            const S = extern struct { a: u8, e: E, f: F, c: u8 };";
        let l = zig(src, "S");
        assert_eq!(zoff(&l, "e"), 4);
        assert_eq!(zoff(&l, "f"), 8);
        assert_eq!(zoff(&l, "c"), 9);
    }

    #[test]
    fn field_align_attribute_overrides_alignment() {
        let l = zig(
            "const S = extern struct { a: u8, b: u32 align(16), c: u8 };",
            "S",
        );
        assert_eq!(zoff(&l, "b"), 16);
        assert_eq!(l.total_size, 32);
        assert_eq!(l.align, 16);
    }

    #[test]
    fn packed_struct_size_is_its_backing_integer() {
        let l = zig(
            "const P = packed struct { a: u3, b: u5, c: u8, d: u1 };",
            "P",
        );
        assert_eq!(l.total_size, 4); // u17
    }

    #[test]
    fn error_unions_hold_the_payload_and_a_16_bit_code() {
        let by = |ty: &str| zig(&format!("const S = struct {{ v: {ty} }};"), "S").total_size;
        assert_eq!(by("anyerror!u8"), 4);
        assert_eq!(by("anyerror!u32"), 8);
        assert_eq!(by("anyerror!u64"), 16);
        assert_eq!(by("anyerror!void"), 2);
    }

    #[test]
    fn optional_slice_has_no_tag() {
        let l = zig("const S = struct { v: ?[]u8 };", "S");
        assert_eq!(l.total_size, 16);
    }
}
