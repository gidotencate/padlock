// padlock-macros — compile-time struct layout assertions.
//
// Provides:
//   #[padlock::assert_no_padding]   — fails to compile if the struct has padding
//   #[padlock::assert_size(N)]      — fails to compile if size_of != N

extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Fields, ItemStruct};

// ── #[padlock::assert_no_padding] ────────────────────────────────────────────

/// Attribute macro that causes a **compile-time error** if the struct has
/// any padding bytes.
///
/// Padding is detected by asserting that `size_of::<Struct>()` equals the sum
/// of `size_of::<FieldType>()` for every field. If the compiler inserts padding
/// (alignment gaps or trailing bytes), the sizes will differ and the assertion
/// fails.
///
/// # Example
///
/// ```rust,ignore
/// #[padlock::assert_no_padding]
/// #[repr(C)]
/// struct Packed {
///     a: u64,
///     b: u32,
///     c: u32,
/// }
/// // ✓ compiles: 8 + 4 + 4 = 16 = size_of::<Packed>()
///
/// #[padlock::assert_no_padding]
/// struct Padded {
///     a: u8,
///     b: u64,  // ← 7 bytes of padding inserted before this
/// }
/// // ✗ compile error: 1 + 8 = 9 ≠ 16 = size_of::<Padded>()
/// ```
///
/// # Limitations
///
/// - Works for named-field structs only (tuple structs and unit structs are
///   accepted without checking).
/// - Generic structs are accepted but the assertion uses concrete field type
///   tokens; monomorphisation errors may give confusing messages.
/// - The check operates on `size_of` values, which means repr(Rust) structs
///   where the compiler reorders fields but eliminates padding will still pass.
#[proc_macro_attribute]
pub fn assert_no_padding(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let expanded = emit_no_padding_assertion(&input);
    TokenStream::from(quote! {
        #input
        #expanded
    })
}

fn emit_no_padding_assertion(input: &ItemStruct) -> TokenStream2 {
    let struct_name = &input.ident;
    let const_ident = format_ident!(
        "_PADLOCK_ASSERT_NO_PADDING_{}",
        struct_name.to_string().to_uppercase()
    );

    let field_types: Vec<_> = match &input.fields {
        Fields::Named(nf) => nf.named.iter().map(|f| &f.ty).collect(),
        Fields::Unnamed(uf) => uf.unnamed.iter().map(|f| &f.ty).collect(),
        Fields::Unit => {
            // Unit structs have no fields; size_of is 0, nothing to check.
            return quote! {
                const #const_ident: () = ();
            };
        }
    };

    if field_types.is_empty() {
        return quote! {
            const #const_ident: () = ();
        };
    }

    // Build: size_of::<F1>() + size_of::<F2>() + ...
    let field_sizes = field_types.iter().map(|ty| {
        quote! { ::std::mem::size_of::<#ty>() }
    });

    // The assertion: size_of::<Struct>() == sum(size_of::<FieldType>())
    // If this fails, the compiler prints something like:
    //   assertion `left == right` failed
    //   left: 16
    //   right: 9
    // Combined with the const name it's clear which struct triggered it.
    quote! {
        const #const_ident: () = {
            let struct_size = ::std::mem::size_of::<#struct_name>();
            let field_sum: usize = 0 #( + #field_sizes )*;
            assert!(
                struct_size == field_sum,
                concat!(
                    "padlock: struct `",
                    stringify!(#struct_name),
                    "` has padding — size_of != sum of field sizes. ",
                    "Reorder fields by descending alignment or add #[repr(packed)]."
                )
            );
        };
    }
}

// ── #[padlock::assert_size(N)] ────────────────────────────────────────────────

/// Attribute macro that causes a **compile-time error** if the struct's
/// `size_of` is not exactly `N` bytes.
///
/// Useful for locking down a struct's total size against accidental growth.
///
/// # Example
///
/// ```rust,ignore
/// #[padlock::assert_size(64)]
/// struct CacheLine {
///     data: [u8; 64],
/// }
/// ```
#[proc_macro_attribute]
pub fn assert_size(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let expected: syn::LitInt = match syn::parse(attr) {
        Ok(n) => n,
        Err(e) => return e.to_compile_error().into(),
    };

    let struct_name = &input.ident;
    let const_ident = format_ident!(
        "_PADLOCK_ASSERT_SIZE_{}",
        struct_name.to_string().to_uppercase()
    );

    let expanded = quote! {
        #input

        const #const_ident: () = {
            let actual = ::std::mem::size_of::<#struct_name>();
            let expected: usize = #expected;
            assert!(
                actual == expected,
                concat!(
                    "padlock: struct `",
                    stringify!(#struct_name),
                    "` has unexpected size. Check for accidental padding or field additions."
                )
            );
        };
    };

    TokenStream::from(expanded)
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn no_padding_assertion_for_unit_struct_is_empty_const() {
        let item: ItemStruct = parse_quote! { struct Unit; };
        let ts = emit_no_padding_assertion(&item);
        let s = ts.to_string();
        // Should produce a trivial `const ...: () = ();`
        assert!(s.contains("()"));
        // Should NOT produce a size_of assertion
        assert!(!s.contains("size_of"));
    }

    #[test]
    fn no_padding_assertion_contains_struct_name() {
        let item: ItemStruct = parse_quote! {
            struct MyStruct {
                a: u64,
                b: u32,
            }
        };
        let ts = emit_no_padding_assertion(&item);
        let s = ts.to_string();
        assert!(
            s.contains("MY_STRUCT") || s.contains("MyStruct") || s.contains("my_struct"),
            "expected struct name reference in: {s}"
        );
    }

    #[test]
    fn no_padding_assertion_includes_size_of_fields() {
        let item: ItemStruct = parse_quote! {
            struct Foo {
                a: u8,
                b: u64,
            }
        };
        let ts = emit_no_padding_assertion(&item);
        let s = ts.to_string();
        assert!(s.contains("size_of"), "expected size_of in: {s}");
        assert!(s.contains("u8"), "expected u8 in: {s}");
        assert!(s.contains("u64"), "expected u64 in: {s}");
    }

    #[test]
    fn no_padding_assertion_empty_named_fields_is_trivial() {
        // A struct with no fields (struct Foo {}) — edge case
        let item: ItemStruct = parse_quote! { struct Empty {} };
        let ts = emit_no_padding_assertion(&item);
        let s = ts.to_string();
        assert!(
            !s.contains("size_of"),
            "empty struct should not generate size_of check"
        );
    }

    #[test]
    fn no_padding_const_name_is_uppercase() {
        let item: ItemStruct = parse_quote! {
            struct FooBar { x: u32 }
        };
        let ts = emit_no_padding_assertion(&item);
        let s = ts.to_string();
        // The generated const ident should be FOOBAR (uppercase of struct name)
        assert!(s.contains("FOOBAR"), "expected FOOBAR in const name: {s}");
    }

    #[test]
    fn assert_message_contains_struct_name() {
        let item: ItemStruct = parse_quote! {
            struct Suspect { a: u8, b: u64 }
        };
        let ts = emit_no_padding_assertion(&item);
        let s = ts.to_string();
        assert!(
            s.contains("Suspect"),
            "expected struct name in assertion message: {s}"
        );
    }
}
