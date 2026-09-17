//! CD.6a — `Effect::OpenPicker { query }`: a picker can open already narrowed.
//!
//! emacs's `completing-read` takes an initial input; org-roam's node insert
//! passes the active region there, so selecting a phrase and pressing
//! `C-c n i` opens the picker filtered to it. Before this only a LIVE source
//! could seed its query, and the node pickers are static.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn boot_with_two_buffers() -> (Editor, tempfile::TempDir) {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let dir = tempfile::tempdir().unwrap();
    for name in ["alpha-notes.txt", "beta-notes.txt"] {
        let path = dir.path().join(name);
        std::fs::write(&path, "x\n").unwrap();
        let _ = editor.do_edit(Some(path), false);
    }
    (editor, dir)
}

fn visible_rows(editor: &Editor) -> Vec<String> {
    // `candidates` is the rendered, FILTERED list.
    let picker = editor.picker.as_ref().expect("a picker is open");
    picker
        .candidates
        .iter()
        .map(|c| c.raw.display.clone())
        .collect()
}

/// A static source seats with the query in the prompt and the rows narrowed.
#[test]
fn a_static_picker_opens_narrowed_to_its_seed() {
    let (mut editor, _dir) = boot_with_two_buffers();

    let _ = editor.open_picker_for_effect(
        "buffers".to_string(),
        Vec::new(),
        None,
        None,
        Some("alpha".to_string()),
    );

    let picker = editor.picker.as_ref().expect("the picker opened");
    assert_eq!(picker.query, "alpha", "the prompt shows the seed");
    assert_eq!(picker.query_cursor, "alpha".len(), "the caret is after it");
    let seeded = visible_rows(&editor);

    // The contract is "as if typed": the same rows, in the same order, as
    // clearing the prompt and typing the seed. Compared rather than spelled
    // out, and in the SAME editor, because the matcher is fuzzy and the rows
    // are tempdir paths — a random path can subsequence-match a short seed.
    let picker = editor.picker.as_mut().unwrap();
    picker.query.clear();
    picker.refilter();
    let unfiltered = visible_rows(&editor);
    let picker = editor.picker.as_mut().unwrap();
    picker.query = "alpha".to_string();
    picker.refilter();
    let by_hand = visible_rows(&editor);
    let names = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|r| r.rsplit('/').next().unwrap_or(r).to_string())
            .collect()
    };
    assert_eq!(
        names(&seeded),
        names(&by_hand),
        "a seed filters as typing does"
    );
    // `[no name]` (the boot buffer) cannot match `alpha` under any path, so
    // its absence is the deterministic proof that the seed filtered.
    assert!(
        unfiltered.iter().any(|r| r == "[no name]") && !seeded.iter().any(|r| r == "[no name]"),
        "…and it did filter: {unfiltered:?} → {seeded:?}"
    );
    assert!(
        seeded
            .first()
            .is_some_and(|r| r.ends_with("alpha-notes.txt")),
        "the substring match ranks first: {seeded:?}"
    );
}

/// No seed is an empty prompt, as before.
#[test]
fn no_seed_opens_an_empty_prompt() {
    let (mut editor, _dir) = boot_with_two_buffers();
    let _ = editor.open_picker_for_effect("buffers".to_string(), Vec::new(), None, None, None);
    assert_eq!(editor.picker.as_ref().unwrap().query, "");
}

/// A refused open must not hand its seed to the next picker — the rollback the
/// fill target already has (YR.6).
#[test]
fn a_refused_open_leaves_no_seed_behind() {
    let (mut editor, _dir) = boot_with_two_buffers();
    let _ = editor.open_picker_for_effect(
        "no-such-source".to_string(),
        Vec::new(),
        None,
        None,
        Some("stale".to_string()),
    );
    assert!(editor.picker.is_none());
    assert!(editor.pending_picker_query.is_none());

    let _ = editor.open_picker_for_effect("buffers".to_string(), Vec::new(), None, None, None);
    assert_eq!(editor.picker.as_ref().unwrap().query, "");
}
