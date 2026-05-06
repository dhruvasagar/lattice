#![allow(clippy::unwrap_used, clippy::panic)]
//! Integration tests for the document-sync layer (Phase 4.1.c).
//!
//! End-to-end coverage against the in-process MockServer:
//!
//! - `didOpen` arrives with language id + version 1 + initial text
//! - `record_edit` queues; `flush` sends one `didChange` with the
//!   queued events under Incremental sync mode
//! - Multi-edit batching: three edits between flushes → one
//!   didChange with three change events
//! - Full sync mode: queued events are dropped, the entire
//!   post-edit text is sent
//! - `didClose` arrives + the mirror is dropped (subsequent
//!   record_edit fails)
//! - Version monotonically increases across edits
//! - utf-8 negotiated → no column conversion
//! - utf-16 negotiated → range characters reflect utf-16 units

mod common;

use std::str::FromStr;
use std::time::Duration;

use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    PositionEncodingKind, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};

use lattice_lsp::DocSync;
use lattice_lsp::error::LspResult;

use common::{MockServer, default_server_capabilities};

fn caps_with_sync(kind: TextDocumentSyncKind, encoding: PositionEncodingKind) -> ServerCapabilities {
    let mut c = default_server_capabilities();
    c.text_document_sync = Some(TextDocumentSyncCapability::Kind(kind));
    c.position_encoding = Some(encoding);
    c
}

async fn mock_with_server_caps(caps: ServerCapabilities) -> MockServer {
    MockServer::start_with_capabilities(caps).await
}

// ---------- Bridge helpers (Phase 4.x DocSync refactor) ----------
//
// DocSync is now pure state -- it returns LSP params instead of
// sending. These helpers wrap the new API + push the returned
// params through the mock server's handle so the test bodies stay
// readable. Each test owns its own DocSync; capabilities come off
// the mock server's handshake.

fn t_open(sync: &mut DocSync, server: &MockServer, uri: Uri, lang: &str, text: &str) {
    let params = sync.open(uri, lang, text);
    server
        .handle
        .notify("textDocument/didOpen", params)
        .expect("notify didOpen");
}

fn t_record(
    sync: &mut DocSync,
    server: &MockServer,
    uri: &Uri,
    edit: &lattice_protocol::edit::Edit,
) -> LspResult<()> {
    let caps = server.handle.capabilities();
    sync.record_edit(&caps, uri, edit)
}

fn t_flush(sync: &mut DocSync, server: &MockServer, uri: &Uri) {
    let caps = server.handle.capabilities();
    if let Some(params) = sync.take_flush_payload(&caps, uri) {
        server
            .handle
            .notify("textDocument/didChange", params)
            .expect("notify didChange");
    }
}

fn t_flush_all(sync: &mut DocSync, server: &MockServer) {
    let caps = server.handle.capabilities();
    for (_uri, params) in sync.take_flush_all_payloads(&caps) {
        server
            .handle
            .notify("textDocument/didChange", params)
            .expect("notify didChange (flush_all)");
    }
}

fn t_close(sync: &mut DocSync, server: &MockServer, uri: &Uri) {
    let caps = server.handle.capabilities();
    if let Some(payloads) = sync.close(&caps, uri) {
        if let Some(final_changes) = payloads.final_changes {
            server
                .handle
                .notify("textDocument/didChange", final_changes)
                .expect("notify final didChange");
        }
        server
            .handle
            .notify("textDocument/didClose", payloads.close)
            .expect("notify didClose");
    }
}

#[tokio::test]
async fn open_emits_did_open_with_initial_text_and_version_one() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/x.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "fn main() {}");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let opened = notes.iter().find(|n| n.method == "textDocument/didOpen");
    let opened = opened.expect("expected didOpen");
    let params: DidOpenTextDocumentParams =
        serde_json::from_value(opened.params.clone().unwrap()).unwrap();
    assert_eq!(params.text_document.uri.as_str(), "file:///tmp/x.rs");
    assert_eq!(params.text_document.language_id, "rust");
    assert_eq!(params.text_document.version, 1);
    assert_eq!(params.text_document.text, "fn main() {}");
    assert_eq!(sync.version(&uri), Some(1));
}

#[tokio::test]
async fn record_then_flush_emits_one_did_change_with_queued_events() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/y.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "abc\n");

    // Insert "x" at start.
    t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 0), "x"))
        .unwrap();
    // Append "y" at end of line 0 ("xabc" now).
    t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 4), "y"))
        .unwrap();

    assert!(sync.has_pending(&uri));
    t_flush(&mut sync, &server, &uri);
    assert!(!sync.has_pending(&uri));
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let changed = notes
        .iter()
        .find(|n| n.method == "textDocument/didChange")
        .expect("expected didChange");
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(changed.params.clone().unwrap()).unwrap();
    assert_eq!(params.text_document.version, 3); // 1 (open) + 2 edits
    assert_eq!(params.content_changes.len(), 2);
    // Each change has a range (Incremental).
    assert!(params.content_changes.iter().all(|c| c.range.is_some()));
}

#[tokio::test]
async fn full_sync_mode_sends_entire_text_no_range() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::FULL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/full.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "fn a() {}\n");

    t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 5), "_b"))
        .unwrap();
    t_flush(&mut sync, &server, &uri);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let changed = notes
        .iter()
        .find(|n| n.method == "textDocument/didChange")
        .expect("expected didChange");
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(changed.params.clone().unwrap()).unwrap();
    assert_eq!(params.content_changes.len(), 1);
    assert!(
        params.content_changes[0].range.is_none(),
        "Full sync sends a single change with no range"
    );
    // Inserted "_b" at byte 5 (just before ')'); mirror is now
    // "fn a(_b) {}\n".
    assert_eq!(params.content_changes[0].text, "fn a(_b) {}\n");
}

#[tokio::test]
async fn close_flushes_pending_then_sends_did_close() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/c.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "fn x() {}\n");
    t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 0), "// "))
        .unwrap();
    t_close(&mut sync, &server, &uri);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let methods: Vec<&str> = notes
        .iter()
        .map(|n| n.method.as_str())
        .filter(|m| {
            matches!(*m, "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didClose")
        })
        .collect();
    // Expected order: didOpen, didChange (flushed by close), didClose.
    assert_eq!(
        methods,
        vec![
            "textDocument/didOpen",
            "textDocument/didChange",
            "textDocument/didClose",
        ]
    );

    // didClose carries the URI we opened.
    let closed_note = notes
        .iter()
        .find(|n| n.method == "textDocument/didClose")
        .unwrap();
    let params: DidCloseTextDocumentParams =
        serde_json::from_value(closed_note.params.clone().unwrap()).unwrap();
    assert_eq!(params.text_document.uri, uri);

    assert!(!sync.is_open(&uri));
}

#[tokio::test]
async fn record_after_close_is_error() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/r.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "x");
    t_close(&mut sync, &server, &uri);
    let r = t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 0), "a"));
    assert!(r.is_err());
}

#[tokio::test]
async fn utf16_negotiated_encodes_columns_in_code_units() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF16,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/utf16.rs").unwrap();
    // 😀 = 4 utf-8 bytes, 2 utf-16 units.
    t_open(&mut sync, &server, uri.clone(), "rust", "x😀y");
    // Insert "z" between '😀' and 'y' (lattice byte offset 5).
    t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 5), "z"))
        .unwrap();
    t_flush(&mut sync, &server, &uri);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let changed = notes
        .iter()
        .find(|n| n.method == "textDocument/didChange")
        .expect("expected didChange");
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(changed.params.clone().unwrap()).unwrap();
    let r = params.content_changes[0].range.expect("range");
    // utf-16 column for byte 5 = "x" (1) + "😀" (2) = 3.
    assert_eq!(r.start.character, 3);
    assert_eq!(r.end.character, 3);
}

#[tokio::test]
async fn utf8_negotiated_keeps_byte_offsets_unchanged() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/u8.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "x😀y");
    t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 5), "z"))
        .unwrap();
    t_flush(&mut sync, &server, &uri);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let changed = notes
        .iter()
        .find(|n| n.method == "textDocument/didChange")
        .expect("expected didChange");
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(changed.params.clone().unwrap()).unwrap();
    let r = params.content_changes[0].range.expect("range");
    assert_eq!(r.start.character, 5);
    assert_eq!(r.end.character, 5);
}

#[tokio::test]
async fn version_increments_per_edit() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/v.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "");
    assert_eq!(sync.version(&uri), Some(1));
    t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 0), "a"))
        .unwrap();
    assert_eq!(sync.version(&uri), Some(2));
    t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 1), "b"))
        .unwrap();
    assert_eq!(sync.version(&uri), Some(3));
}

#[tokio::test]
async fn replace_edit_round_trips_with_correct_range_and_text() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/repl.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "alpha beta\n");
    // Replace "beta" (bytes 6..10 on line 0) with "gamma".
    t_record(
        &mut sync,
        &server,
        &uri,
        &Edit::replace(
            Range::new(Position::new(0, 6), Position::new(0, 10)),
            "gamma",
        ),
    )
    .unwrap();
    t_flush(&mut sync, &server, &uri);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let changed = notes
        .iter()
        .find(|n| n.method == "textDocument/didChange")
        .expect("expected didChange");
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(changed.params.clone().unwrap()).unwrap();
    let c = &params.content_changes[0];
    let r = c.range.expect("range");
    assert_eq!(r.start.character, 6);
    assert_eq!(r.end.character, 10);
    assert_eq!(c.text, "gamma");
}

#[tokio::test]
async fn flush_all_emits_per_open_doc() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let a = Uri::from_str("file:///tmp/a.rs").unwrap();
    let b = Uri::from_str("file:///tmp/b.rs").unwrap();
    t_open(&mut sync, &server, a.clone(), "rust", "// a");
    t_open(&mut sync, &server, b.clone(), "rust", "// b");
    t_record(&mut sync, &server, &a, &Edit::insert(Position::new(0, 4), "1"))
        .unwrap();
    t_record(&mut sync, &server, &b, &Edit::insert(Position::new(0, 4), "2"))
        .unwrap();
    t_flush_all(&mut sync, &server);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    let changes: Vec<_> = notes
        .iter()
        .filter(|n| n.method == "textDocument/didChange")
        .collect();
    assert_eq!(changes.len(), 2);
}

#[tokio::test]
async fn no_flush_when_nothing_queued() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///tmp/q.rs").unwrap();
    t_open(&mut sync, &server, uri.clone(), "rust", "x");
    // Wait for the didOpen notification to land at the mock so
    // the baseline count is stable.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let baseline = server.mock.notifications().await.len();
    t_flush(&mut sync, &server, &uri);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let after = server.mock.notifications().await.len();
    assert_eq!(after, baseline, "flush of empty queue is a no-op");
}

#[tokio::test]
async fn record_edit_on_unopened_uri_is_error() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new();
    let uri = Uri::from_str("file:///never-opened.rs").unwrap();
    let r = t_record(&mut sync, &server, &uri, &Edit::insert(Position::new(0, 0), "x"));
    assert!(r.is_err());
}
