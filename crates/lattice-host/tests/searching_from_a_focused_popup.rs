//! `/` inside a focused popup must not tear the popup down or wedge the editor.
//!
//! ## The report
//!
//! > if I focus in an lsp hover popup and start searching, first the screen
//! > jumps, the cursor goes into insert mode `|`, then the popup completely
//! > disappears as you type characters to search for, the buffer beneath is
//! > still inactive and now I can't even do anything […] I can switch to a tab
//! > / split and get back the cursor.
//!
//! ## The mechanism
//!
//! `Editor::dispatch`'s hover auto-dismiss (Issue #20) asks whether the popup
//! is in **State A** — shown, but never focused — and answered it with
//! `active_buffer == BufferKind::Document`. That reads as "the document has
//! focus, so the popup does not", and it holds until a minibuffer opens:
//! `*search-line*`, `*command-line*` and prompt buffers are synthetic
//! **Documents**, so focusing one from inside a focused popup flips
//! `active_buffer` back to `Document` while `popup_focused` stays true.
//!
//! The popup was then treated as State A, and every character typed into the
//! search line moved `Editor::cursor` — the *search line's* cursor — which the
//! motion check read as the document's caret drifting off the anchored symbol.
//! So it dismissed the popup mid-search, restoring `prev_pane_for_popup` over a
//! pane the minibuffer owned. `<Esc>` afterwards restored `prior_active_buffer`
//! (`Help`) with `popup_buffer` already `None`: an editor showing an inactive
//! document that answered no keys until the user switched pane, which is
//! exactly what the report says recovered it.
//!
//! ## Why these assertions and not `popup_focused`
//!
//! Each test drives the reported SEQUENCE and asserts what the user could see —
//! the popup is still there, the pane is still theirs, the editor still
//! answers. Asserting the internal flag would pass on a fix that set the flag
//! and left the teardown, and the teardown is the part that was visible.

#![allow(clippy::unwrap_used)]

use lattice_core::{BufferKind, Document as CoreDocument};
use lattice_host::editor::Editor;

/// A hover-style floating popup: markdown major, help + hover minors. The
/// hover minor is what arms the auto-dismiss, so a popup without it would make
/// every test here vacuous.
fn open_hover_popup(editor: &mut Editor) {
    let content = lattice_help::parse_help_lines(
        "hover",
        vec!["fn beta(x: u32) -> u32".to_string(), "the docs".to_string()],
    );
    let _ =
        editor.open_floating_popup(content, lattice_host::popup::PopupPlacement::CursorAnchored);
    let popup = editor.popup_buffer.expect("the float registered a buffer");
    assert!(
        editor
            .active_modes
            .get(&popup)
            .map(|m| m.minors().contains(&lattice_mode::HoverMode::mode_id()))
            .unwrap_or(false),
        "hover-mode is what arms the auto-dismiss — without it these tests \
         prove nothing"
    );
}

fn focus_it(editor: &mut Editor) {
    editor.focus_help_popup();
    assert!(editor.popup_focused, "the popup has focus");
}

/// The report, reduced: type into a search opened from a focused popup.
#[test]
fn typing_a_search_from_a_focused_popup_keeps_the_popup() {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\ngamma\n"));
    open_hover_popup(&mut editor);
    focus_it(&mut editor);
    let popup = editor.popup_buffer.expect("a popup");

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    // `Action::Insert`, one character per dispatch — which is how the search
    // line is actually typed into (`ModalState::Search` routes through the
    // universal Insert dispatcher, and an unbound printable falls through to
    // `Action::Insert`), and the only way to see this fail. The auto-dismiss
    // compares the cursor before and after ONE dispatch, so a test that seeded
    // the whole pattern with `set_search_line_text` and then dispatched would
    // find the caret unmoved and pass on the bug. `Action::SearchAppend` is
    // likewise useless here — MB.5a left it an empty arm.
    for c in ["b", "e", "t"] {
        let _ = editor.dispatch(lattice_host::action::Action::Insert(c.to_string()));
        assert_eq!(
            editor.popup_buffer,
            Some(popup),
            "the popup must survive typing `{c}` into the search line — it is \
             anchored to a symbol the document's caret never left"
        );
    }
    assert_eq!(
        editor.search_pattern(),
        "bet",
        "…and the characters reached the search line, or the loop above \
         asserted nothing"
    );
}

/// …and leaving the search puts the user back in the popup, not in a pane that
/// answers nothing.
#[test]
fn cancelling_the_search_returns_to_the_focused_popup() {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\ngamma\n"));
    open_hover_popup(&mut editor);
    focus_it(&mut editor);
    let popup = editor.popup_buffer.expect("a popup");

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    for c in ["b", "e", "t"] {
        let _ = editor.dispatch(lattice_host::action::Action::Insert(c.to_string()));
    }
    editor.do_search_line_cancel();

    assert_eq!(
        editor.popup_buffer,
        Some(popup),
        "the popup is still open after `<Esc>`"
    );
    assert!(editor.popup_focused, "and still focused");
    assert_eq!(
        editor.active_buffer_id(),
        popup,
        "so keys resolve against the popup — `active_buffer` saying `Help` \
         with no popup behind it is the wedge the report describes"
    );
    assert_eq!(
        editor.active_buffer,
        BufferKind::Help,
        "the two answers to `which buffer is this` must agree"
    );
}

/// The behaviour the auto-dismiss exists for is unchanged: with the popup
/// UNFOCUSED, moving the document's caret still takes it down.
///
/// Without this the fix could be "never auto-dismiss", which trades a wedge
/// for a stale popup that follows you down the file.
#[test]
fn an_unfocused_popup_still_dismisses_when_the_document_caret_moves() {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\ngamma\n"));
    open_hover_popup(&mut editor);
    assert!(
        !editor.popup_focused,
        "a floating popup opens passive — State A"
    );

    let id = editor
        .registry
        .load()
        .id_by_name("motion:line-down")
        .expect("the down motion is a builtin");
    let _ = editor.dispatch(lattice_host::action::Action::Invoke(
        lattice_grammar::CommandInvocation::of(id),
    ));
    assert_eq!(
        editor.popup_buffer, None,
        "an unfocused hover popup is anchored to the symbol under the caret; \
         moving off it must still dismiss"
    );
}
