//! OC.10 — deleting the buffer you were typing in leaves Insert mode.
//!
//! `Editor::modal` is ONE field, not per-buffer. So a chord that closes the
//! active buffer while the editor is in Insert used to leave the state machine
//! in Insert, and the successor buffer then received the user's keystrokes as
//! literal text — in a buffer they never chose to edit.
//!
//! Unreachable until a mode bound a buffer-closing chord in Insert. org-capture
//! now does (`<C-c><C-c>` files the entry and dismisses the buffer, and a
//! capture buffer is one you arrive in already typing), which is what surfaced
//! it. The fix is here rather than in org because nothing about it is
//! org-shaped: any mode that binds such a chord hits it, and requiring each one
//! to remember an `EnterMode` first is exactly the kind of rule that gets
//! forgotten silently.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::ModalState;
use lattice_host::editor::Editor;

fn boot_with_two_buffers() -> Editor {
    let mut e = Editor::boot(CoreDocument::from_text("first\n"));
    // A second listed buffer, so `:bd` has a successor and is not refused by
    // the "cannot delete the only buffer" guard.
    let dir = std::env::temp_dir().join(format!(
        "lattice-oc10-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let second = dir.join("second.org");
    std::fs::write(&second, "second\n").unwrap();
    e.resolve_path_to_buffer(&second)
        .expect("the second buffer opens");
    e
}

/// The headline: Insert does not survive the buffer it belonged to.
#[test]
fn deleting_the_active_buffer_from_insert_returns_to_normal() {
    let mut e = boot_with_two_buffers();
    e.enter_mode(ModalState::Insert);
    assert_eq!(e.modal, ModalState::Insert, "sanity");

    assert!(e.do_buffer_delete(true), "the delete goes through");
    assert_eq!(
        e.modal,
        ModalState::Normal,
        "the successor buffer must not receive keystrokes as text"
    );
}

/// Replace is insert-like and carries the same hazard.
#[test]
fn replace_mode_is_left_too() {
    let mut e = boot_with_two_buffers();
    e.enter_mode(ModalState::Replace);

    assert!(e.do_buffer_delete(true));
    assert_eq!(e.modal, ModalState::Normal);
}

/// **A REFUSED delete must not drop the user out of Insert.**
///
/// The reset sits past every guard for this reason. `:bd` on a dirty buffer
/// without `!` is an error the user recovers from by carrying on typing, and
/// silently switching them to Normal mid-word would be a worse bug than the
/// one being fixed — it looks like dropped input.
#[test]
fn a_refused_delete_leaves_the_mode_alone() {
    let mut e = Editor::boot(CoreDocument::from_text("only\n"));
    e.enter_mode(ModalState::Insert);

    assert!(
        !e.do_buffer_delete(false),
        "the only buffer cannot be deleted"
    );
    assert_eq!(
        e.modal,
        ModalState::Insert,
        "a refused delete changes nothing, mode included"
    );
}

/// Deleting from Normal is untouched — no spurious mode churn, and in
/// particular no cursor nudge from a needless `enter_mode`.
#[test]
fn deleting_from_normal_mode_changes_nothing() {
    let mut e = boot_with_two_buffers();
    assert_eq!(e.modal, ModalState::Normal);
    let before = e.cursor;

    assert!(e.do_buffer_delete(true));
    assert_eq!(e.modal, ModalState::Normal);
    assert_eq!(
        e.cursor, before,
        "`enter_mode(Normal)` pulls the cursor back one byte; it must not run \
         when we were already in Normal"
    );
}
