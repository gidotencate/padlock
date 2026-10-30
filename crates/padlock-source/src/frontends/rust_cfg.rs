// padlock-source/src/frontends/rust_cfg.rs
//
// `#[cfg(...)]` evaluation for the Rust frontend. A struct, field or variant
// whose predicate is known to be false for the target architecture is removed
// before analysis, so `#[cfg(target_pointer_width = "32")] struct X` is not
// laid out on a 64-bit target. A predicate padlock cannot decide (`feature`,
// `target_os` on a bare `x86_64`, …) keeps the item, as before.

use padlock_core::arch::ArchConfig;
use syn::visit_mut::VisitMut;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }
}

fn tri(b: bool) -> Tri {
    if b { Tri::True } else { Tri::False }
}

fn target_arch(arch: &ArchConfig) -> &'static str {
    match arch.name {
        "x86_64" | "x86_64-msvc" => "x86_64",
        "aarch64" | "aarch64-apple" => "aarch64",
        "wasm32" => "wasm32",
        "riscv64" => "riscv64",
        "cortex_m" | "cortex_m4" => "arm",
        "x86" => "x86",
        "avr" => "avr",
        _ => "",
    }
}

/// `Some(os)` when the architecture name pins the operating system.
fn target_os(arch: &ArchConfig) -> Option<&'static str> {
    match arch.name {
        "x86_64-msvc" => Some("windows"),
        "aarch64-apple" => Some("macos"),
        "cortex_m" | "cortex_m4" | "avr" => Some("none"),
        "wasm32" => Some("unknown"),
        _ => None,
    }
}

fn key_value(key: &str, value: &str, arch: &ArchConfig) -> Tri {
    match key {
        "target_pointer_width" => tri(value == (arch.pointer_size * 8).to_string()),
        "target_arch" => {
            let a = target_arch(arch);
            if a.is_empty() {
                Tri::Unknown
            } else {
                tri(a == value)
            }
        }
        "target_endian" => tri(value
            == match arch.endianness {
                padlock_core::arch::Endianness::Little => "little",
                padlock_core::arch::Endianness::Big => "big",
            }),
        "target_os" => match target_os(arch) {
            Some(os) => tri(os == value),
            None => Tri::Unknown,
        },
        "target_family" => match target_os(arch) {
            Some("windows") => tri(value == "windows"),
            Some("macos") => tri(value == "unix"),
            Some("none") => Tri::Unknown,
            Some(_) => tri(value == "wasm"),
            None => Tri::Unknown,
        },
        "target_env" => match arch.name {
            "x86_64-msvc" => tri(value == "msvc"),
            _ => Tri::Unknown,
        },
        "target_vendor" => match arch.name {
            "aarch64-apple" => tri(value == "apple"),
            "x86_64-msvc" => tri(value == "pc"),
            _ => Tri::Unknown,
        },
        _ => Tri::Unknown,
    }
}

fn word(name: &str, arch: &ArchConfig) -> Tri {
    match name {
        "unix" => match target_os(arch) {
            Some("windows") | Some("none") | Some("unknown") => Tri::False,
            Some(_) => Tri::True,
            None => Tri::Unknown,
        },
        "windows" => match target_os(arch) {
            Some(os) => tri(os == "windows"),
            None => Tri::False,
        },
        // Layouts are analyzed for the library, not its test harness or docs.
        "test" | "doc" | "doctest" | "miri" => Tri::False,
        _ => Tri::Unknown,
    }
}

fn eval_meta(meta: &syn::Meta, arch: &ArchConfig) -> Tri {
    match meta {
        syn::Meta::Path(p) => p
            .get_ident()
            .map(|i| word(&i.to_string(), arch))
            .unwrap_or(Tri::Unknown),
        syn::Meta::NameValue(nv) => {
            let (
                Some(key),
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(v),
                    ..
                }),
            ) = (nv.path.get_ident(), &nv.value)
            else {
                return Tri::Unknown;
            };
            key_value(&key.to_string(), &v.value(), arch)
        }
        syn::Meta::List(list) => {
            let Some(name) = list.path.get_ident().map(|i| i.to_string()) else {
                return Tri::Unknown;
            };
            let Ok(args) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return Tri::Unknown;
            };
            let vals = args.iter().map(|m| eval_meta(m, arch));
            match name.as_str() {
                "not" => vals
                    .into_iter()
                    .next()
                    .map(Tri::not)
                    .unwrap_or(Tri::Unknown),
                "all" => {
                    let v: Vec<Tri> = vals.collect();
                    if v.contains(&Tri::False) {
                        Tri::False
                    } else if v.contains(&Tri::Unknown) {
                        Tri::Unknown
                    } else {
                        Tri::True
                    }
                }
                "any" => {
                    let v: Vec<Tri> = vals.collect();
                    if v.contains(&Tri::True) {
                        Tri::True
                    } else if v.contains(&Tri::Unknown) {
                        Tri::Unknown
                    } else {
                        Tri::False
                    }
                }
                _ => Tri::Unknown,
            }
        }
    }
}

/// True when some `#[cfg(...)]` on the item is known to be false.
fn disabled(attrs: &[syn::Attribute], arch: &ArchConfig) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.parse_args::<syn::Meta>()
                .map(|m| eval_meta(&m, arch) == Tri::False)
                .unwrap_or(false)
    })
}

struct Stripper<'a>(&'a ArchConfig);

impl VisitMut for Stripper<'_> {
    fn visit_file_mut(&mut self, file: &mut syn::File) {
        file.items.retain(|i| !item_disabled(i, self.0));
        syn::visit_mut::visit_file_mut(self, file);
    }

    fn visit_item_mod_mut(&mut self, m: &mut syn::ItemMod) {
        if let Some((_, items)) = &mut m.content {
            items.retain(|i| !item_disabled(i, self.0));
        }
        syn::visit_mut::visit_item_mod_mut(self, m);
    }

    fn visit_fields_named_mut(&mut self, f: &mut syn::FieldsNamed) {
        let arch = self.0;
        let kept: syn::punctuated::Punctuated<syn::Field, syn::Token![,]> =
            std::mem::take(&mut f.named)
                .into_pairs()
                .filter(|p| !disabled(&p.value().attrs, arch))
                .collect();
        f.named = kept;
        syn::visit_mut::visit_fields_named_mut(self, f);
    }

    fn visit_fields_unnamed_mut(&mut self, f: &mut syn::FieldsUnnamed) {
        let arch = self.0;
        let kept: syn::punctuated::Punctuated<syn::Field, syn::Token![,]> =
            std::mem::take(&mut f.unnamed)
                .into_pairs()
                .filter(|p| !disabled(&p.value().attrs, arch))
                .collect();
        f.unnamed = kept;
        syn::visit_mut::visit_fields_unnamed_mut(self, f);
    }

    fn visit_item_enum_mut(&mut self, e: &mut syn::ItemEnum) {
        let arch = self.0;
        let kept: syn::punctuated::Punctuated<syn::Variant, syn::Token![,]> =
            std::mem::take(&mut e.variants)
                .into_pairs()
                .filter(|p| !disabled(&p.value().attrs, arch))
                .collect();
        e.variants = kept;
        syn::visit_mut::visit_item_enum_mut(self, e);
    }
}

fn item_disabled(item: &syn::Item, arch: &ArchConfig) -> bool {
    let attrs: &[syn::Attribute] = match item {
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        _ => return false,
    };
    disabled(attrs, arch)
}

/// Drop everything the target's `cfg` predicates switch off.
pub(crate) fn strip_disabled(file: &mut syn::File, arch: &ArchConfig) {
    if file.attrs.iter().any(|a| a.path().is_ident("cfg")) {
        // An inner `#![cfg(...)]` that is false disables the whole file.
        if disabled(&file.attrs, arch) {
            file.items.clear();
            return;
        }
    }
    Stripper(arch).visit_file_mut(file);
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::{AVR, X86_64_MSVC, X86_64_SYSV};

    fn names(src: &str, arch: &ArchConfig) -> Vec<String> {
        let mut f: syn::File = syn::parse_str(src).unwrap();
        strip_disabled(&mut f, arch);
        f.items
            .iter()
            .filter_map(|i| match i {
                syn::Item::Struct(s) => Some(s.ident.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn pointer_width_selects_one_definition() {
        let src = r#"
            #[cfg(target_pointer_width = "32")] struct W { a: u32 }
            #[cfg(target_pointer_width = "64")] struct W2 { a: u64 }
        "#;
        assert_eq!(names(src, &X86_64_SYSV), ["W2"]);
    }

    #[test]
    fn unknown_predicates_keep_the_item() {
        let src = r#"
            #[cfg(feature = "x")] struct A { a: u8 }
            #[cfg(not(feature = "x"))] struct B { a: u8 }
            #[cfg(all(unix, feature = "x"))] struct C { a: u8 }
        "#;
        assert_eq!(names(src, &X86_64_SYSV), ["A", "B", "C"]);
    }

    #[test]
    fn os_predicates() {
        let src = "#[cfg(windows)] struct Win { a: u8 }\n#[cfg(unix)] struct Nix { a: u8 }\n";
        assert_eq!(names(src, &X86_64_MSVC), ["Win"]);
        assert_eq!(names(src, &X86_64_SYSV), ["Nix"]);
        assert!(names(src, &AVR).is_empty());
    }

    #[test]
    fn fields_and_variants_are_stripped() {
        let mut f: syn::File = syn::parse_str(
            "struct S { a: u8, #[cfg(target_pointer_width = \"32\")] b: u32, c: u16 }\n\
             enum E { A, #[cfg(windows)] B, C }",
        )
        .unwrap();
        strip_disabled(&mut f, &X86_64_SYSV);
        let syn::Item::Struct(s) = &f.items[0] else {
            panic!()
        };
        assert_eq!(s.fields.len(), 2);
        let syn::Item::Enum(e) = &f.items[1] else {
            panic!()
        };
        assert_eq!(e.variants.len(), 2);
    }

    #[test]
    fn nested_mod_items_are_stripped() {
        let mut f: syn::File =
            syn::parse_str("mod m { #[cfg(windows)] struct Gone { a: u8 } struct Kept { a: u8 } }")
                .unwrap();
        strip_disabled(&mut f, &X86_64_SYSV);
        let syn::Item::Mod(m) = &f.items[0] else {
            panic!()
        };
        assert_eq!(m.content.as_ref().unwrap().1.len(), 1);
    }
}
