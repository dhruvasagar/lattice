//! `ActiveModes`: the major + ordered minors set per buffer.
//!
//! M.3 lands `ActiveModes` as a field on `Document`; M.1 keeps
//! it as a standalone type that tests can construct directly.
//! The registry's activation / deactivation methods take
//! `&mut ActiveModes` as a parameter so they can mutate the set
//! without owning per-buffer state -- letting `lattice-mode` be
//! buffer-storage-agnostic for the foundation slice.
//!
//! Minor mode order matters (it's the activation order; later
//! activations override earlier ones at the option-resolution
//! layer per `mode-architecture.md` §6.2). The registry keeps
//! this ordering stable: activate appends to the end,
//! deactivate removes by id without disturbing relative order
//! of the survivors.

use smallvec::SmallVec;

use crate::mode::ModeId;

/// The set of modes active on a buffer. Exactly one major (or
/// none, prior to first major activation), plus any number of
/// minors in activation order.
///
/// Ordering invariant: minors are stored in the order they were
/// activated; when an active mode is deactivated, the survivors
/// retain their relative positions (no reshuffling). This is
/// what M.2's option-resolution layer relies on for "later
/// activation wins" tie-breaking.
///
/// `SmallVec` keeps the typical case (0-4 minors) inline. A
/// buffer with more active minors spills to the heap; that's
/// fine -- the hot path is option resolution against the cached
/// `ResolvedOptions`, not iteration of `ActiveModes`.
#[derive(Debug, Default, Clone)]
pub struct ActiveModes {
    major: Option<ModeId>,
    minors: SmallVec<[ModeId; 4]>,
}

impl ActiveModes {
    /// Construct an empty set (no major, no minors).
    pub fn new() -> Self {
        Self::default()
    }

    /// The active major, if any. `None` until the first major
    /// activation runs (e.g. a freshly-opened scratch buffer
    /// before mode resolution).
    pub fn major(&self) -> Option<ModeId> {
        self.major
    }

    /// All active minors, in activation order.
    pub fn minors(&self) -> &[ModeId] {
        &self.minors
    }

    /// True iff this minor is currently active.
    pub fn has_minor(&self, mode: ModeId) -> bool {
        self.minors.contains(&mode)
    }

    /// True iff `mode` is the active major.
    pub fn is_active_major(&self, mode: ModeId) -> bool {
        self.major == Some(mode)
    }

    /// True iff `mode` is active in any role (major or minor).
    pub fn is_active(&self, mode: ModeId) -> bool {
        self.is_active_major(mode) || self.has_minor(mode)
    }

    // -------- Mutation API: registry-only --------
    //
    // These methods are pub(crate) so only the `registry` module
    // can drive transitions. External callers go through
    // `ModeRegistry::activate_*` / `deactivate_*` which validate
    // capabilities + conflicts before calling these.

    /// Set the major mode. Registry calls this AFTER any
    /// previous major's `on_deactivate` ran and AFTER the new
    /// major's `on_activate` ran successfully.
    pub(crate) fn set_major(&mut self, mode: Option<ModeId>) {
        self.major = mode;
    }

    /// Append a minor in activation order. Registry calls this
    /// AFTER the minor's `on_activate` ran successfully.
    /// No-op if the minor is already active (idempotent).
    pub(crate) fn push_minor(&mut self, mode: ModeId) {
        if !self.has_minor(mode) {
            self.minors.push(mode);
        }
    }

    /// Remove a minor by id. Registry calls this AFTER the
    /// minor's `on_deactivate` ran. Returns true iff the minor
    /// was actually active (no-op if not).
    pub(crate) fn remove_minor(&mut self, mode: ModeId) -> bool {
        if let Some(idx) = self.minors.iter().position(|&m| m == mode) {
            self.minors.remove(idx);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn defaults_are_empty() {
        let a = ActiveModes::new();
        assert_eq!(a.major(), None);
        assert!(a.minors().is_empty());
    }

    #[test]
    fn set_major_round_trips() {
        let mut a = ActiveModes::new();
        let m = ModeId::new("rust-mode");
        a.set_major(Some(m));
        assert_eq!(a.major(), Some(m));
        assert!(a.is_active_major(m));
        assert!(a.is_active(m));
    }

    #[test]
    fn minors_preserve_activation_order() {
        let mut a = ActiveModes::new();
        let one = ModeId::new("lsp-mode");
        let two = ModeId::new("lsp-completion-mode");
        let three = ModeId::new("lsp-diagnostics-mode");
        a.push_minor(one);
        a.push_minor(two);
        a.push_minor(three);
        assert_eq!(a.minors(), &[one, two, three]);
    }

    #[test]
    fn push_minor_is_idempotent() {
        let mut a = ActiveModes::new();
        let m = ModeId::new("git-blame-mode");
        a.push_minor(m);
        a.push_minor(m);
        assert_eq!(a.minors(), &[m]);
    }

    #[test]
    fn remove_minor_preserves_relative_order() {
        let mut a = ActiveModes::new();
        let one = ModeId::new("a-mode");
        let two = ModeId::new("b-mode");
        let three = ModeId::new("c-mode");
        a.push_minor(one);
        a.push_minor(two);
        a.push_minor(three);
        assert!(a.remove_minor(two));
        assert_eq!(a.minors(), &[one, three]);
    }

    #[test]
    fn remove_minor_returns_false_for_inactive() {
        let mut a = ActiveModes::new();
        assert!(!a.remove_minor(ModeId::new("never-activated-mode")));
    }
}
