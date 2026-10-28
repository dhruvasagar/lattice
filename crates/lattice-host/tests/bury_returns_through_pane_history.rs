//! `Effect::BuryBuffer` (magit's `q`, every in-pane view's `q`) returns the
//! pane to the buffer it showed before the view — read from the pane's own
//! buffer trail, not from an editor-wide slot.
//!
//! The reported fault: closing magit-status went back to the dashboard even
//! though a file had been opened just before. The return address was one
//! editor-wide slot, written only when empty and cleared only by a bury or a
//! delete. Leaving a view any other way — `<CR>` onto a file, `:e`, a picker —
//! left it holding the FIRST origin, so the next `q` went there instead of
//! the file the user had just come from. Being editor-wide, it could also
//! hand one pane another pane's buffer.
//!
//! The pane's buffer trail (PBH) already records every buffer the pane
//! showed, previews excluded by construction. A bury walks it back past the
//! view being buried and any other in-pane view stacked on top of it, so a
//! chain of views (status → log → `q`) still returns to the file.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::SplitOrientation;
use lattice_host::editor::Editor;

const VIEW: (&str, &str) = ("*plugins*", "plugins-mode");
const OTHER_VIEW: (&str, &str) = ("*plugin-trace*", "plugin-trace-mode");

fn file(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let text: String = (0..30).map(|i| format!("{name} line {i}\n")).collect();
    std::fs::write(&path, text).unwrap();
    path
}

fn open_view(e: &mut Editor, (name, mode): (&str, &str)) -> lattice_core::BufferId {
    e.open_synthetic_buffer(name, mode);
    let id = e.buffers.by_name(name).expect("view exists");
    assert_eq!(e.active_pane_buffer_id(), id, "the view took the pane");
    id
}

/// The report, step for step: a view opened over the start-up buffer, a file
/// visited from it, the view opened again, then `q`.
#[tokio::test]
async fn q_returns_to_the_file_visited_from_the_view_not_the_first_origin() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Editor::boot(CoreDocument::from_text("start-up buffer\n"));
    let start_up = e.active_pane_buffer_id();

    open_view(&mut e, VIEW);
    e.do_edit(Some(file(dir.path(), "visited.txt")), false);
    let visited = e.active_pane_buffer_id();
    assert_ne!(visited, start_up, "precondition");
    open_view(&mut e, VIEW);

    assert!(e.bury_buffer(), "there is somewhere to return to");

    assert_eq!(
        e.active_pane_buffer_id(),
        visited,
        "q returns to the file opened just before the view, not to the \
         buffer the first view was opened over"
    );
    assert_eq!(e.document_buffer_id, visited, "and it is the live document");
}

/// A view opened from another view returns past both to the file.
#[tokio::test]
async fn a_chain_of_views_buries_back_to_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(file(dir.path(), "a.txt")), false);
    let origin = e.active_pane_buffer_id();

    open_view(&mut e, VIEW);
    open_view(&mut e, OTHER_VIEW);

    assert!(e.bury_buffer());
    assert_eq!(e.active_pane_buffer_id(), origin);
}

/// The cursor comes back to where it was in the file.
#[tokio::test]
async fn burying_restores_the_cursor_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(file(dir.path(), "a.txt")), false);
    e.cursor = lattice_protocol::position::Position::new(12, 3);

    open_view(&mut e, VIEW);
    assert!(e.bury_buffer());

    assert_eq!(e.cursor, lattice_protocol::position::Position::new(12, 3));
}

/// A bury on a buffer that is not an in-pane view does nothing, so a mode
/// can bind `q` without a guard.
#[tokio::test]
async fn burying_a_file_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(file(dir.path(), "a.txt")), false);
    let id = e.active_pane_buffer_id();

    assert!(!e.bury_buffer());
    assert_eq!(e.active_pane_buffer_id(), id);
}

/// Each pane returns to its own buffer. With one editor-wide slot, a view
/// opened (and left open) in one pane decided where a view in another pane
/// went back to.
#[tokio::test]
async fn each_pane_buries_to_its_own_previous_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(file(dir.path(), "left.txt")), false);
    open_view(&mut e, VIEW);

    e.do_split_pane(SplitOrientation::Vertical);
    e.do_edit(Some(file(dir.path(), "right.txt")), false);
    let right = e.active_pane_buffer_id();
    open_view(&mut e, OTHER_VIEW);

    assert!(e.bury_buffer());
    assert_eq!(
        e.active_pane_buffer_id(),
        right,
        "this pane's own file, not the other pane's"
    );
}
