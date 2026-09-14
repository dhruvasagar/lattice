//! MO.2 — what the wheel and the pointer do once a renderer has
//! resolved them to a buffer position.
//!
//! Design:
//! [`docs/dev/architecture/mouse.md`](../../../docs/dev/architecture/mouse.md).
//!
//! ## What is actually at risk
//!
//! Not "does the cursor move" — that is one assignment. The risk is the
//! two rules that separate the gestures from each other, both of which
//! are invisible in a single-pane test:
//!
//! 1. **A wheel does not steal focus, a click does.** Scrolling a
//!    reference split while typing in another is the case that rule
//!    exists for, and a handler that focused on scroll would pass every
//!    single-pane assertion.
//! 2. **A drag is Visual mode, not a private selection.** If it were
//!    anything else, `d` and `y` would not operate on it, and nothing
//!    about the cursor position would say so.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::PaneId;
use lattice_grammar::{ModalState, VisualKind};
use lattice_host::editor::Editor;

fn boot(text: &str) -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text(text));
    editor.viewport_height = 10;
    for leaf in editor.pane_tree.leaves_mut() {
        leaf.viewport_height = 10;
    }
    editor
}

fn numbered(lines: u32) -> String {
    (0..lines).map(|i| format!("line{i}\n")).collect()
}

fn active(editor: &Editor) -> PaneId {
    editor.pane_tree.active().id
}

fn scroll_of(editor: &Editor, pane: PaneId) -> u32 {
    editor
        .pane_tree
        .leaves()
        .iter()
        .find(|p| p.id == pane)
        .map(|p| p.scroll)
        .expect("pane exists")
}

// ── the wheel ────────────────────────────────────────────────────────

/// One notch is vim's `mousescroll` default of three lines, and the
/// direction is the obvious one.
#[test]
fn a_wheel_notch_scrolls_the_pane_three_lines() {
    let mut editor = boot(&numbered(200));
    let pane = active(&editor);

    editor.do_mouse_scroll(pane, true);
    assert_eq!(editor.scroll, 3, "one notch down is three lines");

    editor.do_mouse_scroll(pane, false);
    assert_eq!(editor.scroll, 0, "and back up again");
}

/// Scrolling the top of the buffer upwards stops rather than wrapping
/// or underflowing.
#[test]
fn scrolling_up_at_the_top_stays_at_the_top() {
    let mut editor = boot(&numbered(200));
    let pane = active(&editor);

    editor.do_mouse_scroll(pane, false);

    assert_eq!(editor.scroll, 0);
}

/// **A wheel over another pane scrolls THAT pane and leaves focus
/// alone.** The whole point of hit-testing the pointer rather than
/// assuming the active pane — and the assertion a single-pane test
/// cannot make.
#[test]
fn a_wheel_over_an_inactive_pane_scrolls_it_without_taking_focus() {
    let mut editor = boot(&numbered(200));
    editor.do_split_pane(lattice_core::ui::pane::SplitOrientation::Horizontal);
    assert_eq!(
        editor.pane_tree.leaves().len(),
        2,
        "precondition: two panes"
    );

    let focused = active(&editor);
    let other = editor
        .pane_tree
        .leaves()
        .iter()
        .map(|p| p.id)
        .find(|id| *id != focused)
        .expect("a second pane");

    editor.do_mouse_scroll(other, true);

    assert_eq!(active(&editor), focused, "the wheel must not move focus");
    assert_eq!(
        scroll_of(&editor, other),
        3,
        "the pane under the pointer scrolled"
    );
    assert_eq!(editor.scroll, 0, "the focused pane did not");
}

/// A wheel naming a pane that no longer exists is ignored rather than
/// panicking. Events outlive layouts: a split can close between the
/// press and the drain.
#[test]
fn a_wheel_over_a_vanished_pane_is_ignored() {
    let mut editor = boot(&numbered(200));

    editor.do_mouse_scroll(PaneId(9999), true);

    assert_eq!(editor.scroll, 0);
}

// ── press and drag ───────────────────────────────────────────────────

/// A press moves the cursor to where it landed.
#[test]
fn a_press_moves_the_cursor_to_the_clicked_position() {
    let mut editor = boot(&numbered(50));

    editor.do_mouse_goto(active(&editor), 7, 3, false);

    assert_eq!(editor.cursor.line, 7);
    assert_eq!(editor.cursor.byte, 3);
}

/// A press past the end of the buffer lands on its last line rather
/// than propagating a position no buffer has. Terminals report a cell
/// for every row, including the blank ones below the text, so this is
/// the ordinary case and not a defensive one.
#[test]
fn a_press_below_the_last_line_clamps_into_the_buffer() {
    let mut editor = boot(&numbered(5));

    editor.do_mouse_goto(active(&editor), 400, 0, false);

    assert_eq!(editor.cursor.line, 4, "the last line");
}

/// …and past the end of a line clamps to that line's end.
#[test]
fn a_press_right_of_a_line_clamps_to_its_end() {
    let mut editor = boot("ab\nlonger line\n");

    editor.do_mouse_goto(active(&editor), 0, 99, false);

    assert_eq!(editor.cursor.line, 0);
    assert_eq!(editor.cursor.byte, 2, "`ab` is two bytes long");
}

/// **A click into another pane focuses it** — the half of the gesture
/// the wheel deliberately does not do.
#[test]
fn a_press_in_another_pane_focuses_that_pane() {
    let mut editor = boot(&numbered(50));
    editor.do_split_pane(lattice_core::ui::pane::SplitOrientation::Horizontal);

    let focused = active(&editor);
    let other = editor
        .pane_tree
        .leaves()
        .iter()
        .map(|p| p.id)
        .find(|id| *id != focused)
        .expect("a second pane");

    editor.do_mouse_goto(other, 4, 0, false);

    assert_eq!(active(&editor), other, "clicking into a split moves to it");
    assert_eq!(editor.cursor.line, 4);
}

/// **A drag is Visual mode.** Not a parallel selection concept — the
/// region is live in `ModalState::Visual(Charwise)` with the press's
/// position as the anchor, which is what makes `d` / `y` / any operator
/// work on it with no new machinery.
#[test]
fn a_drag_selects_in_visual_mode_anchored_at_the_press() {
    let mut editor = boot(&numbered(50));
    let pane = active(&editor);

    editor.do_mouse_goto(pane, 3, 1, false);
    editor.do_mouse_goto(pane, 6, 4, true);

    assert!(
        matches!(editor.modal, ModalState::Visual(VisualKind::Charwise)),
        "a drag leaves the editor in charwise Visual, got {:?}",
        editor.modal
    );
    assert_eq!(
        editor.visual_anchor.map(|p| (p.line, p.byte)),
        Some((3, 1)),
        "the anchor is where the press landed, not where the drag is"
    );
    assert_eq!((editor.cursor.line, editor.cursor.byte), (6, 4));
}

/// Continuing a drag moves the head and keeps the anchor put. A handler
/// that re-anchored on every drag event would collapse the selection to
/// nothing and still look like it was tracking the mouse.
#[test]
fn continuing_a_drag_keeps_the_original_anchor() {
    let mut editor = boot(&numbered(50));
    let pane = active(&editor);

    editor.do_mouse_goto(pane, 3, 1, false);
    editor.do_mouse_goto(pane, 6, 4, true);
    editor.do_mouse_goto(pane, 9, 2, true);

    assert_eq!(editor.visual_anchor.map(|p| (p.line, p.byte)), Some((3, 1)));
    assert_eq!((editor.cursor.line, editor.cursor.byte), (9, 2));
}

/// Dragging backwards is an ordinary Visual selection with the head
/// before the anchor — no special casing, because Visual already
/// handles it.
#[test]
fn dragging_upwards_puts_the_head_before_the_anchor() {
    let mut editor = boot(&numbered(50));
    let pane = active(&editor);

    editor.do_mouse_goto(pane, 8, 0, false);
    editor.do_mouse_goto(pane, 2, 0, true);

    assert_eq!(editor.visual_anchor.map(|p| p.line), Some(8));
    assert_eq!(editor.cursor.line, 2);
    assert!(matches!(
        editor.modal,
        ModalState::Visual(VisualKind::Charwise)
    ));
}

/// A plain press ends a selection, which is what a click means in every
/// other editor. Without this a drag would leave Visual active and the
/// next click would silently extend it.
#[test]
fn a_press_leaves_visual_mode() {
    let mut editor = boot(&numbered(50));
    let pane = active(&editor);

    editor.do_mouse_goto(pane, 3, 0, false);
    editor.do_mouse_goto(pane, 6, 0, true);
    assert!(matches!(editor.modal, ModalState::Visual(_)));

    editor.do_mouse_goto(pane, 1, 0, false);

    assert!(
        matches!(editor.modal, ModalState::Normal),
        "a fresh press drops the old selection, got {:?}",
        editor.modal
    );
    assert_eq!(editor.visual_anchor, None);
    assert_eq!(editor.cursor.line, 1);
}

/// A press naming a pane that no longer exists is ignored, and — the
/// part worth asserting — it does NOT move the cursor of whatever pane
/// happens to be focused instead.
#[test]
fn a_press_in_a_vanished_pane_is_ignored() {
    let mut editor = boot(&numbered(50));
    editor.do_mouse_goto(active(&editor), 5, 0, false);

    editor.do_mouse_goto(PaneId(9999), 20, 0, false);

    assert_eq!(editor.cursor.line, 5, "the live pane's cursor is untouched");
}
