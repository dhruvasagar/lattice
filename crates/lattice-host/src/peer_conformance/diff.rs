//! Diff scenarios: the keys a diff session's modes bind, through each
//! peer's key path.

use super::completion::settle;
use super::llm::runtime;
use super::{PeerUnderTest, boot, press, temp_path};

/// `]n` / `[n` walk a conflict session's conflicts and pass over the region
/// git merged cleanly; `]c` still stops there.
///
/// The keys live on `diff-conflict-mode`'s layer, which a peer only reaches
/// once the session's conflict mode is active and its keymap overlays are
/// synced, so this is the peer's layer wiring as much as the motion.
pub fn conflict_keys_walk_conflicts_only<P: PeerUnderTest>() {
    // The session's hunk compute and its publish forwarder run as tasks.
    let _rt = runtime();
    let mut peer = boot::<P>("x\n");
    let working = temp_path("conflict-walk");
    std::fs::write(
        &working,
        "a\nmerged cleanly\nb\n\
         <<<<<<< HEAD\no1\n=======\nt1\n>>>>>>> x\n\
         c\n\
         <<<<<<< HEAD\no2\n=======\nt2\n>>>>>>> x\n\
         d\n",
    )
    .expect("writes the working file");
    for (name, text) in [
        ("*ours*", "a\nb\no1\nc\no2\nd\n"),
        ("*theirs*", "a\nmerged cleanly\nb\nt1\nc\nt2\nd\n"),
    ] {
        let id = peer.editor_mut().ensure_named_synthetic_document(
            name,
            lattice_mode::ModeId::new("fundamental-mode"),
            crate::synthetic_buffers::SYNTHETIC_BUFFER_FLAGS,
        );
        peer.editor_mut().replace_owned_buffer(id, text);
    }
    peer.editor_mut()
        .do_diff_resolve("*ours*", working.clone(), "*theirs*");
    peer.republish();

    let conflict_mode = crate::diff::mode::DiffConflictMode::mode_id();
    assert!(
        settle(&mut peer, |p| {
            p.editor()
                .render_state
                .load()
                .translator
                .active_minor_modes
                .contains(&conflict_mode)
        }),
        "precondition: diff-conflict-mode is an active layer, or `]n` has \
         nothing to resolve against"
    );
    let line = |p: &P| p.editor().cursor.line;
    assert_eq!(line(&peer), 3, "opens on the first conflict");

    press(&mut peer, "]n");
    assert_eq!(line(&peer), 9, "`]n` to the second conflict");
    press(&mut peer, "]n");
    assert_eq!(line(&peer), 3, "`]n` wraps to the first");
    press(&mut peer, "[n");
    assert_eq!(line(&peer), 9, "`[n` wraps to the last");
    press(&mut peer, "[n");
    assert_eq!(line(&peer), 3, "`[n` to the previous conflict");

    press(&mut peer, "gg]c");
    assert_eq!(line(&peer), 1, "`]c` still stops at the clean merge");

    let _ = std::fs::remove_file(&working);
}
