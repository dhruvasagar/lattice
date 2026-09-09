//! Pane zoom (ZP series) — host integration tests.
//!
//! Design: `docs/dev/architecture/pane-zoom.md`.
//! Slice plan: `docs/dev/operations/slice-plans/pane-zoom.md`.
//!
//! The tree semantics are unit-tested in `lattice_core::ui::pane`.
//! These cover the part that needs a real `Editor`: that the chords
//! and the ex-command actually reach `do_toggle_zoom_pane`.
//!
//! Everything here drives `dispatch_chord`, never `do_toggle_zoom_pane`
//! directly. A pane chord lives behind the `<C-w>` pending state, and a
//! test that calls the handler passes whether or not the binding is
//! wired — which is exactly the hole that has swallowed chords before
//! (see `modal-states-need-a-dispatch-arm`).

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::SplitOrientation;
use lattice_host::editor::Editor;
use lattice_protocol::chord::KeyChord;

/// Boot an editor with two panes, so zoom is not a no-op.
fn boot_split() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("line-0\nline-1\nline-2\n"));
    editor.pane_tree.split_active(SplitOrientation::Vertical);
    editor
}

fn press(editor: &mut Editor, chords: &[KeyChord]) {
    let mut partial = Vec::new();
    for c in chords {
        let _ = editor.dispatch_chord(c.clone(), &mut partial);
    }
}

fn ctrl_w() -> KeyChord {
    KeyChord::ctrl('w')
}

/// Type `:<cmd><CR>` through the real command line, so the test
/// exercises name resolution and the `AppEffect` carrier the same way
/// a user does.
fn ex(editor: &mut Editor, cmd: &str) {
    press(editor, &[KeyChord::char(':')]);
    for c in cmd.chars() {
        press(editor, &[KeyChord::char(c)]);
    }
    press(
        editor,
        &[KeyChord::special(
            lattice_protocol::chord::SpecialKey::Enter,
        )],
    );
}

#[test]
fn ctrl_w_z_toggles_zoom() {
    let mut e = boot_split();
    assert!(!e.pane_tree.is_zoomed(), "starts unzoomed");

    press(&mut e, &[ctrl_w(), KeyChord::char('z')]);
    assert!(e.pane_tree.is_zoomed(), "<C-w>z zoomed the active pane");

    press(&mut e, &[ctrl_w(), KeyChord::char('z')]);
    assert!(!e.pane_tree.is_zoomed(), "<C-w>z again restored the layout");
}

/// The ctrl-modified twin. Every other pane chord accepts a held
/// Ctrl on the second key, and a chord that works only when you
/// release it mid-sequence is the kind of gap nobody reports.
#[test]
fn ctrl_w_ctrl_z_toggles_zoom() {
    let mut e = boot_split();
    press(&mut e, &[ctrl_w(), KeyChord::ctrl('z')]);
    assert!(e.pane_tree.is_zoomed());
}

#[test]
fn zoom_pane_ex_command_toggles_zoom() {
    let mut e = boot_split();
    ex(&mut e, "zoom-pane");
    assert!(e.pane_tree.is_zoomed(), ":zoom-pane zoomed");
    ex(&mut e, "zoom-pane");
    assert!(!e.pane_tree.is_zoomed(), ":zoom-pane unzoomed");
}

/// A chord that silently does nothing reads as a broken binding, so
/// the single-pane case echoes instead.
#[test]
fn zooming_a_single_pane_tab_says_so() {
    let mut e = Editor::boot(CoreDocument::from_text("only\n"));
    press(&mut e, &[ctrl_w(), KeyChord::char('z')]);
    assert!(!e.pane_tree.is_zoomed());
    let echo = e.last_message.as_ref().map(|m| m.text.clone());
    assert!(
        echo.as_deref().is_some_and(|m| m.contains("one pane")),
        "expected an echo explaining the no-op, got {echo:?}"
    );
}

/// The user-visible half of the zoomed-is-active invariant: the
/// navigation keys are the escape hatch. `<C-w>l` while zoomed must
/// land in the pane to the right AND drop the zoom — not silently do
/// nothing, which is what a zoom-aware `navigate` would produce.
#[test]
fn navigating_out_of_a_zoomed_pane_moves_focus_and_unzooms() {
    let mut e = boot_split();
    let left = e.pane_tree.active().id;
    press(&mut e, &[ctrl_w(), KeyChord::char('z')]);
    assert!(e.pane_tree.is_zoomed());

    press(&mut e, &[ctrl_w(), KeyChord::char('l')]);
    assert!(!e.pane_tree.is_zoomed(), "navigation dropped the zoom");
    assert_ne!(
        e.pane_tree.active().id,
        left,
        "and focus actually moved to the right-hand pane"
    );
}
