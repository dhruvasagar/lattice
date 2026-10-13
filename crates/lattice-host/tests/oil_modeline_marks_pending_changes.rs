//! GD.7d: an oil pane's modeline says when the listing has edits that
//! have not been applied, and says it from the buffer's ordinary
//! modified flag.
//!
//! The terminal used to compute this on the paint path, by rendering the
//! whole listing to a string every frame and comparing it to the
//! snapshot. That was removed with the terminal's label table (GD.7b),
//! and the marker with it. It comes back here from a flag that is read
//! in O(1) — which first had to be made true: an owner-written buffer
//! read as modified from the moment it was filled.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_host::modeline::{CORE_PATH, resolve_builtin_content};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

fn boot() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

fn path_segment(editor: &mut Editor) -> String {
    let rs = editor.build_render_state();
    let pane = *editor.pane_tree.active();
    resolve_builtin_content(CORE_PATH, &pane, true, &rs).plain()
}

#[test]
fn an_oil_listing_is_marked_only_while_it_has_unapplied_edits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.txt"), "a\n").unwrap();
    let mut editor = boot();
    editor.do_open_oil(Some(dir.path().to_path_buf()));
    let want = format!("[oil] {}", dir.path().display());

    assert_eq!(
        path_segment(&mut editor),
        want,
        "a listing the editor just wrote is not a modified buffer"
    );

    // Rename an entry by typing in the listing.
    lattice_runtime::block_on(
        editor
            .document
            .apply_edit(Edit::insert(Position::new(0, 0), "x")),
    )
    .unwrap();
    assert_eq!(path_segment(&mut editor), format!("{want} [+]"));

    // Take it back: nothing is pending any more.
    lattice_runtime::block_on(editor.document.undo()).unwrap();
    assert_eq!(path_segment(&mut editor), want);
}

/// The tree's text is also owner-written, and the user cannot edit it:
/// it must never carry the marker.
#[test]
fn a_file_tree_is_never_marked_modified() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.txt"), "a\n").unwrap();
    let mut editor = boot();
    editor.do_open_file_tree(Some(dir.path().to_path_buf()));
    let segment = path_segment(&mut editor);
    assert!(
        segment.starts_with("[tree] "),
        "the tree pane is labelled; got {segment:?}"
    );
    assert!(!segment.ends_with("[+]"), "got {segment:?}");
}
