//! XF.3 — `Effect::WriteToFile` end to end, natively.
//!
//! The effect archive, refile and capture were blocked on: move text into a
//! file the editor may not have open, and optionally remove it from where it
//! came from.
//!
//! The tests that matter here are the failure ones. `cut` exists inside this
//! effect rather than beside it because as two effects the outcomes are "the
//! subtree exists twice" and "the subtree is **gone**" — and an effect cannot
//! report failure (XF.0), so two ordered effects could not have been made to
//! depend on each other. `a_failed_insert_leaves_the_source_untouched` is the
//! assertion that whole design decision reduces to.
//!
//! Design: `cross-file-writes.md` §5 / §8.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use lattice_core::Document as CoreDocument;
use lattice_grammar::{Effect, FileAnchor};
use lattice_host::editor::Editor;
use lattice_protocol::position::{Position, Range};

fn tmp(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lattice-xf3-{tag}-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn boot(text: &str) -> Editor {
    Editor::boot(CoreDocument::from_text(text))
}

fn target_text(editor: &Editor, path: &std::path::Path) -> String {
    let id = editor
        .find_document_by_path(path)
        .expect("the target was opened");
    editor
        .buffers
        .document_handle(id)
        .unwrap()
        .snapshot()
        .text()
        .to_string()
}

fn source_text(editor: &Editor) -> String {
    editor.document.snapshot().text().to_string()
}

/// Append into a file the editor had never opened — archive's shape.
#[test]
fn text_lands_at_the_end_of_an_unopened_file() {
    let dir = tmp("append");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "* Old\n").unwrap();

    let mut editor = boot("* Keep\n");
    editor.handle_effect(Effect::WriteToFile {
        path: archive.clone(),
        anchor: FileAnchor::End,
        text: "* Archived\n".to_string(),
        cut: None,
        create_parents: false,
    });

    assert_eq!(target_text(&editor, &archive), "* Old\n* Archived\n");
    assert_eq!(source_text(&editor), "* Keep\n", "no cut, no change here");
}

#[test]
fn start_puts_the_text_first() {
    let dir = tmp("start");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "* Old\n").unwrap();

    let mut editor = boot("x\n");
    editor.handle_effect(Effect::WriteToFile {
        path: archive.clone(),
        anchor: FileAnchor::Start,
        text: "* First\n".to_string(),
        cut: None,
        create_parents: false,
    });

    assert_eq!(target_text(&editor, &archive), "* First\n* Old\n");
}

/// Capture's first run: the file does not exist yet.
#[test]
fn a_nonexistent_target_is_created_and_written() {
    let dir = tmp("capture");
    let capture = dir.join("capture.org");
    assert!(!capture.exists());

    let mut editor = boot("x\n");
    editor.handle_effect(Effect::WriteToFile {
        path: capture.clone(),
        anchor: FileAnchor::End,
        text: "* Captured\n".to_string(),
        cut: None,
        create_parents: false,
    });

    assert_eq!(target_text(&editor, &capture), "* Captured\n");
}

/// A target whose last line has no trailing newline. "After the last line"
/// has to mean a NEW line, or the append splices onto the existing one —
/// `"notes"` + `"* Archived\n"` would become `"notes* Archived"`, quietly
/// corrupting a line the user did not touch.
///
/// The producer cannot prevent this: it has never read the target, so it
/// cannot know whether a separator is needed. The host supplies it.
#[test]
fn appending_to_a_file_without_a_trailing_newline_starts_a_new_line() {
    let dir = tmp("no-trailing-nl");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "notes").unwrap();

    let mut editor = boot("x\n");
    editor.handle_effect(Effect::WriteToFile {
        path: archive.clone(),
        anchor: FileAnchor::End,
        text: "* Archived\n".to_string(),
        cut: None,
        create_parents: false,
    });

    assert_eq!(
        target_text(&editor, &archive),
        "notes\n* Archived\n",
        "the existing last line survives intact"
    );
}

/// …and a file that DOES end with a newline gets no extra one, or every
/// append would grow a blank line.
#[test]
fn appending_to_a_file_with_a_trailing_newline_adds_no_blank_line() {
    let dir = tmp("trailing-nl");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "notes\n").unwrap();

    let mut editor = boot("x\n");
    for _ in 0..2 {
        editor.handle_effect(Effect::WriteToFile {
            path: archive.clone(),
            anchor: FileAnchor::End,
            text: "* Archived\n".to_string(),
            cut: None,
            create_parents: false,
        });
    }

    assert_eq!(
        target_text(&editor, &archive),
        "notes\n* Archived\n* Archived\n",
        "repeated appends must not accumulate blank lines between entries"
    );
}

/// `Line(n)` past the end is the same append, not a failure — the clamp XF.1
/// pinned, seen through the applier.
#[test]
fn a_line_anchor_past_the_end_appends() {
    let dir = tmp("clamp");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "one\ntwo\n").unwrap();

    let mut editor = boot("x\n");
    editor.handle_effect(Effect::WriteToFile {
        path: archive.clone(),
        anchor: FileAnchor::Line(99),
        text: "three\n".to_string(),
        cut: None,
        create_parents: false,
    });

    assert_eq!(target_text(&editor, &archive), "one\ntwo\nthree\n");
}

/// The move: text appears in the target AND leaves the source, in one effect.
#[test]
fn a_cut_moves_the_text_rather_than_copying_it() {
    let dir = tmp("move");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "").unwrap();

    let mut editor = boot("* Keep\n* Move me\n* Also keep\n");
    editor.handle_effect(Effect::WriteToFile {
        path: archive.clone(),
        anchor: FileAnchor::End,
        text: "* Move me\n".to_string(),
        cut: Some(Range::new(Position::new(1, 0), Position::new(2, 0))),
        create_parents: false,
    });

    assert_eq!(target_text(&editor, &archive), "* Move me\n");
    assert_eq!(
        source_text(&editor),
        "* Keep\n* Also keep\n",
        "and it is gone from where it came from"
    );
}

/// **The assertion the one-effect design reduces to.**
///
/// The target cannot be written (its parent directory does not exist), so the
/// insert fails. The cut must not run. As two ordered effects it would have,
/// and the user's text would be gone with nowhere to have gone to.
#[test]
fn a_failed_insert_leaves_the_source_untouched() {
    let dir = tmp("failed-insert");
    let unwritable = dir.join("nope").join("deeper").join("archive.org");

    let mut editor = boot("* Keep\n* Move me\n* Also keep\n");
    let before = source_text(&editor);

    editor.handle_effect(Effect::WriteToFile {
        path: unwritable,
        anchor: FileAnchor::End,
        text: "* Move me\n".to_string(),
        cut: Some(Range::new(Position::new(1, 0), Position::new(2, 0))),
        create_parents: false,
    });

    assert_eq!(
        source_text(&editor),
        before,
        "the insert could not land, so the cut must not have run — this is \
         the difference between an error and losing the user's text"
    );
}

/// A directory target fails the same way, and the source survives the same
/// way. A second shape of the same guarantee, because the first one's failure
/// happens in a different branch.
#[test]
fn a_directory_target_also_leaves_the_source_untouched() {
    let dir = tmp("dir-target");

    let mut editor = boot("* Keep\n* Move me\n");
    let before = source_text(&editor);

    editor.handle_effect(Effect::WriteToFile {
        path: dir,
        anchor: FileAnchor::End,
        text: "* Move me\n".to_string(),
        cut: Some(Range::new(Position::new(1, 0), Position::new(2, 0))),
        create_parents: false,
    });

    assert_eq!(source_text(&editor), before);
}

/// An already-open target is written in place — the reuse XF.2 guarantees,
/// seen from this side. If a second buffer were opened, the user's unsaved
/// edit below would be invisible to the write and lost on the next save.
#[test]
fn an_already_open_target_is_written_in_place() {
    let dir = tmp("open-target");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "on disk\n").unwrap();

    let mut editor = boot("x\n");
    // Open it, edit it, and go back to the source buffer.
    editor.do_edit(Some(archive.clone()), false);
    let opened = editor.document_buffer_id;
    editor
        .apply_edit_blocking(lattice_protocol::edit::Edit::insert(
            Position::new(0, 0),
            "UNSAVED ",
        ))
        .unwrap();
    let scratch = editor.buffers.document_ids_sorted()[0];
    let _ = editor.activate_buffer(scratch);

    editor.handle_effect(Effect::WriteToFile {
        path: archive.clone(),
        anchor: FileAnchor::End,
        text: "appended\n".to_string(),
        cut: None,
        create_parents: false,
    });

    let after = editor
        .buffers
        .document_handle(opened)
        .unwrap()
        .snapshot()
        .text()
        .to_string();
    assert_eq!(
        after, "UNSAVED on disk\nappended\n",
        "written into the buffer the user already had open, unsaved edit and \
         all — not a fresh read from disk"
    );
}

/// The target is left MODIFIED, not saved. Emacs's `org-refile` and
/// `org-archive-subtree` both do; a plugin that silently writes files is a
/// larger authority than one that edits buffers.
#[test]
fn the_target_is_left_unsaved_so_the_disk_is_untouched() {
    let dir = tmp("unsaved");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "* Old\n").unwrap();

    let mut editor = boot("x\n");
    editor.handle_effect(Effect::WriteToFile {
        path: archive.clone(),
        anchor: FileAnchor::End,
        text: "* Archived\n".to_string(),
        cut: None,
        create_parents: false,
    });

    assert_eq!(
        std::fs::read_to_string(&archive).unwrap(),
        "* Old\n",
        "the buffer changed; the file on disk did not until the user writes it"
    );
    assert_eq!(target_text(&editor, &archive), "* Old\n* Archived\n");
}

/// Each buffer undoes its own half. Making the move one undo step would need
/// a cross-buffer undo group, which the undo model does not have and should
/// not grow for this (design §5).
#[test]
fn each_buffer_undoes_its_own_half_of_the_move() {
    let dir = tmp("undo");
    let archive = dir.join("archive.org");
    std::fs::write(&archive, "").unwrap();

    let mut editor = boot("* Keep\n* Move me\n");
    editor.handle_effect(Effect::WriteToFile {
        path: archive.clone(),
        anchor: FileAnchor::End,
        text: "* Move me\n".to_string(),
        cut: Some(Range::new(Position::new(1, 0), Position::new(2, 0))),
        create_parents: false,
    });
    assert_eq!(source_text(&editor), "* Keep\n");

    // `u` in the source restores the cut half, and only that half.
    editor.undo_blocking().expect("undo");
    assert_eq!(source_text(&editor), "* Keep\n* Move me\n");
    assert_eq!(
        target_text(&editor, &archive),
        "* Move me\n",
        "the target keeps its insert — the two edits are separately undoable"
    );
}

/// OR.10 — a missing parent directory is refused by default.
///
/// The refusal exists because creating directories is a larger authority than
/// creating a file, and a typo'd path must not silently build a tree. This is
/// the default half of that rule.
#[test]
fn a_missing_parent_directory_is_refused_by_default() {
    let dir = tmp("no-parent");
    let target = dir.join("nested").join("notes.org");

    let mut editor = boot("* Keep\n");
    editor.handle_effect(Effect::WriteToFile {
        path: target.clone(),
        anchor: FileAnchor::End,
        text: "* Filed\n".to_string(),
        cut: None,
        create_parents: false,
    });

    assert!(
        !target.parent().unwrap().exists(),
        "no tree was built from a path nobody vouched for"
    );
    let msg = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("no such directory"),
        "and the user is told which directory is missing: {msg:?}"
    );
}

/// …and `create_parents` is the producer opting out of it.
///
/// org-roam's `daily/YYYY-MM-DD.org` is the case: the folder is named by an
/// option with a default, no user ever types it, and without this the feature's
/// FIRST use is the one that fails.
#[test]
fn create_parents_makes_the_directory_the_producer_owns() {
    let dir = tmp("create-parent");
    let target = dir.join("daily").join("2026-08-30.org");

    let mut editor = boot("* Keep\n");
    editor.handle_effect(Effect::WriteToFile {
        path: target.clone(),
        anchor: FileAnchor::End,
        text: "#+title: 2026-08-30\n".to_string(),
        cut: None,
        create_parents: true,
    });

    assert!(
        target.parent().unwrap().is_dir(),
        "the directory was created, so the draft can be saved to it later"
    );
    assert_eq!(target_text(&editor, &target), "#+title: 2026-08-30\n");
}

/// More than one missing level, which is what `mkdir -p` means and what a
/// single `create_dir` would not do.
#[test]
fn create_parents_builds_every_missing_level() {
    let dir = tmp("create-deep");
    let target = dir.join("a").join("b").join("c").join("notes.org");

    let mut editor = boot("* Keep\n");
    editor.handle_effect(Effect::WriteToFile {
        path: target.clone(),
        anchor: FileAnchor::End,
        text: "* Filed\n".to_string(),
        cut: None,
        create_parents: true,
    });

    assert_eq!(target_text(&editor, &target), "* Filed\n");
}
