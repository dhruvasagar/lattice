//! `Effect::DisplayBuffer` shows a buffer beside the one the user is working
//! in and leaves focus where it was: emacs's `display-buffer` next to
//! `switch-to-buffer`.
//!
//! Two kinds of output need it. An LLM answer must appear without taking
//! the caret, the mode or the pane away from the code it is about, and a
//! second answer must replace the first rather than add a split per
//! request. An export's file lands seconds after its chord, when the user
//! is typing elsewhere, so it opens in a split or a background tab.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::DisplayTarget;
use lattice_grammar::{DisplaySource, Effect};
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

const MODE: &str = "plugin-output-mode";

fn synthetic(name: &str, mode: &str, content: &str) -> DisplaySource {
    DisplaySource::Synthetic {
        name: name.to_string(),
        mode_id: mode.to_string(),
        content: Some(content.to_string()),
        cursor: None,
        activate_minor: None,
    }
}

fn display(editor: &mut Editor, name: &str, mode: &str) {
    let _ = editor.handle_effect(Effect::DisplayBuffer {
        source: Box::new(synthetic(name, mode, &format!("{name} body\n"))),
        target: DisplayTarget::VSplit,
    });
}

fn display_file(editor: &mut Editor, path: &std::path::Path, target: DisplayTarget) {
    let _ = editor.handle_effect(Effect::DisplayBuffer {
        source: Box::new(DisplaySource::File(path.to_path_buf())),
        target,
    });
}

/// Every pane's buffer text, in this tab, in leaf order.
fn texts(editor: &Editor, panes: &[lattice_core::ui::pane::PaneState]) -> Vec<String> {
    panes
        .iter()
        .map(|p| {
            editor
                .buffers
                .document_handle(p.buffer_id)
                .map(|d| d.snapshot().text().to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// A file on disk holding `text`, in a directory of its own.
fn file_with(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plan.html");
    std::fs::write(&path, text).unwrap();
    (dir, path)
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
    let _ = editor.handle_effect(Effect::DisplayBuffer {
        source: Box::new(synthetic("*answer:1*", MODE, "a different seed\n")),
        target: DisplayTarget::VSplit,
    });
    assert_eq!(text_of(&editor, "*answer:1*"), "*answer:1* body\n");
}

/// A file no buffer holds opens in a new pane beside the user's, which
/// keeps focus, its buffer and its caret.
#[tokio::test]
async fn a_file_opens_beside_and_focus_stays() {
    let mut editor = working_editor();
    let (pane, buffer) = (editor.pane_tree.active_index(), editor.active_buffer_id());
    let (_dir, path) = file_with("<p>exported</p>\n");

    display_file(&mut editor, &path, DisplayTarget::VSplit);

    assert_eq!(editor.pane_tree.leaves().len(), 2, "a split for the file");
    assert_eq!(
        editor.pane_tree.active_index(),
        pane,
        "the same pane is active"
    );
    assert_eq!(editor.active_buffer_id(), buffer, "showing the same buffer");
    assert_eq!(editor.cursor, Position::new(1, 4), "with the caret unmoved");
    assert_eq!(
        editor.document.text(),
        "fn main() {\n    work();\n}\n",
        "the live document is still the user's"
    );
    let shown = texts(&editor, editor.pane_tree.leaves());
    assert!(
        shown.contains(&"<p>exported</p>\n".to_string()),
        "{shown:?}"
    );
}

/// Shown again, as a re-export does: the pane it is in is left as it is,
/// not split again.
#[tokio::test]
async fn a_file_shown_again_reuses_its_pane() {
    let mut editor = working_editor();
    let (_dir, path) = file_with("<p>exported</p>\n");
    display_file(&mut editor, &path, DisplayTarget::VSplit);
    display_file(&mut editor, &path, DisplayTarget::VSplit);
    assert_eq!(editor.pane_tree.leaves().len(), 2);
}

/// A tab target adds a tab after the active one without switching to it.
#[tokio::test]
async fn a_tab_target_adds_a_tab_in_the_background() {
    for source in ["file", "synthetic"] {
        let mut editor = working_editor();
        let buffer = editor.active_buffer_id();
        let (_dir, path) = file_with("<p>exported</p>\n");
        match source {
            "file" => display_file(&mut editor, &path, DisplayTarget::Tab),
            _ => {
                let _ = editor.handle_effect(Effect::DisplayBuffer {
                    source: Box::new(synthetic("*Org HTML Export*", MODE, "<p>exported</p>\n")),
                    target: DisplayTarget::Tab,
                });
            }
        }
        assert_eq!(editor.tabs.len(), 2, "{source}: a second tab");
        assert_eq!(editor.active_tab, 0, "{source}: not switched to");
        assert_eq!(editor.active_buffer_id(), buffer, "{source}: same buffer");
        assert_eq!(
            editor.cursor,
            Position::new(1, 4),
            "{source}: caret unmoved"
        );
        assert_eq!(
            editor.pane_tree.leaves().len(),
            1,
            "{source}: no split here"
        );
        let shown = texts(&editor, editor.tabs[1].panes.leaves());
        assert_eq!(shown, vec!["<p>exported</p>\n".to_string()], "{source}");

        // Shown again: the tab that has it is enough.
        match source {
            "file" => display_file(&mut editor, &path, DisplayTarget::Tab),
            _ => {
                let _ = editor.handle_effect(Effect::DisplayBuffer {
                    source: Box::new(synthetic("*Org HTML Export*", MODE, "")),
                    target: DisplayTarget::Tab,
                });
            }
        }
        assert_eq!(editor.tabs.len(), 2, "{source}: not a tab per display");
    }
}

/// A buffer the user is looking at needs no second pane.
#[tokio::test]
async fn a_buffer_in_the_active_pane_is_left_there() {
    let mut editor = working_editor();
    let _ = editor.handle_effect(Effect::OpenSyntheticBuffer {
        name: "*answer:1*".to_string(),
        mode_id: MODE.to_string(),
        content: Some("being read\n".to_string()),
        cursor: None,
        activate_minor: None,
    });
    display(&mut editor, "*answer:1*", MODE);
    assert_eq!(editor.pane_tree.leaves().len(), 1);
}

/// A path that cannot be shown leaves the layout as it was, and says why.
#[tokio::test]
async fn a_directory_is_refused_and_nothing_is_split() {
    let mut editor = working_editor();
    let dir = tempfile::tempdir().unwrap();
    display_file(&mut editor, dir.path(), DisplayTarget::VSplit);
    assert_eq!(editor.pane_tree.leaves().len(), 1);
    assert_eq!(editor.tabs.len(), 1);
    assert!(
        editor
            .last_message
            .as_ref()
            .is_some_and(|m| m.text.contains("directory")),
        "{:?}",
        editor.last_message
    );
}
