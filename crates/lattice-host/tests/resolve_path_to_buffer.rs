//! XF.2 — path → buffer, the one primitive cross-file writes needed.
//!
//! `Effect::WriteToFile` names a file. Everything downstream addresses a
//! `BufferId`. This is the step between, and its contract has three parts
//! that each have a way of going quietly wrong:
//!
//! - **Reuse an already-open buffer.** Not an optimisation — opening a second
//!   buffer over a file with unsaved changes and editing the copy loses the
//!   user's work with nothing to show for it.
//! - **Do not steal focus.** The user archived a subtree; they did not ask to
//!   navigate.
//! - **Create a missing file, refuse a missing directory.** Capture's first
//!   run creates its file; a typo'd path should not silently build a tree.
//!
//! Design: `cross-file-writes.md` §7 / §8.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn tmp(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lattice-xf2-{tag}-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

fn text_of(editor: &Editor, id: lattice_core::BufferId) -> String {
    editor
        .buffers
        .document_handle(id)
        .expect("the buffer is registered")
        .snapshot()
        .text()
        .to_string()
}

#[test]
fn an_unopened_file_is_opened_with_its_contents() {
    let dir = tmp("open");
    let file = dir.join("notes.org");
    std::fs::write(&file, "* One\n* Two\n").unwrap();

    let mut editor = boot();
    let id = editor.resolve_path_to_buffer(&file).expect("opens");

    assert_eq!(text_of(&editor, id), "* One\n* Two\n");
}

/// The headline correctness case. A second buffer over the same file would
/// mean the user's unsaved work is invisible to the write, and whichever of
/// the two saves last silently wins.
#[test]
fn an_already_open_buffer_is_reused_with_its_unsaved_changes() {
    let dir = tmp("reuse");
    let file = dir.join("notes.org");
    std::fs::write(&file, "on disk\n").unwrap();

    let mut editor = boot();
    editor.do_edit(Some(file.clone()), false);
    let opened = editor.document_buffer_id;

    // An unsaved edit — the thing that would be lost.
    editor
        .apply_edit_blocking(lattice_protocol::edit::Edit::insert(
            lattice_protocol::position::Position::new(0, 0),
            "UNSAVED ",
        ))
        .expect("edit applies");
    assert_eq!(text_of(&editor, opened), "UNSAVED on disk\n");

    let resolved = editor.resolve_path_to_buffer(&file).expect("resolves");

    assert_eq!(
        resolved, opened,
        "the same file must resolve to the buffer already showing it"
    );
    assert_eq!(
        text_of(&editor, resolved),
        "UNSAVED on disk\n",
        "…and to its unsaved state, not a re-read from disk — re-reading is \
         how the user's work disappears"
    );
}

/// The same file named differently is still the same file, and this is the
/// reuse check's real test.
///
/// `Path` equality is component-wise, so a `.` segment collapses for free —
/// but **`..` does not**: `Path::new("/a/c/../b") == Path::new("/a/b")` is
/// `false`. A comparison that only got the free case would open a second
/// buffer over an already-open file whenever a producer's path happened to
/// contain `..`, and the user's unsaved work in the first one would be
/// invisible to the write.
#[test]
fn a_path_spelled_differently_still_finds_the_open_buffer() {
    let dir = tmp("normalise");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let file = dir.join("notes.org");
    std::fs::write(&file, "body\n").unwrap();

    let mut editor = boot();
    let first = editor.resolve_path_to_buffer(&file).expect("opens");

    // The free case.
    let dotted = dir.join(".").join("notes.org");
    assert_eq!(
        editor.resolve_path_to_buffer(&dotted).expect("resolves"),
        first,
        "`dir/./notes.org` is `dir/notes.org`"
    );

    // The case `Path` equality does NOT give you.
    let updotted = dir.join("sub").join("..").join("notes.org");
    assert_eq!(
        editor.resolve_path_to_buffer(&updotted).expect("resolves"),
        first,
        "`dir/sub/../notes.org` is `dir/notes.org` too — two buffers here \
         would each hold half the user's edits, and the last save would win"
    );
}

/// The same, from the other direction: a buffer opened through an awkward
/// path is still found by its plain one. The canonicalisation has to happen
/// on BOTH sides, not just the incoming target.
#[test]
fn a_buffer_opened_by_an_awkward_path_is_found_by_its_plain_one() {
    let dir = tmp("awkward-first");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let file = dir.join("notes.org");
    std::fs::write(&file, "body\n").unwrap();

    let mut editor = boot();
    let first = editor
        .resolve_path_to_buffer(&dir.join("sub").join("..").join("notes.org"))
        .expect("opens");
    let second = editor.resolve_path_to_buffer(&file).expect("resolves");

    assert_eq!(first, second);
}

/// A plugin's write must not move the user. They pressed a key to file a
/// subtree somewhere, not to go there.
#[test]
fn opening_a_target_does_not_steal_focus() {
    let dir = tmp("focus");
    let file = dir.join("archive.org");
    std::fs::write(&file, "archived\n").unwrap();

    let mut editor = boot();
    let before = editor.active_pane_buffer_id();

    let opened = editor.resolve_path_to_buffer(&file).expect("opens");

    assert_ne!(opened, before, "a new buffer really was created");
    assert_eq!(
        editor.active_pane_buffer_id(),
        before,
        "and the active pane did not move to it"
    );
}

/// It lands in the registry as an ordinary listed document, so `:ls` shows it
/// and `:w` can save it. A hidden buffer would mean a plugin mutating
/// something the user has no way to see or write.
#[test]
fn the_target_is_an_ordinary_listed_buffer() {
    let dir = tmp("listed");
    let file = dir.join("archive.org");
    std::fs::write(&file, "archived\n").unwrap();

    let mut editor = boot();
    let id = editor.resolve_path_to_buffer(&file).expect("opens");

    let mut listed = Vec::new();
    editor.buffers.for_each(|e| {
        if e.flags.listed {
            listed.push(e.id);
        }
    });
    assert!(
        listed.contains(&id),
        "the write target must be visible in `:ls`"
    );
    assert_eq!(
        editor.find_document_by_path(&file),
        Some(id),
        "…and findable by its path, or the next write opens a second one"
    );
}

/// Capture's first run: the file does not exist yet and creating it is the
/// point.
#[test]
fn a_missing_file_opens_empty_rather_than_failing() {
    let dir = tmp("create");
    let file = dir.join("capture.org");
    assert!(!file.exists());

    let mut editor = boot();
    let id = editor.resolve_path_to_buffer(&file).expect("creates");

    assert_eq!(text_of(&editor, id), "");
    assert_eq!(
        editor
            .buffers
            .document_handle(id)
            .unwrap()
            .snapshot()
            .path()
            .map(|p| p.to_path_buf()),
        Some(file),
        "the path is set, so `:w` knows where it goes"
    );
}

/// A missing PARENT is refused. Creating directories is a larger authority
/// than creating a file, and a typo'd capture target should not silently
/// build a tree nobody asked for.
#[test]
fn a_missing_parent_directory_is_an_error_not_a_mkdir() {
    let dir = tmp("noparent");
    let file = dir.join("nope").join("deeper").join("capture.org");

    let mut editor = boot();
    let err = editor.resolve_path_to_buffer(&file).expect_err("refuses");

    assert!(err.contains("no such directory"), "got {err}");
    assert!(
        !dir.join("nope").exists(),
        "and it did not create the tree on the way to failing"
    );
}

/// A directory is not a file. Without this the open would either fail
/// obscurely deep in the read or produce an empty buffer whose `:w` would
/// then try to write over a directory.
#[test]
fn a_directory_target_is_refused() {
    let dir = tmp("isdir");
    let mut editor = boot();

    let err = editor.resolve_path_to_buffer(&dir).expect_err("refuses");
    assert!(err.contains("is a directory"), "got {err}");
}

/// Resolving twice is idempotent — the second call must not create a second
/// buffer for a file the first one opened.
#[test]
fn resolving_the_same_path_twice_yields_one_buffer() {
    let dir = tmp("twice");
    let file = dir.join("notes.org");
    std::fs::write(&file, "body\n").unwrap();

    let mut editor = boot();
    let a = editor.resolve_path_to_buffer(&file).expect("first");
    let b = editor.resolve_path_to_buffer(&file).expect("second");
    assert_eq!(a, b);

    let mut count = 0;
    editor.buffers.for_each(|e| {
        if e.id == a {
            count += 1;
        }
    });
    assert_eq!(count, 1);
}
