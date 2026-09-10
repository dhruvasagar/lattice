//! A passive popup's dismiss must not move the pane off a synthetic buffer
//! that was opened while the popup was up.
//!
//! **Reported against org-capture.** `<leader>` → transient → template →
//! `%^{…}` prompts → the `*org-capture*` draft opens → `C-c C-c` files the note
//! and closes the draft. What actually happened: the note was filed, and the
//! buffer that got deleted was **whatever the user was on before the capture
//! started** — the draft stayed alive, unsaved, and `:q` then refused to quit
//! citing a buffer the user could not see. Two runs, two different pre-capture
//! buffers (an oil listing, then the boot buffer), the same shape both times.
//!
//! The log pinned the mechanism to a *silent* pane move: `activate_buffer`
//! echoes `switched to buffer #N` on every real activation, and between the
//! draft opening and the finalize there is no such line — yet
//! `do_buffer_delete` resolved its target (`active_pane_buffer_id()`) to the
//! pre-capture buffer. Exactly one code path moves `pane.buffer_id` without
//! going through `activate_buffer`: `dismiss_popup`'s stash-restore branch.
//!
//! And it can reach that branch holding a stash it did not write.
//! `open_synthetic_buffer_seeded` writes `prev_pane_for_popup` for magit's `q`
//! (bury-back), gated only on the slot being empty — it does not ask whether a
//! popup is currently up. So an unrelated passive popup that is dismissed
//! afterwards consumes that stash and hand-restores the pane to the buffer the
//! synthetic one was opened *from*, leaving `document_buffer_id` and
//! `pane.buffer_id` disagreeing.
//!
//! MG.47 patched one call site of this (`:` in a magit buffer) by guarding the
//! dismiss, on the reasoning that "State A leaves `prev_pane_for_popup` as
//! `None` (PU-A.1a) — so this guard cannot move the pane". These pin that the
//! claim is false whenever a synthetic buffer opened *under* the popup.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::ui::popup::{PopupFocus, PopupPlacement};
use lattice_host::editor::Editor;

/// Open a passive (State A) popup, then a synthetic buffer under it, then
/// dismiss the popup. The pane must still show the synthetic buffer.
#[tokio::test]
async fn dismissing_a_passive_popup_leaves_the_panes_buffer_alone() {
    let mut editor = Editor::boot(CoreDocument::from_text("the file the user was on\n"));
    let origin = editor.document_buffer_id;

    // State A: shown, never focused — a hover, a signature help, a transient
    // menu. The user's flow had one up when the capture draft opened.
    let _ = editor.open_popup_named(
        "*a-passive-popup*",
        "help-mode",
        PopupPlacement::default(),
        PopupFocus::Passive,
    );
    assert!(editor.popup_buffer.is_some(), "precondition: a popup is up");
    assert!(
        !editor.popup_focused,
        "precondition: it is State A (never focused)"
    );

    editor.open_synthetic_buffer("*plugins*", "plugins-mode");
    let synthetic = editor.buffers.by_name("*plugins*").unwrap();
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        synthetic,
        "precondition: the open committed the synthetic buffer to the pane",
    );

    editor.dismiss_popup();

    assert_eq!(
        editor.pane_tree.active().buffer_id,
        synthetic,
        "dismissing an unrelated popup must not move the pane — the popup \
         never owned this pane, and the stash it consumed was written by the \
         synthetic open for magit's `q`, not by the popup",
    );
    assert_ne!(
        editor.pane_tree.active().buffer_id,
        origin,
        "specifically: it must not fall back to the buffer the synthetic one \
         was opened from",
    );
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        editor.document_buffer_id,
        "pane and editing focus must agree — disagreement is what makes every \
         `active_pane_buffer_id()` consumer act on the wrong buffer",
    );
}

/// The consequence, in the shape it was reported: `:bd` after that dismiss
/// deletes the buffer the user came from and orphans the one on screen.
#[tokio::test]
async fn a_buffer_close_after_that_dismiss_kills_the_right_buffer() {
    let mut editor = Editor::boot(CoreDocument::from_text("the file the user was on\n"));
    let origin = editor.document_buffer_id;

    let _ = editor.open_popup_named(
        "*a-passive-popup*",
        "help-mode",
        PopupPlacement::default(),
        PopupFocus::Passive,
    );
    editor.open_synthetic_buffer("*plugins*", "plugins-mode");
    let synthetic = editor.buffers.by_name("*plugins*").unwrap();
    editor.dismiss_popup();

    // `C-c C-c`'s tail: the mode closes its own buffer. It names no id — the
    // vocabulary has none — so it means "the one on screen".
    editor.do_buffer_delete(true);

    assert!(
        editor.buffers.by_name("*plugins*").is_none(),
        "the buffer that was on screen is the one that closes; leaving it \
         alive is the orphaned capture draft — unsaved, unreachable, and the \
         reason `:q` then refuses to quit",
    );
    assert!(
        editor.buffers.contains(origin),
        "the buffer the user was on before must survive — deleting it is the \
         reported bug (buffer #{}, an oil listing in one run and the boot \
         buffer in the next)",
        origin.0,
    );
    let _ = synthetic;
}
