//! A request reaches the server after every edit recorded before it.
//!
//! The actor debounces `didChange` (50 ms after the last edit) so a burst
//! of keystrokes is one notification. A request does not wait: it goes on
//! the wire at once. Without a flush between the two, a request made
//! right after typing — which is when the editor makes them — reached the
//! server before the edit did, and the server answered for the text as it
//! was before.
//!
//! For semantic tokens that answer was then filed as current, never
//! refetched, and painted onto lines it no longer matched: keyword
//! colours inside comments, with `<C-l>` repainting the same wrong
//! colours. Neovim flushes pending changes before every request for the
//! same reason; Helix never debounces.

#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use common::MockServer;
use lsp_types::{
    PositionEncodingKind, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use serde_json::{Value, json};

use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

fn incremental() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        position_encoding: Some(PositionEncodingKind::UTF8),
        ..Default::default()
    }
}

async fn opened_and_edited() -> (MockServer, lsp_types::Uri) {
    let server = MockServer::start_with_capabilities(incremental()).await;
    let uri = lattice_lsp::actor::uri_from_path(std::path::Path::new("/tmp/order.rs"));
    server
        .handle
        .open_doc(uri.clone(), "rust", "fn main() {}\n")
        .unwrap();
    server
        .handle
        .record_edit(uri.clone(), Edit::insert(Position::new(0, 0), "// x\n"))
        .unwrap();
    (server, uri)
}

fn position_of(order: &[String], method: &str) -> Option<usize> {
    order.iter().position(|m| m == method)
}

#[tokio::test]
async fn a_request_right_after_an_edit_reaches_the_server_after_it() {
    let (server, uri) = opened_and_edited().await;
    // Inside the debounce window — the editor's semantic-token request
    // goes out on the tick right after the keystroke.
    let _ = server
        .handle
        .request::<_, Value>(
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": uri.as_str() } }),
        )
        .await;

    let order = server.mock.arrival_order().await;
    let change = position_of(&order, "textDocument/didChange");
    let request = position_of(&order, "textDocument/semanticTokens/full");
    assert!(
        matches!((change, request), (Some(c), Some(r)) if c < r),
        "the edit must be on the wire before the request that follows it; got {order:?}"
    );
}

/// Notifications that describe the document (`didSave`) are ordered the
/// same way: a save notification ahead of the edits it saved would tell
/// the server the file on disk is the text it has not been sent yet.
#[tokio::test]
async fn a_notification_right_after_an_edit_reaches_the_server_after_it() {
    let (server, uri) = opened_and_edited().await;
    server
        .handle
        .notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri.as_str() } }),
        )
        .unwrap();
    // A request behind the notification, so awaiting it means both are
    // on the wire.
    let _ = server
        .handle
        .request::<_, Value>("workspace/symbol", json!({ "query": "" }))
        .await;

    let order = server.mock.arrival_order().await;
    let change = position_of(&order, "textDocument/didChange");
    let save = position_of(&order, "textDocument/didSave");
    assert!(
        matches!((change, save), (Some(c), Some(s)) if c < s),
        "didChange must precede the didSave sent after it; got {order:?}"
    );
}

/// The flush sends the edits once: the debounce timer firing later must
/// not send them a second time.
#[tokio::test]
async fn flushing_for_a_request_does_not_send_the_edits_twice() {
    let (server, uri) = opened_and_edited().await;
    let _ = server
        .handle
        .request::<_, Value>(
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": uri.as_str() } }),
        )
        .await;
    // Past the debounce window.
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let changes = server
        .mock
        .arrival_order()
        .await
        .into_iter()
        .filter(|m| m == "textDocument/didChange")
        .count();
    assert_eq!(changes, 1);
}
