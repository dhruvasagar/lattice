//! Regression: in the TUI the ACTIVE pane paints from the shared
//! `self.document` slot (`app.ad().snapshot`), so `self.document` MUST follow
//! the focused pane's buffer for EVERY kind, and the OUTGOING buffer's syntax
//! must be stashed before `active_buffer` flips to the destination kind. Two
//! bugs lived in the pane-cycle path (`<C-w>w` → `activate_pane` →
//! `load_active_pane` → `sync_active_document_to_pane`), both TUI-only because
//! GPUI sources every pane from its own `pane.buffer_id` handle:
//!
//!  1. `sync_active_document_to_pane` kind-gated `self.document` to
//!     `Document | Messages | Multibuffer`, so cycling focus onto an Oil /
//!     FileTree pane left `self.document` on the previously-focused buffer —
//!     the oil/filetree pane then painted THAT other buffer's content when
//!     focused (correct while unfocused, since the inactive path reads the
//!     pane's own handle). The kind gate is gone; the document-handle lookup is
//!     the only gate now.
//!
//!  2. `load_active_pane` set `active_buffer` to the destination kind BEFORE
//!     `snapshot_active_document` ran, whose guard is `active_buffer ==
//!     Document`. Switching a syntax-highlighted Document pane to a
//!     non-Document pane (the org agenda — a Multibuffer — or oil) therefore
//!     dropped the document's syntax handle unsaved: the file lost its colour
//!     and no redraw brought it back. The stash now runs while `active_buffer`
//!     still names the buffer being left.
//!
//! Both are driven through the real pane-cycle, never `activate_document`.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::SplitOrientation;
use lattice_host::editor::Editor;

fn boot() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("scratch-0\nscratch-1\n"))
}

/// Move focus to the next pane exactly as `<C-w>w` does.
fn cycle_pane(editor: &mut Editor) {
    let target = editor.pane_tree.next_pane();
    editor.activate_pane(target);
}

#[test]
fn cycling_focus_back_to_an_oil_pane_shows_oil_not_the_other_buffer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.txt"), "a\n").unwrap();

    let mut editor = boot();
    // Two panes so focus can cycle between them; oil replaces the active pane.
    editor.do_split_pane(SplitOrientation::Vertical);
    editor.do_open_oil(Some(dir.path().to_path_buf()));
    let oil_id = editor.active_pane_buffer_id();
    let opened = editor.document.snapshot().buffer.as_string();
    assert!(
        opened.contains("alpha.txt"),
        "sanity: oil lists the directory it opened; got {opened:?}"
    );

    // Away to the other (scratch) pane, then back to the oil pane — the exact
    // flow that used to leave the oil pane painting the scratch buffer.
    cycle_pane(&mut editor);
    assert_ne!(
        editor.active_pane_buffer_id(),
        oil_id,
        "sanity: focus actually moved off the oil pane"
    );
    cycle_pane(&mut editor);

    assert_eq!(
        editor.active_pane_buffer_id(),
        oil_id,
        "sanity: the oil pane is focused again"
    );
    assert_eq!(
        editor.document_buffer_id, oil_id,
        "self.document followed the focused oil pane (bug: it used to stay on the other buffer)"
    );
    let body = editor.document.snapshot().buffer.as_string();
    assert!(
        body.contains("alpha.txt"),
        "the focused oil pane renders oil content, not the other buffer's; got {body:?}"
    );
}

#[test]
fn cycling_focus_back_to_a_file_tree_pane_shows_the_tree_not_the_other_buffer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("beta.txt"), "b\n").unwrap();

    let mut editor = boot();
    editor.do_split_pane(SplitOrientation::Vertical);
    editor.do_open_file_tree(Some(dir.path().to_path_buf()));
    let tree_id = editor.active_pane_buffer_id();
    let opened = editor.document.snapshot().buffer.as_string();
    assert!(
        opened.contains("beta.txt"),
        "sanity: the file tree lists its root; got {opened:?}"
    );

    cycle_pane(&mut editor);
    assert_ne!(editor.active_pane_buffer_id(), tree_id);
    cycle_pane(&mut editor);

    assert_eq!(
        editor.document_buffer_id, tree_id,
        "self.document followed the focused file-tree pane"
    );
    let body = editor.document.snapshot().buffer.as_string();
    assert!(
        body.contains("beta.txt"),
        "the focused file-tree pane renders tree content, not the other buffer's; got {body:?}"
    );
}

#[test]
fn cycling_a_syntax_highlighted_document_pane_to_a_non_document_pane_keeps_its_syntax() {
    // Bug #1's mechanism: leaving a Document-with-syntax for a non-Document pane
    // (oil here; the org agenda / a Multibuffer hits the identical code) must
    // stash the document's syntax handle rather than drop it.
    let dir = tempfile::tempdir().unwrap();
    let rs = dir.path().join("main.rs");
    std::fs::write(&rs, "fn main() {\n    let x = 1;\n}\n").unwrap();

    let mut editor = boot();
    // Order matters (see the multibuffer sibling of this test): open oil FIRST,
    // then the .rs fresh into a split, so the .rs's syntax is live in
    // `self.syntax` with `buffer_locals[rs]` still None when the pane-cycle
    // fires. Opening the .rs first and reaching oil via `activate_document`
    // would pre-stash the syntax (the safe path) and mask the bug.
    editor.do_open_oil(Some(dir.path().to_path_buf()));
    let oil_id = editor.active_pane_buffer_id();
    editor.do_split_pane(SplitOrientation::Vertical);
    editor.do_edit(Some(rs.clone()), false);
    let rs_id = editor.active_pane_buffer_id();
    assert_ne!(
        oil_id, rs_id,
        "sanity: oil is a distinct buffer from the .rs"
    );
    if editor.document_syntax_for(rs_id).is_none() {
        // No rust grammar in this build → nothing to retain; the assertion
        // below would be vacuous, so skip loudly rather than pass emptily.
        eprintln!("skipping: no live syntax handle for main.rs (rust grammar unavailable)");
        return;
    }

    // The .rs is active with its syntax live and unstashed. Cycle to the oil
    // (non-Document) pane — the FIRST switch away from the .rs, the switch that
    // used to drop its syntax.
    cycle_pane(&mut editor);
    assert_eq!(
        editor.active_pane_buffer_id(),
        oil_id,
        "sanity: the oil pane is focused"
    );

    assert!(
        editor.document_syntax_for(rs_id).is_some(),
        "the .rs file's syntax handle survived the switch to the non-Document pane \
         (bug: it was dropped unsaved because active_buffer flipped before the stash)"
    );
}
