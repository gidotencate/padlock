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
// Scope is intentionally narrow for a first version: diagnostics + hover for
// the five source languages padlock-source supports. No code actions, no
// binary (DWARF/BTF) analysis, no workspace-wide analysis — the CLI already
// covers those, and this server's job is live-editing feedback.

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::{
    Diagnostic, DiagnosticSeverity, Hover, HoverContents, HoverProviderCapability, MarkupContent,
    MarkupKind, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use padlock_core::arch::X86_64_SYSV;
use padlock_core::findings::{Finding, StructReport};

mod analysis;

use analysis::analyze_text;

fn main() -> anyhow::Result<()> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
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

/// Per-document analysis cache, keyed by URI. Holds the struct reports from
/// the most recent analysis so the hover handler doesn't have to re-parse.
// Keyed by the URI's string form rather than `Uri` itself: `fluent_uri::Uri`
// caches parsed segments in a `Cell`, which trips clippy's mutable-key-type
// lint even though Hash/Eq are stably derived from `as_str()`.
type DocStore = HashMap<String, Vec<StructReport>>;

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

    let report = analyze_text(text, &lang, &X86_64_SYSV);
    let diagnostics = report
        .structs
        .iter()
        .flat_map(diagnostics_for_struct)
        .collect::<Vec<_>>();

    docs.insert(uri.as_str().to_string(), report.structs);
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

fn build_hover(docs: &DocStore, uri: &Uri, position: Position) -> Option<Hover> {
    let structs = docs.get(uri.as_str())?;
    let line = position.line + 1; // padlock's source_line is 1-based.
    let s = structs.iter().find(|s| s.source_line == Some(line))?;
    if s.findings.is_empty() {
        return None;
    }

    let mut text = format!("**padlock** — `{}`\n\n", s.struct_name);
    text += &format!("Score **{:.0}**/100 · {}B", s.score, s.total_size);
    if s.wasted_bytes > 0 {
        text += &format!(" · **{}B wasted**", s.wasted_bytes);
    }
    text += "\n\n";
    for f in &s.findings {
        text += &format!("- **{}** — {}\n", f.kind_name(), format_message(s, f));
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: text,
        }),
        range: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn padded_c_struct() -> &'static str {
        "struct Connection { char a; double b; char c; int d; };"
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
        );
        assert!(report.structs.is_empty());
    }

    #[test]
    fn diagnostics_for_struct_converts_line_to_zero_based() {
        let report = analyze_text(
            padded_c_struct(),
            &padlock_source::SourceLanguage::C,
            &X86_64_SYSV,
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
        let report = analyze_text(
            padded_c_struct(),
            &padlock_source::SourceLanguage::C,
            &X86_64_SYSV,
        );
        let mut docs: DocStore = HashMap::new();
        let uri = Uri::from_str("file:///tmp/conn.c").unwrap();
        docs.insert(uri.as_str().to_string(), report.structs);

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
}
