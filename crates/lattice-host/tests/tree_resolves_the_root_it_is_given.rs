//! `:Tree <root>` resolves the root the user typed.
//!
//! The argument went to the file-tree walker verbatim: no `~` expansion, and a
//! relative root resolved against the process cwd rather than the editor's
//! working directory. `:Tree ~/notes` looked for a directory literally named
//! `~` beside the cwd, found nothing, and opened an empty tree — which reads
//! as "that directory is empty", not "that is not a path". Same silent shape
//! as the rest of this family.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

#[test]
fn a_relative_tree_root_joins_the_editors_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/a.txt"), "x").unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.current_dir = Some(dir.path().to_path_buf());

    editor.do_open_file_tree(Some(std::path::PathBuf::from("sub")));

    assert!(
        editor
            .file_tree_with_root(&dir.path().join("sub"))
            .is_some(),
        "the tree must be rooted under the editor's cwd, not the process cwd"
    );
}

#[test]
fn an_absolute_tree_root_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("a.txt"), "x").unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.current_dir = Some(dir.path().to_path_buf());

    editor.do_open_file_tree(Some(elsewhere.path().to_path_buf()));

    assert!(editor.file_tree_with_root(elsewhere.path()).is_some());
}
