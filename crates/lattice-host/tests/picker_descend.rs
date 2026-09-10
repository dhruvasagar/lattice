//! PC.10 — `<C-l>` goes into the selected candidate, `<C-h>` comes back out.
//!
//! Design:
//! [`docs/dev/architecture/project-commands.md`](../../../docs/dev/architecture/project-commands.md)
//! §9 H5. Slice plan: PC.10.
//!
//! ## What is actually at risk here
//!
//! Not "does `dir-pick` descend" — that is one line over a candidate's text,
//! and PC.9 pins the listing it depends on. The risk is the OTHER pickers.
//! `descend` / `ascend` are trait hooks with `None` defaults, and a wiring
//! that ignored the default would give every picker in the editor a `<C-l>`
//! that silently rewrites its query. That failure is invisible in a
//! `dir-pick` test and obvious in a `buffers` one, so both are here.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_protocol::KeyChord;

/// The REAL path — chord → `dispatch_chord` → `input::translate` → the
/// dispatch arm. Dispatching `Action::PickerDescend` directly would pass
/// against a `<C-l>` that was never bound, which is the half of this slice
/// that can actually go missing.
fn press(editor: &mut Editor, ch: char) -> Action {
    let mut partial: Vec<KeyChord> = Vec::new();
    editor.dispatch_chord(KeyChord::ctrl(ch), &mut partial)
}

/// A tree two levels deep, so descending has somewhere to go and ascending
/// has somewhere to come back to.
fn tree() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("alpha").join("inner")).unwrap();
    std::fs::create_dir_all(dir.path().join("beta")).unwrap();
    dir
}

fn open_dir_pick(root: &std::path::Path) -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("committed\n"));
    let _ = editor.open_picker(
        lattice_picker::DIR_PICK_SOURCE.to_string(),
        vec![root.to_string_lossy().to_string()],
    );
    assert!(
        editor.picker.is_some(),
        "precondition: `dir-pick` seated a picker at {}",
        root.display()
    );
    editor
}

fn query(editor: &Editor) -> String {
    editor
        .picker
        .as_ref()
        .map(|p| p.query.clone())
        .unwrap_or_default()
}

fn rows(editor: &Editor) -> Vec<String> {
    editor
        .picker
        .as_ref()
        .map(|p| p.candidates.iter().map(|c| c.raw.text.clone()).collect())
        .unwrap_or_default()
}

/// `<C-l>` replaces the query with the selected directory, so the next
/// listing is of its children rather than its siblings.
#[test]
fn descending_makes_the_selected_directory_the_query() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick(&root);

    let selected = rows(&editor).first().cloned().expect("the tree has rows");
    assert!(
        selected.ends_with('/'),
        "precondition: rows are directories: {selected}"
    );

    press(&mut editor, 'l');

    assert_eq!(
        query(&editor),
        selected,
        "the selected row's own path becomes the query — trailing slash and \
         all, because that is the prefix that lists its CONTENTS rather than \
         its siblings"
    );
}

/// `<C-h>` drops the last component. Round-tripping is the assertion, because
/// an ascend that merely *changed* the query would pass a one-sided test.
#[test]
fn ascending_undoes_a_descend() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick(&root);

    let before = query(&editor);
    press(&mut editor, 'l');
    assert_ne!(query(&editor), before, "precondition: the descend moved");

    press(&mut editor, 'h');

    assert_eq!(
        query(&editor),
        format!("{}/", root.to_string_lossy()),
        "back to the directory we were listing"
    );
}

/// The root is a fixed point. Emptying the query there would silently
/// relocate the user somewhere they never asked to be.
#[test]
fn ascending_stops_at_the_filesystem_root() {
    let mut editor = Editor::boot(CoreDocument::from_text("committed\n"));
    let _ = editor.open_picker(
        lattice_picker::DIR_PICK_SOURCE.to_string(),
        vec!["/".to_string()],
    );
    assert!(editor.picker.is_some(), "precondition: `/` seats a picker");

    for _ in 0..5 {
        press(&mut editor, 'h');
    }

    assert_eq!(
        query(&editor),
        "/",
        "five presses at the root leave the query at the root"
    );
}

/// **The one that could silently break every other picker.** `descend` and
/// `ascend` default to `None`, and a wiring that ignored the default would
/// give `<C-l>` a meaning in pickers that have none — rewriting a `buffers`
/// query to a candidate's text, which is not remotely what the key says.
///
/// `buffers` also is not live, which is the second half of the gate: a static
/// source's rows come from `init` and are fuzzy-refiltered, so rewriting its
/// query would filter the rows it already has rather than fetch new ones.
#[test]
fn a_picker_with_no_notion_of_depth_ignores_both_keys() {
    let mut editor = Editor::boot(CoreDocument::from_text("committed\n"));
    let _ = editor.open_picker("buffers".to_string(), Vec::new());
    assert!(editor.picker.is_some(), "precondition: `buffers` seats");

    let before_query = query(&editor);
    let before_rows = rows(&editor);

    press(&mut editor, 'l');
    press(&mut editor, 'h');

    assert_eq!(
        query(&editor),
        before_query,
        "`<C-l>` / `<C-h>` must not touch the query of a picker whose source \
         takes the `None` default"
    );
    assert_eq!(
        rows(&editor),
        before_rows,
        "and must not disturb its candidates either"
    );
}

/// Neither key may close the picker or accept anything. They are navigation,
/// and a `<C-l>` that fell through to accept would open whatever was
/// selected — the worst possible reading of "go deeper".
#[test]
fn neither_key_accepts_or_dismisses() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick(&root);

    press(&mut editor, 'l');
    assert!(editor.picker.is_some(), "`<C-l>` leaves the picker open");

    press(&mut editor, 'h');
    assert!(editor.picker.is_some(), "`<C-h>` leaves the picker open");
}
