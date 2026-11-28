//! A conflict opened for resolution (magit's `dd` on an unmerged file) lays out
//! ours | working | theirs in equal thirds.
//!
//! The open splits the ours pane for the working file and then splits the
//! working pane for theirs. Each split halves the pane it splits, so without
//! equalizing, ours kept half the width and the other two a quarter each.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::PaneNode;
use lattice_host::editor::Editor;
use lattice_host::synthetic_buffers::SYNTHETIC_BUFFER_FLAGS;
use lattice_mode::ModeId;

fn side(editor: &mut Editor, name: &str, text: &str) {
    let id = editor.ensure_named_synthetic_document(
        name,
        ModeId::new("fundamental-mode"),
        SYNTHETIC_BUFFER_FLAGS,
    );
    editor.replace_owned_buffer(id, text);
}

#[tokio::test(flavor = "current_thread")]
async fn the_three_panes_share_the_width_equally() {
    let dir = tempfile::TempDir::new().unwrap();
    let working = dir.path().join("notes.org");
    std::fs::write(
        &working,
        "keep\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\nkeep too\n",
    )
    .unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    side(&mut editor, "*ours*", "keep\nours\nkeep too\n");
    side(&mut editor, "*theirs*", "keep\ntheirs\nkeep too\n");

    editor.do_diff_resolve("*ours*", working, "*theirs*");

    assert_eq!(
        editor.pane_tree.leaves().len(),
        3,
        "ours | working | theirs"
    );
    let PaneNode::VerticalSplit { ratio, right, .. } = editor.pane_tree.root() else {
        panic!(
            "expected a vertical split, got {:?}",
            editor.pane_tree.root()
        );
    };
    assert!(
        (*ratio - 1.0 / 3.0).abs() < 1e-4,
        "ours takes a third, got {ratio}"
    );
    let PaneNode::VerticalSplit { ratio: inner, .. } = &**right else {
        panic!("expected working | theirs, got {right:?}");
    };
    assert!(
        (*inner - 0.5).abs() < 1e-4,
        "working and theirs split the rest evenly, got {inner}"
    );
}
