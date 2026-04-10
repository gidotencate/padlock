// padlock-source/src/lib.rs

pub mod concurrency;
pub mod fixgen;
pub mod frontends;

use std::collections::HashMap;
use std::path::Path;

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{StructLayout, TypeInfo};

#[derive(Debug, Clone, PartialEq)]
pub enum SourceLanguage {
    C,
    Cpp,
    Rust,
    Go,
    Zig,
}

/// Detect language from file extension.
pub fn detect_language(path: &Path) -> Option<SourceLanguage> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("c") | Some("h") => Some(SourceLanguage::C),
        Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") => Some(SourceLanguage::Cpp),
        Some("rs") => Some(SourceLanguage::Rust),
        Some("go") => Some(SourceLanguage::Go),
        Some("zig") => Some(SourceLanguage::Zig),
        _ => None,
    }
}

/// Parse a source file and return struct layouts.
pub fn parse_source(path: &Path, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let lang = detect_language(path)
        .ok_or_else(|| anyhow::anyhow!("unsupported file type: {}", path.display()))?;
    let source = std::fs::read_to_string(path)?;
    let mut layouts = parse_source_str(&source, &lang, arch)?;
    let file_str = path.to_string_lossy().into_owned();
    for layout in &mut layouts {
        layout.source_file = Some(file_str.clone());
    }
    Ok(layouts)
}

/// Parse source text directly (useful for tests and piped input).
pub fn parse_source_str(
    source: &str,
    lang: &SourceLanguage,
    arch: &'static ArchConfig,
) -> anyhow::Result<Vec<StructLayout>> {
    let mut layouts = match lang {
        SourceLanguage::C => frontends::c_cpp::parse_c(source, arch)?,
        SourceLanguage::Cpp => frontends::c_cpp::parse_cpp(source, arch)?,
        SourceLanguage::Rust => frontends::rust::parse_rust(source, arch)?,
        SourceLanguage::Go => frontends::go::parse_go(source, arch)?,
        SourceLanguage::Zig => frontends::zig::parse_zig(source, arch)?,
    };

    // Resolve fields whose type names match other structs in this file.
    // This makes nested struct sizes accurate (instead of defaulting to pointer size).
    resolve_nested_structs(&mut layouts);

    // Annotate concurrency patterns
    for layout in &mut layouts {
        concurrency::annotate_concurrency(layout, lang);
    }

    // Remove structs explicitly opted out via `// padlock:ignore`
    layouts.retain(|layout| !is_padlock_ignored(source, &layout.name));

    Ok(layouts)
}

// ── nested struct resolution ──────────────────────────────────────────────────

/// Returns true if `name` is a well-known primitive type name in any supported
/// language. These must never be shadowed by a user-defined struct name.
fn is_known_primitive(name: &str) -> bool {
    matches!(
        name,
        // Rust primitives
        "bool" | "u8" | "i8" | "u16" | "i16" | "u32" | "i32" | "f32" | "u64" | "i64" | "f64"
            | "u128" | "i128" | "usize" | "isize" | "char" | "str"
            // C/C++ primitives
            | "int" | "long" | "short" | "float" | "double" | "void"
            | "int8_t" | "uint8_t" | "int16_t" | "uint16_t" | "int32_t" | "uint32_t"
            | "int64_t" | "uint64_t" | "size_t" | "ssize_t" | "ptrdiff_t"
            | "intptr_t" | "uintptr_t" | "_Bool"
            // Go primitives
            | "int8" | "uint8" | "byte" | "int16" | "uint16" | "int32" | "uint32"
            | "int64" | "uint64" | "float32" | "float64" | "complex64" | "complex128"
            | "rune" | "string" | "error"
            // SIMD
            | "__m64" | "__m128" | "__m128d" | "__m128i"
            | "__m256" | "__m256d" | "__m256i"
            | "__m512" | "__m512d" | "__m512i"
    )
}

/// Resolve fields whose type name matches another parsed struct.
///
/// Runs in a loop until stable to handle transitive nesting (struct A contains
/// B which contains C). In practice, 2–3 iterations suffice for typical code.
fn resolve_nested_structs(layouts: &mut [StructLayout]) {
    loop {
        // Build name → (total_size, align) from whatever we have so far.
        let known: HashMap<String, (usize, usize)> = layouts
            .iter()
            .map(|l| (l.name.clone(), (l.total_size, l.align)))
            .collect();

        let mut changed_any = false;

        for layout in layouts.iter_mut() {
            let mut changed = false;

            for field in layout.fields.iter_mut() {
                // Extract the type name from Primitive or Opaque variants.
                // Struct/Pointer/Array variants are already correctly sized.
                let type_name: String = match &field.ty {
                    TypeInfo::Primitive { name, .. } | TypeInfo::Opaque { name, .. } => {
                        name.clone()
                    }
                    _ => continue,
                };

                // Never shadow built-in primitives.
                if is_known_primitive(&type_name) {
                    continue;
                }

                // Don't resolve a struct to itself (circular).
                if type_name == layout.name {
                    continue;
                }

                if let Some(&(struct_size, struct_align)) = known.get(&type_name) {
                    // Only update if the size would change — avoids infinite loops
                    // for pointer-sized structs that already have the right size.
                    if field.size == struct_size && field.align == struct_align {
                        continue;
                    }
                    let eff_align = if layout.is_packed { 1 } else { struct_align };
                    field.ty = TypeInfo::Opaque {
                        name: type_name,
                        size: struct_size,
                        align: struct_align,
                    };
                    field.size = struct_size;
                    field.align = eff_align;
                    changed = true;
                }
            }

            if changed {
                resimulate_layout(layout);
                changed_any = true;
            }
        }

        if !changed_any {
            break;
        }
    }
}

/// Re-simulate field offsets and total_size after field sizes have been updated.
fn resimulate_layout(layout: &mut StructLayout) {
    if layout.is_union {
        for field in layout.fields.iter_mut() {
            field.offset = 0;
        }
        let max_size = layout.fields.iter().map(|f| f.size).max().unwrap_or(0);
        let max_align = layout.fields.iter().map(|f| f.align).max().unwrap_or(1);
        layout.total_size = if max_align > 0 {
            max_size.next_multiple_of(max_align)
        } else {
            max_size
        };
        layout.align = max_align;
        return;
    }

    let packed = layout.is_packed;
    let mut offset = 0usize;
    let mut struct_align = 1usize;

    for field in layout.fields.iter_mut() {
        let eff_align = if packed { 1 } else { field.align };
        if eff_align > 0 {
            offset = offset.next_multiple_of(eff_align);
        }
        field.offset = offset;
        offset += field.size;
        struct_align = struct_align.max(eff_align);
    }

    if !packed && struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }

    layout.total_size = offset;
    layout.align = struct_align;
}

/// Returns `true` if a `// padlock:ignore` comment appears on the line
/// immediately before (or inline on the same line as) the struct/union/type
/// declaration for `struct_name`.
///
/// This allows callers to suppress analysis for a specific struct by writing:
/// ```c
/// // padlock:ignore
/// struct MySpecialLayout { ... };
/// ```
fn is_padlock_ignored(source: &str, struct_name: &str) -> bool {
    // Keywords that introduce named type definitions across all supported languages
    for keyword in &["struct", "union", "type"] {
        let needle = format!("{keyword} {struct_name}");
        let mut search = 0usize;
        while let Some(rel) = source[search..].find(&needle) {
            let abs = search + rel;
            // Ensure the character after the name is a word boundary (not part of a longer name)
            let after_name = abs + needle.len();
            let is_boundary = source[after_name..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if is_boundary {
                let line_start = source[..abs].rfind('\n').map(|i| i + 1).unwrap_or(0);
                // Check the line containing the struct keyword for an inline annotation
                let line_end = source[abs..]
                    .find('\n')
                    .map(|i| abs + i)
                    .unwrap_or(source.len());
                if source[line_start..line_end].contains("padlock:ignore") {
                    return true;
                }
                // Check the immediately preceding line for an annotation comment.
                // Only accept it if the preceding line is a pure comment (starts with `//`
                // after trimming), so that an inline annotation on a prior struct's closing
                // line doesn't accidentally suppress the following struct.
                if line_start > 0 {
                    let prev_end = line_start - 1;
                    let prev_start = source[..prev_end].rfind('\n').map(|i| i + 1).unwrap_or(0);
                    let prev_trimmed = source[prev_start..prev_end].trim();
                    if prev_trimmed.starts_with("//") && prev_trimmed.contains("padlock:ignore") {
                        return true;
                    }
                }
            }
            search = abs + 1;
        }
    }
    false
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn detect_c_extensions() {
        assert_eq!(detect_language(Path::new("foo.c")), Some(SourceLanguage::C));
        assert_eq!(detect_language(Path::new("foo.h")), Some(SourceLanguage::C));
    }

    #[test]
    fn detect_cpp_extensions() {
        assert_eq!(
            detect_language(Path::new("foo.cpp")),
            Some(SourceLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.cc")),
            Some(SourceLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.hpp")),
            Some(SourceLanguage::Cpp)
        );
    }

    #[test]
    fn detect_rust_extension() {
        assert_eq!(
            detect_language(Path::new("foo.rs")),
            Some(SourceLanguage::Rust)
        );
    }

    #[test]
    fn detect_go_extension() {
        assert_eq!(
            detect_language(Path::new("foo.go")),
            Some(SourceLanguage::Go)
        );
    }

    #[test]
    fn detect_zig_extension() {
        assert_eq!(
            detect_language(Path::new("foo.zig")),
            Some(SourceLanguage::Zig)
        );
    }

    #[test]
    fn detect_unknown_is_none() {
        assert_eq!(detect_language(Path::new("foo.py")), None);
        assert_eq!(detect_language(Path::new("foo")), None);
    }

    #[test]
    fn parse_source_str_c_roundtrip() {
        let src = "struct Point { int x; int y; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
    }

    #[test]
    fn parse_source_str_rust_roundtrip() {
        let src = "struct Foo { x: u32, y: u64 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Foo");
    }

    #[test]
    fn padlock_ignore_suppresses_c_struct() {
        let src = "// padlock:ignore\nstruct Hidden { int x; int y; };\nstruct Visible { int a; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
    }

    #[test]
    fn padlock_ignore_inline_suppresses_c_struct() {
        // Inline annotation on the struct's own line suppresses it, but must NOT
        // suppress the struct that follows (the next struct's preceding line is a
        // code line with a trailing comment, not a pure `//` comment line).
        let src = "struct Hidden { int x; }; // padlock:ignore\nstruct Visible { int a; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1, "only Visible should remain");
        assert_eq!(layouts[0].name, "Visible");
    }

    #[test]
    fn padlock_ignore_suppresses_rust_struct() {
        let src = "// padlock:ignore\nstruct Hidden { x: u32 }\nstruct Visible { a: u32 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
    }

    #[test]
    fn padlock_ignore_without_annotation_keeps_struct() {
        let src = "struct Visible { int x; int y; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
    }

    // ── nested struct resolution ───────────────────────────────────────────────

    #[test]
    fn nested_rust_struct_size_resolved() {
        // Inner is 8 bytes. Outer has a field of type Inner.
        // Without resolution, Inner's field size would be pointer_size (8) — coincidentally
        // correct here, but offset placement still validates the pass runs.
        let src = "struct Inner { x: u64 }\nstruct Outer { a: u8, b: Inner }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let outer = layouts.iter().find(|l| l.name == "Outer").unwrap();
        let b = outer.fields.iter().find(|f| f.name == "b").unwrap();
        assert_eq!(b.size, 8, "Inner is 8 bytes");
        assert_eq!(b.align, 8, "Inner aligns to 8");
        // Outer: u8 at 0, [7 pad], Inner at 8 → total 16
        assert_eq!(outer.total_size, 16);
    }

    #[test]
    fn nested_rust_struct_non_pointer_size_resolved() {
        // Point is 8 bytes (two i32). Line contains two Points — should be 16 bytes, not
        // 2 * pointer_size = 16 (same here, but alignment is distinct).
        let src = "struct Point { x: i32, y: i32 }\nstruct Line { a: Point, b: Point }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let line = layouts.iter().find(|l| l.name == "Line").unwrap();
        assert_eq!(line.total_size, 16);
        assert_eq!(line.fields[0].size, 8);
        assert_eq!(line.fields[1].size, 8);
        assert_eq!(line.fields[1].offset, 8);
    }

    #[test]
    fn nested_rust_struct_large_inner_triggers_padding() {
        // SmallHeader: bool (1 byte). BigPayload: [u64; 4] = 32 bytes.
        // Wrapper { flag: SmallHeader, data: BigPayload }
        // Without resolution: SmallHeader is pointer-sized (8), total 8+32=40 → wrong.
        // With resolution: SmallHeader is 1 byte, then 7 pad, then BigPayload at 8 → total 40.
        // Actually u64 array: [u64;4] parsed as Array of 4 u64 = 32 bytes, align 8.
        let src = "struct SmallHeader { flag: bool }\nstruct Wrapper { h: SmallHeader, data: u64 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let wrapper = layouts.iter().find(|l| l.name == "Wrapper").unwrap();
        let h = wrapper.fields.iter().find(|f| f.name == "h").unwrap();
        // SmallHeader has total_size=1, align=1
        assert_eq!(h.size, 1, "SmallHeader resolved to 1 byte");
        assert_eq!(h.align, 1);
        // data (u64, align 8) should be at offset 8 (7 bytes padding after SmallHeader)
        let data = wrapper.fields.iter().find(|f| f.name == "data").unwrap();
        assert_eq!(data.offset, 8);
        assert_eq!(wrapper.total_size, 16);
    }

    #[test]
    fn nested_c_struct_resolved() {
        let src =
            "struct Vec2 { float x; float y; };\nstruct Rect { struct Vec2 tl; struct Vec2 br; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        let rect = layouts.iter().find(|l| l.name == "Rect").unwrap();
        // Each Vec2 is 8 bytes (two floats). Rect = 16 bytes, no padding.
        assert_eq!(rect.total_size, 16, "Rect should be 16 bytes");
        assert_eq!(rect.fields[0].size, 8);
        assert_eq!(rect.fields[1].size, 8);
        assert_eq!(rect.fields[1].offset, 8);
    }

    #[test]
    fn nested_go_struct_resolved() {
        let src = "package p\ntype Vec2 struct { X float32; Y float32 }\ntype Rect struct { TL Vec2; BR Vec2 }";
        let layouts = parse_source_str(src, &SourceLanguage::Go, &X86_64_SYSV).unwrap();
        let rect = layouts.iter().find(|l| l.name == "Rect").unwrap();
        assert_eq!(rect.total_size, 16);
        assert_eq!(rect.fields[0].size, 8);
        assert_eq!(rect.fields[1].size, 8);
        assert_eq!(rect.fields[1].offset, 8);
    }

    #[test]
    fn primitive_types_not_shadowed_by_struct_resolution() {
        // A struct named "u64" would be very unusual, but primitives must not be overwritten.
        let src = "struct Wrapper { x: u64, y: bool }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        let w = &layouts[0];
        let x = w.fields.iter().find(|f| f.name == "x").unwrap();
        assert_eq!(x.size, 8, "u64 must stay 8 bytes");
    }

    #[test]
    fn is_padlock_ignored_does_not_match_partial_names() {
        // "struct Foo" annotation must not suppress "struct FooBar"
        assert!(!is_padlock_ignored(
            "// padlock:ignore\nstruct FooBar { int x; };",
            "Foo"
        ));
    }
}
