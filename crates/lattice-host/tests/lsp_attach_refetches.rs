//! A buffer's language server becoming ready refetches what was asked of it
//! too early — semantic tokens and inlay hints — and does so without a key
//! being pressed.
//!
//! The bug: those are requested on the first ticks after a file opens, which
//! can be before its server has finished `initialize`. That request is
//! answered with nothing and cached at the current text version, so every
//! later tick sees "already fetched", and the server — never asked — sends no
//! `refresh`. Semantic colour then arrived only after an edit or `:e!`, and
//! whether it did depended on boot timing.
//!
//! No key is pressed between the server becoming ready and the assertions.
//! The wake assertion is a contract check rather than the regression guard:
//! the modeline forwarder also wakes on this event, so it held before the fix
//! too. What the fix adds — and what fails without it — is the refetch
//! marking.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

#[tokio::test]
async fn a_server_becoming_ready_refetches_tokens_and_hints_without_a_keypress() {
    let mut editor = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let buffer = editor.document_buffer_id;

    // Drop any wake boot itself left behind, so the one asserted below can
    // only be the attach's.
    let _ = tokio::time::timeout(Duration::from_millis(50), editor.async_landed.notified()).await;
    assert!(!editor.semantic_tokens_refresh_pending.contains(&buffer));
    assert!(!editor.inlay_refresh_pending.contains(&buffer));

    // What `lsp-mode` publishes once the server is initialized and has the
    // file open.
    editor
        .event_bus
        .publish_typed(lattice_lsp::LspBufferAttached {
            id: lattice_protocol::ids::DocumentId::new(buffer.0 as u64),
        });

    // It wakes the editor on its own…
    tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified())
        .await
        .expect("the server becoming ready must wake the editor, not wait for a key");

    // …and the tick that wake runs marks both for a refetch.
    editor.drain_lsp_attach_events();
    assert!(
        editor.semantic_tokens_refresh_pending.contains(&buffer),
        "semantic tokens are re-requested, overriding the empty early answer"
    );
    assert!(
        editor.inlay_refresh_pending.contains(&buffer),
        "and so are inlay hints, which had the same trap"
    );
}
