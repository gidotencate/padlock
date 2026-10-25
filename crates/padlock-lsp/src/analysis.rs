use padlock_core::arch::ArchConfig;
use padlock_core::config::Config;
use padlock_core::findings::Report;
use padlock_source::SourceLanguage;
use std::path::Path;

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
    path: Option<&Path>,
    text: &str,
    lang: &SourceLanguage,
    arch: &'static ArchConfig,
    config: &Config,
) -> Report {
    // The parse settings below are process-global; hold this lock until the
    // parse is done so concurrent callers (parallel tests) can't swap them
    // out from under each other.
    static PARSE_SETTINGS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = PARSE_SETTINGS.lock().unwrap_or_else(|e| e.into_inner());

    // Parse settings from `.padlock.toml`, as the CLI applies them.
    padlock_source::set_preprocessor_defines(&config.defines);
    padlock_source::set_declared_order(config.declared_order);
    padlock_source::set_configured_instantiations(
        config
            .instantiate
            .iter()
            .map(|(name, args)| (name.clone(), padlock_source::split_type_args(args)))
            .collect(),
    );
    let layouts = match parse_buffer(path, text, lang, arch) {
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

/// Layouts of the buffer `text`: against its package, crate or includes when
/// the file's path is known and they can be found, on its own otherwise.
pub fn parse_buffer(
    path: Option<&Path>,
    text: &str,
    lang: &SourceLanguage,
    arch: &'static ArchConfig,
) -> anyhow::Result<Vec<padlock_core::ir::StructLayout>> {
    match path {
        Some(p) => padlock_source::parse_source_text(p, text, lang, arch),
        None => padlock_source::parse_source_str(text, lang, arch),
    }
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
            None,
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
        let report = analyze_text(
            None,
            padded_c_struct(),
            &SourceLanguage::C,
            &X86_64_SYSV,
            &config,
        );
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
        let report = analyze_text(
            None,
            padded_c_struct(),
            &SourceLanguage::C,
            &X86_64_SYSV,
            &config,
        );
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

    #[test]
    fn config_defines_select_ifdef_branches() {
        let src = "struct S {\n char a;\n#ifdef WIDE\n long w;\n#endif\n};\n";
        let plain = analyze_text(
            None,
            src,
            &SourceLanguage::C,
            &X86_64_SYSV,
            &Config::default(),
        );
        assert_eq!(plain.structs[0].total_size, 1);
        let cfg = Config {
            defines: vec!["WIDE".to_string()],
            ..Config::default()
        };
        let wide = analyze_text(None, src, &SourceLanguage::C, &X86_64_SYSV, &cfg);
        assert_eq!(wide.structs[0].total_size, 16);
        // Reset the process-global setting for other tests.
        analyze_text(
            None,
            src,
            &SourceLanguage::C,
            &X86_64_SYSV,
            &Config::default(),
        );
    }

    #[test]
    fn unsaved_go_buffer_sees_the_rest_of_its_package() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("geom.go"),
            "package p\n\ntype Rect struct{ A, B, C, D int }\n",
        )
        .unwrap();
        let file = dir.path().join("main.go");
        std::fs::write(&file, "package p\n\ntype T struct{ x int }\n").unwrap();
        // The editor buffer differs from the saved file.
        let buffer = "package p\n\ntype T struct {\n\tflag bool\n\tr    Rect\n}\n";
        let report = analyze_text(
            Some(&file),
            buffer,
            &SourceLanguage::Go,
            &X86_64_SYSV,
            &Config::default(),
        );
        let t = report
            .structs
            .iter()
            .find(|s| s.struct_name == "T")
            .unwrap();
        // bool + 7 padding + Rect (32) from geom.go.
        assert_eq!(t.total_size, 40);
        assert!(t.uncertain_fields.is_empty(), "{:?}", t.uncertain_fields);
    }

    #[test]
    fn unsaved_rust_buffer_sees_the_rest_of_its_crate() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "mod inner;\n").unwrap();
        std::fs::write(
            src.join("inner.rs"),
            "pub struct Inner { pub a: u64, pub b: u32 }\n",
        )
        .unwrap();
        let file = src.join("lib.rs");
        let buffer = "mod inner;\npub struct Uses { pub x: inner::Inner, pub f: u8 }\n";
        let report = analyze_text(
            Some(&file),
            buffer,
            &SourceLanguage::Rust,
            &X86_64_SYSV,
            &Config::default(),
        );
        let u = report
            .structs
            .iter()
            .find(|s| s.struct_name == "Uses")
            .unwrap();
        assert_eq!(u.total_size, 24);
        assert!(u.uncertain_fields.is_empty(), "{:?}", u.uncertain_fields);
    }

    #[test]
    fn c_buffer_sees_the_headers_it_includes() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("types.h"),
            "typedef long stamp_t;\n#define N 4\n",
        )
        .unwrap();
        let file = dir.path().join("a.c");
        let buffer = "#include \"types.h\"\nstruct S { char c; stamp_t t; char b[N]; };\n";
        let report = analyze_text(
            Some(&file),
            buffer,
            &SourceLanguage::C,
            &X86_64_SYSV,
            &Config::default(),
        );
        let s = report
            .structs
            .iter()
            .find(|s| s.struct_name == "S")
            .unwrap();
        // char + 7, long 8, char[4] + 4 = 24.
        assert_eq!(s.total_size, 24);
        assert!(s.uncertain_fields.is_empty(), "{:?}", s.uncertain_fields);
    }

    #[test]
    fn without_context_the_buffer_is_analyzed_alone() {
        // A file that does not exist (unsaved): no package, no includes.
        let report = analyze_text(
            Some(Path::new("/nonexistent/new.go")),
            "package p\n\ntype T struct{ a int8; b int64 }\n",
            &SourceLanguage::Go,
            &X86_64_SYSV,
            &Config::default(),
        );
        assert_eq!(report.structs[0].total_size, 16);
    }
}
