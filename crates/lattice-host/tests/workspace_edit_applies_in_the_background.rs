//! A WorkspaceEdit lands in every file it names without moving focus, and
//! every file it touched is saved (WE.2 / WE.3).
//!
//! ## The trap this closes
//!
//! `:lsp-rename` opened each other file in the active pane (`:e`), so the
//! user ended on whichever file came last, applied the edit, and saved none
//! of them: a rename left a trail of dirty buffers to visit and `:w`. Code
//! actions and a server's `workspace/applyEdit` took the same route. An edit
//! to a buffer other than the active one also never reached the language
//! server as a `didChange`.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_lsp::lsp_types::{Position, Range, TextEdit, Uri};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lattice-we-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `foo` → `bar` at `line`, columns `[col, col + 3)`.
fn rename_at(line: u32, col: u32) -> TextEdit {
    TextEdit {
        range: Range {
            start: Position {
                line,
                character: col,
            },
            end: Position {
                line,
                character: col + 3,
            },
        },
        new_text: "bar".into(),
    }
}

fn uri(path: &Path) -> Uri {
    lattice_lsp::actor::uri_from_path(path)
}

fn disk(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// The buffer open on `path`, if any, by the path it carries.
fn buffer_on(editor: &Editor, path: &Path) -> Option<lattice_host::buffers::BufferId> {
    editor.buffers.sorted_ids().into_iter().find(|&id| {
        editor
            .buffers
            .document_handle(id)
            .and_then(|h| h.path())
            .is_some_and(|p| p == path)
    })
}

fn editor_on(path: &Path) -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::open(path).unwrap())
}

#[test]
fn a_rename_saves_every_file_and_opens_none() {
    let dir = Dir::new("saves");
    let main = dir.file("main.rs", "fn foo() {}\nfn main() { foo(); }\n");
    let other = dir.file("other.rs", "use crate::foo;\n");
    let mut editor = editor_on(&main);
    let active = editor.active_pane_buffer_id();

    let _ = editor.apply_rename_workspace_edit(
        vec![
            (uri(&main), vec![rename_at(0, 3), rename_at(1, 12)]),
            (uri(&other), vec![rename_at(0, 11)]),
        ],
        "bar".into(),
    );

    assert_eq!(editor.active_pane_buffer_id(), active, "focus never moved");
    assert_eq!(disk(&main), "fn bar() {}\nfn main() { bar(); }\n");
    assert_eq!(
        disk(&other),
        "use crate::bar;\n",
        "the other file was saved"
    );
    assert!(
        !editor.document.snapshot().dirty,
        "the active buffer was saved too"
    );
    assert!(
        buffer_on(&editor, &other).is_none(),
        "a file that was not open is closed again once saved"
    );
    let echo = editor.last_message.as_ref().unwrap();
    assert_eq!(echo.text, "rename -> bar: 3 edits across 2 files, saved");
}

#[test]
fn an_open_buffer_is_edited_where_it_is_and_saved() {
    let dir = Dir::new("open");
    let main = dir.file("main.rs", "fn main() {}\n");
    let other = dir.file("other.rs", "fn foo() {}\n");
    let mut editor = editor_on(&main);
    let active = editor.active_pane_buffer_id();
    let peer = editor.resolve_path_to_buffer(&other).unwrap();

    let _ = editor
        .apply_rename_workspace_edit(vec![(uri(&other), vec![rename_at(0, 3)])], "bar".into());

    assert_eq!(editor.active_pane_buffer_id(), active, "focus never moved");
    assert_eq!(
        buffer_on(&editor, &other),
        Some(peer),
        "the open buffer is the one edited, and it stays open"
    );
    let snap = editor.buffers.document_handle(peer).unwrap().snapshot();
    assert_eq!(snap.buffer.as_string(), "fn bar() {}\n");
    assert!(!snap.dirty, "and saved");
    assert_eq!(disk(&other), "fn bar() {}\n");
}

#[test]
fn with_saving_off_the_files_stay_open_and_dirty_for_review() {
    let dir = Dir::new("unsaved");
    let main = dir.file("main.rs", "fn main() {}\n");
    let other = dir.file("other.rs", "fn foo() {}\n");
    let mut editor = editor_on(&main);
    let active = editor.active_pane_buffer_id();
    editor
        .config
        .set_typed::<lattice_config::core_options::LspSaveWorkspaceEdits>(false)
        .unwrap();

    let _ = editor
        .apply_rename_workspace_edit(vec![(uri(&other), vec![rename_at(0, 3)])], "bar".into());

    assert_eq!(editor.active_pane_buffer_id(), active, "focus never moved");
    assert_eq!(disk(&other), "fn foo() {}\n", "nothing was written");
    let kept = buffer_on(&editor, &other).expect("the edited file stays open");
    assert!(
        editor.buffers.listed_ids_sorted().contains(&kept),
        "listed, so `:ls` finds it"
    );
    let snap = editor.buffers.document_handle(kept).unwrap().snapshot();
    assert_eq!(snap.buffer.as_string(), "fn bar() {}\n");
    assert!(snap.dirty, "the edit is there, unsaved");
    let echo = editor.last_message.as_ref().unwrap();
    assert!(
        echo.text.contains("unsaved:") && echo.text.contains("other.rs"),
        "the echo names what was left unsaved: {}",
        echo.text
    );
}

#[test]
fn a_missing_file_is_reported_and_the_rest_still_lands() {
    let dir = Dir::new("missing");
    let main = dir.file("main.rs", "fn foo() {}\n");
    let gone = dir.0.join("gone.rs");
    let mut editor = editor_on(&main);

    let _ = editor.apply_rename_workspace_edit(
        vec![
            (uri(&gone), vec![rename_at(0, 3)]),
            (uri(&main), vec![rename_at(0, 3)]),
        ],
        "bar".into(),
    );

    assert_eq!(disk(&main), "fn bar() {}\n", "the file that exists landed");
    let echo = editor.last_message.as_ref().unwrap();
    assert_eq!(echo.level, lattice_host::action::EchoLevel::Warn);
    assert!(
        echo.text.contains("failed:") && echo.text.contains("gone.rs"),
        "the failure is named: {}",
        echo.text
    );
}

/// One undo puts an open buffer's part of the rename back.
#[test]
fn each_file_is_one_undo_unit() {
    let dir = Dir::new("undo");
    let main = dir.file("main.rs", "fn foo() { foo(); }\n");
    let mut editor = editor_on(&main);

    let _ = editor.apply_rename_workspace_edit(
        vec![(uri(&main), vec![rename_at(0, 3), rename_at(0, 11)])],
        "bar".into(),
    );
    assert_eq!(
        editor.document.snapshot().buffer.as_string(),
        "fn bar() { bar(); }\n"
    );
    let _ = editor.undo_blocking();
    assert_eq!(
        editor.document.snapshot().buffer.as_string(),
        "fn foo() { foo(); }\n"
    );
}
