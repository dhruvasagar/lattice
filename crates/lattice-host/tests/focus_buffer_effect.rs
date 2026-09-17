//! CD.1 — `Effect::FocusBuffer(id)` shows a buffer known only by its id.
//!
//! The peer of `apply-edit`'s `target`: a plugin that fired a capture from
//! some buffer knows that buffer by id and nothing else, and on commit it must
//! land the user back there. Applied host-side, so the same arm serves the
//! keystroke path and the off-keystroke drains.

#![allow(clippy::unwrap_used)]

use lattice_core::{BufferId, Document as CoreDocument};
use lattice_grammar::Effect;
use lattice_host::editor::Editor;

/// An editor showing a second file, with the first document's id returned.
fn two_buffers() -> (Editor, BufferId, BufferId, tempfile::TempDir) {
    let mut editor = Editor::boot(CoreDocument::from_text("first\n"));
    let first = editor.active_pane_buffer_id();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("second.txt");
    std::fs::write(&path, "second\n").unwrap();
    let _ = editor.do_edit(Some(path), false);
    let second = editor.active_pane_buffer_id();
    assert_ne!(first, second, "precondition: the second file is showing");
    (editor, first, second, dir)
}

#[test]
fn focusing_a_buffer_by_id_shows_it_in_the_active_pane() {
    let (mut editor, first, _second, _dir) = two_buffers();

    let _ = editor.handle_effect(Effect::FocusBuffer(first.0));

    assert_eq!(editor.active_pane_buffer_id(), first);
    assert_eq!(
        editor.active_text().as_string(),
        "first\n",
        "the pane shows that buffer's text, not just its id"
    );
}

/// A buffer closing between an action and its effect is an ordinary race:
/// nothing moves, nothing panics, and the user is not told about it.
#[test]
fn focusing_a_closed_buffer_changes_nothing() {
    let (mut editor, first, second, _dir) = two_buffers();
    let _ = editor.handle_effect(Effect::FocusBuffer(first.0));
    // Close `first`; `second` becomes the successor.
    assert!(editor.do_buffer_delete(true));
    let showing = editor.active_pane_buffer_id();
    assert_eq!(showing, second);
    let message_before = editor.last_message.clone();

    let _ = editor.handle_effect(Effect::FocusBuffer(first.0));
    let _ = editor.handle_effect(Effect::FocusBuffer(9_999_999));

    assert_eq!(editor.active_pane_buffer_id(), showing, "no pane change");
    assert_eq!(
        editor.last_message.as_ref().map(|m| m.text.clone()),
        message_before.as_ref().map(|m| m.text.clone()),
        "no echo — `activate_buffer` would have said `buffer not found`"
    );
}

/// Focusing the buffer already showing is a no-op, not an error.
#[test]
fn focusing_the_current_buffer_is_harmless() {
    let (mut editor, _first, second, _dir) = two_buffers();
    let _ = editor.handle_effect(Effect::FocusBuffer(second.0));
    assert_eq!(editor.active_pane_buffer_id(), second);
}
