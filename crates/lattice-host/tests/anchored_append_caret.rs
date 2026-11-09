//! LL.12: the caret follows an anchored append (`append_to_tracked`,
//! streamed inline output) in an ordinary buffer, in the active pane and in
//! one the user is not in, without moving a caret the host placed itself.
//!
//! Driven through `dispatch_fused`, the production keystroke entry that
//! adopts an owner write's caret before the key runs, and through
//! `activate_pane`, the `<C-w>w` path.
//!
//! LL.13a: a stream writes from another thread, so an append can land
//! after the key's dispatch adopted the caret and before its edit reaches
//! the document. Bare `Editor::dispatch` runs the key without that
//! adoption, which is exactly the state inside the window, so the race is
//! reproduced deterministically rather than by timing.

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

/// Insert mode, the caret at the end of a streaming response, and a chunk
/// lands inside the key's dispatch: the character goes after the chunk, where
/// the caret visibly is, and the response stays contiguous.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_typed_while_a_chunk_lands_goes_after_the_chunk() {
    let mut editor = boot("ab\n");
    let id = anchor(&editor, 2).await;
    editor
        .document
        .append_to_tracked(id, "R1".to_string())
        .await
        .unwrap();
    key(&mut editor, Action::None); // seen: the next key adopts nothing
    editor.set_cursor_clamped(Position::new(0, 4));
    key(&mut editor, Action::None);
    assert_eq!(editor.cursor, Position::new(0, 4), "at the end of `R1`");
    // The window: adopted already, the chunk lands, then the key's edit.
    editor
        .document
        .append_to_tracked(id, "R2".to_string())
        .await
        .unwrap();
    let _ = editor.dispatch(Action::Insert("X".to_string()));
    assert_eq!(text(&editor), "abR1R2X\n");
    let span = editor
        .document
        .snapshot()
        .tracked_ranges
        .get(id)
        .map(|r| r.start..r.end)
        .unwrap();
    assert_eq!(&text(&editor)[span], "R1R2");
    assert_eq!(editor.cursor, Position::new(0, 7), "after the `X`");
}

/// The host writes its caret through at the end of every key. One computed
/// before an append it had not seen must not put the caret back: the next
/// key still lands on the text the caret was on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_caret_written_through_after_an_unseen_append_stays_on_its_text() {
    let mut editor = boot("ab\ncd\n");
    editor.set_cursor_clamped(Position::new(1, 0));
    key(&mut editor, Action::None);
    let id = anchor(&editor, 3).await;
    editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    // A motion inside the window: to (1, 1) in the coordinates the host saw,
    // written through (at the end of `dispatch`) after the append.
    editor.set_cursor_clamped(Position::new(1, 1));
    let _ = editor.dispatch(Action::None);
    key(&mut editor, Action::Insert("X".to_string()));
    assert_eq!(text(&editor), "ab\nnew\ncXd\n");
}

/// The host's own edit, then an append before it reads the version back:
/// the append is still one it has not seen, so its caret follows it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_append_just_after_the_hosts_own_edit_is_still_adopted() {
    let mut editor = boot("ab\ncd\n");
    editor.set_cursor_clamped(Position::new(1, 1));
    key(&mut editor, Action::None);
    let id = anchor(&editor, 3).await;
    key(&mut editor, Action::Insert("X".to_string()));
    editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    key(&mut editor, Action::Insert("Y".to_string()));
    assert_eq!(text(&editor), "ab\nnew\ncXYd\n");
}

/// LL.13b: a stream's text reaches the screen on the editor's wake, with no
/// key. The tick that wake runs adopts the caret the append moved, so the
/// caret never shows on text it was not on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wake_after_an_append_moves_the_caret_without_a_key() {
    let mut editor = boot("ab\ncd\n");
    editor.set_cursor_clamped(Position::new(1, 1));
    key(&mut editor, Action::None);
    let id = anchor(&editor, 3).await;
    editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    let _ = editor.run_tick_pending();
    assert_eq!(editor.cursor, Position::new(2, 1), "still on `cd`'s `d`");
}

/// A Visual selection below a stream keeps covering the same text: the
/// append moves its anchor with its head, so it does not stretch over the
/// lines that arrived.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_append_moves_a_visual_selection_whole() {
    let mut editor = boot("ab\ncd\nef\n");
    editor.set_cursor_clamped(Position::new(1, 0));
    key(&mut editor, Action::None);
    let mut partial = Vec::new();
    let _ = editor.dispatch_chord(lattice_host::chord::KeyChord::char('v'), &mut partial);
    let _ = editor.dispatch_chord(lattice_host::chord::KeyChord::char('j'), &mut partial);
    key(&mut editor, Action::None);
    assert_eq!(editor.visual_anchor, Some(Position::new(1, 0)));
    assert_eq!(editor.cursor.line, 2);

    let id = anchor(&editor, 0).await;
    editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    key(&mut editor, Action::None);
    assert_eq!(
        editor.visual_anchor,
        Some(Position::new(2, 0)),
        "anchor moved"
    );
    assert_eq!(editor.cursor.line, 3, "head moved");
}

/// Closing a pane that was away drops its caret anchor: the buffer stops
/// tracking a range nothing will ever read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_panes_anchor_is_untracked() {
    let mut editor = boot("ab\ncd\n");
    key(&mut editor, Action::None);
    let tracked = |e: &Editor| e.document.snapshot().tracked_ranges.len();
    let before = tracked(&editor);
    let first = editor.pane_tree.active_index();
    editor.do_split_pane(SplitOrientation::Vertical);
    let second = (0..editor.pane_tree.leaves().len())
        .find(|&i| i != first)
        .expect("a second pane");
    editor.activate_pane(second);
    assert!(
        !editor.pane_caret_anchors.is_empty(),
        "the first pane is anchored"
    );

    editor.do_only_pane();
    assert!(editor.pane_caret_anchors.is_empty());
    // The untrack goes to the document's actor; wait for its turn.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while tracked(&editor) != before && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(tracked(&editor), before);
}

/// `dd` in the window between the key adopting the caret and the operator
/// reaching the document, with a chunk landing above it: the line the caret
/// was on is deleted, not the chunk that took its old line number.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_run_while_a_chunk_lands_acts_on_the_carets_text() {
    use lattice_grammar::{CommandInvocation, Range};
    let mut editor = boot("ab\ncd\nef\n");
    editor.set_cursor_clamped(Position::new(1, 1));
    key(&mut editor, Action::None);
    let id = anchor(&editor, 3).await;
    editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    // What the doubled chord binds (`keymap_normal.rs`).
    let dd = CommandInvocation::of(editor.builtins.delete.0).with_range(Range::CurrentLine);
    let _ = editor.dispatch(Action::Invoke(dd));
    assert_eq!(text(&editor), "ab\nnew\nef\n");
}

/// The key after an operator: `dd` is the host's own edit, so the version it
/// leaves is one the host has seen. A chunk lands, then a motion's caret is
/// written through: it is rebased over the chunk, and the key lands where the
/// caret is.
/// The host did not record operators' versions, so the rebase failed and the
/// document's stale selection was used instead of the caret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_key_after_an_operator_still_follows_an_append() {
    use lattice_grammar::{CommandInvocation, Range};
    let mut editor = boot("ab\ncd\nef\n");
    editor.set_cursor_clamped(Position::new(1, 0));
    key(&mut editor, Action::None);
    let id = anchor(&editor, 3).await;
    let dd = CommandInvocation::of(editor.builtins.delete.0).with_range(Range::CurrentLine);
    key(&mut editor, Action::Invoke(dd));
    assert_eq!(text(&editor), "ab\nef\n");
    editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    // A motion inside the window: to the `f` in the coordinates the host saw,
    // written through after the append.
    editor.set_cursor_clamped(Position::new(1, 1));
    let _ = editor.dispatch(Action::None);
    key(&mut editor, Action::Insert("X".to_string()));
    assert_eq!(text(&editor), "ab\nnew\neXf\n");
}

/// The same after `u`: undo is the host's own edit too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_key_after_an_undo_still_follows_an_append() {
    let mut editor = boot("ab\ncd\n");
    editor.set_cursor_clamped(Position::new(1, 0));
    key(&mut editor, Action::None);
    let id = anchor(&editor, 3).await;
    key(&mut editor, Action::Insert("Z".to_string()));
    key(&mut editor, Action::Undo);
    assert_eq!(text(&editor), "ab\ncd\n");
    editor
        .document
        .append_to_tracked(id, "new\n".to_string())
        .await
        .unwrap();
    editor.set_cursor_clamped(Position::new(1, 0));
    let _ = editor.dispatch(Action::None);
    key(&mut editor, Action::Insert("X".to_string()));
    assert_eq!(text(&editor), "ab\nnew\nXcd\n");
}
