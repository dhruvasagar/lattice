//! `<C-w>T` — move the active pane to a new tab — must bring the moved
//! buffer live in the new tab, not just point the pane at it.
//!
//! The reported fault: after `<C-w>T` the new tab showed the wrong file,
//! lost syntax highlighting, and the moved buffer's actions did nothing
//! until it was opened again — worst for synthetic buffers. The handler
//! closed the pane first (making the SIBLING's buffer the live document),
//! planted the moved buffer's id in a fresh tab, and then restored only the
//! buffer kind, cursor and scroll by hand. The live document, its syntax
//! handle and `document_buffer_id` stayed the sibling's, so every consumer
//! of "the active document" disagreed with the pane. Every other pane / tab
//! switch funnels through `load_active_pane`; this one now does too.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::SplitOrientation;
use lattice_host::editor::Editor;

/// An editor with `first` in one pane and `second` focused in a split.
fn two_files_split(dir: &std::path::Path, second: &str) -> (Editor, std::path::PathBuf) {
    let first = dir.join("first.txt");
    let second = dir.join(second);
    std::fs::write(&first, "first file\n").unwrap();
    std::fs::write(&second, "fn second() {}\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(first), false);
    e.do_split_pane(SplitOrientation::Vertical);
    e.do_edit(Some(second.clone()), false);
    (e, second)
}

#[test]
fn the_moved_file_is_the_live_document_in_the_new_tab() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, second) = two_files_split(dir.path(), "second.txt");
    let moved = e.active_pane_buffer_id();

    e.do_move_pane_to_new_tab();

    assert_eq!(e.tabs.len(), 2, "a new tab was opened");
    assert_eq!(e.active_tab, 1, "and focused");
    assert_eq!(e.pane_tree.active().buffer_id, moved, "the pane shows it");
    assert_eq!(
        e.document_buffer_id, moved,
        "the live document is the moved buffer, not the sibling left behind"
    );
    assert_eq!(e.document.path().as_deref(), Some(second.as_path()));
    assert_eq!(e.document.snapshot().text(), "fn second() {}\n");
}

/// The tab left behind keeps showing the other file.
#[test]
fn the_tab_left_behind_still_shows_the_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, _) = two_files_split(dir.path(), "second.txt");

    e.do_move_pane_to_new_tab();
    e.do_prev_tab();

    let path = e.document.path().unwrap();
    assert_eq!(path.file_name().unwrap(), "first.txt");
    assert_eq!(e.document.snapshot().text(), "first file\n");
}

/// Syntax rides with the live document; a moved file must keep its handle.
#[test]
fn the_moved_file_keeps_its_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, _) = two_files_split(dir.path(), "second.rs");
    let moved = e.active_pane_buffer_id();
    if e.syntax.is_none() {
        // No rust grammar in this build → nothing to keep; skip loudly
        // rather than pass emptily.
        eprintln!("skipping: no live syntax handle for second.rs (rust grammar unavailable)");
        return;
    }

    e.do_move_pane_to_new_tab();

    assert_eq!(e.document_buffer_id, moved);
    assert!(
        e.syntax.is_some(),
        "the moved file's syntax handle is live in the new tab"
    );
}

/// Synthetic buffers were the worst case in the report.
#[tokio::test]
async fn a_moved_synthetic_buffer_is_live_in_the_new_tab() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.txt");
    std::fs::write(&first, "first file\n").unwrap();

    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(first), false);
    e.do_split_pane(SplitOrientation::Vertical);
    e.open_synthetic_buffer("*plugins*", "plugins-mode");
    let synthetic = e.buffers.by_name("*plugins*").expect("synthetic exists");
    assert_eq!(e.active_pane_buffer_id(), synthetic, "precondition");

    e.do_move_pane_to_new_tab();

    assert_eq!(e.pane_tree.active().buffer_id, synthetic);
    assert_eq!(
        e.document_buffer_id, synthetic,
        "the synthetic buffer is the live document, not the file left behind"
    );
    assert!(e.document.path().is_none(), "no file path leaked in");
}
