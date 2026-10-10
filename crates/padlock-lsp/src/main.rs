// padlock-lsp — a minimal language server exposing padlock's struct-layout
// findings (padding waste, reorder suggestions, false sharing, locality
// issues) as LSP diagnostics and hover text.
//
// This lets any LSP-capable editor (Neovim, Helix, Zed, JetBrains via the
// LSP4IJ plugin, Sublime, etc.) get the same feedback the VS Code extension
// provides, without a bespoke per-editor integration. It reuses
// `padlock_source::parse_source_str` directly (in-process, on the live
// buffer text) rather than shelling out to the `padlock` binary, so results
// reflect unsaved edits immediately on `didChange`.
//
// Scope is intentionally narrow for a first version: diagnostics + hover +
// reorder code actions for the five source languages padlock-source
// supports. No binary (DWARF/BTF) analysis, no workspace-wide analysis — the
// CLI already covers those, and this server's job is live-editing feedback.

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, Diagnostic, DiagnosticSeverity, DocumentSymbol,
    DocumentSymbolParams, DocumentSymbolResponse, Hover, HoverContents, HoverProviderCapability,
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams, InlayHintTooltip, MarkupContent,
    MarkupKind, OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities, SymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
};
use padlock_core::arch::{ArchConfig, X86_64_SYSV, arch_by_name};
use padlock_core::config::Config;
use padlock_core::findings::{Finding, StructReport};
use padlock_core::ir::StructLayout;
use padlock_source::SourceLanguage;

mod analysis;

use analysis::analyze_text;

fn main() -> anyhow::Result<()> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        ..Default::default()
    };
    let initialize_params = connection.initialize(serde_json::to_value(capabilities)?)?;
    let _ = initialize_params;

    run_server(&connection)?;

    // Drop the connection (and its sender half) before joining the I/O
    // threads: the stdout writer thread only exits once every sender is
    // gone, so joining while `connection` is still alive deadlocks forever
    // on `exit` — the editor's shutdown would hang instead of completing.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// Per-document state: the live buffer text (needed by code actions to
/// produce a fix — `fixgen::apply_fixes_*` rewrites source text, it doesn't
/// work from the IR alone) plus the most recent analysis, so hover and code
/// actions don't have to re-run diagnostics from scratch. `structs` already
/// reflects the project's `.padlock.toml` (`ignore` and severity filtering
/// applied in `analyze_text`) so every reader of this cache — hover, code
/// actions, inlay hints, document symbols — honours it for free. `arch` is
/// cached alongside so code actions re-parse with the same architecture the
/// diagnostics were computed against, instead of silently drifting back to
/// the default on an `arch.override` project.
struct DocState {
    text: String,
    lang: SourceLanguage,
    structs: Vec<StructReport>,
    arch: &'static ArchConfig,
}

/// Resolves `arch.override` from `.padlock.toml` via the same short names
/// the CLI's `--target` flag accepts (`aarch64`, `wasm32`, ...). Unlike the
/// CLI, there's no host-architecture fallback for an unrecognised name —
/// padlock-lsp deliberately doesn't depend on padlock-dwarf for that, so an
/// invalid override just falls back to the same `X86_64_SYSV` default as no
/// override at all.
fn resolve_arch(config: &Config) -> &'static ArchConfig {
    config
        .arch_override
        .as_deref()
        .and_then(arch_by_name)
        .unwrap_or(&X86_64_SYSV)
}

// Keyed by the URI's string form rather than `Uri` itself: `fluent_uri::Uri`
// caches parsed segments in a `Cell`, which trips clippy's mutable-key-type
// lint even though Hash/Eq are stably derived from `as_str()`.
type DocStore = HashMap<String, DocState>;

fn run_server(connection: &Connection) -> anyhow::Result<()> {
    let mut docs: DocStore = HashMap::new();

    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                handle_request(connection, &docs, req)?;
            }
            Message::Notification(not) => {
                handle_notification(connection, &mut docs, not)?;
            }
            Message::Response(_) => {
                // We never send requests to the client, so no responses expected.
            }
        }
    }
    Ok(())
}

fn handle_request(connection: &Connection, docs: &DocStore, req: Request) -> anyhow::Result<()> {
    if req.method == lsp_types::request::HoverRequest::METHOD {
        let params: lsp_types::HoverParams = serde_json::from_value(req.params)?;
        let hover = build_hover(
            docs,
            &params.text_document_position_params.text_document.uri,
            params.text_document_position_params.position,
        );
        send_response(connection, req.id, hover)?;
        return Ok(());
    }

    if req.method == lsp_types::request::CodeActionRequest::METHOD {
        let params: CodeActionParams = serde_json::from_value(req.params)?;
        let actions = build_code_actions(docs, &params);
        send_response(connection, req.id, actions)?;
        return Ok(());
    }

    if req.method == lsp_types::request::InlayHintRequest::METHOD {
        let params: InlayHintParams = serde_json::from_value(req.params)?;
        let hints = build_inlay_hints(docs, &params);
        send_response(connection, req.id, hints)?;
        return Ok(());
    }

    if req.method == lsp_types::request::DocumentSymbolRequest::METHOD {
        let params: DocumentSymbolParams = serde_json::from_value(req.params)?;
        let symbols = build_document_symbols(docs, &params);
        send_response(connection, req.id, symbols)?;
        return Ok(());
    }

    // Unknown request — respond with MethodNotFound rather than hanging the client.
    let resp = Response::new_err(
        req.id,
        ErrorCode::MethodNotFound as i32,
        format!("unhandled method: {}", req.method),
    );
    connection.sender.send(Message::Response(resp))?;
    Ok(())
}

fn send_response<T: serde::Serialize>(
    connection: &Connection,
    id: RequestId,
    value: T,
) -> anyhow::Result<()> {
    let resp = Response::new_ok(id, serde_json::to_value(value)?);
    connection.sender.send(Message::Response(resp))?;
    Ok(())
}

fn handle_notification(
    connection: &Connection,
    docs: &mut DocStore,
    not: Notification,
) -> anyhow::Result<()> {
    use lsp_types::notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    };

    match not.method.as_str() {
        m if m == DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams = serde_json::from_value(not.params)?;
            analyze_and_publish(
                connection,
                docs,
                &params.text_document.uri,
                &params.text_document.text,
            )?;
        }
        m if m == DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(not.params)?;
            // Full sync: the last content change carries the entire document text.
            if let Some(change) = params.content_changes.into_iter().last() {
                analyze_and_publish(connection, docs, &params.text_document.uri, &change.text)?;
            }
        }
        m if m == DidSaveTextDocument::METHOD => {
            let params: lsp_types::DidSaveTextDocumentParams = serde_json::from_value(not.params)?;
            if let Some(text) = params.text {
                analyze_and_publish(connection, docs, &params.text_document.uri, &text)?;
            }
        }
        m if m == DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams = serde_json::from_value(not.params)?;
            docs.remove(params.text_document.uri.as_str());
            publish_diagnostics(connection, &params.text_document.uri, &[])?;
        }
        _ => {}
    }
    Ok(())
}

/// `lsp_types::Uri` (a `fluent_uri` wrapper) has no filesystem-path
/// conversion of its own; round-trip through `url::Url`, which does.
fn uri_to_path(uri: &Uri) -> Option<std::path::PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

fn analyze_and_publish(
    connection: &Connection,
    docs: &mut DocStore,
    uri: &Uri,
    text: &str,
) -> anyhow::Result<()> {
    let Some(path) = uri_to_path(uri) else {
        return Ok(());
    };
    let Some(lang) = padlock_source::detect_language(&path) else {
        return Ok(());
    };

    let config = Config::for_path(&path);
    let arch = resolve_arch(&config);
    let report = analyze_text(text, &lang, arch, &config);
    let diagnostics = report
        .structs
        .iter()
        .flat_map(diagnostics_for_struct)
        .collect::<Vec<_>>();

    docs.insert(
        uri.as_str().to_string(),
        DocState {
            text: text.to_string(),
            lang,
            structs: report.structs,
            arch,
        },
    );
    publish_diagnostics(connection, uri, &diagnostics)?;
    Ok(())
}

fn publish_diagnostics(
    connection: &Connection,
    uri: &Uri,
    diagnostics: &[Diagnostic],
) -> anyhow::Result<()> {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics: diagnostics.to_vec(),
        version: None,
    };
    let notification = Notification::new(
        lsp_types::notification::PublishDiagnostics::METHOD.to_string(),
        serde_json::to_value(params)?,
    );
    connection
        .sender
        .send(Message::Notification(notification))?;
    Ok(())
}

fn diagnostics_for_struct(s: &StructReport) -> Vec<Diagnostic> {
    let Some(line) = s.source_line else {
        return Vec::new();
    };
    let line = line.saturating_sub(1); // LSP lines are 0-based; padlock's are 1-based.
    let range = Range::new(Position::new(line, 0), Position::new(line, u32::MAX));

    s.findings
        .iter()
        .map(|f| Diagnostic {
            range,
            severity: Some(map_severity(f.severity())),
            source: Some("padlock".to_string()),
            code: Some(lsp_types::NumberOrString::String(f.kind_name().to_string())),
            message: format_message(s, f),
            ..Default::default()
        })
        .collect()
}

fn map_severity(s: &padlock_core::findings::Severity) -> DiagnosticSeverity {
    match s {
        padlock_core::findings::Severity::High => DiagnosticSeverity::WARNING,
        padlock_core::findings::Severity::Medium => DiagnosticSeverity::INFORMATION,
        padlock_core::findings::Severity::Low => DiagnosticSeverity::HINT,
    }
}

fn format_message(s: &StructReport, f: &Finding) -> String {
    match f {
        Finding::PaddingWaste {
            wasted_bytes,
            waste_pct,
            ..
        } => format!(
            "{}: {}B wasted ({:.0}% of {}B).",
            s.struct_name, wasted_bytes, waste_pct, s.total_size
        ),
        Finding::ReorderSuggestion {
            savings,
            optimized_size,
            ..
        } => format!(
            "{}: reordering fields saves {}B ({}B → {}B).",
            s.struct_name, savings, s.total_size, optimized_size
        ),
        Finding::FalseSharing { is_inferred, .. } => {
            let note = if *is_inferred {
                " (inferred from type names — verify with profiling or add guard annotations)"
            } else {
                ""
            };
            format!(
                "{}: false sharing — concurrently-accessed fields share a cache line.{}",
                s.struct_name, note
            )
        }
        Finding::LocalityIssue { is_inferred, .. } => {
            let note = if *is_inferred {
                " (inferred from type names — verify with profiling)"
            } else {
                ""
            };
            format!(
                "{}: hot and cold fields are interleaved.{}",
                s.struct_name, note
            )
        }
    }
}

/// Shared full-detail markdown body for a struct's findings — used by both
/// hover (as the content) and inlay hints (as the tooltip shown on hover
/// over the hint itself), so the two surfaces stay in sync.
fn struct_markdown(s: &StructReport) -> String {
    let mut text = format!("**padlock** — `{}`\n\n", s.struct_name);
    text += &format!("Score **{:.0}**/100 · {}B", s.score, s.total_size);
    if s.wasted_bytes > 0 {
        text += &format!(" · **{}B wasted**", s.wasted_bytes);
    }
    text += "\n\n";
    for f in &s.findings {
        text += &format!("- **{}** — {}\n", f.kind_name(), format_message(s, f));
    }
    text
}

fn build_hover(docs: &DocStore, uri: &Uri, position: Position) -> Option<Hover> {
    let doc = docs.get(uri.as_str())?;
    let line = position.line + 1; // padlock's source_line is 1-based.
    let s = doc.structs.iter().find(|s| s.source_line == Some(line))?;
    if s.findings.is_empty() {
        return None;
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: struct_markdown(s),
        }),
        range: None,
    })
}

/// Compact one-line label for a struct's inlay hint — the full detail lives
/// in the hint's tooltip (`struct_markdown`), reusing the same split hover
/// already uses (compact signal always visible, detail on demand).
fn inlay_label(s: &StructReport) -> Option<String> {
    if s.wasted_bytes > 0 {
        return Some(format!(
            " {}B wasted (score {:.0})",
            s.wasted_bytes, s.score
        ));
    }
    if let Some(Finding::ReorderSuggestion { savings, .. }) = s
        .findings
        .iter()
        .find(|f| matches!(f, Finding::ReorderSuggestion { .. }))
    {
        return Some(format!(" reorder saves {savings}B"));
    }
    if s.findings
        .iter()
        .any(|f| matches!(f, Finding::FalseSharing { .. }))
    {
        return Some(" false sharing".to_string());
    }
    if s.findings
        .iter()
        .any(|f| matches!(f, Finding::LocalityIssue { .. }))
    {
        return Some(" locality issue".to_string());
    }
    None
}

/// Character offset (UTF-16 code units, per the LSP spec) of the end of
/// `line` in `text` — used to anchor the inlay hint after the struct's
/// declaration rather than overlapping it.
fn line_end_char(text: &str, line: u32) -> u32 {
    text.split('\n')
        .nth(line as usize)
        .map(|l| l.encode_utf16().count() as u32)
        .unwrap_or(0)
}

fn build_inlay_hints(docs: &DocStore, params: &InlayHintParams) -> Vec<InlayHint> {
    let Some(doc) = docs.get(params.text_document.uri.as_str()) else {
        return Vec::new();
    };

    doc.structs
        .iter()
        .filter_map(|s| {
            let line0 = s.source_line?.saturating_sub(1); // LSP lines are 0-based.
            if line0 < params.range.start.line || line0 > params.range.end.line {
                return None;
            }
            let label = inlay_label(s)?;

            Some(InlayHint {
                position: Position::new(line0, line_end_char(&doc.text, line0)),
                label: InlayHintLabel::String(label),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: Some(InlayHintTooltip::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: struct_markdown(s),
                })),
                padding_left: Some(true),
                padding_right: None,
                data: None,
            })
        })
        .collect()
}

/// One outline entry per struct, named and scored (`N bytes · score X`) so
/// the editor's outline/breadcrumb view works as a quick severity overview
/// without opening hover on each one. `structs` is already filtered by
/// `.padlock.toml` (see `DocState`'s doc comment), so an ignored struct
/// doesn't show up here either.
#[allow(deprecated)] // `DocumentSymbol::deprecated` has no Default to omit it via.
fn build_document_symbols(
    docs: &DocStore,
    params: &DocumentSymbolParams,
) -> DocumentSymbolResponse {
    let Some(doc) = docs.get(params.text_document.uri.as_str()) else {
        return DocumentSymbolResponse::Nested(Vec::new());
    };

    let symbols = doc
        .structs
        .iter()
        .filter_map(|s| {
            let line0 = s.source_line?.saturating_sub(1); // LSP lines are 0-based.
            let range = Range::new(
                Position::new(line0, 0),
                Position::new(line0, line_end_char(&doc.text, line0)),
            );
            Some(DocumentSymbol {
                name: s.struct_name.clone(),
                detail: Some(format!("{}B · score {:.0}", s.total_size, s.score)),
                kind: SymbolKind::STRUCT,
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            })
        })
        .collect();

    DocumentSymbolResponse::Nested(symbols)
}

fn build_code_actions(docs: &DocStore, params: &CodeActionParams) -> Vec<CodeActionOrCommand> {
    let uri = &params.text_document.uri;
    let Some(doc) = docs.get(uri.as_str()) else {
        return Vec::new();
    };

    let reorder_names: Vec<&str> = doc
        .structs
        .iter()
        .filter(|s| {
            s.findings
                .iter()
                .any(|f| matches!(f, Finding::ReorderSuggestion { .. }))
        })
        .map(|s| s.struct_name.as_str())
        .collect();
    if reorder_names.is_empty() {
        return Vec::new();
    }

    // fixgen::apply_fixes_* rewrites source text from the IR, so re-parse
    // the cached buffer rather than trying to derive layouts from the
    // already-scored StructReport (which doesn't carry per-field IR). Uses
    // doc.arch (not the bare default) so a reorder computed here matches
    // the architecture the diagnostics were scored against.
    let Ok(layouts) = padlock_source::parse_source_str(&doc.text, &doc.lang, doc.arch) else {
        return Vec::new();
    };

    let range_line = params.range.start.line + 1; // padlock's source_line is 1-based.
    let in_range_struct = doc.structs.iter().find(|s| {
        s.source_line == Some(range_line) && reorder_names.contains(&s.struct_name.as_str())
    });

    let mut actions = Vec::new();

    if let Some(s) = in_range_struct
        && let Some(layout) = layouts.iter().find(|l| l.name == s.struct_name)
        && let Some(action) = reorder_action(
            format!("Reorder `{}` fields (padlock)", s.struct_name),
            uri,
            doc,
            &[layout],
            true,
        )
    {
        actions.push(action);
    }

    // Only offer "fix all" when there's more than one — otherwise it's a
    // duplicate of the single-struct action above.
    if reorder_names.len() > 1 {
        let to_fix: Vec<&StructLayout> = layouts
            .iter()
            .filter(|l| reorder_names.contains(&l.name.as_str()))
            .collect();
        if let Some(action) = reorder_action(
            "Fix all reorder suggestions in file (padlock)".to_string(),
            uri,
            doc,
            &to_fix,
            false,
        ) {
            actions.push(action);
        }
    }

    actions
}

/// End position of `text` as an LSP `Position` (line/char both 0-based,
/// char counted in UTF-16 code units per the LSP spec) — used for a
/// whole-document `TextEdit` range. Computed precisely rather than with a
/// `u32::MAX` sentinel: not every LSP client clamps an out-of-bounds
/// position as forgivingly as VS Code does.
fn end_position(text: &str) -> Position {
    let line_count = text.split('\n').count();
    let last_line = text.rsplit('\n').next().unwrap_or("");
    Position::new(
        (line_count - 1) as u32,
        last_line.encode_utf16().count() as u32,
    )
}

fn reorder_action(
    title: String,
    uri: &Uri,
    doc: &DocState,
    layouts: &[&StructLayout],
    is_preferred: bool,
) -> Option<CodeActionOrCommand> {
    if layouts.is_empty() {
        return None;
    }
    let fixed = apply_fix(&doc.lang, &doc.text, layouts);
    if fixed == doc.text {
        return None;
    }

    let full_range = Range::new(Position::new(0, 0), end_position(&doc.text));
    // WorkspaceEdit::changes is a HashMap<Uri, _> in lsp_types itself — not
    // our choice of key type. fluent_uri::Uri's Hash/Eq are stably derived
    // from as_str() despite the Cell clippy's lint is reacting to; see the
    // DocStore comment above for the same reasoning.
    #[allow(clippy::mutable_key_type)]
    let changes = HashMap::from([(uri.clone(), vec![TextEdit::new(full_range, fixed)])]);

    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        is_preferred: Some(is_preferred),
        ..Default::default()
    }))
}

fn apply_fix(lang: &SourceLanguage, source: &str, layouts: &[&StructLayout]) -> String {
    use padlock_source::fixgen;
    match lang {
        SourceLanguage::C | SourceLanguage::Cpp => fixgen::apply_fixes_c(source, layouts),
        SourceLanguage::Rust => fixgen::apply_fixes_rust(source, layouts),
        SourceLanguage::Go => fixgen::apply_fixes_go(source, layouts),
        SourceLanguage::Zig => fixgen::apply_fixes_zig(source, layouts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn padded_c_struct() -> &'static str {
        "struct Connection { char a; double b; char c; int d; };"
    }

    fn insert_doc(docs: &mut DocStore, uri: &Uri, text: &str, lang: SourceLanguage) {
        let report = analyze_text(text, &lang, &X86_64_SYSV, &Config::default());
        docs.insert(
            uri.as_str().to_string(),
            DocState {
                text: text.to_string(),
                lang,
                structs: report.structs,
                arch: &X86_64_SYSV,
            },
        );
    }

    #[test]
    fn uri_to_path_roundtrips_a_file_uri() {
        // `url::Url::to_file_path()` requires a drive letter on Windows and
        // a root-relative path on Unix — a real editor always sends the
        // platform-appropriate form, so the test must too.
        #[cfg(windows)]
        let (uri_str, expected) = ("file:///C:/tmp/foo.c", "C:\\tmp\\foo.c");
        #[cfg(not(windows))]
        let (uri_str, expected) = ("file:///tmp/foo.c", "/tmp/foo.c");

        let uri = Uri::from_str(uri_str).unwrap();
        assert_eq!(uri_to_path(&uri), Some(std::path::PathBuf::from(expected)));
    }

    #[test]
    fn uri_to_path_rejects_non_file_scheme() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        assert_eq!(uri_to_path(&uri), None);
    }

    #[test]
    fn analyze_text_finds_padding_waste_in_c_struct() {
        let report = analyze_text(
            padded_c_struct(),
            &padlock_source::SourceLanguage::C,
            &X86_64_SYSV,
            &Config::default(),
        );
        assert_eq!(report.structs.len(), 1);
        assert!(report.structs[0].wasted_bytes > 0);
    }

    #[test]
    fn analyze_text_on_malformed_source_returns_empty_report_not_panic() {
        let report = analyze_text(
            "struct {{{ nonsense",
            &padlock_source::SourceLanguage::C,
            &X86_64_SYSV,
            &Config::default(),
        );
        assert!(report.structs.is_empty());
    }

    #[test]
    fn diagnostics_for_struct_converts_line_to_zero_based() {
        let report = analyze_text(
            padded_c_struct(),
            &padlock_source::SourceLanguage::C,
            &X86_64_SYSV,
            &Config::default(),
        );
        let s = &report.structs[0];
        assert_eq!(s.source_line, Some(1));
        let diags = diagnostics_for_struct(s);
        assert!(!diags.is_empty());
        assert_eq!(diags[0].range.start.line, 0);
    }

    #[test]
    fn diagnostics_for_struct_with_no_source_line_emits_nothing() {
        let report = analyze_text(
            padded_c_struct(),
            &padlock_source::SourceLanguage::C,
            &X86_64_SYSV,
            &Config::default(),
        );
        let mut s = report.structs.into_iter().next().unwrap();
        s.source_line = None;
        assert!(diagnostics_for_struct(&s).is_empty());
    }

    #[test]
    fn build_hover_returns_none_for_unknown_document() {
        let docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/missing.c").unwrap();
        assert!(build_hover(&docs, &uri, Position::new(0, 0)).is_none());
    }

    #[test]
    fn build_hover_returns_markdown_for_known_struct_line() {
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/conn.c").unwrap();
        insert_doc(&mut docs, &uri, padded_c_struct(), SourceLanguage::C);

        let hover = build_hover(&docs, &uri, Position::new(0, 0)).expect("hover for line 0");
        match hover.contents {
            HoverContents::Markup(m) => assert!(m.value.contains("Connection")),
            _ => panic!("expected markup contents"),
        }
    }

    #[test]
    fn map_severity_orders_high_above_medium_above_low() {
        use padlock_core::findings::Severity;
        assert_eq!(map_severity(&Severity::High), DiagnosticSeverity::WARNING);
        assert_eq!(
            map_severity(&Severity::Medium),
            DiagnosticSeverity::INFORMATION
        );
        assert_eq!(map_severity(&Severity::Low), DiagnosticSeverity::HINT);
    }

    fn code_action_params(uri: &Uri, line: u32) -> CodeActionParams {
        CodeActionParams {
            text_document: lsp_types::TextDocumentIdentifier::new(uri.clone()),
            range: Range::new(Position::new(line, 0), Position::new(line, 0)),
            context: lsp_types::CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[test]
    fn end_position_counts_lines_and_utf16_units() {
        assert_eq!(end_position("abc"), Position::new(0, 3));
        assert_eq!(end_position("a\nbc"), Position::new(1, 2));
        assert_eq!(end_position(""), Position::new(0, 0));
    }

    #[test]
    // Same false-positive as `reorder_action`'s allow above: `changes` is a
    // `HashMap<Uri, _>` owned by lsp_types, not a type this code chose.
    #[allow(clippy::mutable_key_type)]
    fn code_action_offers_reorder_fix_for_padded_struct() {
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/conn.c").unwrap();
        insert_doc(&mut docs, &uri, padded_c_struct(), SourceLanguage::C);

        let actions = build_code_actions(&docs, &code_action_params(&uri, 0));
        assert_eq!(
            actions.len(),
            1,
            "single struct: one reorder action, no redundant fix-all"
        );
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected a CodeAction, not a Command");
        };
        assert!(action.title.contains("Connection"));
        assert_eq!(action.is_preferred, Some(true));
        let edit = action.edit.as_ref().expect("edit present");
        let changes = edit.changes.as_ref().expect("changes present");
        let edits = changes.get(&uri).expect("edit for this uri");
        assert_eq!(edits.len(), 1);
        assert_ne!(edits[0].new_text, padded_c_struct());
        assert!(edits[0].new_text.contains("Connection"));
    }

    #[test]
    fn code_action_on_already_optimal_struct_is_empty() {
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/ok.c").unwrap();
        // Already descending-alignment order: nothing to reorder.
        insert_doc(
            &mut docs,
            &uri,
            "struct Ok { double b; int d; char a; char c; };",
            SourceLanguage::C,
        );

        assert!(build_code_actions(&docs, &code_action_params(&uri, 0)).is_empty());
    }

    #[test]
    fn code_action_offers_fix_all_when_multiple_structs_need_reorder() {
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/multi.c").unwrap();
        let src = "struct A { char a; double b; char c; int d; };\n\
                   struct B { char e; double f; char g; int h; };\n";
        insert_doc(&mut docs, &uri, src, SourceLanguage::C);

        let actions = build_code_actions(&docs, &code_action_params(&uri, 0));
        // One action scoped to the struct on line 0, plus one "fix all".
        assert_eq!(actions.len(), 2);
        let titles: Vec<&str> = actions
            .iter()
            .map(|a| match a {
                CodeActionOrCommand::CodeAction(a) => a.title.as_str(),
                CodeActionOrCommand::Command(c) => c.title.as_str(),
            })
            .collect();
        assert!(
            titles
                .iter()
                .any(|t| t.contains("fix all") || t.contains("Fix all"))
        );
    }

    #[test]
    fn code_action_for_unknown_document_is_empty() {
        let docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/missing.c").unwrap();
        assert!(build_code_actions(&docs, &code_action_params(&uri, 0)).is_empty());
    }

    fn inlay_hint_params(uri: &Uri, start_line: u32, end_line: u32) -> InlayHintParams {
        InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: lsp_types::TextDocumentIdentifier::new(uri.clone()),
            range: Range::new(Position::new(start_line, 0), Position::new(end_line, 0)),
        }
    }

    #[test]
    fn line_end_char_counts_utf16_units_per_line() {
        assert_eq!(line_end_char("abc\nde", 0), 3);
        assert_eq!(line_end_char("abc\nde", 1), 2);
        assert_eq!(line_end_char("abc\nde", 5), 0);
    }

    #[test]
    fn inlay_hint_reports_wasted_bytes_for_padded_struct() {
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/conn.c").unwrap();
        insert_doc(&mut docs, &uri, padded_c_struct(), SourceLanguage::C);

        let hints = build_inlay_hints(&docs, &inlay_hint_params(&uri, 0, 0));
        assert_eq!(hints.len(), 1);
        let InlayHintLabel::String(label) = &hints[0].label else {
            panic!("expected string label");
        };
        assert!(label.contains("wasted"));
        assert_eq!(hints[0].position.line, 0);
        assert!(hints[0].tooltip.is_some());
    }

    #[test]
    fn inlay_hint_on_struct_with_no_findings_is_empty() {
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/ok.c").unwrap();
        // Two 8-byte-aligned fields, no gaps, no trailing padding: genuinely
        // zero findings (unlike the 4-field fixture used for code actions,
        // which still has 2B of unavoidable trailing padding).
        insert_doc(
            &mut docs,
            &uri,
            "struct Ok { double b; long d; };",
            SourceLanguage::C,
        );

        assert!(build_inlay_hints(&docs, &inlay_hint_params(&uri, 0, 0)).is_empty());
    }

    #[test]
    fn inlay_hint_respects_requested_line_range() {
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/conn.c").unwrap();
        insert_doc(&mut docs, &uri, padded_c_struct(), SourceLanguage::C);

        // Struct is on line 0; a range starting at line 5 should exclude it.
        assert!(build_inlay_hints(&docs, &inlay_hint_params(&uri, 5, 10)).is_empty());
    }

    #[test]
    fn inlay_hint_for_unknown_document_is_empty() {
        let docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/missing.c").unwrap();
        assert!(build_inlay_hints(&docs, &inlay_hint_params(&uri, 0, 0)).is_empty());
    }

    #[test]
    fn resolve_arch_honors_valid_override() {
        let config = Config {
            arch_override: Some("aarch64".to_string()),
            ..Config::default()
        };
        assert_eq!(resolve_arch(&config).name, "aarch64");
    }

    #[test]
    fn resolve_arch_falls_back_on_unknown_override() {
        let config = Config {
            arch_override: Some("not-a-real-arch".to_string()),
            ..Config::default()
        };
        assert_eq!(resolve_arch(&config).name, X86_64_SYSV.name);
    }

    #[test]
    fn resolve_arch_falls_back_when_absent() {
        assert_eq!(resolve_arch(&Config::default()).name, X86_64_SYSV.name);
    }

    fn document_symbol_params(uri: &Uri) -> DocumentSymbolParams {
        DocumentSymbolParams {
            text_document: lsp_types::TextDocumentIdentifier::new(uri.clone()),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[test]
    fn document_symbols_lists_struct_with_name_and_detail() {
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/conn.c").unwrap();
        insert_doc(&mut docs, &uri, padded_c_struct(), SourceLanguage::C);

        let DocumentSymbolResponse::Nested(symbols) =
            build_document_symbols(&docs, &document_symbol_params(&uri))
        else {
            panic!("expected Nested response");
        };
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Connection");
        assert_eq!(symbols[0].kind, SymbolKind::STRUCT);
        assert!(symbols[0].detail.as_ref().unwrap().contains("score"));
        assert_eq!(symbols[0].range.start.line, 0);
    }

    #[test]
    fn document_symbols_for_unknown_document_is_empty() {
        let docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/missing.c").unwrap();
        let DocumentSymbolResponse::Nested(symbols) =
            build_document_symbols(&docs, &document_symbol_params(&uri))
        else {
            panic!("expected Nested response");
        };
        assert!(symbols.is_empty());
    }
}
