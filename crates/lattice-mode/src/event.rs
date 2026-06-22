//! Typed internal mode-dispatch signals.
//!
//! MA.1 split the mode-event surface: the four **observable**
//! lifecycle transitions (`MajorEntered` / `MajorExiting` /
//! `MinorActivated` / `MinorDeactivated`, design.md §5.10.1) moved to
//! the `lattice_protocol::Event` enum so hooks and the EF.1
//! `EventFilter` apply to them uniformly (mode-architecture.md §7.4).
//! What stays here is the dispatcher's **internal** signalling that
//! isn't part of the public lifecycle catalog:
//!
//! - `ModeActivationFailed` — `on_activate` returned `Err`; the
//!   App-side subscriber rolls back `active_modes` / `mode_guards`.
//! - `OptionConflict` — two active minors disagree on an option
//!   (M.2 emits this; reserved).
//!
//! Both ride the typed bus (`EventBus::publish_typed` /
//! `subscribe_typed`) because they carry richer payloads (a reason
//! string, a conflicting-mode set) and have a single internal
//! consumer rather than open subscription.

use lattice_protocol::ids::BufferId;
use smallvec::SmallVec;

use crate::error::ModeActivationError;
use crate::mode::ModeId;

/// Internal mode-dispatch signals on the typed event bus
/// (`ModeActivationFailed` / `OptionConflict`). The observable
/// lifecycle quartet lives on the [`lattice_protocol::Event`] enum
/// (MA.1) — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeEvent {
    /// M-async.2: `on_activate` returned `Err`. Published from
    /// the spawned lifecycle task instead of the success lifecycle
    /// event. `active_modes` was mutated
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
        let a = ModeEvent::ModeActivationFailed {
            buffer: buf,
            mode,
            reason: "boom".into(),
        };
        let b = ModeEvent::ModeActivationFailed {
            buffer: buf,
            mode,
            reason: "boom".into(),
        };
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
