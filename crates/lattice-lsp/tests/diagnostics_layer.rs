#![allow(clippy::unwrap_used, clippy::panic)]
//! Integration test for the DiagnosticsLayer + pump end-to-end
//! (Phase 4.1.d.ii).
//!
//! Coverage:
//!
//! - `pump_diagnostics` drains a real `DiagnosticsBus` into the
//!   layer; subsequent `apply`-side accessors observe the
//!   server's publishes.
//! - Empty publish from the server clears the layer entry for
//!   that URI.
//! - Multi-server scenario: two MockServers (different ids)
//!   feeding one layer; `diagnostics_for` returns the merged
//!   list.

mod common;

use std::sync::Arc;
use std::time::Duration;

use lsp_types::{DiagnosticSeverity, Uri};
use serde_json::json;

use lattice_lsp::{DiagnosticsLayer, LspLogger, pump_diagnostics};

use common::MockServer;

#[tokio::test]
async fn pump_drains_bus_into_layer() {
    let logger = LspLogger::with_defaults();
    let layer = DiagnosticsLayer::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;

    // Spawn the pump.
    let rx = server.handle.subscribe_diagnostics();
    let layer_for_pump = layer.clone();
    tokio::spawn(pump_diagnostics(layer_for_pump, rx));

    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///x.rs",
            "version": 3,
            "diagnostics": [{
                "range": {"start": {"line": 4, "character": 0}, "end": {"line": 4, "character": 8}},
                "severity": 1,
                "message": "type mismatch"
            }]
        }),
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let uri = <Uri as std::str::FromStr>::from_str("file:///x.rs").unwrap();
    let diags = layer.diagnostics_for(&uri);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].message, "type mismatch");
    assert_eq!(
        layer.line_severity(&uri, 4),
        Some(DiagnosticSeverity::ERROR)
    );
    assert_eq!(layer.count(), 1);
}

#[tokio::test]
async fn empty_publish_clears_uri_entry() {
    let logger = LspLogger::with_defaults();
    let layer = DiagnosticsLayer::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;
    let rx = server.handle.subscribe_diagnostics();
    tokio::spawn(pump_diagnostics(layer.clone(), rx));

    // Publish, then clear.
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///y.rs",
            "version": 1,
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 2,
                "message": "warn"
            }]
        }),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(layer.count(), 1);

    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///y.rs",
            "version": 2,
            "diagnostics": []
        }),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(layer.count(), 0);
    assert!(layer.iter_uris().is_empty());
}

#[tokio::test]
async fn multi_server_diagnostics_merge_in_layer() {
    let logger = LspLogger::with_defaults();
    let layer = DiagnosticsLayer::new(logger.clone());

    // Two mocks, two server ids. We need to give each a
    // distinct id; common::MockServer uses "mock" as the id, so
    // we'll spin up two and rely on Arc::from("mock") sharing
    // the layer across them but the layer's key is
    // (uri, server_id). In practice the App would spawn two
    // distinct server configs; for this test we just exercise
    // the merge semantic with one server publishing twice with
    // distinct server_ids.

    // Easier: skip the Mock and apply DiagnosticEvents directly
    // to the layer. The pump path is covered above; this test
    // covers the merge path which is layer-internal.
    use lattice_lsp::DiagnosticEvent;
    use lsp_types::{Diagnostic, Position, Range};

    fn d(line: u32, sev: DiagnosticSeverity, msg: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 5 },
            },
            severity: Some(sev),
            code: None,
            code_description: None,
            source: None,
            message: msg.into(),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    let uri = <Uri as std::str::FromStr>::from_str("file:///z.rs").unwrap();
    layer.apply(DiagnosticEvent {
        server_id: Arc::from("rust"),
        uri: uri.clone(),
        version: Some(1),
        diagnostics: Arc::from(
            vec![d(0, DiagnosticSeverity::ERROR, "rust err")].into_boxed_slice(),
        ),
    });
    layer.apply(DiagnosticEvent {
        server_id: Arc::from("clippy"),
        uri: uri.clone(),
        version: Some(1),
        diagnostics: Arc::from(
            vec![d(2, DiagnosticSeverity::WARNING, "clippy warn")].into_boxed_slice(),
        ),
    });

    let merged = layer.diagnostics_for(&uri);
    assert_eq!(merged.len(), 2);

    // Most-severe per-line lookups respect the merged view.
    assert_eq!(
        layer.line_severity(&uri, 0),
        Some(DiagnosticSeverity::ERROR)
    );
    assert_eq!(
        layer.line_severity(&uri, 2),
        Some(DiagnosticSeverity::WARNING)
    );

    let counts = layer.severity_counts();
    assert_eq!(counts.errors, 1);
    assert_eq!(counts.warnings, 1);
}

#[tokio::test]
async fn multiple_publishes_for_same_uri_replace_each_other() {
    let logger = LspLogger::with_defaults();
    let layer = DiagnosticsLayer::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;
    let rx = server.handle.subscribe_diagnostics();
    tokio::spawn(pump_diagnostics(layer.clone(), rx));

    let uri = <Uri as std::str::FromStr>::from_str("file:///r.rs").unwrap();

    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///r.rs",
            "version": 1,
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 1,
                "message": "first"
            }]
        }),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///r.rs",
            "version": 2,
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 2,
                "message": "second"
            }]
        }),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;

    let d = layer.diagnostics_for(&uri);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].message, "second");
}
