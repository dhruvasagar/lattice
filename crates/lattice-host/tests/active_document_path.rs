//! OM.6b — the file behind the buffer a plugin action is reading.
//!
//! The action gate hands a grammar plugin a throwaway
//! `Document::from_buffer(active_text())`, and a throwaway document has no
//! path unless one is put there. That is what
//! [`Editor::active_document_path`] supplies, and what `document.path()`
//! answers from inside the guest.
//!
//! The end-to-end assertion (a real org guest deriving `<file>_archive`) lives
//! in the org plugin's own repo, because that is where the guest is. What
//! belongs here is the half the host owns: the path is the one the text came
//! from, and a buffer with no file says so rather than guessing.

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
    let dir = std::env::temp_dir().join(format!("lattice-om6b0-{tag}-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn an_open_file_reports_its_own_path() {
    let dir = tmp("open");
    let file = dir.join("notes.org");
    std::fs::write(&file, "* One\n").unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.do_edit(Some(file.clone()), false);

    assert_eq!(
        editor.active_document_path().map(|p| (*p).clone()),
        Some(file),
        "the path a plugin action derives `<file>_archive` from"
    );
}

/// A scratch buffer answers `None`. The guest is then the one that decides
/// what to do about it — inventing a path here would write somewhere the user
/// never named.
#[test]
fn a_buffer_with_no_file_reports_none() {
    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    assert!(editor.active_document_path().is_none());
}

/// The path must track the buffer the text comes from, not the one that was
/// open first — a guest handed the text of one file and the path of another
/// would file a subtree into a file it never read.
#[test]
fn switching_files_switches_the_path_with_the_text() {
    let dir = tmp("switch");
    let first = dir.join("first.org");
    let second = dir.join("second.org");
    std::fs::write(&first, "* First\n").unwrap();
    std::fs::write(&second, "* Second\n").unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.do_edit(Some(first), false);
    editor.do_edit(Some(second.clone()), false);

    assert_eq!(
        editor.active_document_path().map(|p| (*p).clone()),
        Some(second)
    );
    assert!(
        editor.active_text().as_string().contains("Second"),
        "sanity: the text moved too"
    );
}
