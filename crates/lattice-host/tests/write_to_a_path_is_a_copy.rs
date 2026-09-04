//! `:w {file}` writes a copy. It does not rename the buffer.
//!
//! Design: `docs/dev/architecture/focused-surface.md` §4.
//!
//! ## What this fixes
//!
//! `do_write(Some(path))` called `save_as`, which sets `Document::path`. That
//! is vim's `:saveas`, not vim's `:w {file}`, and the difference is shaped
//! like data loss rather than like a wrong message:
//!
//! > You are editing `a.rs`. You type `:w /tmp/backup.rs` to snapshot it.
//! > Your buffer silently BECOMES `/tmp/backup.rs`. Every later `:w` writes
//! > there, and `a.rs` never sees another edit.
//!
//! ## The one deliberate divergence from vim
//!
//! An unnamed WRITABLE buffer adopts the path. Vim leaves it `[No Name]` and
//! makes the next bare `:w` fail, which is a wart people work around rather
//! than want — "UX is the higher court, within reason", and the divergence is
//! one condition wide.
//!
//! `writable` is the load-bearing half: a hover popup is unnamed too, and
//! letting it adopt would leave an ephemeral help buffer claiming to be the
//! user's file.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

/// A named buffer keeps its name, and its later `:w` still writes ITS file.
///
/// The second half is the point. A test that only checked the path field
/// would pass on an implementation that renamed the buffer and happened to
/// report the old name.
#[test]
fn writing_a_named_buffer_elsewhere_does_not_steal_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let mine = dir.path().join("a.rs");
    std::fs::write(&mine, "fn main() {}\n").unwrap();
    let backup = dir.path().join("backup.rs");

    let mut editor = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    editor.do_edit(Some(mine.clone()), false);
    editor.run_tick_pending();

    editor.do_write(Some(backup.clone()));
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        "fn main() {}\n",
        "the copy was written"
    );
    assert_eq!(
        editor
            .document
            .snapshot()
            .path
            .as_deref()
            .map(|p| p.to_path_buf()),
        Some(mine.clone()),
        "…and the buffer is still `a.rs` — `:w {{file}}` is a copy, not a rename"
    );

    // The half that makes it data loss: a later bare `:w` must reach the
    // file the user thinks they are editing.
    editor.do_write(None);
    let msg = editor.last_message.as_ref().map(|m| m.text.clone());
    assert_eq!(
        msg.as_deref(),
        Some(&format!("\"{}\" written", mine.display())[..]),
        "the bare `:w` wrote `a.rs`, not the backup"
    );
}

/// The deliberate divergence: an unnamed WRITABLE buffer adopts.
#[test]
fn an_unnamed_writable_buffer_adopts_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("new.rs");

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    assert!(
        editor.document.snapshot().path.is_none(),
        "the fixture starts unnamed, or this proves nothing"
    );

    editor.do_write(Some(out.clone()));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "scratch\n");
    assert_eq!(
        editor
            .document
            .snapshot()
            .path
            .as_deref()
            .map(|p| p.to_path_buf()),
        Some(out),
        "a new file keeps the name you just gave it — the one place this \
         diverges from vim, and on purpose"
    );
}

/// A read-only buffer does NOT adopt, even though it is unnamed.
///
/// This is the popup case. Without the `writable` half of the condition, a
/// hover popup — unnamed like any scratch buffer — would come out of
/// `:w notes.md` claiming to BE `notes.md`.
#[test]
fn a_read_only_unnamed_buffer_writes_a_copy_and_stays_anonymous() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("hover.md");

    let mut editor = Editor::boot(CoreDocument::from_text("the file\n"));
    let content = lattice_help::parse_help_lines(
        "hover",
        vec!["fn widget() -> u32".to_string(), "the docs".to_string()],
    );
    let _ =
        editor.open_floating_popup(content, lattice_host::popup::PopupPlacement::CursorAnchored);
    editor.focus_help_popup();

    editor.do_write(Some(out.clone()));
    let written = std::fs::read_to_string(&out).unwrap();
    assert!(
        written.contains("widget"),
        "the popup's content was exported: {written:?}"
    );
    assert!(
        editor.document.snapshot().path.is_none(),
        "…and the popup did not adopt the path — an ephemeral help buffer \
         must never come out of a `:w` claiming to be the user's file"
    );
}

/// A copy does not mark the buffer clean. `DocumentSaved` says "this document
/// reached disk", which is false of a copy — and a dirty buffer that looks
/// saved is how work gets lost at `:q`.
#[test]
fn writing_a_copy_leaves_the_buffer_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let mine = dir.path().join("a.rs");
    std::fs::write(&mine, "one\n").unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("one\n"));
    editor.do_edit(Some(mine.clone()), false);
    editor.run_tick_pending();

    // Make it dirty, then export a copy.
    let _ = editor.dispatch(lattice_host::action::Action::Insert("x".to_string()));
    assert!(editor.document.snapshot().dirty, "the fixture is dirty");

    editor.do_write(Some(dir.path().join("copy.rs")));
    assert!(
        editor.document.snapshot().dirty,
        "the buffer still has unsaved changes against ITS OWN file — a copy \
         is not a save"
    );
}
