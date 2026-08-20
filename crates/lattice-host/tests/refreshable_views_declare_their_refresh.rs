//! PD.7c — a view that inherits `gr` must have something for it to do.
//!
//! `refreshable-view-mode` binds `gr` and **nothing else**: it resolves the
//! chord to whichever active mode declared a `refresh_action()` and dispatches
//! that. So a mode that joins the cascade without declaring one gets a `gr`
//! that resolves to nothing — a key the user presses, the help lists, and the
//! mode's own comments describe, which silently does nothing.
//!
//! `gr_is_declared_once.rs` guards the other direction (nobody binds `gr`
//! themselves). This guards the direction that actually bit:
//! `magit-project-diff-mode` implied `refreshable-view-mode` "for `gr`" and
//! declared no action, so the view had no refresh at all — found while giving
//! its headerline a "gr to refresh" note to point at.
//!
//! The enumeration comes from the booted mode registry for the same reason
//! `every_mode_has_a_help_page.rs` does: it is the only place every
//! mode-owning crate has registered, and grepping the source instead sweeps in
//! test fixtures until the guard gets weakened into uselessness.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::RefreshableViewMode;

#[test]
fn every_mode_that_inherits_gr_declares_what_it_refreshes() {
    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let registry = editor.mode_registry.load();
    let refreshable = RefreshableViewMode::mode_id();

    let mut silent: Vec<String> = registry
        .iter()
        .filter(|(_, mode)| mode.implies().contains(&refreshable))
        .filter(|(_, mode)| mode.refresh_action().is_none())
        .map(|(id, _)| id.to_string())
        .collect();
    silent.sort();

    assert!(
        silent.is_empty(),
        "these modes pull in `refreshable-view-mode` (so `gr` is bound in \
         their buffers) but declare no `refresh_action()`, leaving `gr` \
         resolving to nothing: {silent:?}\n\n\
         Either declare `fn refresh_action(&self) -> Option<&'static str>` \
         naming an action the mode's own crate registers, or stop implying \
         `refreshable-view-mode` — inheriting a chord you cannot answer is \
         worse than not having it, because it reads as a broken key rather \
         than an absent feature."
    );
}
