//! Placeholder types for the declarative contributions side of
//! [`crate::Mode`].
//!
//! These types are stubs in M.1: empty `Default`-able structs
//! that let the trait surface be complete and testable without
//! pulling in dependencies on subsystems that haven't been
//! refactored yet. Real impls land in:
//!
//! - [`OptionOverrideSet`] -- M.2 (option resolution layer with
//!   typed identities, layered priority, conflict policy).
//! - [`Keymap`] -- when the layered keymap registry from
//!   `keymap-architecture.md` exposes a public mode-contribution
//!   type. Until then, the placeholder lets modes declare
//!   "intent to contribute a keymap layer" without forcing
//!   `lattice-mode` to depend on `lattice-grammar`.
//! - [`Subscription`] -- when the typed event bus stabilises a
//!   mode-side subscription type (DESIGN.md §5.10).
//! - [`DecorationProvider`] -- M.4 / decoration registry.
//!
//! Tests in this crate construct empty contributions; the
//! registry doesn't apply them in M.1, only forwards them
//! through the lifecycle. Real application happens in the
//! slice that lands the corresponding subsystem.

/// Stub. M.2 replaces this with the real layered-priority
/// override set keyed on option type identity.
#[derive(Debug, Default, Clone)]
pub struct OptionOverrideSet {
    _private: (),
}

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
