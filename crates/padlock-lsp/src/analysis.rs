use padlock_core::arch::ArchConfig;
use padlock_core::findings::Report;
use padlock_source::SourceLanguage;

/// Parse `text` as `lang` and run all analysis passes, in-process.
///
/// Parse errors (malformed source mid-edit, which is the normal state while
/// typing) produce an empty report rather than an error — diagnostics simply
/// go quiet until the buffer parses again, matching how editors expect a
/// language server to behave on invalid syntax.
pub fn analyze_text(text: &str, lang: &SourceLanguage, arch: &'static ArchConfig) -> Report {
    match padlock_source::parse_source_str(text, lang, arch) {
        Ok(layouts) => Report::from_layouts(&layouts),
        Err(_) => Report::from_layouts(&[]),
    }
}
