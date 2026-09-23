//! `:w <path>` resolves the path the user typed.
//!
//! `do_write` handed its argument to the filesystem verbatim: no `~`
//! expansion, and a relative path resolved against the PROCESS cwd rather
//! than the editor's working directory. `:e` has gone through
//! `normalize_user_path_with_cwd` for a long time, so `:e` and `:w` disagreed
//! about what the same typed path meant.
//!
//! The failure worth fearing is not the error. `:w ~/notes/x.org` usually
//! fails outright, which is at least visible — but if a directory literally
//! named `~` sits beside you, it SUCCEEDS into the wrong place, reports
//! `"~/notes/x.org" written`, and the file the user meant never changes.
//!
//! `~` expansion itself is `lattice_core::home`'s, tested there and in
//! `normalize_user_path`'s own tests. What these pin is that `:w` goes
//! through that resolution at all — the wire, not the helper.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

/// A relative `:w` target joins the EDITOR's working directory, the one `:cd`
/// moved, not whatever directory the process happens to have been launched in.
#[test]
fn a_relative_write_target_joins_the_editors_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = Editor::boot(CoreDocument::from_text("hello\n"));
    editor.current_dir = Some(dir.path().to_path_buf());

    editor.do_write(Some(std::path::PathBuf::from("out.txt")));

    assert_eq!(
        std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
        "hello\n",
        "a relative `:w` must land under the editor's cwd, as `:e` already did"
    );
}

/// The echo names where the bytes actually went. Reporting the typed path
/// back is how a write into the wrong place reads like a write into the right
/// one.
#[test]
fn the_echo_names_the_resolved_path_not_the_typed_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = Editor::boot(CoreDocument::from_text("hello\n"));
    editor.current_dir = Some(dir.path().to_path_buf());

    editor.do_write(Some(std::path::PathBuf::from("out.txt")));

    let message = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        message.contains(&dir.path().join("out.txt").display().to_string()),
        "expected the resolved path in {message:?}"
    );
}

/// An absolute target is untouched — the resolution must not "helpfully"
/// re-root a path that already says where it goes.
#[test]
fn an_absolute_write_target_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let target = elsewhere.path().join("out.txt");
    let mut editor = Editor::boot(CoreDocument::from_text("hello\n"));
    editor.current_dir = Some(dir.path().to_path_buf());

    editor.do_write(Some(target.clone()));

    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello\n");
}
