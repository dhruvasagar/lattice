//! AW.1 — every channel-delivered LSP result must fire `async_landed`.
//!
//! Root cause (see `docs/dev/operations/slice-plans/lsp-async-wake.md`): the
//! passive-decoration LSP requests (foldingRange / semanticTokens / inlayHint /
//! documentHighlight) fire `Editor::async_landed` after landing their result,
//! so the actor's `async_landed.notified()` arm drains + publishes + repaints
//! **off-keystroke**. The user-facing *action* requests (references, nav,
//! hover, …) deliver via an `mpsc` channel drained by `run_tick_pending` but
//! fire nothing — so their picker / jump / popup waits for the next keystroke.
//! That is the "`gr` often needs an extra key before the picker opens" report.
//!
//! `async_landed` is the SINGLE renderer-agnostic wake (§12): the actor arm
//! turns one `notify_one()` into the full drain + publish + paint for BOTH the
//! TUI and GPUI peers. These pins prove the request path fires it.
//!
//! Setup: a real booted editor (LSP modes registered) with lsp-mode active on a
//! URI-mapped buffer and NO servers attached. Each request then reaches its
//! spawned task, hits the `NoServers` arm, and (AW.1) must fire `async_landed`.
//! The task runs on the shared LSP runtime; `Notify` is cross-runtime, so the
//! `#[tokio::test]` runtime's `notified().await` observes it (permit-style, so
//! a notify that lands before the await is not lost — no race).

#![allow(clippy::unwrap_used)]

use std::str::FromStr;
use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_lsp::modes::{LspHoverMode, LspMode, LspNavMode};

/// Boot a real editor with a non-empty buffer, activate lsp-mode (cascades to
/// its sub-modes: nav / hover / …), and map a URI so the LSP gate passes and
/// the cursor position resolves. No servers are attached, so every request
/// reaches its spawned task and hits the `NoServers` arm.
fn boot_lsp_editor() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let doc_id = editor.document_buffer_id;
    let uri = lattice_lsp::Uri::from_str("file:///tmp/aw.rs").unwrap();
    editor.buffer_uris.insert(doc_id, uri);
    let _ = editor.activate_mode_by_id(doc_id, LspMode::mode_id());
    assert!(
        editor.lsp_mode_enabled_for(doc_id),
        "lsp-mode must be active after activate_mode_by_id"
    );
    assert!(
        editor.minor_mode_enabled_for(doc_id, LspNavMode::mode_id()),
        "activating lsp-mode must cascade lsp-nav-mode (gate for gr/gd)"
    );
    assert!(
        editor.minor_mode_enabled_for(doc_id, LspHoverMode::mode_id()),
        "activating lsp-mode must cascade lsp-hover-mode (gate for K)"
    );
    editor
}

/// Await `async_landed` with a bounded timeout. Returns true iff it fired.
async fn landed_within(editor: &Editor, secs: u64) -> bool {
    tokio::time::timeout(Duration::from_secs(secs), editor.async_landed.notified())
        .await
        .is_ok()
}

/// Drain every `async_landed` permit accumulated during mode activation
/// (activating lsp-mode publishes lifecycle events that boot-wired forwarders
/// turn into wakes) and wait for quiescence, so a later `landed_within`
/// measures ONLY the request under test. Loops until a quiet window elapses
/// with no further notify.
async fn settle(editor: &Editor) {
    while tokio::time::timeout(Duration::from_millis(100), editor.async_landed.notified())
        .await
        .is_ok()
    {}
}

#[tokio::test]
async fn references_request_fires_async_landed_off_keystroke() {
    let mut editor = boot_lsp_editor();
    settle(&editor).await;
    editor.lsp_references_request();
    assert!(
        landed_within(&editor, 2).await,
        "gr (lsp_references_request) must fire async_landed after the result \
         lands so the actor opens the picker WITHOUT a keystroke (AW.1)"
    );
}

#[tokio::test]
async fn nav_request_fires_async_landed_off_keystroke() {
    let mut editor = boot_lsp_editor();
    settle(&editor).await;
    editor.lsp_nav_request(lattice_lsp::cache::LspNavKind::Definition);
    assert!(
        landed_within(&editor, 2).await,
        "gd (lsp_nav_request) must fire async_landed off-keystroke (AW.1)"
    );
}

#[tokio::test]
async fn hover_request_fires_async_landed_off_keystroke() {
    let mut editor = boot_lsp_editor();
    settle(&editor).await;
    editor.lsp_hover_request();
    assert!(
        landed_within(&editor, 2).await,
        "K (lsp_hover_request) must fire async_landed off-keystroke — the \
         renderer-agnostic wake, not the TUI-broken paint_request clone (AW.1)"
    );
}

// ── AW.2 — cancel position-anchored lookups on cursor move ──────────────────

#[tokio::test]
async fn cursor_move_cancels_in_flight_references() {
    let mut editor = boot_lsp_editor();
    settle(&editor).await;
    editor.lsp_references_request();
    assert!(
        editor.pending_references_rx.is_some() && editor.pending_references_token.is_some(),
        "gr must leave an in-flight references request pending"
    );
    // Move the cursor through the real dispatch chokepoint (EnterAppend = vim
    // `a`: one byte right + Insert). The result is now anchored to a symbol the
    // cursor has left, so AW.2 must cancel + clear it.
    let _ = editor.dispatch(lattice_host::action::Action::EnterAppend);
    assert!(
        editor.pending_references_rx.is_none(),
        "moving the cursor must clear the stale in-flight references lookup (AW.2)"
    );
    assert!(
        editor.pending_references_token.is_none(),
        "moving the cursor must cancel + drop the references token (AW.2)"
    );
}

#[tokio::test]
async fn non_moving_dispatch_keeps_in_flight_references() {
    let mut editor = boot_lsp_editor();
    settle(&editor).await;
    editor.lsp_references_request();
    let cursor_before = editor.cursor;
    // A dispatch that does NOT move the cursor must NOT cancel the lookup —
    // the cancel is gated strictly on cursor motion.
    let _ = editor.dispatch(lattice_host::action::Action::None);
    assert_eq!(
        editor.cursor, cursor_before,
        "Action::None must not move the cursor"
    );
    assert!(
        editor.pending_references_rx.is_some(),
        "a non-moving dispatch must leave the in-flight references lookup intact (AW.2)"
    );
}

#[tokio::test]
async fn control_no_request_does_not_fire_async_landed() {
    let editor = boot_lsp_editor();
    settle(&editor).await;
    assert!(
        !landed_within(&editor, 1).await,
        "CONTROL: after settle, no request means no async_landed (proves isolation)"
    );
}
