//! `ModeRegistry`: register modes, look them up, drive
//! activation / deactivation against a per-buffer `ActiveModes`.
//!
//! M.1 keeps the registry **bus-agnostic**: activation and
//! deactivation methods return the events they would have
//! published as a `Vec<ModeEvent>`, instead of dispatching to
//! the typed event bus directly. This lets `lattice-mode` be
//! tested without spinning up the bus, and lets the bus
//! integration land cleanly in M.4 by having callers forward
//! the returned events.
//!
//! Validation order before activation:
//! 1. Mode is registered (`ModeActivationError::NotRegistered`).
//! 2. Mode kind matches the call (`WrongKind`).
//! 3. Buffer satisfies required capabilities
//!    (`MissingCapability`).
//! 4. No conflict with already-active modes (`Conflict`) --
//!    unless the policy auto-deactivates conflicting minors.
//! 5. All `implies` dependencies are registered
//!    (`UnregisteredDependency`).
//! 6. `on_activate` runs; lifecycle errors surface as
//!    `LifecycleFailed`.
//! 7. Declarative contributions are conceptually applied (M.1
//!    is a no-op here -- M.2+ slices wire in the option /
//!    keymap / decoration / subscription registries).

use std::collections::HashMap;
use std::sync::Arc;

use lattice_protocol::ids::BufferId;

use crate::active::ActiveModes;
use crate::capability::CapabilitySet;
use crate::context::ModeContext;
use crate::error::ModeActivationError;
use crate::event::ModeEvent;
use crate::locals::BufferLocals;
use crate::mode::{Mode, ModeId, ModeKind};

/// Why a registration failed. Distinct from
/// [`ModeActivationError`] because registration happens once
/// per mode, before any buffer ever activates it; activation
/// errors are per-buffer and per-attempt.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RegistrationError {
    /// A mode with this id is already registered. Names are
    /// canonical; double-registration is a build-config bug.
    #[error("mode `{0}` is already registered")]
    Duplicate(ModeId),
}

/// Mode registry. Owns the catalogue of registered modes and
/// drives activation / deactivation. Bus-agnostic: callers get
/// events as a return value and forward them as needed.
///
/// The registry does not own per-buffer `ActiveModes` -- each
/// buffer carries its own (M.3 lands the field on `Document`).
/// Activation methods take `&mut ActiveModes` so the registry
/// can mutate the caller's set after validation succeeds.
///
/// `Clone` is cheap: each entry is an `Arc<dyn Mode>` so the
/// HashMap clone is shallow over the values. Used by tests that
/// need to register a test-only mode post-boot via
/// `Arc::make_mut(&mut app.mode_registry)`; production code
/// constructs the registry once at boot and never clones.
#[derive(Clone)]
pub struct ModeRegistry {
    modes: HashMap<ModeId, Arc<dyn Mode>>,
}

impl Default for ModeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ModeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModeRegistry")
            .field("count", &self.modes.len())
            .finish_non_exhaustive()
    }
}

impl ModeRegistry {
    pub fn new() -> Self {
        Self {
            modes: HashMap::new(),
        }
    }

    /// Register a mode. Same id twice is a `Duplicate` error;
    /// the registry is single-writer at startup so the typical
    /// caller registers all modes before any buffer activates.
    pub fn register<M: Mode>(&mut self, mode: M) -> Result<ModeId, RegistrationError> {
        let id = mode.id();
        if self.modes.contains_key(&id) {
            return Err(RegistrationError::Duplicate(id));
        }
        self.modes.insert(id, Arc::new(mode));
        Ok(id)
    }

    /// True iff this id is registered (any kind).
    pub fn is_registered(&self, id: ModeId) -> bool {
        self.modes.contains_key(&id)
    }

    /// Look up a registered mode by id.
    pub fn get(&self, id: ModeId) -> Option<Arc<dyn Mode>> {
        self.modes.get(&id).cloned()
    }

    /// Iterate every registered mode's `(id, kind)`. Used at boot
    /// by the App to auto-generate toggle ex-commands per mode-
    /// architecture §9.6.1, and by `:list-modes` (M.8) to render
    /// the catalogue.
    pub fn iter_meta(&self) -> impl Iterator<Item = (ModeId, ModeKind)> + '_ {
        self.modes.iter().map(|(id, mode)| (*id, mode.kind()))
    }

    /// Number of registered modes (any kind).
    pub fn len(&self) -> usize {
        self.modes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modes.is_empty()
    }

    /// Activate a major mode on `buffer`. If a different major
    /// is currently active, it is deactivated first (the
    /// previous major's `on_deactivate` runs, `MajorExiting`
    /// fires, then the new major's `on_activate` runs and
    /// `MajorEntered` fires). Idempotent: activating the
    /// already-active major triggers a *reload* (deactivate
    /// then re-activate, per `mode-architecture.md` §9.6).
    ///
    /// `caps` is the buffer's current capability set. The new
    /// mode's `required_capabilities` must be a subset.
    ///
    /// Implied modes (`Mode::implies`) are auto-activated as
    /// minors after the major lands; failure on an implied
    /// activation rolls back the major activation.
    pub fn activate_major(
        &self,
        active: &mut ActiveModes,
        locals: &mut BufferLocals,
        config: &lattice_config::ConfigRegistry,
        events: &std::sync::Arc<lattice_runtime::EventBus>,
        services: &crate::services::ServiceRegistry,
        buffer: BufferId,
        mode: ModeId,
        caps: CapabilitySet,
    ) -> Result<Vec<ModeEvent>, ModeActivationError> {
        let entry = self
            .modes
            .get(&mode)
            .ok_or(ModeActivationError::NotRegistered(mode))?;
        if entry.kind() != ModeKind::Major {
            return Err(ModeActivationError::WrongKind { mode });
        }
        let missing = entry.required_capabilities() - caps;
        if !missing.is_empty() {
            return Err(ModeActivationError::MissingCapability { mode, missing });
        }

        let mut emitted = Vec::new();

        // Tear down current major (if any). For self-reload this
        // is intentional -- we want `on_deactivate` then
        // `on_activate` to run, idempotent setup contract.
        if let Some(prev_id) = active.major() {
            // MajorExiting fires BEFORE on_deactivate (per §7).
            emitted.push(ModeEvent::MajorExiting {
                buffer,
                mode: prev_id,
            });
            if let Some(prev) = self.modes.get(&prev_id) {
                let mut prev_ctx = ModeContext::new(buffer, prev_id, locals, config, events, services);
                if let Err(e) = prev.on_deactivate(&mut prev_ctx) {
                    return Err(e);
                }
            }
            active.set_major(None);
        }

        // Run the new major's on_activate.
        {
            let mut ctx = ModeContext::new(buffer, mode, locals, config, events, services);
            if let Err(e) = entry.on_activate(&mut ctx) {
                return Err(e);
            }
        }
        active.set_major(Some(mode));
        emitted.push(ModeEvent::MajorEntered { buffer, mode });

        // Auto-activate implied minors (recursive).
        for &dep in entry.implies() {
            if !self.is_registered(dep) {
                return Err(ModeActivationError::UnregisteredDependency { mode, dep });
            }
            // Skip if already active (idempotent implies).
            if active.has_minor(dep) {
                continue;
            }
            let dep_events =
                self.activate_minor_inner(active, locals, config, events, services, buffer, dep, caps)?;
            emitted.extend(dep_events);
        }

        Ok(emitted)
    }

    /// Activate a minor mode on `buffer`. Validates capabilities,
    /// conflicts (rejects on conflict; auto-deactivation policy
    /// is M.6+ when LSP sub-modes have real conflict cases),
    /// and dependency presence.
    pub fn activate_minor(
        &self,
        active: &mut ActiveModes,
        locals: &mut BufferLocals,
        config: &lattice_config::ConfigRegistry,
        events: &std::sync::Arc<lattice_runtime::EventBus>,
        services: &crate::services::ServiceRegistry,
        buffer: BufferId,
        mode: ModeId,
        caps: CapabilitySet,
    ) -> Result<Vec<ModeEvent>, ModeActivationError> {
        self.activate_minor_inner(active, locals, config, events, services, buffer, mode, caps)
    }

    fn activate_minor_inner(
        &self,
        active: &mut ActiveModes,
        locals: &mut BufferLocals,
        config: &lattice_config::ConfigRegistry,
        events: &std::sync::Arc<lattice_runtime::EventBus>,
        services: &crate::services::ServiceRegistry,
        buffer: BufferId,
        mode: ModeId,
        caps: CapabilitySet,
    ) -> Result<Vec<ModeEvent>, ModeActivationError> {
        let entry = self
            .modes
            .get(&mode)
            .ok_or(ModeActivationError::NotRegistered(mode))?;
        if entry.kind() != ModeKind::Minor {
            return Err(ModeActivationError::WrongKind { mode });
        }
        // Idempotent: re-activating a live minor is a no-op
        // (does NOT trigger reload for minors -- that's a
        // major-mode-specific contract). M.9 may add an
        // explicit `reload` API if needed.
        if active.has_minor(mode) {
            return Ok(Vec::new());
        }
        let missing = entry.required_capabilities() - caps;
        if !missing.is_empty() {
            return Err(ModeActivationError::MissingCapability { mode, missing });
        }
        // Conflict check: if any active mode (major or minor)
        // is in this mode's conflicts list, reject. Symmetric
        // check: if this mode is in any active mode's conflicts
        // list, also reject.
        for &c in entry.conflicts_with() {
            if active.is_active(c) {
                return Err(ModeActivationError::Conflict { mode, active: c });
            }
        }
        if let Some(major) = active.major() {
            if let Some(major_entry) = self.modes.get(&major) {
                if major_entry.conflicts_with().contains(&mode) {
                    return Err(ModeActivationError::Conflict {
                        mode,
                        active: major,
                    });
                }
            }
        }
        for &active_minor in active.minors() {
            if let Some(minor_entry) = self.modes.get(&active_minor) {
                if minor_entry.conflicts_with().contains(&mode) {
                    return Err(ModeActivationError::Conflict {
                        mode,
                        active: active_minor,
                    });
                }
            }
        }

        {
            let mut ctx = ModeContext::new(buffer, mode, locals, config, events, services);
            if let Err(e) = entry.on_activate(&mut ctx) {
                return Err(e);
            }
        }
        active.push_minor(mode);
        let mut emitted = vec![ModeEvent::MinorActivated { buffer, mode }];

        // Recurse into implied minors.
        for &dep in entry.implies() {
            if !self.is_registered(dep) {
                return Err(ModeActivationError::UnregisteredDependency { mode, dep });
            }
            if active.has_minor(dep) {
                continue;
            }
            let dep_events =
                self.activate_minor_inner(active, locals, config, events, services, buffer, dep, caps)?;
            emitted.extend(dep_events);
        }

        Ok(emitted)
    }

    /// Deactivate a minor mode. Idempotent: deactivating an
    /// already-inactive mode is a no-op (returns empty events).
    /// Per `mode-architecture.md` §7.1, this is synchronous --
    /// `on_deactivate` runs before this returns; subscribers
    /// then receive `MinorDeactivated`.
    pub fn deactivate_minor(
        &self,
        active: &mut ActiveModes,
        locals: &mut BufferLocals,
        config: &lattice_config::ConfigRegistry,
        events: &std::sync::Arc<lattice_runtime::EventBus>,
        services: &crate::services::ServiceRegistry,
        buffer: BufferId,
        mode: ModeId,
    ) -> Result<Vec<ModeEvent>, ModeActivationError> {
        if !active.has_minor(mode) {
            return Ok(Vec::new());
        }
        let entry = self
            .modes
            .get(&mode)
            .ok_or(ModeActivationError::NotRegistered(mode))?;
        // Phase 3: collect the implies list *before* running
        // the mode's own on_deactivate so a deactivate hook
        // that touches its own implies (uncommon) can't see
        // a half-cleaned set. The implies cascade fires
        // *after* the mode's own teardown, mirroring activate
        // (mode's own setup before implied minors land).
        let implies: Vec<ModeId> = entry.implies().to_vec();
        let mut ctx = ModeContext::new(buffer, mode, locals, config, events, services);
        entry.on_deactivate(&mut ctx)?;
        active.remove_minor(mode);
        let mut emitted = vec![ModeEvent::MinorDeactivated { buffer, mode }];
        // Cascade-deactivate every implied minor that's still
        // active. Symmetric to the activate cascade in
        // `activate_minor_inner` (which uses `Mode::implies()`
        // to know what to activate).
        for &dep in &implies {
            if !active.has_minor(dep) {
                continue;
            }
            let dep_events =
                self.deactivate_minor(active, locals, config, events, services, buffer, dep)?;
            emitted.extend(dep_events);
        }
        Ok(emitted)
    }

    /// Deactivate the active major mode (if any). Returns
    /// empty events if no major is active.
    pub fn deactivate_major(
        &self,
        active: &mut ActiveModes,
        locals: &mut BufferLocals,
        config: &lattice_config::ConfigRegistry,
        events: &std::sync::Arc<lattice_runtime::EventBus>,
        services: &crate::services::ServiceRegistry,
        buffer: BufferId,
    ) -> Result<Vec<ModeEvent>, ModeActivationError> {
        let Some(mode) = active.major() else {
            return Ok(Vec::new());
        };
        let entry = self
            .modes
            .get(&mode)
            .ok_or(ModeActivationError::NotRegistered(mode))?;
        let mut ctx = ModeContext::new(buffer, mode, locals, config, events, services);
        let result_events = vec![ModeEvent::MajorExiting { buffer, mode }];
        entry.on_deactivate(&mut ctx)?;
        active.set_major(None);
        Ok(result_events)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc as StdArc;

    /// Tracking mock mode for tests. Counts on_activate /
    /// on_deactivate calls and exposes hooks for asserting
    /// lifecycle ordering.
    struct MockMode {
        id: ModeId,
        kind: ModeKind,
        required: CapabilitySet,
        conflicts: Vec<ModeId>,
        implies: Vec<ModeId>,
        activate_calls: StdArc<AtomicU32>,
        deactivate_calls: StdArc<AtomicU32>,
    }

    impl MockMode {
        fn major(name: &str) -> Self {
            Self {
                id: ModeId::new(name),
                kind: ModeKind::Major,
                required: CapabilitySet::empty(),
                conflicts: Vec::new(),
                implies: Vec::new(),
                activate_calls: StdArc::new(AtomicU32::new(0)),
                deactivate_calls: StdArc::new(AtomicU32::new(0)),
            }
        }
        fn minor(name: &str) -> Self {
            Self {
                id: ModeId::new(name),
                kind: ModeKind::Minor,
                required: CapabilitySet::empty(),
                conflicts: Vec::new(),
                implies: Vec::new(),
                activate_calls: StdArc::new(AtomicU32::new(0)),
                deactivate_calls: StdArc::new(AtomicU32::new(0)),
            }
        }
        fn requires(mut self, caps: CapabilitySet) -> Self {
            self.required = caps;
            self
        }
        fn conflicting_with(mut self, other: ModeId) -> Self {
            self.conflicts.push(other);
            self
        }
        fn implying(mut self, other: ModeId) -> Self {
            self.implies.push(other);
            self
        }
    }

    impl Mode for MockMode {
        fn id(&self) -> ModeId {
            self.id
        }
        fn kind(&self) -> ModeKind {
            self.kind
        }
        fn required_capabilities(&self) -> CapabilitySet {
            self.required
        }
        fn conflicts_with(&self) -> &[ModeId] {
            &self.conflicts
        }
        fn implies(&self) -> &[ModeId] {
            &self.implies
        }
        fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
            self.activate_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
            self.deactivate_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn buf() -> BufferId {
        BufferId::new(1)
    }

    /// Test fixture: fresh empty BufferLocals. M.3.2.a wires
    /// every activation method to take `&mut BufferLocals`;
    /// tests that don't care about locals just construct an
    /// empty one and pass `&mut l`.
    fn locals() -> BufferLocals {
        BufferLocals::new()
    }

    /// Test fixture: a fresh empty typed-options registry, the
    /// minimum needed by [`ModeContext::new`]. Tests that don't
    /// exercise option mutation can just pass `&cfg()` once at
    /// the start.
    fn cfg() -> lattice_config::ConfigRegistry {
        lattice_config::ConfigRegistry::new()
    }

    /// Test fixture: a fresh `EventBus` wrapped in `Arc`, the
    /// minimum needed by [`ModeContext::new`]. Tests that don't
    /// exercise typed-event publication just pass `&events()`.
    fn events() -> std::sync::Arc<lattice_runtime::EventBus> {
        std::sync::Arc::new(lattice_runtime::EventBus::new())
    }

    /// Test fixture: empty service registry. Modes that don't
    /// pull from `ctx.service::<T>()` ignore it; tests that do
    /// (Phase 3 LspMode tests in `lattice-lsp`) build their
    /// own pre-populated registry.
    fn svc() -> crate::services::ServiceRegistry {
        crate::services::ServiceRegistry::new()
    }

    #[test]
    fn register_and_lookup() {
        let mut r = ModeRegistry::new();
        let id = r.register(MockMode::major("rust-mode")).unwrap();
        assert!(r.is_registered(id));
        assert_eq!(r.get(id).unwrap().kind(), ModeKind::Major);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn duplicate_registration_fails() {
        let mut r = ModeRegistry::new();
        r.register(MockMode::major("rust-mode")).unwrap();
        let err = r.register(MockMode::major("rust-mode")).unwrap_err();
        assert!(matches!(err, RegistrationError::Duplicate(_)));
    }

    #[test]
    fn activate_unregistered_major_fails() {
        let r = ModeRegistry::new();
        let mut a = ActiveModes::new();
        let mut l = locals();
        let err = r
            .activate_major(
                &mut a,
                &mut l,
                &cfg(),
                &events(),
                &svc(),
                buf(),
                ModeId::new("ghost-mode"),
                CapabilitySet::empty(),
            )
            .unwrap_err();
        assert!(matches!(err, ModeActivationError::NotRegistered(_)));
    }

    #[test]
    fn activate_wrong_kind_fails() {
        let mut r = ModeRegistry::new();
        let id = r.register(MockMode::minor("read-only-mode")).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        let err = r
            .activate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap_err();
        assert!(matches!(err, ModeActivationError::WrongKind { .. }));
    }

    #[test]
    fn missing_capability_blocks_activation() {
        let mut r = ModeRegistry::new();
        let id = r
            .register(MockMode::minor("lsp-mode").requires(CapabilitySet::LSP))
            .unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        let err = r
            .activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap_err();
        match err {
            ModeActivationError::MissingCapability { mode, missing } => {
                assert_eq!(mode, id);
                assert_eq!(missing, CapabilitySet::LSP);
            }
            _ => panic!("expected MissingCapability"),
        }
    }

    #[test]
    fn major_activation_emits_entered_event_and_calls_lifecycle() {
        let mock = MockMode::major("rust-mode");
        let activate_counter = mock.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        let events = r
            .activate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ModeEvent::MajorEntered { mode, .. } if mode == id));
        assert_eq!(activate_counter.load(Ordering::SeqCst), 1);
        assert_eq!(a.major(), Some(id));
    }

    #[test]
    fn major_swap_runs_deactivate_then_activate_in_order() {
        let prev = MockMode::major("text-mode");
        let new = MockMode::major("rust-mode");
        let prev_deact = prev.deactivate_calls.clone();
        let new_act = new.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let prev_id = r.register(prev).unwrap();
        let new_id = r.register(new).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        r.activate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), prev_id, CapabilitySet::empty())
            .unwrap();
        let events = r
            .activate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), new_id, CapabilitySet::empty())
            .unwrap();
        // Expected event order: MajorExiting(prev), MajorEntered(new).
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ModeEvent::MajorExiting { mode, .. } if mode == prev_id));
        assert!(matches!(events[1], ModeEvent::MajorEntered { mode, .. } if mode == new_id));
        assert_eq!(prev_deact.load(Ordering::SeqCst), 1);
        assert_eq!(new_act.load(Ordering::SeqCst), 1);
        assert_eq!(a.major(), Some(new_id));
    }

    #[test]
    fn major_reload_runs_deactivate_then_reactivate() {
        let mock = MockMode::major("rust-mode");
        let act = mock.activate_calls.clone();
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        r.activate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap();
        // Reload: activate the same major again.
        r.activate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap();
        assert_eq!(act.load(Ordering::SeqCst), 2);
        assert_eq!(deact.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn minor_activation_appends_in_order() {
        let mut r = ModeRegistry::new();
        let one = r.register(MockMode::minor("a-mode")).unwrap();
        let two = r.register(MockMode::minor("b-mode")).unwrap();
        let three = r.register(MockMode::minor("c-mode")).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        r.activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), one, CapabilitySet::empty())
            .unwrap();
        r.activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), two, CapabilitySet::empty())
            .unwrap();
        r.activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), three, CapabilitySet::empty())
            .unwrap();
        assert_eq!(a.minors(), &[one, two, three]);
    }

    #[test]
    fn minor_re_activation_is_noop() {
        let mock = MockMode::minor("a-mode");
        let act = mock.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        r.activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap();
        let events = r
            .activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap();
        assert!(events.is_empty(), "double-activation should be no-op");
        assert_eq!(act.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn implies_auto_activates_dependency() {
        // relative-line-numbers-mode implies line-numbers-mode.
        let mut r = ModeRegistry::new();
        let lnum = r.register(MockMode::minor("line-numbers-mode")).unwrap();
        let rlnum = r
            .register(MockMode::minor("relative-line-numbers-mode").implying(lnum))
            .unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        let events = r
            .activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), rlnum, CapabilitySet::empty())
            .unwrap();
        // Two events: parent first, then implied dep.
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ModeEvent::MinorActivated { mode, .. } if mode == rlnum));
        assert!(matches!(events[1], ModeEvent::MinorActivated { mode, .. } if mode == lnum));
        assert!(a.has_minor(rlnum));
        assert!(a.has_minor(lnum));
    }

    #[test]
    fn implies_unregistered_dependency_fails() {
        let mut r = ModeRegistry::new();
        let phantom = ModeId::new("ghost-mode");
        let id = r
            .register(MockMode::minor("thing-mode").implying(phantom))
            .unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        let err = r
            .activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap_err();
        assert!(matches!(err, ModeActivationError::UnregisteredDependency { .. }));
    }

    #[test]
    fn conflict_blocks_activation() {
        let mut r = ModeRegistry::new();
        let one_id = ModeId::new("vim-paste-mode");
        let two_id = ModeId::new("auto-pair-mode");
        let one = r
            .register(MockMode::minor("vim-paste-mode").conflicting_with(two_id))
            .unwrap();
        let two = r.register(MockMode::minor("auto-pair-mode")).unwrap();
        assert_eq!(one, one_id);
        assert_eq!(two, two_id);
        let mut a = ActiveModes::new();
        let mut l = locals();
        r.activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), two, CapabilitySet::empty())
            .unwrap();
        let err = r
            .activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), one, CapabilitySet::empty())
            .unwrap_err();
        match err {
            ModeActivationError::Conflict { mode, active } => {
                assert_eq!(mode, one);
                assert_eq!(active, two);
            }
            _ => panic!("expected Conflict"),
        }
    }

    #[test]
    fn conflict_is_symmetric() {
        // Even if A doesn't list B as conflict, B listing A
        // should still reject A's later activation.
        let mut r = ModeRegistry::new();
        let one_id = ModeId::new("a-mode");
        let _two = r
            .register(MockMode::minor("b-mode").conflicting_with(one_id))
            .unwrap();
        let one = r.register(MockMode::minor("a-mode")).unwrap();
        let two = ModeId::new("b-mode");
        assert_eq!(one, one_id);
        let mut a = ActiveModes::new();
        let mut l = locals();
        r.activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), two, CapabilitySet::empty())
            .unwrap();
        // Now activating `one` should fail because `two`
        // declared `one` as a conflict.
        let err = r
            .activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), one, CapabilitySet::empty())
            .unwrap_err();
        assert!(matches!(err, ModeActivationError::Conflict { .. }));
    }

    #[test]
    fn deactivate_minor_emits_event_and_calls_lifecycle() {
        let mock = MockMode::minor("a-mode");
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        r.activate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap();
        let events = r.deactivate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ModeEvent::MinorDeactivated { mode, .. } if mode == id));
        assert_eq!(deact.load(Ordering::SeqCst), 1);
        assert!(!a.has_minor(id));
    }

    #[test]
    fn deactivate_inactive_minor_is_noop() {
        let mut r = ModeRegistry::new();
        let id = r.register(MockMode::minor("a-mode")).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        let events = r.deactivate_minor(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn deactivate_major_runs_lifecycle_and_clears() {
        let mock = MockMode::major("rust-mode");
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut l = locals();
        r.activate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf(), id, CapabilitySet::empty())
            .unwrap();
        let events = r.deactivate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf()).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ModeEvent::MajorExiting { mode, .. } if mode == id));
        assert_eq!(deact.load(Ordering::SeqCst), 1);
        assert_eq!(a.major(), None);
    }

    #[test]
    fn deactivate_major_when_none_active_is_noop() {
        let r = ModeRegistry::new();
        let mut a = ActiveModes::new();
        let mut l = locals();
        let events = r.deactivate_major(&mut a, &mut l, &cfg(), &events(), &svc(), buf()).unwrap();
        assert!(events.is_empty());
    }
}
