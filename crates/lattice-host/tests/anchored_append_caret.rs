//! LL.12: the caret follows an anchored append (`append_to_tracked`,
//! streamed inline output) in an ordinary buffer, in the active pane and in
//! one the user is not in, without moving a caret the host placed itself.
//!
//! Driven through `dispatch_fused`, the production keystroke entry that
//! adopts an owner write's caret before the key runs, and through
//! `activate_pane`, the `<C-w>w` path.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::tracked::{AppendOutcome, Stickiness, TrackedRangeChange, TrackedRangeId};
use lattice_core::ui::pane::SplitOrientation;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

fn boot(text: &str) -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text(text))
}

/// One keystroke through the production entry.
fn key(editor: &mut Editor, action: Action) {
    let kind = editor.active_buffer;
    let _ = editor.dispatch_fused(action, kind, false, false);
}

/// An empty `NeverGrows` anchor at byte `at` of the active document.
async fn anchor(editor: &Editor, at: usize) -> TrackedRangeId {
    let id = TrackedRangeId::next();
    editor
        .document
        .update_tracked_ranges(vec![TrackedRangeChange::Track {
            id,
            bytes: at..at,
            stickiness: Stickiness::NeverGrows,
        }])
        .await
        .unwrap();
    id
}

fn text(editor: &Editor) -> String {
    editor.document.snapshot().buffer.as_string()
}

/// A stream appending above the caret pushes it down, and the next key lands
/// where the caret visibly is, not where it was before the text arrived.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_next_key_lands_where_an_append_moved_the_caret() {
    let mut editor = boot("ab\ncd\n");
    editor.set_cursor_clamped(Position::new(1, 1));
    key(&mut editor, Action::None);
    let id = anchor(&editor, 3).await;
    let out = editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    assert_eq!(out, AppendOutcome::Appended);
    key(&mut editor, Action::Insert("X".to_string()));
    assert_eq!(text(&editor), "ab\nnew\ncXd\n");
}

/// The host's own write after the append already accounts for it; a caret
/// it then places stays where it put it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_caret_the_host_placed_after_an_append_is_not_moved_again() {
    let mut editor = boot("ab\ncd\n");
    editor.set_cursor_clamped(Position::new(1, 1));
    key(&mut editor, Action::None);
    let id = anchor(&editor, 3).await;
    editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    key(&mut editor, Action::Insert("X".to_string()));
    editor.set_cursor_clamped(Position::new(0, 0));
    key(&mut editor, Action::None);
    assert_eq!(editor.cursor, Position::new(0, 0));
}

/// A pane the user is not in keeps its caret on the same text while a stream
/// writes above it, and focusing it does not hand it the other split's caret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inactive_panes_caret_is_carried_by_the_append() {
    let mut editor = boot("ab\ncd\nef\n");
    editor.set_cursor_clamped(Position::new(1, 1));
    key(&mut editor, Action::None);
    let first = editor.pane_tree.active_index();
    editor.do_split_pane(SplitOrientation::Vertical);
    let second = (0..editor.pane_tree.leaves().len())
        .find(|&i| i != first)
        .expect("a second pane");
    // Leave the first pane (its caret on `cd`); the second's caret goes to
    // the top, above where the stream will write.
    editor.activate_pane(second);
    editor.set_cursor_clamped(Position::new(0, 0));
    key(&mut editor, Action::None);

    // Away from the first pane: the stream writes above its caret.
    let id = anchor(&editor, 3).await;
    editor
        .document
        .append_to_tracked(id, "one\ntwo\n".to_string())
        .await
        .unwrap();
    assert_eq!(text(&editor), "ab\none\ntwo\ncd\nef\n");

    editor.activate_pane(first);
    assert_eq!(
        editor.cursor,
        Position::new(3, 1),
        "still on `cd`, two lines lower"
    );
    // The adoption that runs before the next key must not replace it with
    // the second pane's caret, which wrote through last.
    key(&mut editor, Action::None);
    assert_eq!(editor.cursor, Position::new(3, 1));

    editor.activate_pane(second);
    assert_eq!(
        editor.cursor,
        Position::new(0, 0),
        "a caret above the stream stays put"
    );
}

/// A kind with no text of its own refuses a stream rather than writing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_anchor_writes_nothing() {
    let editor = boot("x\n");
    let out = editor
        .document
        .append_to_tracked(TrackedRangeId::next(), "y".to_string())
        .await
        .unwrap();
    assert_eq!(out, AppendOutcome::Unknown);
    assert_eq!(text(&editor), "x\n");
}
