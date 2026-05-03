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

#[tokio::test]
async fn open_emits_did_open_with_initial_text_and_version_one() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/x.rs").unwrap();
    sync.open(uri.clone(), "rust", "fn main() {}").unwrap();
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
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/y.rs").unwrap();
    sync.open(uri.clone(), "rust", "abc\n").unwrap();

    // Insert "x" at start.
    sync.record_edit(&uri, &Edit::insert(Position::new(0, 0), "x"))
        .unwrap();
    // Append "y" at end of line 0 ("xabc" now).
    sync.record_edit(&uri, &Edit::insert(Position::new(0, 4), "y"))
        .unwrap();

    assert!(sync.has_pending(&uri));
    assert!(sync.flush(&uri).is_ok());
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
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/full.rs").unwrap();
    sync.open(uri.clone(), "rust", "fn a() {}\n").unwrap();

    sync.record_edit(&uri, &Edit::insert(Position::new(0, 5), "_b"))
        .unwrap();
    sync.flush(&uri).unwrap();
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
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/c.rs").unwrap();
    sync.open(uri.clone(), "rust", "fn x() {}\n").unwrap();
    sync.record_edit(&uri, &Edit::insert(Position::new(0, 0), "// ")).unwrap();
    sync.close(&uri).unwrap();
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
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/r.rs").unwrap();
    sync.open(uri.clone(), "rust", "x").unwrap();
    sync.close(&uri).unwrap();
    let r = sync.record_edit(&uri, &Edit::insert(Position::new(0, 0), "a"));
    assert!(r.is_err());
}

#[tokio::test]
async fn utf16_negotiated_encodes_columns_in_code_units() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF16,
    ))
    .await;
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/utf16.rs").unwrap();
    // 😀 = 4 utf-8 bytes, 2 utf-16 units.
    sync.open(uri.clone(), "rust", "x😀y").unwrap();
    // Insert "z" between '😀' and 'y' (lattice byte offset 5).
    sync.record_edit(&uri, &Edit::insert(Position::new(0, 5), "z")).unwrap();
    sync.flush(&uri).unwrap();
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
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/u8.rs").unwrap();
    sync.open(uri.clone(), "rust", "x😀y").unwrap();
    sync.record_edit(&uri, &Edit::insert(Position::new(0, 5), "z")).unwrap();
    sync.flush(&uri).unwrap();
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
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/v.rs").unwrap();
    sync.open(uri.clone(), "rust", "").unwrap();
    assert_eq!(sync.version(&uri), Some(1));
    sync.record_edit(&uri, &Edit::insert(Position::new(0, 0), "a")).unwrap();
    assert_eq!(sync.version(&uri), Some(2));
    sync.record_edit(&uri, &Edit::insert(Position::new(0, 1), "b")).unwrap();
    assert_eq!(sync.version(&uri), Some(3));
}

#[tokio::test]
async fn replace_edit_round_trips_with_correct_range_and_text() {
    let server = mock_with_server_caps(caps_with_sync(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/repl.rs").unwrap();
    sync.open(uri.clone(), "rust", "alpha beta\n").unwrap();
    // Replace "beta" (bytes 6..10 on line 0) with "gamma".
    sync.record_edit(
        &uri,
        &Edit::replace(
            Range::new(Position::new(0, 6), Position::new(0, 10)),
            "gamma",
        ),
    )
    .unwrap();
    sync.flush(&uri).unwrap();
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
    let mut sync = DocSync::new(server.handle.clone());
    let a = Uri::from_str("file:///tmp/a.rs").unwrap();
    let b = Uri::from_str("file:///tmp/b.rs").unwrap();
    sync.open(a.clone(), "rust", "// a").unwrap();
    sync.open(b.clone(), "rust", "// b").unwrap();
    sync.record_edit(&a, &Edit::insert(Position::new(0, 4), "1")).unwrap();
    sync.record_edit(&b, &Edit::insert(Position::new(0, 4), "2")).unwrap();
    sync.flush_all().unwrap();
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
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///tmp/q.rs").unwrap();
    sync.open(uri.clone(), "rust", "x").unwrap();
    // Wait for the didOpen notification to land at the mock so
    // the baseline count is stable.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let baseline = server.mock.notifications().await.len();
    sync.flush(&uri).unwrap();
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
    let mut sync = DocSync::new(server.handle.clone());
    let uri = Uri::from_str("file:///never-opened.rs").unwrap();
    let r = sync.record_edit(&uri, &Edit::insert(Position::new(0, 0), "x"));
    assert!(r.is_err());
}
