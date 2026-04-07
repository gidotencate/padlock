// padlock-source/src/lib.rs

pub mod concurrency;
pub mod fixgen;
pub mod frontends;

use std::path::Path;

use padlock_core::arch::ArchConfig;
use padlock_core::ir::StructLayout;

#[derive(Debug, Clone, PartialEq)]
pub enum SourceLanguage {
    C,
    Cpp,
    Rust,
    Go,
}

/// Detect language from file extension.
pub fn detect_language(path: &Path) -> Option<SourceLanguage> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("c") | Some("h")            => Some(SourceLanguage::C),
        Some("cpp") | Some("cc")
        | Some("cxx") | Some("hpp")      => Some(SourceLanguage::Cpp),
        Some("rs")                        => Some(SourceLanguage::Rust),
        Some("go")                        => Some(SourceLanguage::Go),
        _                                 => None,
    }
}

/// Parse a source file and return struct layouts.
pub fn parse_source(path: &Path, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let lang = detect_language(path)
        .ok_or_else(|| anyhow::anyhow!("unsupported file type: {}", path.display()))?;
    let source = std::fs::read_to_string(path)?;
    parse_source_str(&source, &lang, arch)
}

/// Parse source text directly (useful for tests and piped input).
pub fn parse_source_str(
    source: &str,
    lang: &SourceLanguage,
    arch: &'static ArchConfig,
) -> anyhow::Result<Vec<StructLayout>> {
    let mut layouts = match lang {
        SourceLanguage::C    => frontends::c_cpp::parse_c(source, arch)?,
        SourceLanguage::Cpp  => frontends::c_cpp::parse_cpp(source, arch)?,
        SourceLanguage::Rust => frontends::rust::parse_rust(source, arch)?,
        SourceLanguage::Go   => frontends::go::parse_go(source, arch)?,
    };

    // Annotate concurrency patterns
    for layout in &mut layouts {
        concurrency::annotate_concurrency(layout, lang);
    }

    // Remove structs explicitly opted out via `// padlock:ignore`
    layouts.retain(|layout| !is_padlock_ignored(source, &layout.name));

    Ok(layouts)
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
                .map_or(true, |c| !c.is_alphanumeric() && c != '_');
            if is_boundary {
                let line_start = source[..abs].rfind('\n').map(|i| i + 1).unwrap_or(0);
                // Check the line containing the struct keyword for an inline annotation
                let line_end = source[abs..].find('\n').map(|i| abs + i).unwrap_or(source.len());
                if source[line_start..line_end].contains("padlock:ignore") {
                    return true;
                }
                // Check the immediately preceding line for an annotation comment.
                // Only accept it if the preceding line is a pure comment (starts with `//`
                // after trimming), so that an inline annotation on a prior struct's closing
                // line doesn't accidentally suppress the following struct.
                if line_start > 0 {
                    let prev_end   = line_start - 1;
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

    #[test]
    fn detect_c_extensions() {
        assert_eq!(detect_language(Path::new("foo.c")),   Some(SourceLanguage::C));
        assert_eq!(detect_language(Path::new("foo.h")),   Some(SourceLanguage::C));
    }

    #[test]
    fn detect_cpp_extensions() {
        assert_eq!(detect_language(Path::new("foo.cpp")), Some(SourceLanguage::Cpp));
        assert_eq!(detect_language(Path::new("foo.cc")),  Some(SourceLanguage::Cpp));
        assert_eq!(detect_language(Path::new("foo.hpp")), Some(SourceLanguage::Cpp));
    }

    #[test]
    fn detect_rust_extension() {
        assert_eq!(detect_language(Path::new("foo.rs")),  Some(SourceLanguage::Rust));
    }

    #[test]
    fn detect_go_extension() {
        assert_eq!(detect_language(Path::new("foo.go")),  Some(SourceLanguage::Go));
    }

    #[test]
    fn detect_unknown_is_none() {
        assert_eq!(detect_language(Path::new("foo.py")),  None);
        assert_eq!(detect_language(Path::new("foo")),     None);
    }

    #[test]
    fn parse_source_str_c_roundtrip() {
        use padlock_core::arch::X86_64_SYSV;
        let src = "struct Point { int x; int y; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
    }

    #[test]
    fn parse_source_str_rust_roundtrip() {
        use padlock_core::arch::X86_64_SYSV;
        let src = "struct Foo { x: u32, y: u64 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Foo");
    }

    #[test]
    fn padlock_ignore_suppresses_c_struct() {
        use padlock_core::arch::X86_64_SYSV;
        let src = "// padlock:ignore\nstruct Hidden { int x; int y; };\nstruct Visible { int a; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
    }

    #[test]
    fn padlock_ignore_inline_suppresses_c_struct() {
        use padlock_core::arch::X86_64_SYSV;
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
        use padlock_core::arch::X86_64_SYSV;
        let src = "// padlock:ignore\nstruct Hidden { x: u32 }\nstruct Visible { a: u32 }";
        let layouts = parse_source_str(src, &SourceLanguage::Rust, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
    }

    #[test]
    fn padlock_ignore_without_annotation_keeps_struct() {
        use padlock_core::arch::X86_64_SYSV;
        let src = "struct Visible { int x; int y; };";
        let layouts = parse_source_str(src, &SourceLanguage::C, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Visible");
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
