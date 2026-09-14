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

// ── MO.3: clicking a link ────────────────────────────────────────────
//
// **A press ON a link follows it; a press anywhere else positions.**
// Emacs's `mouse-1-click-follows-link`, and what every help viewer does.
//
// The rule is gated on a link actually being under the position rather
// than on the buffer's kind, which is what makes one rule correct in
// four places at once: help and dashboard seed link ranges, oil and the
// file tree seed none — their `<CR>` follow is a different gesture over
// a different table, and a click there has to stay a plain cursor move
// rather than opening whatever it landed on — and a document has none
// either. A kind test would have had to name all four and would have
// got oil wrong.
mod clicking_a_link {
    use lattice_grammar::Effect;
    use lattice_help::HelpLinkTarget;
    use lattice_host::action::Action;
    use lattice_host::modes::HelpLinks;

    use super::*;

    /// Open the dashboard and return the position of a link that runs
    /// `:tutor`, plus a position that is on no link at all.
    fn dashboard_with_a_link(editor: &mut Editor) -> lattice_protocol::position::Position {
        editor.do_open_dashboard();
        let id = editor.buffers.by_name("*dashboard*").unwrap();
        editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<HelpLinks>())
            .and_then(|hl| {
                hl.0.iter()
                    .find(|l| matches!(&l.target, HelpLinkTarget::Execute(c) if c == "tutor"))
                    .map(|l| l.range.start)
            })
            .expect("the dashboard seeds a `:tutor` link")
    }

    fn followed_the_tutor_link(outcome: &lattice_host::dispatch::DispatchOutcome) -> bool {
        outcome
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Tutor { lesson: None }))
    }

    /// The reported ask: clicking a link navigates.
    #[test]
    fn a_press_on_a_link_follows_it() {
        let mut editor = boot("scratch\n");
        let at = dashboard_with_a_link(&mut editor);
        let pane = active(&editor);

        let outcome = editor.dispatch(Action::MouseGoto {
            pane,
            line: at.line,
            byte: at.byte,
            extend: false,
        });

        assert!(
            followed_the_tutor_link(&outcome),
            "clicking the `:tutor` link should follow it"
        );
    }

    /// …and clicking ordinary text does not, **and says nothing about
    /// it**. Following unconditionally would have worked and echoed "no
    /// link under cursor" on every click on body text, which is why the
    /// gate reads the link table rather than letting the follow report a
    /// miss.
    #[test]
    fn a_press_off_a_link_just_moves_the_cursor() {
        let mut editor = boot("scratch\n");
        let at = dashboard_with_a_link(&mut editor);
        let pane = active(&editor);
        // A column well right of any link label on that row.
        let off = at.byte + 400;
        // Opening the dashboard echoed its own "switched to buffer"
        // line; clear it so what follows is about the click alone.
        editor.last_message = None;

        let outcome = editor.dispatch(Action::MouseGoto {
            pane,
            line: at.line,
            byte: off,
            extend: false,
        });

        assert!(
            !followed_the_tutor_link(&outcome),
            "a click off the link must not follow it"
        );
        assert_eq!(
            editor.last_message.as_ref().map(|m| m.text.as_str()),
            None,
            "and must not echo `no link under cursor` at the user"
        );
    }

    /// **A drag over a link selects it rather than activating it.** A
    /// drag is how you copy a link's text, so following on the way past
    /// would make that impossible — and would fire repeatedly, once per
    /// move event.
    #[test]
    fn a_drag_across_a_link_does_not_follow_it() {
        let mut editor = boot("scratch\n");
        let at = dashboard_with_a_link(&mut editor);
        let pane = active(&editor);

        let outcome = editor.dispatch(Action::MouseGoto {
            pane,
            line: at.line,
            byte: at.byte,
            extend: true,
        });

        assert!(
            !followed_the_tutor_link(&outcome),
            "extending a selection over a link is selecting, not clicking"
        );
    }

    /// A plain document has no link table, so a click is only ever a
    /// cursor move. This is the case a `BufferKind` gate would have had
    /// to remember; the property gate gets it for free.
    #[test]
    fn a_press_in_a_document_never_follows_anything() {
        let mut editor = boot(&numbered(20));
        let pane = active(&editor);

        let outcome = editor.dispatch(Action::MouseGoto {
            pane,
            line: 3,
            byte: 0,
            extend: false,
        });

        assert!(outcome.effects.is_empty(), "a document click emits nothing");
        assert_eq!(editor.cursor.line, 3);
    }
}
