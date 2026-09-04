//! FS.2 / FS.3 — with a popup focused, interactions act on the POPUP.
//!
//! Design: `docs/dev/architecture/focused-surface.md`. Slice plan:
//! `docs/dev/operations/slice-plans/focused-surface.md`.
//!
//! ## The report
//!
//! > for lsp hover popup (`K`) […] as I type the search characters, the
//! > matching runs (hlsearch) in the background buffer […] when I hit `<CR>`,
//! > only then the cursor moves within the popup and jumps to the match.
//!
//! `focus_help_popup` set `cursor`, `scroll` and `active_buffer` and left
//! `self.document` pointing at the file, so every buffer-scoped verb resolved
//! against the file while the caret was painted in the popup. Two identities,
//! one cursor.
//!
//! Its own doc comment had promised the opposite since 5.5.LSP.1: "the popup
//! behaves like any other buffer (vim grammar, `/` search, `:` ex commands
//! operate on the popup's content)". These tests are that sentence.
//!
//! ## Why the fixtures differ in length
//!
//! The popup's content is deliberately SHORTER than the file and shares no
//! words with it. A fixture where both contain the search term, or where both
//! are the same length, passes whichever buffer the verb resolved against —
//! which is exactly how this survived.

#![allow(clippy::unwrap_used)]

use lattice_core::{BufferKind, Document as CoreDocument};
use lattice_host::editor::Editor;

const FILE: &str = "\
the file's first line
the file's second line
the file's third line
the file's fourth line
the file's fifth line
";

/// Focus a hover popup over a file, with the caret on the file's line 3.
fn editor_with_focused_popup() -> (Editor, lattice_core::BufferId) {
    let mut editor = Editor::boot(CoreDocument::from_text(FILE));
    let file = editor.document_buffer_id;
    editor.cursor.line = 3;

    let content = lattice_help::parse_help_lines(
        "hover",
        vec![
            "fn widget(x: u32) -> u32".to_string(),
            "returns a widget".to_string(),
            "see also: gadget".to_string(),
        ],
    );
    let _ =
        editor.open_floating_popup(content, lattice_host::popup::PopupPlacement::CursorAnchored);
    editor.focus_help_popup();
    assert!(editor.popup_focused, "the popup has focus");
    (editor, file)
}

/// The headline: a focused popup IS the active document.
#[test]
fn focusing_a_popup_makes_it_the_active_document() {
    let (editor, file) = editor_with_focused_popup();
    let popup = editor.popup_buffer.expect("a popup buffer");

    assert_eq!(
        editor.document_buffer_id, popup,
        "the focused surface is the active document — this is the whole slice"
    );
    assert_ne!(editor.document_buffer_id, file);
    assert!(
        editor.document.snapshot().text().contains("widget"),
        "…and the active document's CONTENT is the popup's, not the file's"
    );
}

/// The pane still shows the file. Swapping the active document must not
/// swap what is painted behind the popup — the buffer-keyed render path
/// MB.1 built for the command line is what carries this.
#[test]
fn the_pane_still_holds_the_file() {
    let (editor, file) = editor_with_focused_popup();
    assert_eq!(
        editor.pane_tree.active().buffer_id,
        file,
        "the pane's buffer is untouched; only focus moved"
    );
}

/// `/` searches the POPUP. The report, inverted into an assertion.
#[test]
fn searching_a_focused_popup_searches_the_popup() {
    let (mut editor, file) = editor_with_focused_popup();
    let file_cursor_before = editor.cursor;

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    for c in ["g", "a", "d", "g", "e", "t"] {
        let _ = editor.dispatch(lattice_host::action::Action::Insert(c.to_string()));
    }
    assert_eq!(editor.search_pattern(), "gadget");
    editor.do_search_line_submit();

    // `gadget` exists only in the popup — line 2 of its content.
    assert_eq!(
        editor.document_buffer_id,
        editor.popup_buffer.expect("still open"),
        "after submit the popup is focused again, not the file"
    );
    assert_eq!(
        editor.cursor.line, 2,
        "the caret landed on the popup's matching line"
    );
    let _ = file_cursor_before;
    let _ = file;
}

/// …and the file behind is untouched by it.
#[test]
fn searching_a_popup_leaves_the_file_alone() {
    let (mut editor, file) = editor_with_focused_popup();

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    for c in ["w", "i", "d", "g", "e", "t"] {
        let _ = editor.dispatch(lattice_host::action::Action::Insert(c.to_string()));
    }
    editor.do_search_line_submit();

    // Dismiss and look at what the file was left holding.
    editor.dismiss_popup();
    assert_eq!(editor.document_buffer_id, file);
    assert_eq!(
        editor.cursor.line, 3,
        "the file's caret is where it was before the popup opened — a search \
         inside a popup is not a motion in the file"
    );
}

/// `<Esc>` out of the search returns to the POPUP, not past it to the file.
/// This is the nesting FS.1's stack exists for.
#[test]
fn escaping_the_search_returns_to_the_popup() {
    let (mut editor, _file) = editor_with_focused_popup();
    let popup = editor.popup_buffer.expect("a popup");

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    let _ = editor.dispatch(lattice_host::action::Action::Insert("g".to_string()));
    editor.do_search_line_cancel();

    assert_eq!(
        editor.document_buffer_id, popup,
        "one `<Esc>` lands in the popup"
    );
    assert!(editor.popup_focused, "…which still has focus");
    assert_eq!(editor.active_buffer, BufferKind::Help);
}

/// Dismissing the popup unwinds a search opened inside it too — leaving a
/// minibuffer focused over a popup that no longer exists is the original
/// wedge.
#[test]
fn dismissing_takes_a_nested_search_line_with_it() {
    let (mut editor, file) = editor_with_focused_popup();

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    let _ = editor.dispatch(lattice_host::action::Action::Insert("g".to_string()));
    editor.dismiss_popup();

    assert_eq!(editor.document_buffer_id, file, "back in the file");
    assert!(editor.focus_stack.is_empty(), "and nothing is focused");
    assert!(
        !editor.search_line_active(),
        "the search line went with the popup"
    );
    assert_eq!(
        editor.cursor.line, 3,
        "at the caret the file kept throughout"
    );
}

/// Dismissing restores the modal state the popup normalised on open
/// (PU-A.1b) — a user mid-Insert returns to Insert.
#[test]
fn dismissing_restores_the_modal_state() {
    let mut editor = Editor::boot(CoreDocument::from_text(FILE));
    editor.modal = lattice_grammar::ModalState::Insert;
    let content = lattice_help::parse_help_lines("hover", vec!["docs".to_string()]);
    let _ =
        editor.open_floating_popup(content, lattice_host::popup::PopupPlacement::CursorAnchored);
    editor.focus_help_popup();
    assert_eq!(
        editor.modal,
        lattice_grammar::ModalState::Normal,
        "a focus-stealing popup is a Normal-mode surface"
    );

    editor.dismiss_popup();
    assert_eq!(
        editor.modal,
        lattice_grammar::ModalState::Insert,
        "…and dismissing gives the user their mode back"
    );
}
