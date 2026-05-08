//! Errors surfaced by the registry's activation / deactivation
//! path.

use thiserror::Error;

use crate::capability::CapabilitySet;
use crate::mode::ModeId;

/// Why an activation failed. The registry validates capabilities,
/// conflicts, and dependency presence before running any
/// lifecycle hook -- a failure here means no `on_activate` ran
/// and no event published.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ModeActivationError {
    /// `mode` is not in the registry. Either typo'd or not
    /// registered yet.
    #[error("mode `{0}` is not registered")]
    NotRegistered(ModeId),

    /// Buffer lacks one or more capabilities the mode requires.
    /// `missing` is the bitfield of the absent capabilities
    /// (i.e. `mode.required_capabilities() - buffer_capabilities`).
    #[error("mode `{mode}` requires capabilities `{missing:?}` that the buffer lacks")]
    MissingCapability {
        mode: ModeId,
        missing: CapabilitySet,
    },

    /// Activating `mode` would conflict with `active` -- a
    /// declared `conflicts_with` entry on either side. For
    /// minor-mode conflicts the registry can auto-deactivate
    /// the conflicting mode instead, depending on the activation
    /// policy chosen by the caller; this error surfaces when
    /// auto-deactivation is not allowed (e.g. activating a
    /// major mode while another conflicting major is active --
    /// the caller must explicitly request the swap).
    #[error("mode `{mode}` conflicts with active mode `{active}`")]
    Conflict { mode: ModeId, active: ModeId },

    /// `mode` declares an `implies` dependency on `dep`, but
    /// `dep` is not registered. Indicates a build-config bug
    /// (a feature crate registered the parent without its
    /// dependency).
    #[error("mode `{mode}` implies `{dep}` which is not registered")]
    UnregisteredDependency { mode: ModeId, dep: ModeId },

    /// Wrong kind: caller invoked `activate_major` on a minor
    /// mode or vice versa. Indicates a type-bug in the caller;
    /// the trait's `kind()` answers what's expected.
    #[error("mode `{mode}` is the wrong kind for this operation")]
    WrongKind { mode: ModeId },

    /// User-supplied `on_activate` / `on_deactivate` returned
    /// an error. Carries a string description (the trait
    /// surface returns `Result<(), ModeActivationError>`; impls
    /// can construct this variant themselves).
    #[error("mode `{mode}` lifecycle hook failed: {reason}")]
    LifecycleFailed { mode: ModeId, reason: String },
}
