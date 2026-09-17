//! CD.2 — `:e` on a path with nothing there, and `Effect::OpenBufferAt`'s
//! `content` / `activate-minor`.
//!
//! The drafts design (org-capture-drafts.md §2) depends on the first of
//! these: a capture opens on a path that does not exist yet, and aborting it
//! before any `:w` must touch no disk. vim 9.2 was checked for the
//! behaviour: `:e newfile` gives an empty, unmodified buffer, and `:w`
//! creates the file.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::Effect;
use lattice_host::dispatch::DoEditOutcome;
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

const MINOR: &str = "read-only-mode";

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("origin\n"))
}

fn minor_is_active(editor: &Editor) -> bool {
    let id = editor.active_pane_buffer_id();
    editor
        .active_modes
        .get(&id)
        .map(|m| m.is_active(lattice_mode::ModeId::new(MINOR)))
        .unwrap_or(false)
}

#[test]
fn editing_a_missing_file_opens_an_unsaved_empty_buffer() {
    let mut editor = boot();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh.org");

    let outcome = editor.do_edit(Some(path.clone()), false);

    assert!(matches!(outcome, DoEditOutcome::Opened(_)), "{outcome:?}");
    assert_eq!(editor.document.path().as_deref(), Some(path.as_path()));
    assert_eq!(editor.active_text().as_string(), "");
    assert!(!editor.document.dirty(), "vim: a new file is unmodified");
    assert!(!path.exists(), "opening creates nothing on disk");
    assert!(
        editor
            .last_message
            .as_ref()
            .is_some_and(|m| m.text.ends_with("[New]")),
        "vim's wording: {:?}",
        editor.last_message
    );

    // And the first write creates it.
    editor.do_write(None);
    assert!(path.exists(), ":w creates the file");
}

#[test]
fn content_seeds_a_new_file_and_the_cursor_lands_in_it() {
    let mut editor = boot();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("draft.org");

    let _ = editor.open_buffer_at(
        Some(path.clone()),
        Position::new(1, 2),
        false,
        Some("* TODO \n  body\n"),
        None,
    );

    assert_eq!(editor.active_text().as_string(), "* TODO \n  body\n");
    assert_eq!(editor.cursor, Position::new(1, 2));
    assert!(
        editor.document.dirty(),
        "seeded text is unsaved work — the dirty-buffer guard must protect it"
    );
    assert!(
        !path.exists(),
        "seeding writes nothing: abort before :w is clean"
    );
}

/// A saved draft reopened must keep what was typed into it.
#[test]
fn content_is_ignored_for_a_file_that_exists() {
    let mut editor = boot();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saved.org");
    std::fs::write(&path, "what I typed\n").unwrap();

    let _ = editor.open_buffer_at(
        Some(path.clone()),
        Position::ZERO,
        false,
        Some("the template again\n"),
        None,
    );

    assert_eq!(editor.active_text().as_string(), "what I typed\n");
    assert!(!editor.document.dirty());
}

/// Nor for a file already open in a buffer — even one never saved.
#[test]
fn content_is_ignored_for_a_buffer_already_open() {
    let mut editor = boot();
    let origin = editor.active_pane_buffer_id();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("open.org");
    let _ = editor.open_buffer_at(
        Some(path.clone()),
        Position::ZERO,
        false,
        Some("first\n"),
        None,
    );
    let first = editor.active_pane_buffer_id();
    // Leave, then come back with a different seed.
    let _ = editor.handle_effect(Effect::FocusBuffer(origin.0));
    assert_eq!(editor.active_pane_buffer_id(), origin);
    let _ = editor.open_buffer_at(Some(path), Position::ZERO, true, Some("second\n"), None);

    assert_eq!(editor.active_pane_buffer_id(), first, "the same buffer");
    assert_eq!(editor.active_text().as_string(), "first\n");
}

/// The minor is active by the time the call returns — before any frame, so
/// its chords work on the first keystroke.
#[test]
fn the_minor_is_active_before_anything_is_shown() {
    let mut editor = boot();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("with-minor.org");

    let _ = editor.open_buffer_at(Some(path), Position::ZERO, false, None, Some(MINOR));

    assert!(minor_is_active(&editor));
}

/// vim: `:e` under a directory that does not exist still opens a buffer, and
/// only the write fails (E212). The failure is reported, not a panic, and
/// the buffer keeps its text.
#[test]
fn writing_a_new_file_into_a_missing_directory_reports_an_error() {
    let mut editor = boot();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-yet").join("draft.org");

    let _ = editor.open_buffer_at(
        Some(path.clone()),
        Position::ZERO,
        false,
        Some("keep me\n"),
        None,
    );
    editor.do_write(None);

    assert!(!path.exists());
    assert_eq!(editor.active_text().as_string(), "keep me\n");
    assert_eq!(
        editor.last_message.as_ref().map(|m| m.level),
        Some(lattice_host::action::EchoLevel::Error),
        "{:?}",
        editor.last_message
    );
}
