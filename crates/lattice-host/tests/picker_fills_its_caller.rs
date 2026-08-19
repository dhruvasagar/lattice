//! YR.3 — a picker returns text to whatever opened it.
//!
//! The primitive, and the whole of its difficulty is one sentence: **the
//! target is captured when the picker opens, never resolved when it
//! accepts.**
//!
//! By accept time the picker has been dismissed and the modal state that
//! identified the caller is gone, so resolving then reads whatever context is
//! current. In the single-level case — picker over a document, accept, text
//! goes to the document — that is usually the right answer. Which is exactly
//! the trap: it passes the obvious test and fails in the
//! picker-inside-a-prompt case the feature exists for.
//!
//! So every test below that could be satisfied by "put it wherever we are
//! now" is paired with one that could not.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_picker::{FillTarget, PickerAcceptOutcome};

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("hello\n"))
}

fn body(editor: &Editor) -> String {
    editor.document.snapshot().buffer.as_string()
}

fn echo(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

fn fill(editor: &mut Editor, text: &str) {
    let _ = editor.apply_picker_outcome(PickerAcceptOutcome::FillCaller {
        text: text.to_string(),
    });
}

#[test]
fn a_document_target_inserts_at_the_cursor() {
    let mut editor = boot();
    editor.picker_fill_target = Some(FillTarget::Document);
    fill(&mut editor, "XY");
    assert_eq!(body(&editor), "XYhello\n");
}

/// Note `body()` cannot be used to check for a leak here: opening the `:`
/// line makes `*command-line*` the focused buffer, so `editor.document` IS
/// the command line while it is up. Asserting "the document is unchanged"
/// against `editor.document` would be reading the very buffer that is
/// supposed to have changed — it passed for the wrong reason on the first
/// run of this test. The document id changing is the observable that says
/// the text went to a different buffer.
#[test]
fn a_command_line_target_lands_in_the_command_line() {
    let mut editor = boot();
    let document = editor.document_buffer_id;
    editor.picker_fill_target = Some(FillTarget::CommandLine);
    fill(&mut editor, "wq");

    assert!(
        editor.command_line().contains("wq"),
        "the `:` line should hold the text; got {:?}",
        editor.command_line()
    );
    assert_ne!(
        editor.document_buffer_id, document,
        "the text landed in the command-line buffer, not the document"
    );
}

/// **The nested case.** A picker opened from a prompt, accepted after the
/// picker is gone. An implementation that resolved the target at accept time
/// would put the text wherever focus happens to have landed — and would pass
/// every test above while failing this one.
#[test]
fn a_prompt_target_does_not_leak_into_the_document() {
    let mut editor = boot();
    let document_before = body(&editor);

    // The prompt is open, so it is the focused buffer, and its id is what
    // gets captured.
    let _ = editor.open_prompt_line("Name: ".to_string(), String::new(), String::new(), None);
    let prompt_buffer = editor.document_buffer_id;
    editor.picker_fill_target = Some(FillTarget::Prompt {
        buffer: prompt_buffer.0,
    });

    fill(&mut editor, "picked-value");

    assert!(
        editor.prompt_line_text().contains("picked-value"),
        "the prompt that opened the picker must receive the text; got {:?}",
        editor.prompt_line_text()
    );
    assert_ne!(
        body(&editor),
        format!("picked-value{document_before}"),
        "the document must not have received it"
    );
}

/// The named prompt is the point: a target naming *a* prompt is not enough
/// if a different prompt is what is focused when the text arrives.
#[test]
fn a_prompt_target_naming_a_gone_buffer_reports_rather_than_misfiring() {
    let mut editor = boot();
    let before = body(&editor);
    // A buffer id nothing is focused on — the prompt closed, or a second
    // one replaced it.
    editor.picker_fill_target = Some(FillTarget::Prompt { buffer: 9999 });

    fill(&mut editor, "orphan");

    assert_eq!(
        body(&editor),
        before,
        "text meant for a prompt must not fall through to the document"
    );
    assert!(
        echo(&editor).contains("gone"),
        "and the user must be told; got {:?}",
        echo(&editor)
    );
}

/// Text arriving with no captured target is a wiring bug — a source emitted
/// `FillCaller` for a picker opened to act rather than to answer. It must say
/// so: a `<CR>` that visibly does nothing is indistinguishable from a dead
/// keybinding.
#[test]
fn no_captured_target_reports_instead_of_swallowing() {
    let mut editor = boot();
    let before = body(&editor);
    assert!(editor.picker_fill_target.is_none(), "precondition");

    fill(&mut editor, "nowhere");

    assert_eq!(body(&editor), before);
    assert!(
        echo(&editor).contains("nothing was waiting"),
        "got {:?}",
        echo(&editor)
    );
}

/// The target is consumed. A second accept must not land in a caller that
/// has already been filled and moved on.
#[test]
fn the_target_is_consumed_by_the_fill_that_uses_it() {
    let mut editor = boot();
    editor.picker_fill_target = Some(FillTarget::Document);
    fill(&mut editor, "first");
    assert!(editor.picker_fill_target.is_none(), "consumed");

    let after_first = body(&editor);
    fill(&mut editor, "second");
    assert_eq!(
        body(&editor),
        after_first,
        "a second accept has no target and must not insert again"
    );
    assert!(echo(&editor).contains("nothing was waiting"));
}
