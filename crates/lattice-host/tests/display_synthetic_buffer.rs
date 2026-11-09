//! LL.10a — `Effect::DisplaySyntheticBuffer` shows a buffer beside the one
//! the user is working in and leaves focus where it was.
//!
//! The ask operator's answer is the first user: it must appear without
//! taking the caret, the mode or the pane away from the code it is about,
//! and a second answer must replace the first rather than add a split per
//! request. Emacs's `display-buffer` next to `switch-to-buffer`.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::SplitOrientation;
use lattice_grammar::Effect;
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

const MODE: &str = "plugin-output-mode";

fn display(editor: &mut Editor, name: &str, mode: &str) {
    let _ = editor.handle_effect(Effect::DisplaySyntheticBuffer {
        name: name.to_string(),
        mode_id: mode.to_string(),
        content: Some(format!("{name} body\n")),
        split: SplitOrientation::Vertical,
    });
}

/// Names of the buffers each pane shows, in leaf order.
fn shown(editor: &Editor) -> Vec<String> {
    editor
        .pane_tree
        .leaves()
        .iter()
        .map(|p| editor.buffers.name_of(p.buffer_id).unwrap_or_default())
        .collect()
}

fn text_of(editor: &Editor, name: &str) -> String {
    let id = editor.buffers.by_name(name).expect("buffer exists");
    editor
        .buffers
        .document_handle(id)
        .expect("a document")
        .snapshot()
        .text()
        .to_string()
}

fn working_editor() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("fn main() {\n    work();\n}\n"));
    editor.set_cursor_clamped(Position::new(1, 4));
    editor
}

#[tokio::test]
async fn the_buffer_appears_in_a_new_pane_and_focus_stays() {
    let mut editor = working_editor();
    let pane = editor.pane_tree.active_index();
    let buffer = editor.active_buffer_id();

    display(&mut editor, "*answer:1*", MODE);

    assert_eq!(editor.pane_tree.leaves().len(), 2, "a split for the answer");
    assert_eq!(
        editor.pane_tree.active_index(),
        pane,
        "the same pane is active"
    );
    assert_eq!(editor.active_buffer_id(), buffer, "showing the same buffer");
    assert_eq!(editor.cursor, Position::new(1, 4), "with the caret unmoved");
    assert!(shown(&editor).contains(&"*answer:1*".to_string()));
    assert_eq!(text_of(&editor, "*answer:1*"), "*answer:1* body\n");
}

#[tokio::test]
async fn a_second_buffer_in_the_same_mode_reuses_that_pane() {
    let mut editor = working_editor();
    display(&mut editor, "*answer:1*", MODE);
    display(&mut editor, "*answer:2*", MODE);

    assert_eq!(editor.pane_tree.leaves().len(), 2, "no split per request");
    let names = shown(&editor);
    assert!(names.contains(&"*answer:2*".to_string()), "{names:?}");
    assert!(!names.contains(&"*answer:1*".to_string()), "{names:?}");
    // Replaced, not closed: the earlier answer is still a buffer.
    assert!(editor.buffers.by_name("*answer:1*").is_some());
}

#[tokio::test]
async fn displaying_a_shown_buffer_again_changes_nothing() {
    let mut editor = working_editor();
    display(&mut editor, "*answer:1*", MODE);
    let before = shown(&editor);
    display(&mut editor, "*answer:1*", MODE);
    assert_eq!(shown(&editor), before);
}

#[tokio::test]
async fn a_buffer_in_another_mode_gets_its_own_pane() {
    let mut editor = working_editor();
    display(&mut editor, "*answer:1*", MODE);
    display(&mut editor, "*other*", "text-mode");
    assert_eq!(editor.pane_tree.leaves().len(), 3);
}

/// The active pane is never taken, even when it shows a buffer of the
/// reuse mode: that is the buffer the user is reading.
#[tokio::test]
async fn the_active_pane_is_never_reused() {
    let mut editor = working_editor();
    let _ = editor.handle_effect(Effect::OpenSyntheticBuffer {
        name: "*reading*".to_string(),
        mode_id: MODE.to_string(),
        content: Some("being read\n".to_string()),
        cursor: None,
        activate_minor: None,
    });
    let reading = editor.active_buffer_id();

    display(&mut editor, "*answer:1*", MODE);

    assert_eq!(editor.pane_tree.leaves().len(), 2);
    assert_eq!(editor.active_buffer_id(), reading);
}

/// Under zoom only the active pane is drawn, so a reused pane would get
/// the buffer and show nothing.
#[tokio::test]
async fn reusing_a_pane_under_zoom_unzooms() {
    let mut editor = working_editor();
    display(&mut editor, "*answer:1*", MODE);
    assert!(editor.pane_tree.toggle_zoom());

    display(&mut editor, "*answer:2*", MODE);

    assert!(!editor.pane_tree.is_zoomed());
    assert!(shown(&editor).contains(&"*answer:2*".to_string()));
}

/// As `OpenSyntheticBuffer`: a reopened buffer keeps what it has, so text a
/// user added to it is never overwritten by the seed.
#[tokio::test]
async fn the_seed_fills_a_fresh_buffer_only() {
    let mut editor = working_editor();
    display(&mut editor, "*answer:1*", MODE);
    let _ = editor.handle_effect(Effect::DisplaySyntheticBuffer {
        name: "*answer:1*".to_string(),
        mode_id: MODE.to_string(),
        content: Some("a different seed\n".to_string()),
        split: SplitOrientation::Vertical,
    });
    assert_eq!(text_of(&editor, "*answer:1*"), "*answer:1* body\n");
}
