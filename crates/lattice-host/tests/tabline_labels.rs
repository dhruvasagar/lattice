//! Tabline labels (#29) — a tab with no custom label is named after its
//! active pane's buffer.
//!
//! The label used to read only the registry's synthetic-name slot, which
//! file-backed documents never set, so every tab holding a file read
//! `[scratch]`. These tests pin the resolution order: file basename, then
//! synthetic name, then `[scratch]` for the genuinely nameless document.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn tab_labels(editor: &mut Editor) -> Vec<String> {
    editor.publish_render_state();
    let rs = editor.render_state.load();
    rs.tabs
        .items
        .iter()
        .map(|item| item.label.to_string())
        .collect()
}

#[test]
fn a_tab_holding_a_file_is_named_after_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.rs");
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(path), false);

    assert_eq!(tab_labels(&mut e), vec!["main.rs".to_string()]);
}

/// Inactive tabs resolve through the stashed pane tree, not the live one,
/// so a backgrounded file tab has to keep its name.
#[test]
fn a_background_tab_keeps_its_file_name() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.txt");
    let second = dir.path().join("second.txt");
    std::fs::write(&first, "one\n").unwrap();
    std::fs::write(&second, "two\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(first), false);
    e.do_new_tab();
    e.do_edit(Some(second), false);

    assert_eq!(
        tab_labels(&mut e),
        vec!["first.txt".to_string(), "second.txt".to_string()]
    );
}

#[test]
fn an_unnamed_document_is_scratch() {
    let mut e = Editor::boot(CoreDocument::from_text("hello\n"));
    assert_eq!(tab_labels(&mut e), vec!["[scratch]".to_string()]);
}
