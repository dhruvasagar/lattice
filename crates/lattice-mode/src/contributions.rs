//! Declarative contributions on [`crate::Mode`].
//!
//! `Keymap` and `KeymapBinding` moved to `lattice-keymap::contribution`
//! in K.3 (2026-06-07) — re-exported here for backward compatibility.
//!
//! Stubs still pending real impls:
//! - [`Subscription`] -- when the typed event bus stabilises.
//! - [`DecorationProvider`] -- M.4 / decoration registry.

pub use lattice_keymap::{Keymap, KeymapBinding};

/// Stub. Real type lands when the typed event bus stabilises
/// a subscription shape for modes.
#[derive(Debug, Clone)]
pub struct Subscription {
    _private: (),
}

/// Stub. M.4 replaces with the real decoration-provider type.
#[derive(Debug, Clone)]
pub struct DecorationProvider {
    _private: (),
}
