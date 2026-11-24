//! `:e diagram.png` opens the picture, and cannot destroy it.
//!
//! Before `image-mode`, this failed at the very first step: `Document::open`
//! is `read_to_string`, so a PNG never became a buffer at all — the media
//! substrate that draws org's inline images was unreachable for the file
//! itself.
//!
//! `image-mode` PRESENTS its extensions, so the open path builds a buffer
//! with the real path and no content, and the mode's own media producer
//! yields one block at line 0.
//!
//! ## Why the write test is the important one
//!
//! The buffer's text is a placeholder, not the file. Saving it would replace
//! the image with one empty line — a data-loss shape, not a wrong-message
//! shape. Three gates stand in the way and they cover different paths:
//! `ReadOnly` gates insert-mode typing, `read-only-mode` refuses the
//! operators, and `do_write` refuses the save. A save goes through neither of
//! the first two, which is exactly why the third exists.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;

/// Press a literal key sequence, the way the user would.
fn press(editor: &mut Editor, keys: &str) {
    let mut partial = Vec::new();
    for c in keys.chars() {
        let _ = editor.dispatch_chord(KeyChord::char(c), &mut partial);
    }
}

fn write_png(dir: &std::path::Path, name: &str, w: u32, h: u32) -> std::path::PathBuf {
    let path = dir.join(name);
    image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]))
        .save(&path)
        .expect("write png");
    path
}

/// The bytes are never read as text, so the open succeeds where
/// `read_to_string` would have failed.
#[test]
fn opening_an_image_file_succeeds_and_does_not_load_its_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "shot.png", 40, 20);
    let raw = std::fs::read(&png).unwrap();
    assert!(
        String::from_utf8(raw).is_err(),
        "precondition: this PNG is not valid UTF-8, so `read_to_string` fails on it"
    );

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.do_edit(Some(png.clone()), false);
    editor.run_tick_pending();

    assert_eq!(
        editor
            .document
            .snapshot()
            .path
            .as_deref()
            .map(|p| p.to_path_buf()),
        Some(png),
        "the buffer is the real file, listed and named like any other"
    );
    assert!(
        editor.document.text().trim().is_empty(),
        "the buffer holds a placeholder, not the file's bytes"
    );
}

/// `:e shot.png` lands in `image-mode`, not `text-mode`.
#[test]
fn an_image_file_resolves_to_image_mode() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "shot.png", 40, 20);

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.do_edit(Some(png), false);
    editor.run_tick_pending();

    let buffer = editor.document_buffer_id;
    assert_eq!(
        editor.active_modes.get(&buffer).and_then(|a| a.major()),
        Some(lattice_mode::modes::ImageMode::mode_id()),
        "an image file's major is image-mode"
    );
}

/// The one that matters: emptying the buffer and saving must not truncate the
/// image. `dd` is refused by `read-only-mode`; `:w` is refused by `do_write`.
/// Either gate alone would leave the file at risk through the other path.
#[test]
fn deleting_the_line_and_saving_cannot_truncate_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "shot.png", 40, 20);
    let before = std::fs::read(&png).unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.do_edit(Some(png.clone()), false);
    editor.run_tick_pending();

    let text_before = editor.document.text();
    press(&mut editor, "dd");
    editor.run_tick_pending();
    assert_eq!(
        editor.document.text(),
        text_before,
        "`read-only-mode`'s invocation runner must refuse the operator"
    );
    editor.do_write(None);

    assert_eq!(
        std::fs::read(&png).unwrap(),
        before,
        "the image on disk must be byte-identical after `dd` + `:w`"
    );
}

/// And the refusal says so, rather than reporting a successful write of
/// nothing.
#[test]
fn a_bare_write_on_a_read_only_buffer_is_refused_out_loud() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "shot.png", 40, 20);

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.do_edit(Some(png), false);
    editor.run_tick_pending();
    editor.do_write(None);

    let message = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        message.contains("read-only"),
        "expected a read-only refusal, got {message:?}"
    );
}

/// PF.5: a presented file's reload keeps the text its major wrote, and the
/// cursor on it.
///
/// A reload reopens the file into a fresh buffer, and a presented file
/// opens as one empty line that its major fills in later. A PDF's page labels
/// are read off the actor, so restoring the cursor against that one line put
/// every reload of a long PDF on page 1. The fresh buffer starts with the
/// old text instead. A picture shows this without the race: `image-mode`
/// writes no text of its own, so what the buffer holds after the reload is
/// exactly what the reload carried over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_carries_the_majors_text_and_the_cursor_over() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "diagram.png", 4, 4);
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.do_edit(Some(png.clone()), false);
    editor.run_tick_pending();
    let id = editor.document_buffer_id;

    // What a major writes as the buffer's owner: an edit, then clean.
    let handle = editor.buffers.document_handle(id).unwrap();
    handle
        .apply_edit_batch(vec![lattice_protocol::edit::Edit::insert(
            lattice_protocol::position::Position::ZERO,
            "one\ntwo\nthree".to_string(),
        )])
        .await
        .unwrap();
    handle.mark_clean().await.unwrap();
    editor.run_tick_pending();
    editor.cursor.line = 2;

    editor.do_edit(None, true);
    let fresh = editor.document_buffer_id;
    assert_ne!(fresh, id, "a reload is a fresh buffer");
    let text = editor
        .buffers
        .document_handle(fresh)
        .map(|h| h.text())
        .unwrap();
    assert_eq!(text, "one\ntwo\nthree", "the text came over");
    assert_eq!(editor.cursor.line, 2, "and the cursor stayed on it");
    assert!(
        !editor.buffers.document_dirty(fresh),
        "carried text is the owner's, not an edit"
    );
}
