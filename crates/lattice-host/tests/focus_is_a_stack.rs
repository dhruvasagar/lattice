//! FS.1 — focus nests, so the thing that records it is a stack; and focus
//! REPLACES, so not every focus pushes a frame.
//!
//! Design: `docs/dev/architecture/focused-surface.md` §2. Slice plan:
//! `docs/dev/operations/slice-plans/focused-surface.md`.
//!
//! `minibuffer_focus` was a single `Option` written behind an `is_none()`
//! guard. That is correct while only one surface can hold focus and false the
//! moment one opens inside another — a popup holds focus, `/` inside it takes
//! focus again, and the second push recorded nothing, so one restore returned
//! to the FILE and skipped the popup.
//!
//! ## Both halves, because the first fix broke the second
//!
//! Making the push unconditional then broke the case that is NOT nesting: a
//! prompt superseding a prompt. Org's two-hop `org-set-property` opens a
//! prompt from a prompt's submit, and as two frames the final `<CR>` popped
//! the user back into the prompt they had already answered. The org suite
//! caught it; these tests are what would have.
//!
//! ## The fixtures are production shapes
//!
//! Nesting is asserted with a popup and a search line, because that is the
//! only nesting the editor produces. An earlier version of this file used two
//! synthetic buffers as stand-ins and passed while modelling a state nothing
//! creates — the same mistake `install_help` made for three rounds.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn editor_with_focused_popup() -> (Editor, lattice_core::BufferId) {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\ngamma\n"));
    let file = editor.document_buffer_id;
    editor.cursor.line = 2;
    let content = lattice_help::parse_help_lines("hover", vec!["the docs".to_string()]);
    let _ =
        editor.open_floating_popup(content, lattice_host::popup::PopupPlacement::CursorAnchored);
    editor.focus_help_popup();
    (editor, file)
}

/// Two frames, popped in order. The `Option` could not express this: the
/// second focus recorded nothing, so the first restore jumped to the file.
#[test]
fn focus_nests_and_unwinds_one_frame_at_a_time() {
    let (mut editor, file) = editor_with_focused_popup();
    let popup = editor.popup_buffer.expect("a popup");
    assert_eq!(editor.focus_stack.len(), 1, "the popup is frame 1");

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    assert_eq!(
        editor.focus_stack.len(),
        2,
        "a `/` inside a focused popup NESTS — it is a different surface, not \
         a replacement for the popup"
    );

    editor.restore_editing_buffer();
    assert_eq!(
        editor.document_buffer_id, popup,
        "one restore lands in the popup, not past it to the file"
    );

    editor.restore_editing_buffer();
    assert_eq!(editor.document_buffer_id, file);
    assert_eq!(editor.cursor.line, 2, "the file kept its cursor throughout");
    assert!(editor.focus_stack.is_empty());
}

/// `focused_surface()` answers about the INNERMOST frame — "is anything
/// focused" and "what is focused" have to be one question, or they drift.
#[test]
fn the_accessor_reads_the_innermost_frame() {
    let (mut editor, file) = editor_with_focused_popup();
    let popup = editor.popup_buffer.expect("a popup");
    assert_eq!(
        editor.focused_surface().map(|f| f.prior_buffer_id),
        Some(file),
        "frame 1 remembers the file it took focus from"
    );

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    assert_eq!(
        editor.focused_surface().map(|f| f.prior_buffer_id),
        Some(popup),
        "frame 2 remembers the POPUP it took focus from, not the file"
    );
}

/// A prompt superseding a prompt REPLACES its frame.
///
/// The two-hop shape: an action opens a prompt, and that prompt's submit
/// opens another. As two frames the final `<CR>` lands the user back in the
/// prompt they just answered instead of in their file.
#[test]
fn a_prompt_superseding_a_prompt_replaces_its_frame() {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\n"));
    let file = editor.document_buffer_id;
    editor.cursor.line = 1;

    editor.open_prompt_line(
        "First: ".to_string(),
        String::new(),
        "noop-action".to_string(),
        None,
    );
    assert_eq!(editor.focus_stack.len(), 1);

    // The second hop, as `org-set-property` does it.
    editor.open_prompt_line(
        "Second: ".to_string(),
        String::new(),
        "noop-action".to_string(),
        None,
    );
    assert_eq!(
        editor.focus_stack.len(),
        1,
        "a prompt replacing a prompt is not nesting — two frames here means \
         two restores, and the user is left in a minibuffer they answered"
    );
    assert_eq!(
        editor.focused_surface().map(|f| f.prior_buffer_id),
        Some(file),
        "…and the surviving frame remembers the FILE, not the prompt it \
         replaced — that is where one restore has to land"
    );

    editor.restore_editing_buffer();
    assert_eq!(
        editor.document_buffer_id, file,
        "one restore, back in the file"
    );
    assert_eq!(editor.cursor.line, 1);
}

/// Popping an empty stack is a no-op. `restore_editing_buffer` is called from
/// several cancel paths that cannot all prove a frame exists, and a panic
/// there would take the editor down on a stray `<Esc>`.
#[test]
fn restoring_with_nothing_focused_is_a_no_op() {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\n"));
    let file = editor.document_buffer_id;
    editor.cursor.line = 0;
    editor.restore_editing_buffer();
    assert_eq!(editor.document_buffer_id, file);
    assert_eq!(editor.cursor.line, 0, "and it changes nothing");
}

/// The single-frame path is unchanged — the case every existing minibuffer
/// test exercises. Asserted here too so a stack refactor cannot quietly break
/// the common case while the nesting tests still pass.
#[test]
fn one_frame_behaves_exactly_as_before() {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\ngamma\n"));
    let file = editor.document_buffer_id;
    editor.cursor.line = 2;
    editor.scroll = 1;

    editor.open_search_line(lattice_grammar::SearchDirection::Forward);
    assert_eq!(editor.cursor.line, 0, "a focused surface starts at its top");

    editor.restore_editing_buffer();
    assert_eq!(editor.document_buffer_id, file);
    assert_eq!(editor.cursor.line, 2);
    assert_eq!(editor.scroll, 1, "…and the scroll it was left at");
}
