//! Typed lifecycle event payloads.
//!
//! These match the entries documented in DESIGN.md §5.10.1 ("Mode
//! lifecycle"). The registry produces events as a return value
//! from its activation / deactivation methods (M.1 keeps it
//! bus-agnostic for testability); the caller forwards them to
//! the actual typed event bus when one is available (M.4
//! integration).
//!
//! Ordering contract per `mode-architecture.md` §7:
//!
//! - `MajorEntered` runs *after* the trait's `on_activate` hook
//!   so subscribers see a buffer in a consistent state.
//! - `MajorExiting` runs *before* `on_deactivate` so subscribers
//!   can inspect what's about to be torn down.

use lattice_protocol::ids::BufferId;
use smallvec::SmallVec;

use crate::mode::ModeId;

/// Mode lifecycle events. Each variant is a typed payload
/// matching the corresponding registry transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeEvent {
    /// A new major mode is now the active major on `buffer`.
    /// Published *after* the trait's `on_activate` ran. Replaces
    /// any previously active major (the previous major's
    /// `MajorExiting` fires first, see ordering below).
    MajorEntered { buffer: BufferId, mode: ModeId },

    /// The current major mode is about to be deactivated.
    /// Published *before* the trait's `on_deactivate` runs.
    /// Subscribers can inspect the buffer's current state.
    MajorExiting { buffer: BufferId, mode: ModeId },

    /// A minor mode was activated on `buffer`. Published *after*
    /// `on_activate`.
    MinorActivated { buffer: BufferId, mode: ModeId },

    /// A minor mode was deactivated on `buffer`. Published
    /// *after* the registry pops the mode's overrides; per
    /// §7.1, deactivation is synchronous from the user's
    /// perspective, but resource teardown can continue async
    /// post-event.
    MinorDeactivated { buffer: BufferId, mode: ModeId },

    /// Two active minor modes contributed conflicting values
    /// for the same option (M.2 emits this; the variant is
    /// reserved here so M.1 lifecycle code can return events
    /// without backward-incompat). `option` carries the
    /// boundary string name; the resolution layer decides
    /// which mode won. M.1 never produces this variant; M.2
    /// will.
    OptionConflict {
        buffer: BufferId,
        option: &'static str,
        modes: SmallVec<[ModeId; 2]>,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn variants_are_orderable_for_test_assertions() {
        // Test assertions in registry.rs rely on these being
        // PartialEq + Clone. This test pins the contract.
        let buf = BufferId::new(1);
        let mode = ModeId::new("test-mode");
        let a = ModeEvent::MinorActivated { buffer: buf, mode };
        let b = ModeEvent::MinorActivated { buffer: buf, mode };
        assert_eq!(a, b);
        let c = a.clone();
        assert_eq!(a, c);
    }
}
