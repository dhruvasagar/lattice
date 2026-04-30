//! Phase 0 exit criterion (per DESIGN.md §13):
//! "programmatic edit roundtrip"
//!
//! Open a document, apply edits, undo / redo across the history, save, reopen,
//! verify content survives. Exercises the Buffer + Document + UndoStack + file
//! I/O pipeline end-to-end.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};

#[test]
fn open_edit_save_reopen_preserves_content() {
    let dir = unique_tempdir();
    let path = dir.join("file.txt");
    std::fs::write(&path, "Hello, world!").unwrap();

    // 1. Open.
    let mut doc = Document::open(&path).expect("open");
    assert_eq!(doc.text(), "Hello, world!");
    assert!(!doc.dirty());

    // 2. Edit: replace "world" with "lattice".
    let world = Range::new(Position::new(0, 7), Position::new(0, 12));
    doc.apply_edit(Edit::replace(world, "lattice")).unwrap();
    assert_eq!(doc.text(), "Hello, lattice!");
    assert!(doc.dirty());

    // 3. Save.
    doc.save().unwrap();
    assert!(!doc.dirty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Hello, lattice!");

    // 4. Reopen and verify.
    drop(doc);
    let reloaded = Document::open(&path).unwrap();
    assert_eq!(reloaded.text(), "Hello, lattice!");

    cleanup(&dir);
}

#[test]
fn full_undo_redo_walk_preserves_intermediate_state() {
    let mut doc = Document::from_text("");

    // Apply a sequence and capture the text after each step.
    let snapshots = ["a", "ab", "abc", "abcd"];
    for ch in ['a', 'b', 'c', 'd'].iter() {
        let line_len = doc.text().len() as u32;
        doc.apply_edit(Edit::insert(Position::new(0, line_len), ch.to_string()))
            .unwrap();
    }
    assert_eq!(doc.text(), "abcd");

    // Undo all the way back, checking each prior state.
    for expected in snapshots.iter().rev().skip(1) {
        doc.undo().unwrap();
        assert_eq!(doc.text(), *expected);
    }
    doc.undo().unwrap();
    assert_eq!(doc.text(), "");
    assert!(doc.undo().is_err()); // exhausted

    // Redo all the way forward.
    for expected in snapshots.iter() {
        doc.redo().unwrap();
        assert_eq!(doc.text(), *expected);
    }
    assert!(doc.redo().is_err()); // exhausted
}

#[test]
fn batch_edit_is_one_undo_unit_and_round_trips() {
    let mut doc = Document::from_text("plain text");
    let r = Range::new(Position::new(0, 0), Position::new(0, 5));
    doc.apply_edit_batch(vec![
        Edit::replace(r, "fancy"),
        Edit::insert(Position::new(0, 5), "!"),
    ])
    .unwrap();
    assert_eq!(doc.text(), "fancy! text");

    // One undo reverts the entire batch.
    doc.undo().unwrap();
    assert_eq!(doc.text(), "plain text");

    // One redo replays the entire batch.
    doc.redo().unwrap();
    assert_eq!(doc.text(), "fancy! text");
}

#[test]
fn save_as_then_reopen_via_new_path() {
    let dir = unique_tempdir();
    let path = dir.join("output.txt");
    let mut doc = Document::from_text("created in memory");
    doc.save_as(&path).unwrap();

    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.text(), "created in memory");
    cleanup(&dir);
}

fn unique_tempdir() -> std::path::PathBuf {
    let base = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = base.join(format!("lattice-roundtrip-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}
