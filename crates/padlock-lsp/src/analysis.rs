use padlock_core::arch::ArchConfig;
use padlock_core::config::Config;
use padlock_core::findings::Report;
use padlock_source::SourceLanguage;

/// Parse `text` as `lang` and run all analysis passes, in-process, honouring
/// the project's `.padlock.toml` the same way the CLI's `analyze` command
/// does: `ignore` drops layouts before `Report::from_layouts` runs (so
/// ignored structs don't affect `aggregate_score`/`aggregate_grade` either),
/// and `min_severity`/per-struct overrides filter findings afterward via
/// `retain` — leaving `wasted_bytes`/`score` as computed from the full
/// analysis, not recomputed from the filtered subset. Same order, same
/// semantics, so a struct configured out of the CLI's output is also quiet
/// in the editor.
///
/// Parse errors (malformed source mid-edit, which is the normal state while
/// typing) produce an empty report rather than an error — diagnostics simply
/// go quiet until the buffer parses again, matching how editors expect a
/// language server to behave on invalid syntax.
pub fn analyze_text(
    text: &str,
    lang: &SourceLanguage,
    arch: &'static ArchConfig,
    config: &Config,
) -> Report {
    let layouts = match padlock_source::parse_source_str(text, lang, arch) {
        Ok(layouts) => layouts,
        Err(_) => return Report::from_layouts(&[]),
    };
    let kept: Vec<_> = layouts
        .into_iter()
        .filter(|l| !config.is_ignored(&l.name))
        .collect();

    let mut report = Report::from_layouts(&kept);
    for sr in &mut report.structs {
        sr.findings
            .retain(|f| config.should_report_for(&sr.struct_name, f.severity()));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;
    use padlock_core::findings::Severity;

    fn padded_c_struct() -> &'static str {
        "struct Connection { char a; double b; char c; int d; };"
    }

    #[test]
    fn default_config_reports_everything() {
        let report = analyze_text(
            padded_c_struct(),
            &SourceLanguage::C,
            &X86_64_SYSV,
            &Config::default(),
        );
        assert_eq!(report.structs.len(), 1);
        assert!(!report.structs[0].findings.is_empty());
    }

    #[test]
    fn ignored_struct_is_dropped_entirely() {
        let config = Config {
            ignore: vec!["Connection".to_string()],
            ..Config::default()
        };
        let report = analyze_text(padded_c_struct(), &SourceLanguage::C, &X86_64_SYSV, &config);
        assert!(report.structs.is_empty());
    }

    #[test]
    fn min_severity_filters_findings_but_keeps_struct_stats() {
        // padded_c_struct() has both a Medium PaddingWaste and a High
        // ReorderSuggestion finding; min_severity = High should drop the
        // Medium one but leave wasted_bytes/score as computed from the
        // full analysis, matching the CLI's `analyze` command.
        let config = Config {
            min_severity: Severity::High,
            ..Config::default()
        };
        let report = analyze_text(padded_c_struct(), &SourceLanguage::C, &X86_64_SYSV, &config);
        assert_eq!(report.structs.len(), 1);
        let s = &report.structs[0];
        assert!(s.wasted_bytes > 0, "stats stay from the full analysis");
        assert!(
            s.findings
                .iter()
                .all(|f| matches!(f.severity(), Severity::High)),
            "only High findings survive the filter"
        );
    }
}
