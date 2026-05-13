#![allow(clippy::unwrap_used, clippy::panic)]
//! Integration tests for the diagnostics broadcast bus
//! (Phase 4.1.d.i).
//!
//! Coverage:
//!
//! - Server pushes `publishDiagnostics` → subscriber receives a
//!   `DiagnosticEvent` with the URI / version / diagnostics
//!   intact and `server_id` set to the actor's id.
//! - Empty diagnostics array surfaces as a "clear" event
//!   (`is_clear()` true) rather than being filtered out.
//! - Multiple subscribers each receive every event (broadcast
//!   fan-out).
//! - A late subscriber doesn't see prior events.
//! - A malformed `publishDiagnostics` (params that don't match
//!   the LSP shape) is dropped without crashing the actor;
//!   subsequent valid events still arrive.

mod common;

use std::time::Duration;

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
use serde_json::json;

use common::MockServer;

fn make_diag(line: u32, msg: &str) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position { line, character: 0 },
            end: Position { line, character: 5 },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("rustc".into()),
        message: msg.into(),
        related_information: None,
        tags: None,
        data: None,
    }
}

#[tokio::test]
async fn server_publish_routes_to_subscriber_with_uri_version_and_message() {
    let server = MockServer::start().await;
    let mut rx = server.handle.subscribe_diagnostics();
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///foo.rs",
            "version": 7,
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}},
                "severity": 1,
                "source": "rustc",
                "message": "type mismatch"
            }]
        }),
    );
    let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("event arrived")
        .expect("event ok");
    assert_eq!(ev.uri.as_str(), "file:///foo.rs");
    assert_eq!(ev.version, Some(7));
    assert_eq!(ev.diagnostics.len(), 1);
    assert_eq!(ev.diagnostics[0].message, "type mismatch");
    assert_eq!(&*ev.server_id, "mock");
    assert!(!ev.is_clear());
}

#[tokio::test]
async fn empty_diagnostics_is_a_clear_event() {
    let server = MockServer::start().await;
    let mut rx = server.handle.subscribe_diagnostics();
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///clear.rs",
            "diagnostics": []
        }),
    );
    let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(ev.is_clear());
    assert_eq!(ev.version, None);
}

#[tokio::test]
async fn multiple_subscribers_each_receive_every_event() {
    let server = MockServer::start().await;
    let mut a = server.handle.subscribe_diagnostics();
    let mut b = server.handle.subscribe_diagnostics();
    assert_eq!(server.handle.diagnostics_subscriber_count(), 2);
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///fanout.rs",
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}},
                "severity": 2,
                "message": "warning"
            }]
        }),
    );
    let got_a = tokio::time::timeout(Duration::from_secs(1), a.recv())
        .await
        .unwrap()
        .unwrap();
    let got_b = tokio::time::timeout(Duration::from_secs(1), b.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got_a.uri, got_b.uri);
    assert_eq!(got_a.diagnostics[0].message, "warning");
}

#[tokio::test]
async fn late_subscriber_does_not_receive_prior_events() {
    let server = MockServer::start().await;
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///prior.rs",
            "diagnostics": []
        }),
    );
    // Give the actor time to process and broadcast.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut rx = server.handle.subscribe_diagnostics();
    let r = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(r.is_err(), "late subscriber should see no prior events");
}

#[tokio::test]
async fn malformed_publish_diagnostics_does_not_break_subscriber() {
    let server = MockServer::start().await;
    let mut rx = server.handle.subscribe_diagnostics();
    // Garbage params: missing the required `uri` field.
    server
        .mock
        .push_notification("textDocument/publishDiagnostics", json!({"oops": true}));
    // No event should arrive for the malformed publish.
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Now send a well-formed one and ensure it still arrives.
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///ok.rs",
            "diagnostics": []
        }),
    );
    let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ev.uri.as_str(), "file:///ok.rs");
}

#[tokio::test]
async fn many_diagnostics_in_one_publish_arrive_intact() {
    let server = MockServer::start().await;
    let mut rx = server.handle.subscribe_diagnostics();
    let diags: Vec<Diagnostic> = (0..50).map(|i| make_diag(i, &format!("err {i}"))).collect();
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///many.rs",
            "version": 42,
            "diagnostics": diags,
        }),
    );
    let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ev.diagnostics.len(), 50);
    assert_eq!(ev.diagnostics[0].message, "err 0");
    assert_eq!(ev.diagnostics[49].message, "err 49");
}
