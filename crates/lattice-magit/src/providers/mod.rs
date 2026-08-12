//! PD.1 (2026-08-12): multibuffer-backed magit surfaces.
//!
//! These live here rather than in `lattice-multibuffer/src/providers/`
//! per the provider-home reversal (`multibuffer-views.md` §3.7): magit
//! already owns both inputs — `lattice-vcs` for the changed set and
//! `lattice-diff` for the hunks — so the trigger, the keymap, the
//! handler bodies and the view sit in one crate.

pub mod project_diff;
