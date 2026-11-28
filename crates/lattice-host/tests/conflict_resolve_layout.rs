//! A conflict opened for resolution (magit's `dd` on an unmerged file) lays out
//! ours | working | theirs in equal thirds.
//!
//! The open splits the ours pane for the working file and then splits the
//! working pane for theirs. Each split halves the pane it splits, so without
//! equalizing, ours kept half the width and the other two a quarter each.
//!
//! While diffed, the working file also folds by the diff alone
//! (`foldmethod=diff`), so its own folds cannot hide the conflict.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::FoldMethod;
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

/// Tick the way the editor actor's `async_landed` arm does (the tick, then
/// `publish_render_state`, which applies the diff-mode toggles a session
/// queued) until `until` holds; mode activation lands asynchronously.
async fn settle(editor: &mut Editor, until: impl Fn(&Editor) -> bool) -> bool {
    for _ in 0..100 {
        let _ = editor.publish_render_state();
        if until(editor) {
            return true;
        }
        let woken = editor.async_landed.clone();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), woken.notified()).await;
        let _ = editor.run_tick_pending();
        let _ = editor.publish_render_state();
    }
    until(editor)
}

fn hidden(editor: &Editor, line: u32) -> bool {
    editor
        .folds
        .iter()
        .any(|f| f.closed && line > f.start_line && line <= f.end_line)
}

/// While diffed, the working file folds by the diff alone, as vim's
/// `foldmethod=diff`. Its own folds, here markdown headings closed by
/// `foldlevel=0` (org opens that way), hid the conflict inside a collapsed
/// heading.
#[tokio::test(flavor = "current_thread")]
async fn the_working_file_folds_by_the_diff_alone_while_diffed() {
    let dir = tempfile::TempDir::new().unwrap();
    let working = dir.path().join("notes.md");
    std::fs::write(
        &working,
        "# Notes\nkeep\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\nkeep too\n# Other\nx\n",
    )
    .unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    editor.execute_ex_line("set foldmethod=markdown", &mut out);
    editor.execute_ex_line("set foldlevel=0", &mut out);
    side(
        &mut editor,
        "*ours*",
        "# Notes\nkeep\nours\nkeep too\n# Other\nx\n",
    );
    side(
        &mut editor,
        "*theirs*",
        "# Notes\nkeep\ntheirs\nkeep too\n# Other\nx\n",
    );

    editor.do_diff_resolve("*ours*", working, "*theirs*");
    assert!(
        settle(&mut editor, |e| e.foldmethod() == FoldMethod::Diff).await,
        "diff-mode sets foldmethod=diff on the working file, got {:?}",
        editor.foldmethod()
    );
    editor.recompute_folds();
    for line in 2..=6 {
        assert!(
            !hidden(&editor, line),
            "conflict line {line} is visible; folds: {:?}",
            editor.folds
        );
    }

    editor.execute_ex_line("diffoff", &mut out);
    assert!(
        settle(&mut editor, |e| e.foldmethod() != FoldMethod::Diff).await,
        "leaving the diff gives the buffer its own foldmethod back, got {:?}",
        editor.foldmethod()
    );
}
