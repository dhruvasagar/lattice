//! Errors surfaced by the registry's activation / deactivation
//! path.

use thiserror::Error;

use crate::capability::CapabilitySet;
use crate::mode::ModeId;

/// Why an activation failed.
///
/// Two routes, and they reach different places. The registry validates
/// registration, kind, capabilities, conflicts and dependency presence
/// **synchronously** before any lifecycle hook runs: those variants are the
/// `Err` of `activate_major` / `activate_minor`, nothing ran, nothing was
/// published, and the active set is unchanged. An error returned **by
/// `on_activate`** (normally [`LifecycleFailed`](Self::LifecycleFailed))
/// arrives after the activate call has already returned `Ok`; it is
/// published as [`ModeEvent::ModeActivationFailed`](crate::ModeEvent::ModeActivationFailed)
/// and the host rolls back.
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
        /// The mode that could not activate.
        mode: ModeId,
        /// The required capabilities the buffer does not offer.
        missing: CapabilitySet,
    },

    /// Activating `mode` would conflict with `active` -- a
    /// declared `conflicts_with` entry on either side. Raised when
    /// activating a minor (directly or via an `implies` cascade); the
    /// registry never auto-deactivates the other mode, so the caller
    /// must deactivate it and retry if the swap is wanted.
    #[error("mode `{mode}` conflicts with active mode `{active}`")]
    Conflict {
        /// The mode being activated.
        mode: ModeId,
        /// The already-active mode it conflicts with.
        active: ModeId,
    },

    /// `mode` declares an `implies` dependency on `dep`, but
    /// `dep` is not registered. Indicates a build-config bug
    /// (a feature crate registered the parent without its
    /// dependency).
    #[error("mode `{mode}` implies `{dep}` which is not registered")]
    UnregisteredDependency {
        /// The mode declaring the dependency.
        mode: ModeId,
        /// The implied mode that is not registered.
        dep: ModeId,
    },

    /// Wrong kind: caller invoked `activate_major` on a minor
    /// mode or vice versa. Indicates a type-bug in the caller;
    /// the trait's `kind()` answers what's expected.
    #[error("mode `{mode}` is the wrong kind for this operation")]
    WrongKind {
        /// The mode whose kind did not match the call.
        mode: ModeId,
    },

    /// A mode's `on_activate` failed. The variant a mode constructs
    /// itself (with its own id and a human-readable reason) when setup
    /// cannot complete — return it rather than panicking.
    #[error("mode `{mode}` lifecycle hook failed: {reason}")]
    LifecycleFailed {
        /// The mode whose hook failed.
        mode: ModeId,
        /// Why, for the user and the log.
        reason: String,
    },

    /// A mode tried to write a buffer-local owned by a
    /// different mode (M.3.2.a). Reserved for the checked
    /// `ModeContext::set_local` / `remove_local` surface the design
    /// describes (comparing `T::OWNER_MODE` against the activating mode's
    /// id); that surface is not implemented, so nothing constructs this
    /// variant today. See [`BufferLocal`](crate::BufferLocal).
    #[error("mode `{current}` cannot write buffer-local `{local}` (owner is `{owner}`)")]
    WrongOwnerMode {
        /// The currently-activating mode that attempted the
        /// write.
        current: ModeId,
        /// The local's display name (`T::NAME`).
        local: &'static str,
        /// The local's declared owner mode (`T::OWNER_MODE`).
        owner: &'static str,
    },
}
