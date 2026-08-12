//! Crate-level multibuffer events.
//!
//! ## Why this module exists (PV.1, 2026-08-12)
//!
//! [`MultibufferExcerptsReady`] was declared inside
//! `providers::search`, which is behind the `search` cargo feature —
//! and so was `install`'s `wake_on_event` registration for it. That
//! made a **generic** signal ("this view's excerpts changed, repaint
//! without waiting for a keypress") conditional on one provider being
//! compiled in: a `--no-default-features` build, or any provider living
//! outside this crate, had no wake at all and would show the
//! blank-results-until-keypress symptom the event was added to fix.
//!
//! The event is about a multibuffer view, not about searching, so it
//! belongs at the crate root with an unconditional wake. Providers in
//! *other* crates (magit's project-diff is the first) publish it the
//! same way `providers::search` does.

use lattice_core::BufferId;

/// Published by any provider after appending / replacing a view's
/// excerpts, so the cells worker rebuilds the display matrix
/// off-keystroke.
///
/// The subscriber is the boot-registered wake
/// (`install`'s `wake_on_event::<MultibufferExcerptsReady>()`), which
/// fires `async_landed` — the primitive that makes async results reach
/// the screen without a keypress. A provider that appends excerpts and
/// does NOT publish this will appear to work in tests that press a key
/// first, and appear broken in use.
#[derive(Debug, Clone)]
pub struct MultibufferExcerptsReady {
    pub view: BufferId,
}

lattice_protocol::register_event!(
    MultibufferExcerptsReady,
    "multibuffer.excerpts-ready",
    "New excerpts appended to a multibuffer view.",
    "lattice-multibuffer",
);
