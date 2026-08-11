//! LR.1 (2026-08-11): multibuffer-backed LSP surfaces.
//!
//! These live here rather than in `lattice-multibuffer/src/providers/`
//! per the provider-home reversal (`multibuffer-views.md` §3.7): a
//! provider that IS a subsystem's user-facing surface belongs with that
//! subsystem, so its chord, its handler body and its view sit in one
//! crate instead of spread across three.
//!
//! `search` / `narrow` / `problems` stay in `lattice-multibuffer` —
//! they have no owning subsystem.

pub mod references;
