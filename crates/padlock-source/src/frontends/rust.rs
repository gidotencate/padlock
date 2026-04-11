// padlock-source/src/frontends/rust.rs
//
// Extracts struct layouts from Rust source using syn + the Visit API.
// Sizes are approximated from type names using the target arch config.
// Only repr(C) / repr(packed) / plain structs are handled; generics are opaque.

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
use quote::ToTokens;
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

// ── type resolution ───────────────────────────────────────────────────────────

fn rust_type_size_align(ty: &Type, arch: &'static ArchConfig) -> (usize, usize, TypeInfo) {
    match ty {
        Type::Path(tp) => {
            let name = tp
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            let (size, align) = primitive_size_align(&name, arch);
            (size, align, TypeInfo::Primitive { name, size, align })
        }
        Type::Ptr(_) | Type::Reference(_) => {
            let s = arch.pointer_size;
            (s, s, TypeInfo::Pointer { size: s, align: s })
        }
        Type::Array(arr) => {
            let (elem_size, elem_align, elem_ty) = rust_type_size_align(&arr.elem, arch);
            let count = array_len_from_expr(&arr.len);
            let size = elem_size * count;
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

fn primitive_size_align(name: &str, arch: &'static ArchConfig) -> (usize, usize) {
    let ps = arch.pointer_size;
    match name {
        // ── language primitives ───────────────────────────────────────────────
        "bool" | "u8" | "i8" => (1, 1),
        "u16" | "i16" => (2, 2),
        "u32" | "i32" | "f32" => (4, 4),
        "u64" | "i64" | "f64" => (8, 8),
        "u128" | "i128" => (16, 16),
        "usize" | "isize" => (ps, ps),
        "char" => (4, 4), // Rust char is a Unicode scalar (4 bytes)

        // ── std atomics ───────────────────────────────────────────────────────
        "AtomicBool" | "AtomicU8" | "AtomicI8" => (1, 1),
        "AtomicU16" | "AtomicI16" => (2, 2),
        "AtomicU32" | "AtomicI32" => (4, 4),
        "AtomicU64" | "AtomicI64" => (8, 8),
        "AtomicUsize" | "AtomicIsize" | "AtomicPtr" => (ps, ps),

        // ── heap-allocated collections: ptr + len + cap (3 words) ────────────
        // Size is independent of the element type T (generic arg already stripped).
        "Vec" | "String" | "OsString" | "CString" | "PathBuf" => (3 * ps, ps),
        "VecDeque" | "LinkedList" | "BinaryHeap" => (3 * ps, ps),
        "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet" => (3 * ps, ps),

        // ── single-pointer smart pointers ─────────────────────────────────────
        "Box" | "Rc" | "Arc" | "Weak" | "NonNull" | "Cell" => (ps, ps),

        // ── interior-mutability / sync wrappers ───────────────────────────────
        // Size depends on T but pointer-size is a reasonable approximation for
        // display purposes; use binary analysis for precise results.
        "RefCell" | "Mutex" | "RwLock" => (ps, ps),

        // ── channels ─────────────────────────────────────────────────────────
        "Sender" | "Receiver" | "SyncSender" => (ps, ps),

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

fn array_len_from_expr(expr: &syn::Expr) -> usize {
    if let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Int(n),
        ..
    }) = expr
    {
        n.base10_parse::<usize>().unwrap_or(0)
    } else {
        0
    }
}

// ── struct repr detection ─────────────────────────────────────────────────────

fn is_packed(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("repr") && a.to_token_stream().to_string().contains("packed"))
}

fn simulate_rust_layout(
    name: String,
    fields: &[(String, Type)],
    packed: bool,
    arch: &'static ArchConfig,
) -> StructLayout {
    let mut offset = 0usize;
    let mut struct_align = 1usize;
    let mut out_fields: Vec<Field> = Vec::new();

    for (fname, ty) in fields {
        let (size, align, type_info) = rust_type_size_align(ty, arch);
        let effective_align = if packed { 1 } else { align };

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
    }
}

// ── visitor ───────────────────────────────────────────────────────────────────

struct StructVisitor {
    arch: &'static ArchConfig,
    layouts: Vec<StructLayout>,
}

impl<'ast> Visit<'ast> for StructVisitor {
    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        syn::visit::visit_item_struct(self, node); // recurse into nested items

        // Generic structs (e.g. `struct Foo<T>`) cannot be accurately laid out
        // without knowing the concrete type arguments. Skip them rather than
        // producing wrong field sizes for the type parameters.
        if !node.generics.params.is_empty() {
            return;
        }

        let name = node.ident.to_string();
        let packed = is_packed(&node.attrs);

        // Collect (field_name, type, optional_guard)
        let fields: Vec<(String, Type, Option<String>)> = match &node.fields {
            Fields::Named(nf) => nf
                .named
                .iter()
                .map(|f| {
                    let fname = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
                    let guard = extract_guard_from_attrs(&f.attrs);
                    (fname, f.ty.clone(), guard)
                })
                .collect(),
            Fields::Unnamed(uf) => uf
                .unnamed
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let guard = extract_guard_from_attrs(&f.attrs);
                    (format!("_{i}"), f.ty.clone(), guard)
                })
                .collect(),
            Fields::Unit => vec![],
        };

        let name_ty: Vec<(String, Type)> = fields
            .iter()
            .map(|(n, t, _)| (n.clone(), t.clone()))
            .collect();
        let mut layout = simulate_rust_layout(name, &name_ty, packed, self.arch);
        layout.source_line = Some(node.ident.span().start().line as u32);

        // Apply explicit guard annotations; these take precedence over the
        // heuristic type-name pass in concurrency.rs (which skips non-Unknown fields).
        for (i, (_, _, guard)) in fields.iter().enumerate() {
            if let Some(g) = guard {
                layout.fields[i].access = AccessPattern::Concurrent {
                    guard: Some(g.clone()),
                    is_atomic: false,
                };
            }
        }

        self.layouts.push(layout);
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        syn::visit::visit_item_enum(self, node);

        // Skip generic enums (layout depends on unknown type arguments)
        if !node.generics.params.is_empty() {
            return;
        }

        let name = node.ident.to_string();
        let n_variants = node.variants.len();
        if n_variants == 0 {
            return;
        }

        // Discriminant size: smallest integer that fits the variant count.
        // Rust defaults to isize but uses the minimal repr in practice.
        let disc_size: usize = if n_variants <= 256 { 1 } else if n_variants <= 65536 { 2 } else { 4 };

        // Check if all variants are unit (C-like enum, no payload)
        let all_unit = node.variants.iter().all(|v| matches!(v.fields, Fields::Unit));

        if all_unit {
            // Pure discriminant — no payload storage
            let layout = StructLayout {
                name,
                total_size: disc_size,
                align: disc_size,
                fields: vec![Field {
                    name: "__discriminant".to_string(),
                    ty: TypeInfo::Primitive {
                        name: format!("u{}", disc_size * 8),
                        size: disc_size,
                        align: disc_size,
                    },
                    offset: 0,
                    size: disc_size,
                    align: disc_size,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                }],
                source_file: None,
                source_line: Some(node.ident.span().start().line as u32),
                arch: self.arch,
                is_packed: false,
                is_union: false,
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
                let var_layout = simulate_rust_layout(String::new(), &var_fields, false, self.arch);
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

        self.layouts.push(StructLayout {
            name,
            total_size,
            align: total_align,
            fields,
            source_file: None,
            source_line: Some(node.ident.span().start().line as u32),
            arch: self.arch,
            is_packed: false,
            is_union: false,
        });
    }
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_rust(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let file: syn::File = syn::parse_str(source)?;
    let mut visitor = StructVisitor {
        arch,
        layouts: Vec::new(),
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
        let variants: String = (0..300).map(|i| format!("V{i}")).collect::<Vec<_>>().join(", ");
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
}
