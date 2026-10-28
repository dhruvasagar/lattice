//! A lookup jump — `gd` and its LSP siblings, a picker accept, `:cnext`,
//! an editor-requested open — lands its target in view, centred (`zz`),
//! whenever the target was not already comfortably on screen.
//!
//! The bug these pin: the jump wrote the cursor and nothing scrolled. The
//! only scroll clamp runs on the keystroke path, and `gd` lands
//! asynchronously, so the cursor sat on the right line but off-screen
//! until the next keypress. Every test here asserts on the state the jump
//! itself leaves — no keystroke follows — because a test that presses a
//! key first passes on the broken version too.
//!
//! A target already on screen leaves the view alone: moving content the
//! user was reading, to show them something they could already see, is
//! the churn the keystroke UX contract rules out.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

const HEIGHT: u32 = 20;

fn numbered(lines: u32) -> String {
    (0..lines).map(|i| format!("line {i}\n")).collect()
}

/// An editor showing a 200-line file at the top, with a measured viewport.
fn boot_on_file(dir: &std::path::Path, name: &str) -> (Editor, std::path::PathBuf) {
    let path = dir.join(name);
    std::fs::write(&path, numbered(200)).unwrap();
    let mut e = Editor::boot(CoreDocument::from_text(""));
    e.do_edit(Some(path.clone()), false);
    e.viewport_height = HEIGHT;
    e.scroll = 0;
    (e, path)
}

fn lsp_location(path: &std::path::Path, line: u32) -> lattice_lsp::lsp_types::Location {
    let uri = lattice_lsp::actor::uri_from_path(path);
    let pos = lattice_lsp::lsp_types::Position::new(line, 0);
    lattice_lsp::lsp_types::Location::new(uri, lattice_lsp::lsp_types::Range::new(pos, pos))
}

/// `zz` puts `line` half a window down: `HEIGHT / 2` lines above it.
fn centred_scroll(line: u32) -> u32 {
    line - HEIGHT / 2
}

#[test]
fn gd_to_an_off_screen_line_centres_it_without_a_keypress() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, path) = boot_on_file(dir.path(), "a.txt");

    // Exactly what `drain_pending_definitions` calls when the server's
    // single answer lands off-keystroke.
    let _ = e.jump_to_lsp_location(&lsp_location(&path, 150));

    assert_eq!(e.cursor.line, 150);
    assert_eq!(e.scroll, centred_scroll(150), "target centred on landing");
}

#[test]
fn gd_to_another_file_centres_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, _) = boot_on_file(dir.path(), "a.txt");
    let other = dir.path().join("b.txt");
    std::fs::write(&other, numbered(200)).unwrap();

    let _ = e.jump_to_lsp_location(&lsp_location(&other, 120));

    assert_eq!(e.document.path().as_deref(), Some(other.as_path()));
    assert_eq!(e.cursor.line, 120);
    assert_eq!(e.scroll, centred_scroll(120));
}

#[test]
fn gd_to_a_line_already_on_screen_leaves_the_view_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, path) = boot_on_file(dir.path(), "a.txt");

    let _ = e.jump_to_lsp_location(&lsp_location(&path, 8));

    assert_eq!(e.cursor.line, 8);
    assert_eq!(e.scroll, 0, "an on-screen target does not move the view");
}

/// Picker accepts, grep hits, compile jumps and `:cnext` all land through
/// `jump_to_file_line_col`.
#[test]
fn a_file_location_jump_centres_an_off_screen_target() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, _) = boot_on_file(dir.path(), "a.txt");
    let other = dir.path().join("b.txt");
    std::fs::write(&other, numbered(200)).unwrap();

    let _ = e.apply_picker_outcome(lattice_picker::PickerAcceptOutcome::JumpToLocation {
        path: other.clone(),
        line: 90,
        col: 0,
    });

    assert_eq!(e.cursor.line, 90);
    assert_eq!(e.scroll, centred_scroll(90));
}

#[test]
fn a_picker_jump_within_the_buffer_centres_an_off_screen_target() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, _) = boot_on_file(dir.path(), "a.txt");
    let id = e.active_pane_buffer_id();

    let _ = e.apply_picker_outcome(lattice_picker::PickerAcceptOutcome::JumpInBuffer {
        buffer_id: id.0,
        line: 170,
        col: 0,
    });

    assert_eq!(e.cursor.line, 170);
    assert_eq!(e.scroll, centred_scroll(170));
}

/// `Effect::OpenBufferAtColumn` — an LSP `window/showDocument` or an IDE
/// peer asking the editor to show a place.
#[test]
fn an_open_at_column_request_centres_an_off_screen_target() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, _) = boot_on_file(dir.path(), "a.txt");
    let other = dir.path().join("b.txt");
    std::fs::write(&other, numbered(200)).unwrap();

    let _ = e.handle_effect(lattice_grammar::Effect::OpenBufferAtColumn {
        path: Some(other),
        column: Some(lattice_grammar::Utf16Pos { line: 60, col: 0 }),
        force: false,
    });

    assert_eq!(e.cursor.line, 60);
    assert_eq!(e.scroll, centred_scroll(60));
}
