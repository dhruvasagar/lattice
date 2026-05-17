//! Typed lifecycle event payloads.
//!
//! These match the entries documented in DESIGN.md §5.10.1 ("Mode
//! lifecycle"). M-async.2: published on the typed event bus from
//! the dispatcher's spawned lifecycle task (`MajorEntered` /
//! `MinorActivated` on success, `ModeActivationFailed` on
//! lifecycle error). `MajorExiting` / `MinorDeactivated` fire
//! synchronously from the App thread (deactivation is sync).
//!
//! Ordering contract per `mode-architecture.md` §7:
//!
//! - `MajorEntered` / `MinorActivated` fire *after* the trait's
//!   `on_activate` resolves so subscribers see the buffer in a
//!   consistent state.
//! - `MajorExiting` / `MinorDeactivated` fire *before* the Guard
//!   drops so subscribers can inspect what's about to be torn
//!   down.

use lattice_protocol::ids::BufferId;
use smallvec::SmallVec;

use crate::error::ModeActivationError;
use crate::mode::ModeId;

/// Mode lifecycle events. Published on the typed event bus by
/// the dispatcher (`MajorEntered` / `MinorActivated` /
/// `ModeActivationFailed` from the spawned lifecycle task;
/// `MajorExiting` / `MinorDeactivated` synchronously from the
/// App thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeEvent {
    /// A new major mode is now the active major on `buffer`.
    /// Published *after* `on_activate` resolved successfully.
    MajorEntered { buffer: BufferId, mode: ModeId },

    /// The current major mode is about to be deactivated.
    /// Published *before* the Guard drops.
    MajorExiting { buffer: BufferId, mode: ModeId },

    /// A minor mode was activated on `buffer`. Published *after*
    /// `on_activate` resolved successfully.
    MinorActivated { buffer: BufferId, mode: ModeId },

    /// A minor mode was deactivated on `buffer`. Published
    /// *before* the Guard drops.
    MinorDeactivated { buffer: BufferId, mode: ModeId },

    /// M-async.2: `on_activate` returned `Err`. Published from
    /// the spawned lifecycle task instead of `MajorEntered` /
    /// `MinorActivated`. `active_modes` was mutated
    /// synchronously by the dispatcher's sync prefix; M-async.3
    /// adds an App-side subscriber that rolls back on this
    /// event. `reason` is the boundary string of the original
    /// `ModeActivationError` (the error type itself isn't `Eq` /
    /// `Clone`-friendly across crate boundaries; the string is
    /// what subscribers actually use).
    ModeActivationFailed {
        buffer: BufferId,
        mode: ModeId,
        reason: String,
    },

    /// Two active minor modes contributed conflicting values
    /// for the same option (M.2 emits this; reserved here).
    OptionConflict {
        buffer: BufferId,
        option: &'static str,
        modes: SmallVec<[ModeId; 2]>,
    },
}

impl ModeEvent {
    /// Build a `ModeActivationFailed` from a
    /// [`ModeActivationError`]. Stringifies the error so the
    /// event payload is `Clone + Eq` (the error variants
    /// contain registry-internal types that aren't worth
    /// threading through every subscriber).
    pub fn activation_failed(buffer: BufferId, mode: ModeId, err: &ModeActivationError) -> Self {
        Self::ModeActivationFailed {
            buffer,
            mode,
            reason: err.to_string(),
        }
    }
}

// M-async.2: register `ModeEvent` as a typed event so the bus's
// `publish_typed` / `subscribe_typed` API can carry it. One
// type, one event name -- subscribers filter on payload variant.
lattice_protocol::register_event!(
    ModeEvent,
    "mode.lifecycle",
    "Mode lifecycle transitions (MajorEntered, MajorExiting, \
     MinorActivated, MinorDeactivated, ModeActivationFailed).",
    "lattice-mode",
);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn variants_are_orderable_for_test_assertions() {
        let buf = BufferId::new(1);
        let mode = ModeId::new("test-mode");
        let a = ModeEvent::MinorActivated { buffer: buf, mode };
        let b = ModeEvent::MinorActivated { buffer: buf, mode };
        assert_eq!(a, b);
        let c = a.clone();
        assert_eq!(a, c);
    }

    #[test]
    fn activation_failed_builder_carries_reason() {
        let buf = BufferId::new(1);
        let mode = ModeId::new("x-mode");
        let err = ModeActivationError::NotRegistered(mode);
        let evt = ModeEvent::activation_failed(buf, mode, &err);
        match evt {
            ModeEvent::ModeActivationFailed {
                buffer,
                mode: m,
                reason,
            } => {
                assert_eq!(buffer, buf);
                assert_eq!(m, mode);
                assert!(reason.contains("not registered"));
            }
            other => panic!("expected ModeActivationFailed, got {other:?}"),
        }
    }
}
