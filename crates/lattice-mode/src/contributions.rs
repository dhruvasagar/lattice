//! Placeholder types for declarative contributions on
//! [`crate::Mode`] not yet wired in.
//!
//! `OptionOverrideSet` graduated to a real type in
//! [`crate::overrides`] as of M.2.1. The remaining stubs:
//!
//! - [`Keymap`] -- when the layered keymap registry from
//!   `keymap-architecture.md` exposes a public mode-contribution
//!   type. Until then, the placeholder lets modes declare
//!   "intent to contribute a keymap layer" without forcing
//!   `lattice-mode` to depend on `lattice-grammar`.
//! - [`Subscription`] -- when the typed event bus stabilises a
//!   mode-side subscription type (DESIGN.md §5.10).
//! - [`DecorationProvider`] -- M.4 / decoration registry.

/// Stub. Real type lands when the layered keymap registry
/// exposes a mode-contribution type.
#[derive(Debug, Default, Clone)]
pub struct Keymap {
    _private: (),
}

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
