//! Integration tests for the per-actor edit fan-in
//! (`lattice_lsp::fan_in`).
//!
//! These cover the new edit-path architecture (see
//! docs/dev/architecture/lsp-architecture.md §11):
//!
//! - publish on the bus -> actor sees `didChange` with the
//!   right ranges and texts (the case the old supervisor-mutex
//!   path was silently dropping under contention),
//! - FIFO between `OpenDoc` and `RecordEdit` (open precedes the
//!   first edit even when the publisher races),
//! - error modes: unknown URI, missing path, dead actor.

#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use common::MockServer;
use lsp_types::{
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, PositionEncodingKind,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

use lattice_lsp::fan_in;
use lattice_protocol::EventKind;
use lattice_protocol::event::AppliedEdit;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

fn caps_with(
    kind: TextDocumentSyncKind,
    encoding: PositionEncodingKind,
) -> ServerCapabilities {
    let mut c = ServerCapabilities::default();
    c.text_document_sync = Some(TextDocumentSyncCapability::Kind(kind));
    c.position_encoding = Some(encoding);
    c
}

// M.5.5: fan_in subscribes to the typed `LspDocumentChanged`
// event (gated at the App publish site on `lsp-mode`); these
// tests publish that directly via `bus.publish_typed`.
fn doc_changed_event(path: PathBuf, edits: Vec<AppliedEdit>) -> lattice_lsp::LspDocumentChanged {
    lattice_lsp::LspDocumentChanged {
        id: DocumentId::new(1),
        path: Some(path),
        version: 1,
        edits,
    }
}

fn applied_insert(at_col: u32, text: &str) -> AppliedEdit {
    AppliedEdit {
        original_range: Range::empty(Position::new(0, at_col)),
        inserted_range: Range::new(
            Position::new(0, at_col),
            Position::new(0, at_col + text.len() as u32),
        ),
        replaced_text: String::new(),
        inserted_text: text.to_string(),
    }
}

#[tokio::test]
async fn end_to_end_publish_emits_did_change_with_correct_payload() {
    let server = MockServer::start_with_capabilities(caps_with(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;

    let bus = Arc::new(EventBus::new());
    let _sub = fan_in::spawn(server.handle.clone(), bus.clone());

    // Open a document directly on the actor (the supervisor's
    // open path does this in production; fan-in only handles
    // the edit path).
    let path = PathBuf::from("/tmp/x.rs");
    let uri = Uri::from_str("file:///tmp/x.rs").unwrap();
    server
        .handle
        .open_doc(uri.clone(), "rust", "fn main() {}")
        .unwrap();

    // Publish three edits. The fan-in should forward them as
    // RecordEdits in order; the actor's debounce arm coalesces
    // them into one `didChange`.
    let edits = vec![
        applied_insert(0, "a"),
        applied_insert(1, "b"),
        applied_insert(2, "c"),
    ];
    bus.publish_typed(doc_changed_event(path, edits));

    // Wait past the actor's 50ms debounce window.
    tokio::time::sleep(Duration::from_millis(120)).await;

    let notes = server.mock.notifications().await;
    let did_change: Vec<_> = notes
        .iter()
        .filter(|n| n.method == "textDocument/didChange")
        .collect();
    assert_eq!(
        did_change.len(),
        1,
        "expected exactly one coalesced didChange, got {}",
        did_change.len()
    );
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(did_change[0].params.clone().unwrap()).unwrap();
    assert_eq!(params.content_changes.len(), 3);
    let texts: Vec<&str> = params
        .content_changes
        .iter()
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(texts, vec!["a", "b", "c"]);
}

#[tokio::test]
async fn open_doc_precedes_first_edit_on_the_wire() {
    let server = MockServer::start_with_capabilities(caps_with(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;

    let bus = Arc::new(EventBus::new());
    let _sub = fan_in::spawn(server.handle.clone(), bus.clone());

    let path = PathBuf::from("/tmp/x.rs");
    let uri = Uri::from_str("file:///tmp/x.rs").unwrap();

    // Order: OpenDoc, then publish an edit. Both go through
    // the same actor cmd_tx (via different paths), so FIFO
    // guarantees didOpen wins on the wire.
    server.handle.open_doc(uri.clone(), "rust", "").unwrap();
    bus.publish_typed(doc_changed_event(path, vec![applied_insert(0, "x")]));

    tokio::time::sleep(Duration::from_millis(120)).await;

    let notes = server.mock.notifications().await;
    let methods: Vec<&str> = notes.iter().map(|n| n.method.as_str()).collect();
    let open_idx = methods
        .iter()
        .position(|m| *m == "textDocument/didOpen")
        .expect("didOpen should be sent");
    let change_idx = methods
        .iter()
        .position(|m| *m == "textDocument/didChange")
        .expect("didChange should be sent");
    assert!(
        open_idx < change_idx,
        "didOpen ({open_idx}) must precede didChange ({change_idx}); methods={methods:?}"
    );
    let opened: DidOpenTextDocumentParams =
        serde_json::from_value(notes[open_idx].params.clone().unwrap()).unwrap();
    assert_eq!(opened.text_document.text, "");
    let changed: DidChangeTextDocumentParams =
        serde_json::from_value(notes[change_idx].params.clone().unwrap()).unwrap();
    assert_eq!(changed.text_document.version, 2);
    assert_eq!(changed.content_changes[0].text, "x");
}

#[tokio::test]
async fn edit_for_unknown_uri_is_warn_and_skip_not_panic() {
    let server = MockServer::start_with_capabilities(caps_with(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;

    let bus = Arc::new(EventBus::new());
    let _sub = fan_in::spawn(server.handle.clone(), bus.clone());

    // Do NOT open the URI -- the actor has no mirror for it.
    // Publishing edits should warn and skip; the actor must
    // stay alive.
    let path = PathBuf::from("/tmp/never-opened.rs");
    bus.publish_typed(doc_changed_event(path, vec![applied_insert(0, "x")]));
    tokio::time::sleep(Duration::from_millis(120)).await;

    let notes = server.mock.notifications().await;
    assert!(
        !notes.iter().any(|n| n.method == "textDocument/didChange"),
        "no didChange should fire for an unopened URI"
    );

    // Actor still healthy: open after the no-op edit.
    let uri = Uri::from_str("file:///tmp/x.rs").unwrap();
    server.handle.open_doc(uri, "rust", "ok").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let notes = server.mock.notifications().await;
    assert!(
        notes.iter().any(|n| n.method == "textDocument/didOpen"),
        "actor should still process new commands after the bad edit"
    );
}

#[tokio::test]
async fn event_with_no_path_is_ignored() {
    let server = MockServer::start_with_capabilities(caps_with(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let bus = Arc::new(EventBus::new());
    let _sub = fan_in::spawn(server.handle.clone(), bus.clone());

    bus.publish_typed(lattice_lsp::LspDocumentChanged {
        id: DocumentId::new(1),
        path: None, // scratch buffer
        version: 1,
        edits: vec![applied_insert(0, "x")],
    });
    tokio::time::sleep(Duration::from_millis(120)).await;

    let notes = server.mock.notifications().await;
    assert!(
        !notes.iter().any(|n| n.method == "textDocument/didChange"),
        "scratch buffer changes have no URI; fan-in must skip them"
    );
}

#[tokio::test]
async fn many_publishes_during_debounce_coalesce_into_one_did_change() {
    let server = MockServer::start_with_capabilities(caps_with(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let bus = Arc::new(EventBus::new());
    let _sub = fan_in::spawn(server.handle.clone(), bus.clone());

    let uri = Uri::from_str("file:///tmp/x.rs").unwrap();
    let path = PathBuf::from("/tmp/x.rs");
    server.handle.open_doc(uri, "rust", "").unwrap();

    // Burst 50 edits with short gaps (each <50ms).
    for i in 0..50u32 {
        bus.publish_typed(doc_changed_event(
            path.clone(),
            vec![applied_insert(i, "x")],
        ));
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // Wait for the debounce arm to fire after the last edit.
    tokio::time::sleep(Duration::from_millis(120)).await;

    let notes = server.mock.notifications().await;
    let changes: Vec<_> = notes
        .iter()
        .filter(|n| n.method == "textDocument/didChange")
        .collect();
    assert_eq!(
        changes.len(),
        1,
        "all 50 edits should coalesce into one didChange after the debounce; got {}",
        changes.len()
    );
    let params: DidChangeTextDocumentParams =
        serde_json::from_value(changes[0].params.clone().unwrap()).unwrap();
    assert_eq!(params.content_changes.len(), 50);
}

#[tokio::test]
async fn shutdown_unsubscribes_fan_in() {
    let server = MockServer::start_with_capabilities(caps_with(
        TextDocumentSyncKind::INCREMENTAL,
        PositionEncodingKind::UTF8,
    ))
    .await;
    let bus = Arc::new(EventBus::new());
    let sub = fan_in::spawn(server.handle.clone(), bus.clone());

    // Drain a sentinel sub so we know exactly when the bus
    // re-walks its DocumentChanged bucket and prunes dead
    // entries.
    let (probe_tx, _probe_rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kinds(vec![EventKind::DocumentChanged]),
        SubscriptionTarget::Channel(probe_tx),
    );

    // Unsubscribe the fan-in explicitly (the supervisor does
    // this on shutdown). Subsequent publishes must not reach
    // the actor as RecordEdits.
    assert!(bus.unsubscribe(sub));

    let path = PathBuf::from("/tmp/x.rs");
    let uri = Uri::from_str("file:///tmp/x.rs").unwrap();
    server.handle.open_doc(uri, "rust", "").unwrap();
    bus.publish_typed(doc_changed_event(path, vec![applied_insert(0, "x")]));
    tokio::time::sleep(Duration::from_millis(120)).await;

    let notes = server.mock.notifications().await;
    assert!(
        !notes.iter().any(|n| n.method == "textDocument/didChange"),
        "after unsubscribe, fan-in must not feed didChange to the actor"
    );
}
