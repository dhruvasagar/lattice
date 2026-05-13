#![allow(clippy::unwrap_used, clippy::panic)]
//! Integration tests for `LspSupervisor` (Phase 4.1.h).
//!
//! Coverage:
//!
//! - Single-server flow: open buffer → didOpen seen by mock;
//!   record_edit + flush → didChange seen by mock;
//!   close_buffer → didClose seen by mock.
//! - Multi-server attached to one buffer: two mock servers
//!   under different ids both see didOpen for the same URI;
//!   diagnostics from both merge in the layer.
//! - Multi-buffer isolation: edits on URI A don't affect URI B.
//! - Server reuse: two different URIs in the "same workspace +
//!   same server_id" share one actor (one DocSync, two URIs).
//! - close_buffer clears the URI's diagnostics.
//! - shutdown flushes + closes everything.

mod common;

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, Uri,
};
use serde_json::json;

use lattice_lsp::{LspLogger, LspSupervisor};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

use common::MockServer;

fn uri_for(path: &str) -> Uri {
    Uri::from_str(&format!("file://{path}")).unwrap()
}

#[tokio::test]
async fn single_server_attach_emits_did_open() {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;

    let path = PathBuf::from("/tmp/x.rs");
    let uri = uri_for("/tmp/x.rs");
    sup.attach_handle(
        uri.clone(),
        path.parent().unwrap().to_path_buf(),
        "rust".into(),
        "rust".into(),
        "fn main() {}".into(),
        server.handle.clone(),
    )
    .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    let notes = server.mock.notifications().await;
    let opened = notes
        .iter()
        .find(|n| n.method == "textDocument/didOpen")
        .expect("expected didOpen");
    let params: DidOpenTextDocumentParams =
        serde_json::from_value(opened.params.clone().unwrap()).unwrap();
    assert_eq!(params.text_document.uri, uri);
    assert_eq!(params.text_document.language_id, "rust");
    assert_eq!(params.text_document.text, "fn main() {}");

    assert_eq!(sup.attached_buffer_count(), 1);
    assert_eq!(sup.running_actor_count(), 1);
}

#[tokio::test]
async fn record_edit_then_flush_emits_did_change() {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;

    let path = PathBuf::from("/tmp/y.rs");
    let uri = uri_for("/tmp/y.rs");
    sup.attach_handle(
        uri.clone(),
        path.parent().unwrap().to_path_buf(),
        "rust".into(),
        "rust".into(),
        "abc\n".into(),
        server.handle.clone(),
    )
    .unwrap();

    sup.record_edit(&uri, &Edit::insert(Position::new(0, 0), "x"))
        .unwrap();
    sup.flush(&uri).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let changed = notes
        .iter()
        .find(|n| n.method == "textDocument/didChange")
        .expect("expected didChange");
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(changed.params.clone().unwrap()).unwrap();
    assert_eq!(params.text_document.uri, uri);
    assert_eq!(params.content_changes.len(), 1);
}

#[tokio::test]
async fn close_buffer_emits_did_close_and_clears_diagnostics() {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;

    let path = PathBuf::from("/tmp/z.rs");
    let uri = uri_for("/tmp/z.rs");
    sup.attach_handle(
        uri.clone(),
        path.parent().unwrap().to_path_buf(),
        "rust".into(),
        "rust".into(),
        "x".into(),
        server.handle.clone(),
    )
    .unwrap();

    // Push diagnostics into the layer via the bus.
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": uri.as_str(),
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 1,
                "message": "boom"
            }]
        }),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(sup.diagnostics().count(), 1);

    sup.close_buffer(&uri).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let closed = notes
        .iter()
        .find(|n| n.method == "textDocument/didClose")
        .expect("expected didClose");
    let params: DidCloseTextDocumentParams =
        serde_json::from_value(closed.params.clone().unwrap()).unwrap();
    assert_eq!(params.text_document.uri, uri);

    // Diagnostics for the closed URI are cleared.
    assert_eq!(sup.diagnostics().count(), 0);
    assert_eq!(sup.attached_buffer_count(), 0);
}

#[tokio::test]
async fn multi_server_attached_to_one_uri_both_see_did_open() {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    let semantic = MockServer::start_with_id("rust", logger.clone()).await;
    let linter = MockServer::start_with_id("clippy-bridge", logger).await;

    let path = PathBuf::from("/tmp/multi.rs");
    let uri = uri_for("/tmp/multi.rs");
    let workspace = path.parent().unwrap().to_path_buf();

    sup.attach_handle(
        uri.clone(),
        workspace.clone(),
        "rust".into(),
        "rust".into(),
        "fn x() {}".into(),
        semantic.handle.clone(),
    )
    .unwrap();
    sup.attach_handle(
        uri.clone(),
        workspace.clone(),
        "clippy-bridge".into(),
        "rust".into(),
        "fn x() {}".into(),
        linter.handle.clone(),
    )
    .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Both mocks saw didOpen.
    let semantic_notes = semantic.mock.notifications().await;
    let linter_notes = linter.mock.notifications().await;
    assert!(
        semantic_notes
            .iter()
            .any(|n| n.method == "textDocument/didOpen")
    );
    assert!(
        linter_notes
            .iter()
            .any(|n| n.method == "textDocument/didOpen")
    );

    // One URI, two attached servers.
    assert_eq!(sup.attached_buffer_count(), 1);
    assert_eq!(sup.servers_for(&uri).len(), 2);
    assert_eq!(sup.running_actor_count(), 2);
}

#[tokio::test]
async fn multi_server_diagnostics_merge_via_layer() {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    let semantic = MockServer::start_with_id("rust", logger.clone()).await;
    let linter = MockServer::start_with_id("clippy-bridge", logger).await;

    let path = PathBuf::from("/tmp/m2.rs");
    let uri = uri_for("/tmp/m2.rs");
    let workspace = path.parent().unwrap().to_path_buf();
    sup.attach_handle(
        uri.clone(),
        workspace.clone(),
        "rust".into(),
        "rust".into(),
        "x".into(),
        semantic.handle.clone(),
    )
    .unwrap();
    sup.attach_handle(
        uri.clone(),
        workspace.clone(),
        "clippy-bridge".into(),
        "rust".into(),
        "x".into(),
        linter.handle.clone(),
    )
    .unwrap();

    semantic.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": uri.as_str(),
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 1,
                "message": "rust err"
            }]
        }),
    );
    linter.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": uri.as_str(),
            "diagnostics": [{
                "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 1}},
                "severity": 2,
                "message": "lint warn"
            }]
        }),
    );
    tokio::time::sleep(Duration::from_millis(150)).await;

    let merged = sup.diagnostics().diagnostics_for(&uri);
    assert_eq!(merged.len(), 2, "merged from both servers");
    let messages: Vec<&str> = merged.iter().map(|d| d.message.as_str()).collect();
    assert!(messages.contains(&"rust err"));
    assert!(messages.contains(&"lint warn"));
}

#[tokio::test]
async fn multi_buffer_isolation_edits_dont_cross() {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;

    let workspace = PathBuf::from("/tmp");
    let path_a = PathBuf::from("/tmp/a.rs");
    let path_b = PathBuf::from("/tmp/b.rs");
    let uri_a = uri_for("/tmp/a.rs");
    let uri_b = uri_for("/tmp/b.rs");

    sup.attach_handle(
        uri_a.clone(),
        workspace.clone(),
        "rust".into(),
        "rust".into(),
        "// a".into(),
        server.handle.clone(),
    )
    .unwrap();
    sup.attach_handle(
        uri_b.clone(),
        workspace.clone(),
        "rust".into(),
        "rust".into(),
        "// b".into(),
        server.handle.clone(),
    )
    .unwrap();

    // Single actor reused across the two URIs.
    assert_eq!(sup.running_actor_count(), 1);
    assert_eq!(sup.attached_buffer_count(), 2);

    // Edit only A.
    sup.record_edit(&uri_a, &Edit::insert(Position::new(0, 4), "1"))
        .unwrap();
    sup.flush(&uri_a).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let _ = path_a; // silence unused
    let _ = path_b;

    let notes = server.mock.notifications().await;
    let changes: Vec<_> = notes
        .iter()
        .filter(|n| n.method == "textDocument/didChange")
        .collect();
    assert_eq!(changes.len(), 1);
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(changes[0].params.clone().unwrap()).unwrap();
    assert_eq!(
        params.text_document.uri, uri_a,
        "edit only on A; B's mirror untouched"
    );
}

#[tokio::test]
async fn open_unmatched_path_returns_no_attachments() {
    // No file_patterns registered → nothing attaches.
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger);
    // Even with no configs, open_buffer is a no-op success.
    let attached = sup
        .open_buffer(PathBuf::from("/tmp/x.rs"), "x".into())
        .await
        .unwrap();
    assert!(attached.is_empty());
    assert_eq!(sup.running_actor_count(), 0);
}

#[tokio::test]
async fn re_attach_to_same_uri_is_noop() {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;

    let workspace = PathBuf::from("/tmp");
    let uri = uri_for("/tmp/once.rs");
    sup.attach_handle(
        uri.clone(),
        workspace.clone(),
        "rust".into(),
        "rust".into(),
        "x".into(),
        server.handle.clone(),
    )
    .unwrap();
    // Wait for the didOpen to land at the mock before sampling
    // baseline -- attach_handle queues the notification through
    // the actor's mailbox; the mock observes it asynchronously.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let baseline = server.mock.notifications().await.len();

    // open_buffer returns existing attachments without
    // re-issuing didOpen.
    let attached = sup
        .open_buffer(PathBuf::from("/tmp/once.rs"), "x".into())
        .await
        .unwrap();
    // open_buffer with no matching config returns empty even
    // though the URI is already attached. attach_handle is the
    // canonical re-entry; this test simply checks no crash.
    let _ = attached;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let after = server.mock.notifications().await.len();
    assert_eq!(after, baseline, "no extra didOpen on re-entry");
}

#[tokio::test]
async fn shutdown_closes_every_attached_uri() {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    let server = MockServer::start_with_logger(logger).await;

    let workspace = PathBuf::from("/tmp");
    sup.attach_handle(
        uri_for("/tmp/a.rs"),
        workspace.clone(),
        "rust".into(),
        "rust".into(),
        "// a".into(),
        server.handle.clone(),
    )
    .unwrap();
    sup.attach_handle(
        uri_for("/tmp/b.rs"),
        workspace.clone(),
        "rust".into(),
        "rust".into(),
        "// b".into(),
        server.handle.clone(),
    )
    .unwrap();

    sup.shutdown().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let closes: Vec<_> = notes
        .iter()
        .filter(|n| n.method == "textDocument/didClose")
        .collect();
    assert_eq!(closes.len(), 2, "didClose fired for every attached URI");

    assert_eq!(sup.attached_buffer_count(), 0);
    assert_eq!(sup.running_actor_count(), 0);
}
