//! CM.2 (2026-07-22): error list substrate + generic navigation.
//!
//! Exercises the REAL host dispatch path: populate the core error
//! list via `set_error_list`, then drive `do_error_nav` and
//! assert the active buffer / cursor lands on each entry in order
//! (wrapping vim-style), and that each hop records a position-history
//! push (the jump is recorded). Also covers the empty-list fallback
//! to diagnostic hopping (no panic).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::ErrorTarget;
use lattice_host::editor::Editor;
use lattice_host::error_list::{ErrorEntry, ErrorSeverity};

fn write_file(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

fn entry(path: &std::path::Path, line: u32, col: u32, msg: &str) -> ErrorEntry {
    ErrorEntry {
        path: path.to_path_buf(),
        line,
        col,
        severity: ErrorSeverity::Error,
        message: msg.to_string(),
    }
}

#[test]
fn error_next_walks_entries_across_files_and_wraps() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = write_file(dir.path(), "a.txt", "a0\na1\na2\na3\n");
    let file_b = write_file(dir.path(), "b.txt", "b0\nb1\n");

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));

    // Three entries across two files. Index starts at 0 (on entry 0),
    // un-jumped; the first `:cnext` steps to entry 1.
    editor.set_error_list(vec![
        entry(&file_a, 1, 0, "first error"),
        entry(&file_b, 0, 0, "second error"),
        entry(&file_a, 2, 0, "third error"),
    ]);
    assert_eq!(editor.error_list().len(), 3);
    assert!(!editor.error_list().is_empty());
    assert_eq!(editor.error_list().index(), 0);

    let history_before = editor.position_history.len();

    // Next -> entry 1 (b.txt, line 0).
    editor.do_error_nav(ErrorTarget::Next);
    assert_eq!(editor.error_list().index(), 1);
    assert_eq!(
        editor
            .document
            .path()
            .as_deref()
            .and_then(|p| p.file_name()),
        file_b.file_name()
    );
    assert_eq!(editor.cursor.line, 0);
    assert!(
        editor.position_history.len() > history_before,
        "each error hop records a position-history push"
    );

    // Next -> entry 2 (a.txt, line 2).
    editor.do_error_nav(ErrorTarget::Next);
    assert_eq!(editor.error_list().index(), 2);
    assert_eq!(
        editor
            .document
            .path()
            .as_deref()
            .and_then(|p| p.file_name()),
        file_a.file_name()
    );
    assert_eq!(editor.cursor.line, 2);

    // Next past the end wraps back to entry 0 (a.txt, line 1).
    editor.do_error_nav(ErrorTarget::Next);
    assert_eq!(editor.error_list().index(), 0);
    assert_eq!(
        editor
            .document
            .path()
            .as_deref()
            .and_then(|p| p.file_name()),
        file_a.file_name()
    );
    assert_eq!(editor.cursor.line, 1);
}

#[test]
fn error_prev_wraps_and_cc_first_last_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = write_file(dir.path(), "a.txt", "a0\na1\na2\na3\n");
    let file_b = write_file(dir.path(), "b.txt", "b0\nb1\n");

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.set_error_list(vec![
        entry(&file_a, 1, 0, "first"),
        entry(&file_b, 1, 0, "second"),
        entry(&file_a, 3, 0, "third"),
    ]);

    // Prev from index 0 wraps to the last (entry 2, a.txt line 3).
    editor.do_error_nav(ErrorTarget::Prev);
    assert_eq!(editor.error_list().index(), 2);
    assert_eq!(
        editor
            .document
            .path()
            .as_deref()
            .and_then(|p| p.file_name()),
        file_a.file_name()
    );
    assert_eq!(editor.cursor.line, 3);

    // :cc 2 -> entry 1 (b.txt line 1), 1-based.
    editor.do_error_nav(ErrorTarget::Jump(Some(2)));
    assert_eq!(editor.error_list().index(), 1);
    assert_eq!(
        editor
            .document
            .path()
            .as_deref()
            .and_then(|p| p.file_name()),
        file_b.file_name()
    );
    assert_eq!(editor.cursor.line, 1);

    // :cfirst -> entry 0 (a.txt line 1).
    editor.do_error_nav(ErrorTarget::First);
    assert_eq!(editor.error_list().index(), 0);
    assert_eq!(editor.cursor.line, 1);

    // :clast -> entry 2 (a.txt line 3).
    editor.do_error_nav(ErrorTarget::Last);
    assert_eq!(editor.error_list().index(), 2);
    assert_eq!(editor.cursor.line, 3);

    // :cc with an out-of-range N leaves the index unchanged.
    editor.do_error_nav(ErrorTarget::Jump(Some(99)));
    assert_eq!(editor.error_list().index(), 2);
}

#[test]
fn compile_jump_to_location_moves_cursor_and_syncs_index() {
    // CM.3b: `<CR>` on a `*compilation*` location line drives
    // `AppEffect::CompileJumpToLocation`; the host arm jumps to the
    // source (records position history) AND syncs the error list index to
    // the matching entry. Drives the REAL apply path via
    // `apply_app_effect`.
    use lattice_grammar::AppEffect;
    use lattice_host::dispatch::DispatchOutcome;

    let dir = tempfile::tempdir().unwrap();
    let file_a = write_file(dir.path(), "a.txt", "a0\na1\na2\na3\n");
    let file_b = write_file(dir.path(), "b.txt", "b0\nb1\nb2\n");

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.set_error_list(vec![
        entry(&file_a, 1, 0, "first"),
        entry(&file_b, 2, 1, "second"),
        entry(&file_a, 3, 0, "third"),
    ]);
    assert_eq!(editor.error_list().index(), 0);
    let history_before = editor.position_history.len();

    let mut out = DispatchOutcome::default();
    editor.apply_app_effect(
        AppEffect::CompileJumpToLocation {
            path: file_b.clone(),
            line: 2,
            col: 1,
        },
        &mut out,
    );

    assert_eq!(
        editor
            .document
            .path()
            .as_deref()
            .and_then(|p| p.file_name()),
        file_b.file_name(),
        "active buffer switched to the jumped-to source file"
    );
    assert_eq!(editor.cursor.line, 2);
    assert_eq!(editor.cursor.byte, 1);
    assert_eq!(
        editor.error_list().index(),
        1,
        "error list index synced to the entry matching (path, line)"
    );
    assert!(
        editor.position_history.len() > history_before,
        "the jump records a position-history push"
    );
}

#[test]
fn compile_jump_to_location_without_matching_entry_still_jumps() {
    // A parsed location with no matching error entry (e.g. a gnu
    // note that never entered the list) still jumps to source but
    // leaves the error list index untouched (best-effort sync).
    use lattice_grammar::AppEffect;
    use lattice_host::dispatch::DispatchOutcome;

    let dir = tempfile::tempdir().unwrap();
    let file_a = write_file(dir.path(), "a.txt", "a0\na1\na2\n");
    let file_b = write_file(dir.path(), "b.txt", "b0\nb1\n");

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.set_error_list(vec![entry(&file_a, 1, 0, "only")]);
    assert_eq!(editor.error_list().index(), 0);

    let mut out = DispatchOutcome::default();
    editor.apply_app_effect(
        AppEffect::CompileJumpToLocation {
            path: file_b.clone(),
            line: 1,
            col: 0,
        },
        &mut out,
    );

    assert_eq!(
        editor
            .document
            .path()
            .as_deref()
            .and_then(|p| p.file_name()),
        file_b.file_name(),
        "still jumps to the source even with no matching entry"
    );
    assert_eq!(editor.cursor.line, 1);
    assert_eq!(
        editor.error_list().index(),
        0,
        "no matching entry → index unchanged"
    );
}

#[test]
fn empty_list_echoes_no_error_list_for_every_target() {
    // CM.7: the diagnostic fallback was removed. Error-list commands touch
    // ONLY the error list — an empty list echoes `no error list`
    // for EVERY target (diagnostics live on the dedicated `]d`/`[d`).
    let mut editor = Editor::boot(CoreDocument::from_text("one\ntwo\nthree\n"));
    assert!(editor.error_list().is_empty());

    for target in [
        ErrorTarget::Next,
        ErrorTarget::Prev,
        ErrorTarget::First,
        ErrorTarget::Last,
        ErrorTarget::NextFile,
        ErrorTarget::PrevFile,
        ErrorTarget::Jump(Some(3)),
    ] {
        editor.last_message = None;
        editor.do_error_nav(target);
        let msg = editor
            .last_message
            .as_ref()
            .expect("an echo was set for the empty-list target");
        assert_eq!(
            msg.text, "no error list",
            "empty-list {target:?} must echo `no error list`, not navigate diagnostics"
        );
    }
    // The cursor never moved (no diagnostic hop happened).
    assert_eq!(editor.cursor.line, 0);
    assert!(editor.error_list().is_empty());
}

#[test]
fn error_list_picker_lists_entries_and_empty_echoes() {
    // CM.8: `:clist` / `:cl` opens the error list in a picker
    // (parallel to `:diagnostics`).
    let dir = tempfile::tempdir().unwrap();
    let file_a = write_file(dir.path(), "a.txt", "a0\na1\n");
    let file_b = write_file(dir.path(), "b.txt", "b0\nb1\n");

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));

    // Empty list → no picker, echoes "no error list".
    editor.do_list_errors();
    assert!(editor.picker.is_none());
    assert_eq!(
        editor.last_message.as_ref().map(|m| m.text.as_str()),
        Some("no error list")
    );

    editor.set_error_list(vec![
        entry(&file_a, 1, 0, "first"),
        entry(&file_b, 0, 0, "second"),
        entry(&file_a, 0, 0, "third"),
    ]);
    editor.do_list_errors();
    let picker = editor.picker.as_ref().expect("picker opened");
    assert_eq!(picker.title, "error list (3)");
}

#[test]
fn error_list_file_nav_lands_on_first_entry_of_each_file() {
    // CM.7: `:cnextfile` / `:cprevfile` (`]qf` / `[qf`) move a whole
    // file at a time, landing on the first entry of the target file.
    // Traversal happens with NO `*compilation*` buffer open — the list
    // is core Editor state (the vim contract, not emacs).
    let dir = tempfile::tempdir().unwrap();
    let file_a = write_file(dir.path(), "a.txt", "a0\na1\na2\na3\n");
    let file_b = write_file(dir.path(), "b.txt", "b0\nb1\nb2\n");
    let file_c = write_file(dir.path(), "c.txt", "c0\nc1\n");

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    // Independence: no compilation buffer exists.
    assert!(
        editor.buffers.by_name("*compilation*").is_none(),
        "traversal must not require the *compilation* buffer"
    );

    // a.txt (2 entries) → b.txt (2 entries) → c.txt (1 entry).
    editor.set_error_list(vec![
        entry(&file_a, 1, 0, "a-first"),
        entry(&file_a, 3, 0, "a-second"),
        entry(&file_b, 0, 0, "b-first"),
        entry(&file_b, 2, 0, "b-second"),
        entry(&file_c, 1, 0, "c-only"),
    ]);

    let name = |e: &Editor| {
        e.document
            .path()
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_owned())
    };

    // NextFile from a.txt[0] → b.txt's first entry (index 2, line 0).
    editor.do_error_nav(ErrorTarget::NextFile);
    assert_eq!(editor.error_list().index(), 2);
    assert_eq!(name(&editor), file_b.file_name().map(|s| s.to_owned()));
    assert_eq!(editor.cursor.line, 0);

    // NextFile again → c.txt's first entry (index 4, line 1).
    editor.do_error_nav(ErrorTarget::NextFile);
    assert_eq!(editor.error_list().index(), 4);
    assert_eq!(name(&editor), file_c.file_name().map(|s| s.to_owned()));
    assert_eq!(editor.cursor.line, 1);

    // NextFile past the last file wraps to a.txt's first entry.
    editor.do_error_nav(ErrorTarget::NextFile);
    assert_eq!(editor.error_list().index(), 0);
    assert_eq!(name(&editor), file_a.file_name().map(|s| s.to_owned()));
    assert_eq!(editor.cursor.line, 1);

    // PrevFile from a.txt wraps to c.txt's first entry.
    editor.do_error_nav(ErrorTarget::PrevFile);
    assert_eq!(editor.error_list().index(), 4);
    assert_eq!(name(&editor), file_c.file_name().map(|s| s.to_owned()));
}
