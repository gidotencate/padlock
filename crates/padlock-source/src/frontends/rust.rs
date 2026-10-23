// padlock-source/src/frontends/rust.rs
//
// Extracts struct layouts from Rust source using syn + the Visit API.
// Sizes are approximated from type names using the target arch config.
// Only repr(C) / repr(packed) / plain structs are handled; generics are opaque.

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
use quote::ToTokens;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use syn::{Fields, ItemEnum, ItemStruct, Type, visit::Visit};

// ── attribute guard extraction ────────────────────────────────────────────────

/// Extract a lock guard name from field attributes.
///
/// Recognised forms:
/// - `#[lock_protected_by = "mu"]`
/// - `#[protected_by = "mu"]`
/// - `#[guarded_by("mu")]` or `#[guarded_by(mu)]`
/// - `#[pt_guarded_by("mu")]` or `#[pt_guarded_by(mu)]` (pointer variant)
pub fn extract_guard_from_attrs(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        let path = attr.path();
        // Name-value form: #[lock_protected_by = "mu"] / #[protected_by = "mu"]
        if (path.is_ident("lock_protected_by") || path.is_ident("protected_by"))
            && let syn::Meta::NameValue(nv) = &attr.meta
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
        {
            return Some(s.value());
        }
        // List form: #[guarded_by("mu")] / #[guarded_by(mu)] / #[pt_guarded_by(...)]
        if path.is_ident("guarded_by") || path.is_ident("pt_guarded_by") {
            // Try string literal first
            if let Ok(s) = attr.parse_args::<syn::LitStr>() {
                return Some(s.value());
            }
            // Fall back to bare identifier
            if let Ok(id) = attr.parse_args::<syn::Ident>() {
                return Some(id.to_string());
            }
        }
    }
    None
}

// ── file-level context ────────────────────────────────────────────────────────

/// What a pre-pass over the file learns about its own items, so field types can be
/// sized without a type checker.
struct RustCtx {
    arch: &'static ArchConfig,
    /// `const N: usize = 8;` → name → expression text.
    consts: HashMap<String, String>,
    /// Non-generic `type Alias = T;`.
    aliases: HashMap<String, Type>,
    /// Non-generic local structs and enums: sized later by the nested-struct resolver.
    local_types: HashSet<String>,
    /// Set while sizing a field when the result is a best guess (unevaluable array
    /// length, tuple containing a local type, …).
    uncertain: Cell<bool>,
    /// Alias / const recursion guard.
    depth: Cell<u8>,
}

impl RustCtx {
    fn new(arch: &'static ArchConfig) -> Self {
        RustCtx {
            arch,
            consts: HashMap::new(),
            aliases: HashMap::new(),
            local_types: HashSet::new(),
            uncertain: Cell::new(false),
            depth: Cell::new(0),
        }
    }
}

struct ItemCollector<'a> {
    ctx: &'a mut RustCtx,
}

impl<'ast> Visit<'ast> for ItemCollector<'_> {
    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.ctx
            .consts
            .insert(node.ident.to_string(), expr_text(&node.expr));
        syn::visit::visit_item_const(self, node);
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if node.generics.params.is_empty() {
            self.ctx
                .aliases
                .insert(node.ident.to_string(), (*node.ty).clone());
        }
        syn::visit::visit_item_type(self, node);
    }

    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        let generic = node
            .generics
            .params
            .iter()
            .any(|p| matches!(p, syn::GenericParam::Type(_) | syn::GenericParam::Const(_)));
        if !generic {
            self.ctx.local_types.insert(node.ident.to_string());
        }
        syn::visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        let generic = node
            .generics
            .params
            .iter()
            .any(|p| matches!(p, syn::GenericParam::Type(_) | syn::GenericParam::Const(_)));
        if !generic {
            self.ctx.local_types.insert(node.ident.to_string());
        }
        syn::visit::visit_item_enum(self, node);
    }
}

/// Size of `fields` laid out the way rustc lays out a `repr(Rust)` aggregate
/// (tuples, std structs): largest alignment first.
fn reordered_layout(fields: &[(usize, usize)]) -> (usize, usize) {
    let mut sorted = fields.to_vec();
    sorted.sort_by_key(|f| std::cmp::Reverse(f.1));
    let (mut offset, mut align) = (0usize, 1usize);
    for (size, a) in sorted {
        let a = a.max(1);
        offset = offset.next_multiple_of(a) + size;
        align = align.max(a);
    }
    (offset.next_multiple_of(align), align)
}

fn first_type_arg(seg: Option<&syn::PathSegment>) -> Option<&Type> {
    let syn::PathArguments::AngleBracketed(ab) = &seg?.arguments else {
        return None;
    };
    ab.args.iter().find_map(|a| match a {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

/// `dyn Trait`, `[T]` and `str` are unsized: a pointer to one is two words.
fn is_unsized_pointee(ty: &Type) -> bool {
    match ty {
        Type::TraitObject(_) | Type::Slice(_) => true,
        Type::Path(tp) => tp.path.is_ident("str"),
        Type::Paren(p) => is_unsized_pointee(&p.elem),
        _ => false,
    }
}

/// Source text of a constant expression. `quote` prints `size_of :: < u64 > ()`;
/// the evaluator wants `size_of::<u64>()`.
fn expr_text(expr: &syn::Expr) -> String {
    expr.to_token_stream()
        .to_string()
        .replace(" :: ", "::")
        .replace("::< ", "::<")
}

fn eval_rust_len(expr: &syn::Expr, ctx: &RustCtx) -> Option<usize> {
    if ctx.depth.get() > 8 {
        return None;
    }
    let lookup = |id: &str| {
        // `Self::N` / `Type::N` are associated consts we cannot see; plain `N` only.
        ctx.consts.get(id).cloned()
    };
    let sizeof = |ty: &str| {
        let ty: Type = syn::parse_str(ty).ok()?;
        ctx.depth.set(ctx.depth.get() + 1);
        let (size, _, _) = rust_type_size_align(&ty, ctx);
        ctx.depth.set(ctx.depth.get() - 1);
        i64::try_from(size).ok()
    };
    let text = expr_text(expr);
    let n = super::consteval::eval(
        &text,
        &super::consteval::Env {
            lookup: &lookup,
            sizeof: &sizeof,
        },
        0,
    )?;
    usize::try_from(n).ok()
}

// ── type resolution ───────────────────────────────────────────────────────────

fn rust_type_size_align(ty: &Type, ctx: &RustCtx) -> (usize, usize, TypeInfo) {
    let arch = ctx.arch;
    match ty {
        Type::Paren(p) => rust_type_size_align(&p.elem, ctx),
        Type::Group(g) => rust_type_size_align(&g.elem, ctx),
        Type::Path(tp) => {
            let seg = tp.path.segments.last();
            let name = seg.map(|s| s.ident.to_string()).unwrap_or_default();

            // Local `type Alias = T;` — size the aliased type.
            if tp.path.segments.len() == 1
                && matches!(seg.map(|s| &s.arguments), Some(syn::PathArguments::None))
                && let Some(target) = ctx.aliases.get(&name)
                && ctx.depth.get() < 8
            {
                ctx.depth.set(ctx.depth.get() + 1);
                let r = rust_type_size_align(target, ctx);
                ctx.depth.set(ctx.depth.get() - 1);
                return r;
            }

            // Mutex<T> / RwLock<T> / OnceLock<T>: std's futex-based implementation
            // is a repr(Rust) struct { lock word(s), poison flag, data: T }.
            // (macOS and Windows use different primitives — use binary analysis there.)
            if matches!(name.as_str(), "Mutex" | "RwLock" | "OnceLock")
                && let Some(inner_ty) = first_type_arg(seg)
            {
                let (is, ia, _) = rust_type_size_align(inner_ty, ctx);
                let parts: Vec<(usize, usize)> = match name.as_str() {
                    "Mutex" => vec![(4, 4), (1, 1), (is, ia)],
                    "RwLock" => vec![(8, 4), (1, 1), (is, ia)],
                    _ => vec![(4, 4), (is, ia)],
                };
                let (size, align) = reordered_layout(&parts);
                return (size, align, TypeInfo::Primitive { name, size, align });
            }

            // Transparent newtypes: size and alignment equal the inner T.
            // Cell<T>, MaybeUninit<T>, UnsafeCell<T>, Wrapping<T>, Saturating<T>,
            // ManuallyDrop<T> all have `#[repr(transparent)]` and add no overhead.
            // Recurse into the first generic type argument when present.
            if matches!(
                name.as_str(),
                "Cell" | "MaybeUninit" | "UnsafeCell" | "Wrapping" | "Saturating" | "ManuallyDrop"
            ) && let Some(inner_ty) = seg.and_then(|s| {
                if let syn::PathArguments::AngleBracketed(ref ab) = s.arguments {
                    ab.args.iter().find_map(|a| {
                        if let syn::GenericArgument::Type(t) = a {
                            Some(t)
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            }) {
                let (size, align, _) = rust_type_size_align(inner_ty, ctx);
                return (size, align, TypeInfo::Primitive { name, size, align });
            }

            // RefCell<T>: borrow counter (isize, pointer_size bytes at offset 0)
            // followed by UnsafeCell<T> at next_multiple_of(pointer_size, T_align).
            // Verified: RefCell<u8>=16B, RefCell<u32>=16B, RefCell<u64>=16B,
            //           RefCell<()>=8B — all on x86-64.
            if name == "RefCell"
                && let Some(inner_ty) = seg.and_then(|s| {
                    if let syn::PathArguments::AngleBracketed(ref ab) = s.arguments {
                        ab.args.iter().find_map(|a| {
                            if let syn::GenericArgument::Type(t) = a {
                                Some(t)
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    }
                })
            {
                let (inner_size, inner_align, _) = rust_type_size_align(inner_ty, ctx);
                let align = inner_align.max(arch.pointer_size);
                let data_offset = arch.pointer_size.next_multiple_of(inner_align.max(1));
                let size = (data_offset + inner_size).next_multiple_of(align);
                return (size, align, TypeInfo::Primitive { name, size, align });
            }

            // Range<T> { start, end }, RangeInclusive<T> { start, end, exhausted: bool }.
            if matches!(name.as_str(), "Range" | "RangeInclusive")
                && let Some(inner_ty) = first_type_arg(seg)
            {
                let (is, ia, _) = rust_type_size_align(inner_ty, ctx);
                let mut parts = vec![(is, ia), (is, ia)];
                if name == "RangeInclusive" {
                    parts.push((1, 1));
                }
                let (size, align) = reordered_layout(&parts);
                return (size, align, TypeInfo::Primitive { name, size, align });
            }

            // Option<T> niche optimisation: when T has a niche (a bit pattern
            // that cannot occur in a valid T), the compiler encodes the None
            // discriminant into that niche without adding a separate tag byte.
            //
            // Recognised niche-capable types (conservative subset):
            //   NonZeroU8/NonZeroI8/… — niche at 0
            //   &T / &mut T          — niche at null pointer
            //   Box<T> / NonNull<T>  — niche at null pointer
            //
            // For all other Option<T> we fall through to pointer-size, which is
            // a reasonable default (exact size requires full type inference).
            if name == "Option"
                && let Some(inner) = seg.and_then(|s| {
                    if let syn::PathArguments::AngleBracketed(ref ab) = s.arguments {
                        ab.args.iter().find_map(|a| {
                            if let syn::GenericArgument::Type(t) = a {
                                Some(t)
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    }
                })
            {
                if let Some((sz, al)) = option_niche_size(inner, ctx) {
                    return (
                        sz,
                        al,
                        TypeInfo::Primitive {
                            name,
                            size: sz,
                            align: al,
                        },
                    );
                }
                // No niche — conservative: inner size + 1-byte discriminant
                // rounded up to inner alignment.
                let (inner_size, inner_align, _) = rust_type_size_align(inner, ctx);
                let sz = if inner_size == 0 {
                    1 // Option<()> / Option<ZST> = 1 byte
                } else {
                    (inner_size + 1).next_multiple_of(inner_align.max(1))
                };
                return (
                    sz,
                    inner_align.max(1),
                    TypeInfo::Primitive {
                        name,
                        size: sz,
                        align: inner_align.max(1),
                    },
                );
            }

            // Box<dyn Trait>, Arc<dyn Trait>, Rc<dyn Trait>, Weak<dyn Trait> are fat
            // pointers: they hold both a data pointer and a vtable pointer (2 words).
            let is_fat = matches!(name.as_str(), "Box" | "Arc" | "Rc" | "Weak" | "NonNull")
                && first_type_arg(seg).is_some_and(is_unsized_pointee);
            let (size, align) = if is_fat {
                (arch.pointer_size * 2, arch.pointer_size)
            } else {
                primitive_size_align(&name, arch)
            };
            // Arc<T>/Rc<T> record only the outer "Arc"/"Rc" name below — the
            // concurrency heuristic (padlock-source::concurrency) matches on
            // substrings like "Mutex"/"RwLock"/"Atomic" in the type name, and
            // without the inner type's leaf identifier, `Arc<Mutex<T>>` is
            // indistinguishable from a plain `Arc<String>`. Append it (one
            // level only, no deep unwrapping) so the heuristic can tell the
            // two apart instead of treating every Arc/Rc field as a
            // concurrency hazard regardless of what's actually inside.
            let name = if matches!(name.as_str(), "Arc" | "Rc")
                && let Some(inner_leaf) = seg.and_then(|s| {
                    if let syn::PathArguments::AngleBracketed(ref ab) = s.arguments {
                        ab.args.iter().find_map(|a| {
                            if let syn::GenericArgument::Type(Type::Path(inner_tp)) = a {
                                inner_tp.path.segments.last().map(|s| s.ident.to_string())
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    }
                }) {
                format!("{name}<{inner_leaf}>")
            } else {
                name
            };
            (size, align, TypeInfo::Primitive { name, size, align })
        }
        Type::Ptr(p) => {
            let s = arch.pointer_size;
            // *const dyn Trait / *mut dyn Trait are fat pointers (data + vtable)
            let is_fat = is_unsized_pointee(&p.elem);
            let sz = if is_fat { s * 2 } else { s };
            (sz, s, TypeInfo::Pointer { size: sz, align: s })
        }
        Type::Reference(r) => {
            let s = arch.pointer_size;
            // &dyn Trait / &mut dyn Trait are fat pointers (data + vtable)
            let is_fat = is_unsized_pointee(&r.elem);
            let sz = if is_fat { s * 2 } else { s };
            (sz, s, TypeInfo::Pointer { size: sz, align: s })
        }
        Type::Tuple(t) if t.elems.is_empty() => {
            // `()` — the unit type. Zero-sized, align 1.
            (
                0,
                1,
                TypeInfo::Primitive {
                    name: "()".to_string(),
                    size: 0,
                    align: 1,
                },
            )
        }
        Type::Tuple(t) => {
            let parts: Vec<(usize, usize)> = t
                .elems
                .iter()
                .map(|e| {
                    // A local struct's size is only known after the resolver runs.
                    if let Type::Path(tp) = e
                        && tp
                            .path
                            .segments
                            .last()
                            .is_some_and(|s| ctx.local_types.contains(&s.ident.to_string()))
                    {
                        ctx.uncertain.set(true);
                    }
                    let (sz, al, _) = rust_type_size_align(e, ctx);
                    (sz, al)
                })
                .collect();
            let (size, align) = reordered_layout(&parts);
            (
                size,
                align,
                TypeInfo::Opaque {
                    name: "(tuple)".to_string(),
                    size,
                    align,
                },
            )
        }
        Type::Array(arr) => {
            let (elem_size, elem_align, elem_ty) = rust_type_size_align(&arr.elem, ctx);
            let count = match eval_rust_len(&arr.len, ctx) {
                Some(n) => n,
                None => {
                    ctx.uncertain.set(true);
                    1
                }
            };
            let size = elem_size.saturating_mul(count);
            // An array of a local struct: hand the resolver `Name[count]` so it
            // can substitute the real element size.
            if let TypeInfo::Primitive { name, .. } = &elem_ty {
                let (base, inner_count) = match name.split_once('[') {
                    Some((b, rest)) => (b, rest.trim_end_matches(']').parse().unwrap_or(1usize)),
                    None => (name.as_str(), 1),
                };
                if ctx.local_types.contains(base) {
                    let total = count.saturating_mul(inner_count);
                    return (
                        size,
                        elem_align,
                        TypeInfo::Primitive {
                            name: format!("{base}[{total}]"),
                            size,
                            align: elem_align,
                        },
                    );
                }
            }
            (
                size,
                elem_align,
                TypeInfo::Array {
                    element: Box::new(elem_ty),
                    count,
                    size,
                    align: elem_align,
                },
            )
        }
        _ => {
            let s = arch.pointer_size;
            (
                s,
                s,
                TypeInfo::Opaque {
                    name: "(unknown)".into(),
                    size: s,
                    align: s,
                },
            )
        }
    }
}

/// Return `Some((size, align))` when `Option<inner>` can use a niche optimisation,
/// meaning the Option occupies no more space than the inner type itself.
///
/// Recognised niche-capable types:
/// - `NonZeroU8` / `NonZeroI8` … `NonZeroUsize` — niche at zero
/// - `&T` / `&mut T` — niche at null pointer (reference is always non-null)
/// - `Box<T>` / `NonNull<T>` / `Arc<T>` / `Rc<T>` — non-null pointer niche
fn option_niche_size(inner: &Type, ctx: &RustCtx) -> Option<(usize, usize)> {
    let ps = ctx.arch.pointer_size;
    match inner {
        Type::Path(tp) => {
            let seg = tp.path.segments.last();
            let name = seg.map(|s| s.ident.to_string()).unwrap_or_default();
            match name.as_str() {
                "NonZeroU8" | "NonZeroI8" => Some((1, 1)),
                "NonZeroU16" | "NonZeroI16" => Some((2, 2)),
                "NonZeroU32" | "NonZeroI32" => Some((4, 4)),
                "NonZeroU64" | "NonZeroI64" => Some((8, 8)),
                "NonZeroU128" | "NonZeroI128" => Some((16, 16)),
                "NonZeroUsize" | "NonZeroIsize" => Some((ps, ps)),
                // `bool` and `char` have invalid bit patterns to spare.
                "bool" => Some((1, 1)),
                "char" => Some((4, 4)),
                // Box<T>, NonNull<T>, Arc<T>, Rc<T> — non-null pointer niche
                // (two words when the pointee is unsized).
                "Box" | "NonNull" | "Arc" | "Rc" => {
                    if first_type_arg(seg).is_some_and(is_unsized_pointee) {
                        Some((ps * 2, ps))
                    } else {
                        Some((ps, ps))
                    }
                }
                // Heap collections keep a non-null pointer (or a capacity with
                // a reserved range), so `Option` of them costs nothing.
                "Vec" | "String" | "PathBuf" | "OsString" => Some((3 * ps, ps)),
                "VecDeque" => Some((4 * ps, ps)),
                "CString" => Some((2 * ps, ps)),
                // Duration keeps its nanoseconds < 1e9, leaving niche values.
                "Duration" => Some((16, 8)),
                _ => {
                    // Local type aliases: look through them.
                    if tp.path.segments.len() == 1
                        && let Some(target) = ctx.aliases.get(&name)
                    {
                        return option_niche_size(target, ctx);
                    }
                    None
                }
            }
        }
        // &T and &mut T — references are always non-null → niche at null
        Type::Reference(r) => {
            if is_unsized_pointee(&r.elem) {
                Some((ps * 2, ps))
            } else {
                Some((ps, ps))
            }
        }
        _ => None,
    }
}

fn primitive_size_align(name: &str, arch: &'static ArchConfig) -> (usize, usize) {
    let ps = arch.pointer_size;
    match name {
        // ── language primitives ───────────────────────────────────────────────
        "bool" | "u8" | "i8" => (1, 1),
        "u16" | "i16" | "f16" => (2, 2),
        "u32" | "i32" | "f32" => (4, 4),
        "u64" | "i64" | "f64" => (8, 8),
        "u128" | "i128" | "f128" => (16, 16),
        "usize" | "isize" => (ps, ps),
        "char" => (4, 4), // Rust char is a Unicode scalar (4 bytes)

        // NonZero integer types — same size/align as the underlying integer.
        // The niche optimisation means Option<NonZeroU8> == 1 byte, but the
        // struct field itself is identical in size to the plain integer.
        "NonZeroU8" | "NonZeroI8" => (1, 1),
        "NonZeroU16" | "NonZeroI16" => (2, 2),
        "NonZeroU32" | "NonZeroI32" => (4, 4),
        "NonZeroU64" | "NonZeroI64" => (8, 8),
        "NonZeroU128" | "NonZeroI128" => (16, 16),
        "NonZeroUsize" | "NonZeroIsize" => (ps, ps),

        // Wrapping<T>, Saturating<T> — transparent newtype over T.
        // The generic arg has already been stripped, so we get the inner
        // primitive name here; if the stripping didn't happen these fall
        // through to pointer-size, which is acceptable.
        "Wrapping" | "Saturating" => (ps, ps),

        // MaybeUninit<T> and UnsafeCell<T> are transparent newtypes —
        // same size as T. Without knowing T we approximate as pointer-size,
        // which is correct for the common case of wrapping a pointer-sized value.
        "MaybeUninit" | "UnsafeCell" => (ps, ps),

        // ── std atomics ───────────────────────────────────────────────────────
        "AtomicBool" | "AtomicU8" | "AtomicI8" => (1, 1),
        "AtomicU16" | "AtomicI16" => (2, 2),
        "AtomicU32" | "AtomicI32" => (4, 4),
        "AtomicU64" | "AtomicI64" => (8, 8),
        "AtomicUsize" | "AtomicIsize" | "AtomicPtr" => (ps, ps),

        // ── heap-allocated collections ────────────────────────────────────────
        // Vec / String / PathBuf: ptr + len + cap (3 words).
        "Vec" | "String" | "OsString" | "PathBuf" => (3 * ps, ps),
        // CString owns a Box<[u8]>: pointer + length.
        "CString" => (2 * ps, ps),
        // VecDeque: head + len + RawVec(ptr + cap) = 4 words.
        // Verified stable: VecDeque<T>::sizeof == 4×pointer_size regardless of T.
        "VecDeque" => (4 * ps, ps),
        // LinkedList / BinaryHeap: 3-word approximation.
        "LinkedList" | "BinaryHeap" => (3 * ps, ps),
        // HashMap / HashSet: backed by hashbrown::RawTable.
        // RawTableInner (4 words) + RandomState (2×u64) = 6 words on x86-64.
        // Implementation detail of hashbrown — use binary analysis for certainty.
        "HashMap" | "HashSet" => (6 * ps, ps),
        // BTreeMap / BTreeSet: Root option (niche ptr, 16B) + length = 3 words.
        "BTreeMap" | "BTreeSet" => (3 * ps, ps),

        // ── single-pointer smart pointers ─────────────────────────────────────
        // Cell<T> is handled as a transparent newtype in rust_type_size_align;
        // this entry is a fallback for bare `Cell` without a type argument.
        "Box" | "Rc" | "Arc" | "Weak" | "NonNull" | "Cell" => (ps, ps),

        // ── interior-mutability / sync wrappers ───────────────────────────────
        // Size depends on T but pointer-size is a reasonable approximation for
        // display purposes; use binary analysis for precise results.
        "RefCell" | "Mutex" | "RwLock" => (ps, ps),

        // ── channels ─────────────────────────────────────────────────────────
        // std::sync::mpsc endpoints are a flavor enum (tag + pointer): two words.
        "Sender" | "Receiver" | "SyncSender" => (ps * 2, ps),
        // Futex-based Condvar / Once are a single AtomicU32.
        "Condvar" | "Once" => (4, 4),

        // ── zero-sized types ──────────────────────────────────────────────────
        "PhantomData" | "PhantomPinned" => (0, 1),

        // ── common fixed-size stdlib types ────────────────────────────────────
        // Duration: u64 secs (8B) + u32 nanos (4B) → 12B + 4B trailing = 16B
        "Duration" => (16, 8),
        "Instant" | "SystemTime" => (16, 8),

        // ── Pin<T> wraps T, pointer-size approximation ────────────────────────
        "Pin" => (ps, ps),

        // ── x86 SSE / AVX / AVX-512 SIMD types ───────────────────────────────
        "__m64" => (8, 8),
        "__m128" | "__m128d" | "__m128i" => (16, 16),
        "__m256" | "__m256d" | "__m256i" => (32, 32),
        "__m512" | "__m512d" | "__m512i" => (64, 64),

        // ── Rust portable SIMD / packed_simd types ────────────────────────────
        "f32x4" | "i32x4" | "u32x4" => (16, 16),
        "f64x2" | "i64x2" | "u64x2" => (16, 16),
        "f32x8" | "i32x8" | "u32x8" => (32, 32),
        "f64x4" | "i64x4" | "u64x4" => (32, 32),
        "f32x16" | "i32x16" | "u32x16" => (64, 64),

        // ── unknown / third-party / generic type params (T, E, …) ────────────
        _ => (ps, ps),
    }
}

// ── struct repr detection ─────────────────────────────────────────────────────

/// `#[repr(packed)]` → `Some(1)`, `#[repr(packed(N))]` → `Some(N)`, otherwise `None`.
fn packed_limit(attrs: &[syn::Attribute]) -> Option<usize> {
    for attr in attrs {
        if !attr.path().is_ident("repr") {
            continue;
        }
        let ts = attr.to_token_stream().to_string();
        if let Some(pos) = ts.find("packed") {
            let after = ts[pos + "packed".len()..].trim_start();
            let n = after
                .strip_prefix('(')
                .map(|r| {
                    r.chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                })
                .and_then(|d| d.parse::<usize>().ok())
                .filter(|n| *n > 0);
            return Some(n.unwrap_or(1));
        }
    }
    None
}

/// Underlying integer width of a fieldless enum from its `#[repr(…)]`, if any:
/// `u8`/`i8` → 1 … `usize` → pointer width, `C` → `c_int` (4).
fn enum_repr_size(attrs: &[syn::Attribute], arch: &'static ArchConfig) -> Option<usize> {
    for attr in attrs {
        if !attr.path().is_ident("repr") {
            continue;
        }
        let ts = attr.to_token_stream().to_string();
        for tok in ts
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
        {
            match tok {
                "u8" | "i8" => return Some(1),
                "u16" | "i16" => return Some(2),
                "u32" | "i32" | "C" => return Some(4),
                "u64" | "i64" => return Some(8),
                "usize" | "isize" => return Some(arch.pointer_size),
                _ => {}
            }
        }
    }
    None
}

/// Returns `true` when the struct has no repr annotation that fixes the layout
/// (`repr(C)`, `repr(packed)`, `repr(transparent)`).  A struct with only
/// `repr(align(N))` still has an unspecified field order — the compiler may
/// reorder fields freely — so it counts as `repr(Rust)` for warning purposes.
fn is_repr_rust(attrs: &[syn::Attribute]) -> bool {
    !attrs.iter().any(|a| {
        if !a.path().is_ident("repr") {
            return false;
        }
        let ts = a.to_token_stream().to_string();
        ts.contains('C') || ts.contains("packed") || ts.contains("transparent")
    })
}

/// Extract the alignment from `#[repr(align(N))]`. Returns `None` if not present.
fn repr_align(attrs: &[syn::Attribute]) -> Option<usize> {
    for attr in attrs {
        if !attr.path().is_ident("repr") {
            continue;
        }
        let ts = attr.to_token_stream().to_string();
        // Look for `align ( N )` in the token stream string.
        // The tokeniser adds spaces: "repr (align (64))" etc.
        if let Some(start) = ts.find("align") {
            let after = ts[start..].trim_start_matches("align").trim_start();
            if after.starts_with('(') {
                let inner = after.trim_start_matches('(');
                let num_str: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = num_str.parse::<usize>()
                    && n > 0
                    && n.is_power_of_two()
                {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn simulate_rust_layout(
    name: String,
    fields: &[(String, Type)],
    pack: Option<usize>,
    forced_align: Option<usize>,
    ctx: &RustCtx,
) -> StructLayout {
    let arch = ctx.arch;
    let packed = pack == Some(1);
    let mut offset = 0usize;
    let mut struct_align = 1usize;
    let mut out_fields: Vec<Field> = Vec::new();
    let mut uncertain_fields: Vec<String> = Vec::new();

    for (fname, ty) in fields {
        ctx.uncertain.set(false);
        let (size, align, type_info) = rust_type_size_align(ty, ctx);
        if ctx.uncertain.replace(false) {
            uncertain_fields.push(fname.clone());
        }
        let effective_align = match pack {
            Some(limit) => align.min(limit),
            None => align,
        };

        if effective_align > 0 {
            offset = offset.next_multiple_of(effective_align);
        }
        struct_align = struct_align.max(effective_align);

        out_fields.push(Field {
            name: fname.clone(),
            ty: type_info,
            offset,
            size,
            align: effective_align,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        });
        offset += size;
    }

    // Apply repr(align(N)): raise minimum alignment and add trailing padding.
    if let Some(fa) = forced_align
        && fa > struct_align
    {
        struct_align = fa;
    }

    // `repr(packed)` has no trailing padding; `repr(packed(N))` still pads to its
    // (capped) alignment.
    if !packed && struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }

    StructLayout {
        name,
        total_size: offset,
        align: struct_align,
        fields: out_fields,
        source_file: None,
        source_line: None,
        arch,
        is_packed: packed,
        is_union: false,
        is_repr_rust: false, // callers override this after construction
        suppressed_findings: Vec::new(), // callers may override after construction
        uncertain_fields,
    }
}

// ── visitor ───────────────────────────────────────────────────────────────────

struct StructVisitor<'src> {
    ctx: RustCtx,
    layouts: Vec<StructLayout>,
    source: &'src str,
}

impl<'ast, 'src> Visit<'ast> for StructVisitor<'src> {
    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        syn::visit::visit_item_struct(self, node); // recurse into nested items

        // Generic structs with type or const parameters (e.g. `struct Foo<T>`)
        // cannot be accurately laid out without knowing the concrete arguments.
        // Lifetime parameters (e.g. `struct Cache<'a>`) do not affect layout
        // and are safe to ignore — the struct is still analyzable.
        let has_non_lifetime_generics = node
            .generics
            .params
            .iter()
            .any(|p| matches!(p, syn::GenericParam::Type(_) | syn::GenericParam::Const(_)));
        if has_non_lifetime_generics {
            let name = node.ident.to_string();
            crate::record_skipped(
                &name,
                "generic struct — layout depends on type arguments; \
                 use binary analysis for accurate results",
            );
            return;
        }

        let name = node.ident.to_string();
        let pack = packed_limit(&node.attrs);
        let forced_align = repr_align(&node.attrs);

        // Collect (field_name, type, optional_guard, source_line)
        let fields: Vec<(String, Type, Option<String>, u32)> = match &node.fields {
            Fields::Named(nf) => nf
                .named
                .iter()
                .map(|f| {
                    let fname = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
                    let guard = extract_guard_from_attrs(&f.attrs);
                    let line = f
                        .ident
                        .as_ref()
                        .map(|i| i.span().start().line as u32)
                        .unwrap_or(0);
                    (fname, f.ty.clone(), guard, line)
                })
                .collect(),
            Fields::Unnamed(uf) => uf
                .unnamed
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let guard = extract_guard_from_attrs(&f.attrs);
                    // Unnamed fields don't have an ident span; use 0 as a sentinel.
                    (format!("_{i}"), f.ty.clone(), guard, 0u32)
                })
                .collect(),
            Fields::Unit => vec![],
        };

        let name_ty: Vec<(String, Type)> = fields
            .iter()
            .map(|(n, t, _, _)| (n.clone(), t.clone()))
            .collect();
        let mut layout = simulate_rust_layout(name, &name_ty, pack, forced_align, &self.ctx);
        let struct_line = node.ident.span().start().line as u32;
        layout.source_line = Some(struct_line);
        layout.is_repr_rust = is_repr_rust(&node.attrs);
        layout.suppressed_findings =
            super::suppress::suppressed_from_source_line(self.source, struct_line);

        // Apply explicit guard annotations and field source lines.
        for (i, (_, _, guard, field_line)) in fields.iter().enumerate() {
            if *field_line > 0 {
                layout.fields[i].source_line = Some(*field_line);
            }
            if let Some(g) = guard {
                layout.fields[i].access = AccessPattern::Concurrent {
                    guard: Some(g.clone()),
                    is_atomic: false,
                    is_annotated: true,
                };
            }
        }

        self.layouts.push(layout);
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        syn::visit::visit_item_enum(self, node);

        // Skip enums with type or const parameters. Lifetime parameters don't
        // affect layout, so enums with only lifetime params are still analyzed.
        let has_non_lifetime_generics = node
            .generics
            .params
            .iter()
            .any(|p| matches!(p, syn::GenericParam::Type(_) | syn::GenericParam::Const(_)));
        if has_non_lifetime_generics {
            let name = node.ident.to_string();
            crate::record_skipped(
                &name,
                "generic enum — layout depends on type arguments; \
                 use binary analysis for accurate results",
            );
            return;
        }

        let name = node.ident.to_string();
        let n_variants = node.variants.len();
        if n_variants == 0 {
            return;
        }

        // Discriminant size: an explicit `#[repr(u8/u16/u32/u64/C)]` wins; otherwise
        // the smallest integer that fits the variant count (what rustc picks).
        let explicit_repr = enum_repr_size(&node.attrs, self.ctx.arch);
        let disc_size: usize = if let Some(n) = explicit_repr {
            n
        } else if n_variants == 1
            && node
                .variants
                .iter()
                .all(|v| matches!(v.fields, Fields::Unit))
        {
            // A fieldless enum with a single variant carries no information.
            0
        } else if n_variants <= 256 {
            1
        } else if n_variants <= 65536 {
            2
        } else {
            4
        };

        // Check if all variants are unit (C-like enum, no payload)
        let all_unit = node
            .variants
            .iter()
            .all(|v| matches!(v.fields, Fields::Unit));

        if all_unit {
            // Pure discriminant — no payload storage
            let enum_line = node.ident.span().start().line as u32;
            let layout = StructLayout {
                name,
                total_size: disc_size,
                align: disc_size.max(1),
                fields: vec![Field {
                    name: "__discriminant".to_string(),
                    ty: TypeInfo::Primitive {
                        name: format!("u{}", disc_size * 8),
                        size: disc_size,
                        align: disc_size.max(1),
                    },
                    offset: 0,
                    size: disc_size,
                    align: disc_size.max(1),
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                }],
                source_file: None,
                source_line: Some(enum_line),
                arch: self.ctx.arch,
                is_packed: false,
                is_union: false,
                is_repr_rust: is_repr_rust(&node.attrs),
                suppressed_findings: super::suppress::suppressed_from_source_line(
                    self.source,
                    enum_line,
                ),
                uncertain_fields: Vec::new(),
            };
            self.layouts.push(layout);
            return;
        }

        // Data enum: find the maximum variant payload size and alignment.
        let mut max_payload_size = 0usize;
        let mut max_payload_align = 1usize;

        for variant in &node.variants {
            let var_fields: Vec<(String, Type)> = match &variant.fields {
                Fields::Named(nf) => nf
                    .named
                    .iter()
                    .map(|f| {
                        let n = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
                        (n, f.ty.clone())
                    })
                    .collect(),
                Fields::Unnamed(uf) => uf
                    .unnamed
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (format!("_{i}"), f.ty.clone()))
                    .collect(),
                Fields::Unit => vec![],
            };

            if !var_fields.is_empty() {
                let var_layout =
                    simulate_rust_layout(String::new(), &var_fields, None, None, &self.ctx);
                if var_layout.total_size > max_payload_size {
                    max_payload_size = var_layout.total_size;
                }
                max_payload_align = max_payload_align.max(var_layout.align);
            }
        }

        // Conservative model: payload first at offset 0, discriminant immediately after.
        // Rust's actual layout is compiler-controlled (niche optimisation etc.);
        // this model gives a safe upper-bound for padding analysis.
        let payload_align = max_payload_align.max(1);
        let disc_offset = max_payload_size;
        let total_before_pad = disc_offset + disc_size;
        let total_align = payload_align.max(disc_size);
        let total_size = total_before_pad.next_multiple_of(total_align);

        let mut fields: Vec<Field> = Vec::new();
        if max_payload_size > 0 {
            fields.push(Field {
                name: "__payload".to_string(),
                ty: TypeInfo::Opaque {
                    name: format!("largest_variant_payload ({}B)", max_payload_size),
                    size: max_payload_size,
                    align: payload_align,
                },
                offset: 0,
                size: max_payload_size,
                align: payload_align,
                source_file: None,
                source_line: None,
                access: AccessPattern::Unknown,
            });
        }
        fields.push(Field {
            name: "__discriminant".to_string(),
            ty: TypeInfo::Primitive {
                name: format!("u{}", disc_size * 8),
                size: disc_size,
                align: disc_size,
            },
            offset: disc_offset,
            size: disc_size,
            align: disc_size,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        });

        let enum_line = node.ident.span().start().line as u32;
        self.layouts.push(StructLayout {
            name,
            total_size,
            align: total_align,
            fields,
            source_file: None,
            source_line: Some(enum_line),
            arch: self.ctx.arch,
            is_packed: false,
            is_union: false,
            is_repr_rust: is_repr_rust(&node.attrs),
            suppressed_findings: super::suppress::suppressed_from_source_line(
                self.source,
                enum_line,
            ),
            uncertain_fields: Vec::new(),
        });
    }
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_rust(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let file: syn::File = syn::parse_str(source)?;
    let mut ctx = RustCtx::new(arch);
    ItemCollector { ctx: &mut ctx }.visit_file(&file);
    let mut visitor = StructVisitor {
        ctx,
        layouts: Vec::new(),
        source,
    };
    visitor.visit_file(&file);
    Ok(visitor.layouts)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn parse_simple_struct() {
        let src = "struct Foo { a: u8, b: u64, c: u32 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.name, "Foo");
        assert_eq!(l.fields.len(), 3);
        assert_eq!(l.fields[0].size, 1); // u8
        assert_eq!(l.fields[1].size, 8); // u64
        assert_eq!(l.fields[2].size, 4); // u32
    }

    #[test]
    fn layout_includes_padding() {
        // u8 then u64: 7 bytes padding inserted
        let src = "struct T { a: u8, b: u64 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 8); // u64 aligned to 8
        assert_eq!(l.total_size, 16);
        let gaps = padlock_core::ir::find_padding(l);
        assert_eq!(gaps[0].bytes, 7);
    }

    #[test]
    fn multiple_structs_parsed() {
        let src = "struct A { x: u32 } struct B { y: u64 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 2);
    }

    #[test]
    fn packed_struct_no_padding() {
        let src = "#[repr(packed)] struct P { a: u8, b: u64 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert!(l.is_packed);
        assert_eq!(l.fields[1].offset, 1); // no padding, b immediately after a
        let gaps = padlock_core::ir::find_padding(l);
        assert!(gaps.is_empty());
    }

    #[test]
    fn pointer_field_uses_arch_size() {
        let src = "struct S { p: *const u8 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8); // 64-bit pointer
    }

    // ── attribute guard extraction ─────────────────────────────────────────────

    #[test]
    fn lock_protected_by_attr_sets_guard() {
        let src = r#"
struct Cache {
    #[lock_protected_by = "mu"]
    readers: u64,
    mu: u64,
}
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let readers = &layouts[0].fields[0];
        assert_eq!(readers.name, "readers");
        if let AccessPattern::Concurrent { guard, .. } = &readers.access {
            assert_eq!(guard.as_deref(), Some("mu"));
        } else {
            panic!("expected Concurrent, got {:?}", readers.access);
        }
    }

    #[test]
    fn guarded_by_string_attr_sets_guard() {
        let src = r#"
struct S {
    #[guarded_by("lock")]
    value: u32,
}
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        if let AccessPattern::Concurrent { guard, .. } = &layouts[0].fields[0].access {
            assert_eq!(guard.as_deref(), Some("lock"));
        } else {
            panic!("expected Concurrent");
        }
    }

    #[test]
    fn guarded_by_ident_attr_sets_guard() {
        let src = r#"
struct S {
    #[guarded_by(mu)]
    count: u64,
}
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        if let AccessPattern::Concurrent { guard, .. } = &layouts[0].fields[0].access {
            assert_eq!(guard.as_deref(), Some("mu"));
        } else {
            panic!("expected Concurrent");
        }
    }

    #[test]
    fn protected_by_attr_sets_guard() {
        let src = r#"
struct S {
    #[protected_by = "lock_a"]
    x: u64,
}
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        if let AccessPattern::Concurrent { guard, .. } = &layouts[0].fields[0].access {
            assert_eq!(guard.as_deref(), Some("lock_a"));
        } else {
            panic!("expected Concurrent");
        }
    }

    #[test]
    fn different_guards_on_same_cache_line_is_false_sharing() {
        // readers and writers are at offsets 0 and 8 — same cache line (line 0).
        // They have different explicit guards → confirmed false sharing.
        let src = r#"
struct HotPath {
    #[lock_protected_by = "mu_a"]
    readers: u64,
    #[lock_protected_by = "mu_b"]
    writers: u64,
}
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(padlock_core::analysis::false_sharing::has_false_sharing(
            &layouts[0]
        ));
    }

    #[test]
    fn same_guard_on_same_cache_line_is_not_false_sharing() {
        let src = r#"
struct Safe {
    #[lock_protected_by = "mu"]
    a: u64,
    #[lock_protected_by = "mu"]
    b: u64,
}
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(!padlock_core::analysis::false_sharing::has_false_sharing(
            &layouts[0]
        ));
    }

    #[test]
    fn unannotated_field_stays_unknown() {
        let src = "struct S { x: u64 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(matches!(
            layouts[0].fields[0].access,
            AccessPattern::Unknown
        ));
    }

    // ── stdlib type sizes ─────────────────────────────────────────────────────

    #[test]
    fn vec_field_has_three_pointer_size() {
        // Vec<T> is always ptr + len + cap regardless of T
        let src = "struct S { items: Vec<u64> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 24); // 3 × 8 on x86-64
    }

    #[test]
    fn string_field_has_three_pointer_size() {
        let src = "struct S { name: String }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 24);
    }

    #[test]
    fn box_field_has_pointer_size() {
        let src = "struct S { inner: Box<u64> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    #[test]
    fn arc_field_has_pointer_size() {
        let src = "struct S { shared: Arc<Vec<u8>> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    #[test]
    fn phantom_data_is_zero_sized() {
        let src = "struct S { a: u64, _marker: PhantomData<u8> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let marker = layouts[0]
            .fields
            .iter()
            .find(|f| f.name == "_marker")
            .unwrap();
        assert_eq!(marker.size, 0);
    }

    #[test]
    fn duration_field_is_16_bytes() {
        let src = "struct S { timeout: Duration }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16);
    }

    #[test]
    fn atomic_u64_has_correct_size() {
        let src = "struct S { counter: AtomicU64 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    #[test]
    fn atomic_bool_has_correct_size() {
        let src = "struct S { flag: AtomicBool }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 1);
    }

    // ── generic struct skipping ───────────────────────────────────────────────

    #[test]
    fn generic_struct_is_skipped() {
        // Cannot accurately lay out struct Foo<T> without knowing T.
        let src = "struct Wrapper<T> { value: T, count: usize }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.is_empty(),
            "generic structs should be skipped; got {:?}",
            layouts.iter().map(|l| &l.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn generic_struct_with_multiple_params_is_skipped() {
        let src = "struct Pair<A, B> { first: A, second: B }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(layouts.is_empty());
    }

    #[test]
    fn non_generic_struct_still_parsed_when_generic_sibling_exists() {
        let src = r#"
struct Generic<T> { value: T }
struct Concrete { a: u32, b: u64 }
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Concrete");
    }

    // ── enum data variant support ─────────────────────────────────────────────

    #[test]
    fn unit_enum_is_just_discriminant() {
        let src = "enum Color { Red, Green, Blue }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        assert_eq!(l.name, "Color");
        assert_eq!(l.total_size, 1); // 3 variants → u8 discriminant
        assert_eq!(l.fields.len(), 1);
        assert_eq!(l.fields[0].name, "__discriminant");
    }

    #[test]
    fn unit_enum_with_many_variants_uses_u16_discriminant() {
        // Build an enum with 300 variants (> 256)
        let variants: String = (0..300)
            .map(|i| format!("V{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("enum Big {{ {variants} }}");
        let layouts = parse_rust(&src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.total_size, 2); // needs u16
        assert_eq!(l.fields[0].size, 2);
    }

    #[test]
    fn data_enum_total_size_covers_largest_variant() {
        // Quit: no payload; Move: {x: i32, y: i32} = 8B; Write: String = 24B
        // Max payload = 24B (String), disc = 1B → total = 32B (aligned to 8)
        let src = r#"
enum Message {
    Quit,
    Move { x: i32, y: i32 },
    Write(String),
}
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.name, "Message");
        // __payload (24B, align 8) + __discriminant (1B) → padded to 32B
        assert_eq!(l.total_size, 32);
        assert_eq!(l.fields.len(), 2);
        let payload = l.fields.iter().find(|f| f.name == "__payload").unwrap();
        assert_eq!(payload.size, 24); // String = 3×pointer
    }

    #[test]
    fn generic_enum_is_skipped() {
        let src = "enum Wrapper<T> { Some(T), None }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.is_empty(),
            "generic enums should be skipped; got {:?}",
            layouts.iter().map(|l| &l.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_enum_is_skipped() {
        let src = "enum Never {}";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(layouts.is_empty());
    }

    #[test]
    fn enum_with_only_unit_variants_has_no_payload_field() {
        let src = "enum Dir { North, South, East, West }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(!layouts[0].fields.iter().any(|f| f.name == "__payload"));
    }

    #[test]
    fn data_enum_and_sibling_struct_both_parsed() {
        let src = r#"
enum Status { Ok, Err(u32) }
struct Conn { port: u16, status: u32 }
"#;
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 2);
        assert!(layouts.iter().any(|l| l.name == "Status"));
        assert!(layouts.iter().any(|l| l.name == "Conn"));
    }

    // ── bad weather: enums ────────────────────────────────────────────────────

    #[test]
    fn enum_with_only_zero_sized_variants_has_payload_size_zero() {
        // All unit variants → treated as unit enum, total = disc_size
        let src = "enum E { A, B }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.total_size, 1);
    }

    #[test]
    fn enum_mixed_unit_and_data_includes_max_payload() {
        // Mix: unit variant + data variant; payload comes from data variant
        let src = "enum E { Nothing, Data(u64) }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        let payload = l.fields.iter().find(|f| f.name == "__payload").unwrap();
        assert_eq!(payload.size, 8); // u64
    }

    // ── repr(align(N)) ────────────────────────────────────────────────────────

    #[test]
    fn repr_align_raises_struct_alignment() {
        let src = "#[repr(align(64))]\nstruct CacheLine { a: u8, b: u32 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(
            l.align, 64,
            "repr(align(64)) must set struct alignment to 64"
        );
        assert_eq!(l.total_size, 64, "size must be padded to 64 bytes");
    }

    #[test]
    fn repr_align_does_not_shrink_natural_alignment() {
        // repr(align(1)) on a struct whose natural align is 8 — must keep 8
        let src = "#[repr(align(1))]\nstruct S { a: u64 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(
            l.align, 8,
            "natural align must not be reduced below repr(align)"
        );
    }

    #[test]
    fn repr_align_adds_trailing_padding() {
        // u8 + u32 = 5 bytes natural, padded to 8 with align(8)
        let src = "#[repr(align(8))]\nstruct S { a: u8, b: u32 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.total_size, 8);
    }

    #[test]
    fn no_repr_align_has_natural_size() {
        // Baseline: without repr(align), just natural padding
        let src = "struct S { a: u8, b: u32 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        // a:1 + 3 pad + b:4 = 8; align=4
        assert_eq!(l.total_size, 8);
        assert_eq!(l.align, 4);
    }

    // ── tuple structs ─────────────────────────────────────────────────────────

    #[test]
    fn tuple_struct_fields_named_by_index() {
        let src = "struct Pair(u64, u8);";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].name, "_0");
        assert_eq!(l.fields[1].name, "_1");
    }

    #[test]
    fn tuple_struct_layout_follows_alignment() {
        // u64 then u8: no padding before u64, 7 bytes trailing
        let src = "struct S(u64, u8);";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[0].size, 8);
        assert_eq!(l.fields[1].offset, 8);
        assert_eq!(l.fields[1].size, 1);
        assert_eq!(l.total_size, 16);
    }

    #[test]
    fn tuple_struct_with_padding_waste_detected() {
        // u8 then u64: 7 bytes padding
        let src = "struct S(u8, u64);";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].offset, 0); // u8 at 0
        assert_eq!(l.fields[1].offset, 8); // u64 aligned to 8
        assert_eq!(l.total_size, 16);
        let gaps = padlock_core::ir::find_padding(l);
        assert_eq!(gaps[0].bytes, 7);
    }

    // ── type-table tests ──────────────────────────────────────────────────────

    #[test]
    fn nonzero_types_same_size_as_base() {
        assert_eq!(primitive_size_align("NonZeroU8", &X86_64_SYSV), (1, 1));
        assert_eq!(primitive_size_align("NonZeroI8", &X86_64_SYSV), (1, 1));
        assert_eq!(primitive_size_align("NonZeroU16", &X86_64_SYSV), (2, 2));
        assert_eq!(primitive_size_align("NonZeroU32", &X86_64_SYSV), (4, 4));
        assert_eq!(primitive_size_align("NonZeroU64", &X86_64_SYSV), (8, 8));
        assert_eq!(primitive_size_align("NonZeroU128", &X86_64_SYSV), (16, 16));
        assert_eq!(
            primitive_size_align("NonZeroUsize", &X86_64_SYSV),
            (X86_64_SYSV.pointer_size, X86_64_SYSV.pointer_size)
        );
    }

    #[test]
    fn float16_and_float128_correct_size() {
        assert_eq!(primitive_size_align("f16", &X86_64_SYSV), (2, 2));
        assert_eq!(primitive_size_align("f128", &X86_64_SYSV), (16, 16));
    }

    #[test]
    fn rust_struct_with_nonzero_fields() {
        let src = "struct Counts { hits: NonZeroU64, misses: NonZeroU32, flags: u8 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].size, 8); // NonZeroU64
        assert_eq!(l.fields[1].size, 4); // NonZeroU32
        assert_eq!(l.fields[2].size, 1); // u8
        // Total: 8+4+1 = 13, padded to align(8) = 16
        assert_eq!(l.total_size, 16);
    }

    // ── repr(Rust) detection ──────────────────────────────────────────────────

    #[test]
    fn plain_struct_is_repr_rust() {
        let src = "struct Foo { a: u64, b: u32 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(layouts[0].is_repr_rust, "plain struct should be repr(Rust)");
    }

    #[test]
    fn repr_c_struct_is_not_repr_rust() {
        let src = "#[repr(C)] struct Foo { a: u64, b: u32 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(
            !layouts[0].is_repr_rust,
            "repr(C) struct must not be repr(Rust)"
        );
    }

    #[test]
    fn repr_packed_struct_is_not_repr_rust() {
        let src = "#[repr(packed)] struct Foo { a: u64, b: u32 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(
            !layouts[0].is_repr_rust,
            "repr(packed) struct must not be repr(Rust)"
        );
    }

    #[test]
    fn repr_transparent_struct_is_not_repr_rust() {
        let src = "#[repr(transparent)] struct Wrapper(u64);";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(
            !layouts[0].is_repr_rust,
            "repr(transparent) struct must not be repr(Rust)"
        );
    }

    #[test]
    fn plain_enum_is_repr_rust() {
        let src = "enum Color { Red, Green, Blue }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(layouts[0].is_repr_rust, "plain enum should be repr(Rust)");
    }

    #[test]
    fn repr_c_enum_is_not_repr_rust() {
        let src = "#[repr(C)] enum Dir { North, South }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(
            !layouts[0].is_repr_rust,
            "repr(C) enum must not be repr(Rust)"
        );
    }

    // ── dyn Trait fat pointer sizing ──────────────────────────────────────────

    #[test]
    fn box_dyn_trait_is_fat_pointer() {
        // Box<dyn Trait> = data ptr + vtable ptr = 2 words (16B on x86-64)
        let src = "struct S { handler: Box<dyn std::any::Any> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts[0].fields[0].size, 16,
            "Box<dyn Trait> must be 16 bytes (fat pointer)"
        );
    }

    #[test]
    fn arc_dyn_trait_is_fat_pointer() {
        let src = "struct S { shared: Arc<dyn std::fmt::Display> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16);
    }

    #[test]
    fn ref_dyn_trait_is_fat_pointer() {
        // &dyn Trait = data ptr + vtable ptr = 16B
        // Use a non-generic struct — structs with any generic params (including
        // lifetime params) are skipped by the parser.
        let src = "struct S { cb: &'static dyn Fn() }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1, "S must be parsed");
        assert_eq!(
            layouts[0].fields[0].size, 16,
            "&dyn Trait must be a 16-byte fat pointer"
        );
    }

    #[test]
    fn box_concrete_type_is_single_pointer() {
        // Box<u64> is not a fat pointer — just 8B
        let src = "struct S { inner: Box<u64> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts[0].fields[0].size, 8,
            "Box<concrete> must remain 8 bytes"
        );
    }

    // ── transparent newtype wrappers ──────────────────────────────────────────

    #[test]
    fn cell_u8_is_one_byte() {
        let src = "struct S { x: Cell<u8> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts[0].fields[0].size, 1,
            "Cell<u8> must be 1 byte, not pointer-sized"
        );
    }

    #[test]
    fn maybe_uninit_u32_is_four_bytes() {
        let src = "struct S { x: MaybeUninit<u32> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts[0].fields[0].size, 4,
            "MaybeUninit<u32> must be 4 bytes"
        );
    }

    #[test]
    fn wrapping_i16_is_two_bytes() {
        let src = "struct S { x: Wrapping<i16> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts[0].fields[0].size, 2,
            "Wrapping<i16> must be 2 bytes"
        );
    }

    #[test]
    fn manually_drop_u64_is_eight_bytes() {
        let src = "struct S { x: ManuallyDrop<u64> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts[0].fields[0].size, 8,
            "ManuallyDrop<u64> must be 8 bytes"
        );
    }

    #[test]
    fn unsafe_cell_u32_is_four_bytes() {
        let src = "struct S { x: UnsafeCell<u32> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts[0].fields[0].size, 4,
            "UnsafeCell<u32> must be 4 bytes"
        );
    }

    #[test]
    fn transparent_wrapper_affects_total_size() {
        // bool(1) + pad(1) + Cell<u16>(2) = 4B
        let src = "struct S { a: bool, b: Cell<u16> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].size, 1); // bool
        assert_eq!(l.fields[1].size, 2); // Cell<u16>
        assert_eq!(l.total_size, 4);
    }

    #[test]
    fn struct_with_box_dyn_has_correct_layout() {
        // bool(1) + 7-pad + Box<dyn Error>(16) = 24B
        let src = "struct Handler { active: bool, err: Box<dyn std::error::Error> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].size, 1); // bool
        assert_eq!(l.fields[1].size, 16); // Box<dyn Error>
        assert_eq!(l.fields[1].offset, 8); // aligned to pointer_size
        assert_eq!(l.total_size, 24);
    }

    // ── collection and wrapper type sizing ───────────────────────────────────

    #[test]
    fn vec_deque_is_four_pointer_sizes() {
        // VecDeque: head + len + RawVec(ptr + cap) = 4 words
        let src = "struct S { q: VecDeque<u8> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 32); // 4 × 8 on x86-64
    }

    #[test]
    fn hash_map_is_six_pointer_sizes() {
        // HashMap: hashbrown RawTableInner (4 words) + RandomState (2×u64) = 6 words
        let src = "struct S { map: HashMap<u64, u64> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 48); // 6 × 8 on x86-64
    }

    #[test]
    fn hash_set_is_six_pointer_sizes() {
        let src = "struct S { set: HashSet<u64> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 48);
    }

    #[test]
    fn refcell_u8_is_16_bytes() {
        // RefCell<u8>: isize borrow counter (8B) + u8 at 8 → padded to 16 (align 8).
        let src = "struct S { x: RefCell<u8> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16);
    }

    #[test]
    fn refcell_unit_is_8_bytes() {
        // RefCell<()>: isize borrow counter (8B) + 0 = 8B.
        let src = "struct S { x: RefCell<()> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    #[test]
    fn refcell_u64_is_16_bytes() {
        let src = "struct S { x: RefCell<u64> }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 16);
    }

    // ── lifetime-only generic structs ─────────────────────────────────────────

    #[test]
    fn struct_with_only_lifetime_param_is_analyzed() {
        // 'a is a lifetime param — does not affect layout; struct must not be skipped.
        let src = "struct Cache<'a> { ptr: &'a u64, count: u32 }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts.len(),
            1,
            "struct with only lifetime params must be analyzed, not skipped"
        );
        let l = &layouts[0];
        assert_eq!(l.name, "Cache");
        assert_eq!(l.fields[0].size, 8); // &'a u64 = pointer
        assert_eq!(l.fields[1].size, 4); // u32
    }

    #[test]
    fn struct_with_type_param_is_still_skipped() {
        // T is a type param — layout depends on T, must be skipped.
        let src = "struct Wrapper<'a, T> { inner: &'a T }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.is_empty(),
            "struct with type param must be skipped even with a lifetime param"
        );
    }

    #[test]
    fn enum_with_only_lifetime_param_is_analyzed() {
        let src = "enum Ref<'a> { Some(&'a u64), None }";
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        assert_eq!(
            layouts.len(),
            1,
            "enum with only lifetime params must be analyzed, not skipped"
        );
        assert_eq!(layouts[0].name, "Ref");
    }

    // ── Arc<T>/Rc<T> inner-type name enrichment ────────────────────────────
    //
    // Regression tests: the concurrency heuristic in `concurrency.rs` matches
    // on substrings like "Mutex"/"Atomic" in the recorded type name. Without
    // exposing the inner type, `Arc<Mutex<T>>` and `Arc<String>` were
    // indistinguishable (both just "Arc"), which caused every multi-field
    // Arc-holding struct — an extremely common Rust pattern with no
    // synchronization involved at all — to be misclassified as a
    // false-sharing hazard. See concurrency.rs's own tests for the
    // end-to-end effect on `AccessPattern`.

    fn field_type_name(src: &str) -> String {
        let layouts = parse_rust(src, &X86_64_SYSV).unwrap();
        match &layouts[0].fields[0].ty {
            padlock_core::ir::TypeInfo::Primitive { name, .. } => name.clone(),
            other => panic!("expected Primitive, got {other:?}"),
        }
    }

    #[test]
    fn arc_of_plain_type_records_inner_name() {
        assert_eq!(
            field_type_name("struct S { x: std::sync::Arc<String> }"),
            "Arc<String>"
        );
    }

    #[test]
    fn arc_of_mutex_records_inner_name() {
        assert_eq!(
            field_type_name("struct S { x: std::sync::Arc<std::sync::Mutex<u64>> }"),
            "Arc<Mutex>"
        );
    }

    #[test]
    fn rc_of_plain_type_records_inner_name() {
        assert_eq!(
            field_type_name("struct S { x: std::rc::Rc<u32> }"),
            "Rc<u32>"
        );
    }

    #[test]
    fn arc_sizing_unaffected_by_name_enrichment() {
        // The enrichment must not change size/align: Arc<T> is always a
        // thin pointer unless T is a trait object.
        let layouts = parse_rust("struct S { x: std::sync::Arc<String> }", &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 8);
        assert_eq!(layouts[0].fields[0].align, 8);
    }

    #[test]
    fn arc_of_dyn_trait_stays_fat_pointer_unenriched() {
        // dyn Trait generic args aren't Type::Path, so the enrichment
        // doesn't apply — name stays plain "Arc", sizing stays fat (2 words).
        let name = field_type_name("struct S { x: std::sync::Arc<dyn std::fmt::Debug> }");
        assert_eq!(name, "Arc");
        let layouts = parse_rust(
            "struct S { x: std::sync::Arc<dyn std::fmt::Debug> }",
            &X86_64_SYSV,
        )
        .unwrap();
        assert_eq!(layouts[0].fields[0].size, 16);
    }

    // ── field shapes (verified against rustc size_of/offset_of) ───────────────

    fn rs(src: &str, name: &str) -> StructLayout {
        parse_rust(src, &X86_64_SYSV)
            .unwrap()
            .into_iter()
            .find(|l| l.name == name)
            .unwrap_or_else(|| panic!("{name} not parsed"))
    }

    fn off(l: &StructLayout, field: &str) -> usize {
        l.fields.iter().find(|f| f.name == field).unwrap().offset
    }

    #[test]
    fn array_length_may_be_a_const_expression() {
        let src = "const N: usize = 4; const M: usize = N * 2 + size_of::<u64>();
            #[repr(C)] struct S { a: u8, b: [u8; N], c: [u8; M], d: [u16; N as usize], e: u8 }";
        let l = rs(src, "S");
        assert_eq!(off(&l, "c"), 5);
        assert_eq!(off(&l, "d"), 22);
        assert_eq!(off(&l, "e"), 30);
        assert!(l.uncertain_fields.is_empty());
    }

    #[test]
    fn unevaluable_array_length_is_uncertain_not_zero() {
        let l = rs(
            "#[repr(C)] struct S { a: u8, b: [u8; UNKNOWN], c: u8 }",
            "S",
        );
        assert_eq!(l.uncertain_fields, vec!["b".to_string()]);
        assert_eq!(off(&l, "c"), 2);
    }

    #[test]
    fn local_type_alias_is_resolved() {
        let l = rs(
            "type Word = u16; #[repr(C)] struct S { a: u8, w: Word, c: u8 }",
            "S",
        );
        assert_eq!(off(&l, "w"), 2);
    }

    #[test]
    fn array_of_local_struct_is_scaled() {
        let src = "#[repr(C)] struct In { x: u8, y: u32 }
            #[repr(C)] struct S { a: u8, n: [[In; 2]; 3], c: u8 }";
        let l = crate::parse_source_str(src, &crate::SourceLanguage::Rust, &X86_64_SYSV)
            .unwrap()
            .into_iter()
            .find(|l| l.name == "S")
            .unwrap();
        assert_eq!(off(&l, "c"), 52);
    }

    #[test]
    fn tuples_are_reordered_by_alignment() {
        let l = rs(
            "#[repr(C)] struct S { a: u8, t: (u8, u32, u8), c: u8 }",
            "S",
        );
        // (u8, u32, u8) is laid out u32, u8, u8 → 8 bytes, align 4.
        assert_eq!(off(&l, "t"), 4);
        assert_eq!(off(&l, "c"), 12);
    }

    #[test]
    fn references_to_unsized_types_are_fat() {
        let l = rs(
            "#[repr(C)] struct S { a: u8, s: &'static str, b: Box<[u8]>, c: Arc<str>, d: u8 }",
            "S",
        );
        assert_eq!(off(&l, "b"), 24);
        assert_eq!(off(&l, "c"), 40);
        assert_eq!(off(&l, "d"), 56);
    }

    #[test]
    fn option_niches() {
        let l = rs(
            "#[repr(C)] struct S { a: u8, v: Option<Vec<u8>>, b: Option<bool>, s: Option<&'static str>, c: u8 }",
            "S",
        );
        assert_eq!(off(&l, "v"), 8);
        assert_eq!(off(&l, "b"), 32);
        assert_eq!(off(&l, "s"), 40);
        assert_eq!(off(&l, "c"), 56);
    }

    #[test]
    fn std_sync_types_follow_the_futex_layout() {
        let l = rs(
            "#[repr(C)] struct S { a: u8, m: Mutex<u32>, r: RwLock<u8>, c: u8 }",
            "S",
        );
        assert_eq!(off(&l, "m"), 4);
        assert_eq!(off(&l, "r"), 16);
        assert_eq!(off(&l, "c"), 28);
    }

    #[test]
    fn packed_n_caps_alignment() {
        let l = rs(
            "#[repr(C, packed(4))] struct S { a: u8, b: u64, c: u8 }",
            "S",
        );
        assert_eq!(off(&l, "b"), 4);
        assert_eq!(l.total_size, 16);
        assert_eq!(l.align, 4);
    }

    #[test]
    fn fieldless_enum_repr_sets_discriminant_width() {
        let src =
            "#[repr(u16)] enum A { X, Y } #[repr(C)] enum B { X } enum C { X } enum D { X, Y }";
        let by = |n: &str| rs(src, n).total_size;
        assert_eq!(by("A"), 2);
        assert_eq!(by("B"), 4);
        assert_eq!(by("C"), 0);
        assert_eq!(by("D"), 1);
    }
}
