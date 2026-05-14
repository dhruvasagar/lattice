//! `ModeRegistry`: register modes, look them up, drive
//! activation / deactivation against a per-buffer `ActiveModes`
//! and a per-App [`GuardStore`].
//!
//! M-async.1 keeps the registry **bus-agnostic**: activation and
//! deactivation methods return the events they would have
//! published as a `Vec<ModeEvent>`, instead of dispatching to
//! the typed event bus directly. The caller forwards events on
//! to the bus. M-async.2 swaps this for spawn-based dispatch,
//! at which point the registry publishes events from the
//! spawned task and the return type becomes `Result<(), _>`.
//!
//! Validation order before activation:
//! 1. Mode is registered (`ModeActivationError::NotRegistered`).
//! 2. Mode kind matches the call (`WrongKind`).
//! 3. Buffer satisfies required capabilities
//!    (`MissingCapability`).
//! 4. No conflict with already-active modes (`Conflict`).
//! 5. All `implies` dependencies are registered
//!    (`UnregisteredDependency`).
//! 6. `on_activate` runs; lifecycle errors surface as
//!    `LifecycleFailed`. On success the returned Guard is
//!    stashed in the [`GuardStore`] keyed by `(buffer, mode)`.
//! 7. Declarative contributions are applied (option overrides,
//!    keymap layer, subscriptions, decorations).
//!
//! Deactivation removes the Guard from the store; dropping the
//! `Box<dyn Any + Send>` invokes the original Guard's `Drop`
//! impl, which performs every cleanup action. There is no
//! `on_deactivate` -- Drop is the cleanup contract.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use lattice_protocol::ids::BufferId;

use crate::active::ActiveModes;
use crate::capability::CapabilitySet;
use crate::context::ModeContext;
use crate::error::ModeActivationError;
use crate::event::ModeEvent;
use crate::guards::GuardStore;
use crate::mode::{DynMode, Mode, ModeId, ModeKind};

/// Why a registration failed.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RegistrationError {
    #[error("mode `{0}` is already registered")]
    Duplicate(ModeId),
}

/// Mode registry. Owns the catalogue of registered modes
/// (`Arc<dyn DynMode>`) and drives activation / deactivation.
///
/// `Clone` is cheap: each entry is an `Arc<dyn DynMode>` so the
/// HashMap clone is shallow over the values. Used by tests that
/// register a test-only mode post-boot via
/// `Arc::make_mut(&mut app.mode_registry)`; production code
/// constructs the registry once at boot and never clones.
#[derive(Clone)]
pub struct ModeRegistry {
    modes: HashMap<ModeId, Arc<dyn DynMode>>,
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
        let id = <M as Mode>::id(&mode);
        if self.modes.contains_key(&id) {
            return Err(RegistrationError::Duplicate(id));
        }
        // `Arc::new(mode)` constructs `Arc<M>`; the coercion to
        // `Arc<dyn DynMode>` uses the blanket `impl<M: Mode>
        // DynMode for M`.
        let arc: Arc<dyn DynMode> = Arc::new(mode);
        self.modes.insert(id, arc);
        Ok(id)
    }

    /// True iff this id is registered (any kind).
    pub fn is_registered(&self, id: ModeId) -> bool {
        self.modes.contains_key(&id)
    }

    /// Look up a registered mode by id.
    pub fn get(&self, id: ModeId) -> Option<Arc<dyn DynMode>> {
        self.modes.get(&id).cloned()
    }

    /// Iterate every registered mode's `(id, kind)`. Used at boot
    /// by the App to auto-generate toggle ex-commands per mode-
    /// architecture §9.6.1.
    pub fn iter_meta(&self) -> impl Iterator<Item = (ModeId, ModeKind)> + '_ {
        self.modes.iter().map(|(id, mode)| (*id, mode.kind()))
    }

    pub fn len(&self) -> usize {
        self.modes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modes.is_empty()
    }

    /// Activate a major mode on `buffer`. If a different major
    /// is currently active, it is deactivated first (the
    /// previous major's Guard is dropped, `MajorExiting` fires,
    /// then the new major's `on_activate` runs and `MajorEntered`
    /// fires). Idempotent: activating the already-active major
    /// triggers a *reload* (deactivate then re-activate, per
    /// `mode-architecture.md` §9.6).
    ///
    /// `caps` is the buffer's current capability set. The new
    /// mode's `required_capabilities` must be a subset.
    ///
    /// Implied modes (`Mode::implies`) are auto-activated as
    /// minors after the major lands.
    #[allow(clippy::too_many_arguments)]
    pub fn activate_major(
        &self,
        active: &mut ActiveModes,
        guards: &mut GuardStore,
        config: &Arc<lattice_config::ConfigRegistry>,
        events: &Arc<lattice_runtime::EventBus>,
        services: &Arc<crate::services::ServiceRegistry>,
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

        // Tear down current major (if any). `MajorExiting`
        // fires BEFORE the Guard drops (per §7).
        if let Some(prev_id) = active.major() {
            emitted.push(ModeEvent::MajorExiting {
                buffer,
                mode: prev_id,
            });
            // Drop the previous major's Guard. The Box<dyn Any>
            // goes out of scope here; its Drop fires the
            // original Guard's Drop impl.
            let _ = guards.remove(buffer, prev_id);
            active.set_major(None);
        }

        // Run the new major's on_activate, build ctx and drive
        // the lifecycle future synchronously (M-async.1
        // contract: futures complete on first poll; M-async.2
        // swaps for runtime-spawned `.await`).
        let ctx = ModeContext::new(
            buffer,
            mode,
            config.clone(),
            events.clone(),
            services.clone(),
        );
        let guard = poll_now(entry.on_activate_dyn(ctx))?;
        guards.insert(buffer, mode, guard);
        active.set_major(Some(mode));
        emitted.push(ModeEvent::MajorEntered { buffer, mode });

        // Auto-activate implied minors (recursive).
        for &dep in entry.implies() {
            if !self.is_registered(dep) {
                return Err(ModeActivationError::UnregisteredDependency { mode, dep });
            }
            if active.has_minor(dep) {
                continue;
            }
            let dep_events = self.activate_minor_inner(
                active, guards, config, events, services, buffer, dep, caps,
            )?;
            emitted.extend(dep_events);
        }

        Ok(emitted)
    }

    /// Activate a minor mode on `buffer`. Validates capabilities,
    /// conflicts, and dependency presence.
    #[allow(clippy::too_many_arguments)]
    pub fn activate_minor(
        &self,
        active: &mut ActiveModes,
        guards: &mut GuardStore,
        config: &Arc<lattice_config::ConfigRegistry>,
        events: &Arc<lattice_runtime::EventBus>,
        services: &Arc<crate::services::ServiceRegistry>,
        buffer: BufferId,
        mode: ModeId,
        caps: CapabilitySet,
    ) -> Result<Vec<ModeEvent>, ModeActivationError> {
        self.activate_minor_inner(active, guards, config, events, services, buffer, mode, caps)
    }

    #[allow(clippy::too_many_arguments)]
    fn activate_minor_inner(
        &self,
        active: &mut ActiveModes,
        guards: &mut GuardStore,
        config: &Arc<lattice_config::ConfigRegistry>,
        events: &Arc<lattice_runtime::EventBus>,
        services: &Arc<crate::services::ServiceRegistry>,
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
        // major-mode-specific contract).
        if active.has_minor(mode) {
            return Ok(Vec::new());
        }
        let missing = entry.required_capabilities() - caps;
        if !missing.is_empty() {
            return Err(ModeActivationError::MissingCapability { mode, missing });
        }
        // Conflict check: if any active mode (major or minor)
        // is in this mode's conflicts list, reject. Symmetric.
        for &c in entry.conflicts_with() {
            if active.is_active(c) {
                return Err(ModeActivationError::Conflict { mode, active: c });
            }
        }
        if let Some(major) = active.major()
            && let Some(major_entry) = self.modes.get(&major)
            && major_entry.conflicts_with().contains(&mode)
        {
            return Err(ModeActivationError::Conflict {
                mode,
                active: major,
            });
        }
        for &active_minor in active.minors() {
            if let Some(minor_entry) = self.modes.get(&active_minor)
                && minor_entry.conflicts_with().contains(&mode)
            {
                return Err(ModeActivationError::Conflict {
                    mode,
                    active: active_minor,
                });
            }
        }

        let ctx = ModeContext::new(
            buffer,
            mode,
            config.clone(),
            events.clone(),
            services.clone(),
        );
        let guard = poll_now(entry.on_activate_dyn(ctx))?;
        guards.insert(buffer, mode, guard);
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
            let dep_events = self.activate_minor_inner(
                active, guards, config, events, services, buffer, dep, caps,
            )?;
            emitted.extend(dep_events);
        }

        Ok(emitted)
    }

    /// Deactivate a minor mode. Idempotent: deactivating an
    /// already-inactive mode is a no-op (returns empty events).
    /// Drops the Guard from the store; the Guard's `Drop` impl
    /// performs cleanup synchronously (async cleanup uses
    /// `lattice_runtime::spawn_task` fire-and-forget inside
    /// the Guard's `Drop` -- the dispatcher returns
    /// immediately).
    #[allow(clippy::too_many_arguments)]
    pub fn deactivate_minor(
        &self,
        active: &mut ActiveModes,
        guards: &mut GuardStore,
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
        // Collect the implies list before dropping the Guard so
        // a Guard's Drop impl (which may touch its own implies)
        // can't see a half-cleaned set.
        let implies: Vec<ModeId> = entry.implies().to_vec();
        // Drop the Guard. Box<dyn Any + Send> goes out of scope
        // here; the original Guard type's Drop runs.
        let _ = guards.remove(buffer, mode);
        active.remove_minor(mode);
        let mut emitted = vec![ModeEvent::MinorDeactivated { buffer, mode }];
        // Cascade-deactivate every implied minor that's still
        // active.
        for &dep in &implies {
            if !active.has_minor(dep) {
                continue;
            }
            let dep_events = self.deactivate_minor(active, guards, buffer, dep)?;
            emitted.extend(dep_events);
        }
        Ok(emitted)
    }

    /// Deactivate the active major mode (if any). Returns
    /// empty events if no major is active.
    pub fn deactivate_major(
        &self,
        active: &mut ActiveModes,
        guards: &mut GuardStore,
        buffer: BufferId,
    ) -> Result<Vec<ModeEvent>, ModeActivationError> {
        let Some(mode) = active.major() else {
            return Ok(Vec::new());
        };
        let _ = self
            .modes
            .get(&mode)
            .ok_or(ModeActivationError::NotRegistered(mode))?;
        let result_events = vec![ModeEvent::MajorExiting { buffer, mode }];
        let _ = guards.remove(buffer, mode);
        active.set_major(None);
        Ok(result_events)
    }
}

/// Drive an immediately-ready future to completion. M-async.1
/// contract: lifecycle futures complete on first poll
/// (`Box::pin(async { Ok(()) })` for marker modes; sync work
/// wrapped in an async block for stateful modes). M-async.2
/// swaps this for `lattice_runtime::spawn_task`.
///
/// Panics on `Poll::Pending` -- the M-async.1 immediate-ready
/// contract is violated. A mode that needs to `.await` real
/// async work (LSP supervisor handshake, watcher init) lands
/// in M-async.2.
fn poll_now<F: Future>(fut: F) -> F::Output {
    static VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(std::ptr::null(), &VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );
    let raw_waker = RawWaker::new(std::ptr::null(), &VTABLE);
    // SAFETY: the vtable is fully static and the functions are
    // valid (no-op clone returns the same waker; wake/wake_by_ref/
    // drop are no-ops on a null context).
    #[allow(unsafe_code)]
    let waker = unsafe { Waker::from_raw(raw_waker) };
    let mut cx = Context::from_waker(&waker);
    let mut fut: Pin<Box<F>> = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(out) => out,
        Poll::Pending => panic!(
            "poll_now: lifecycle future is Pending -- M-async.1 contract requires immediate readiness; \
             M-async.2 swaps for runtime-spawned drive"
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::mode::LifecycleFuture;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Test mode with a typed Guard that records drop count.
    /// Counts both activate (Guard::new) and deactivate (Drop).
    struct MockMode {
        id: ModeId,
        kind: ModeKind,
        required: CapabilitySet,
        conflicts: Vec<ModeId>,
        implies: Vec<ModeId>,
        activate_calls: StdArc<AtomicU32>,
        deactivate_calls: StdArc<AtomicU32>,
    }

    struct MockGuard {
        deactivate_calls: StdArc<AtomicU32>,
    }

    impl Drop for MockGuard {
        fn drop(&mut self) {
            self.deactivate_calls.fetch_add(1, Ordering::SeqCst);
        }
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
        type Guard = MockGuard;
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
        fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
            self.activate_calls.fetch_add(1, Ordering::SeqCst);
            let deact = self.deactivate_calls.clone();
            Box::pin(async move {
                Ok(MockGuard {
                    deactivate_calls: deact,
                })
            })
        }
    }

    fn buf() -> BufferId {
        BufferId::new(1)
    }

    fn cfg() -> Arc<lattice_config::ConfigRegistry> {
        Arc::new(lattice_config::ConfigRegistry::new())
    }

    fn evts() -> Arc<lattice_runtime::EventBus> {
        Arc::new(lattice_runtime::EventBus::new())
    }

    fn svcs() -> Arc<crate::services::ServiceRegistry> {
        Arc::new(crate::services::ServiceRegistry::new())
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
        let mut g = GuardStore::new();
        let err = r
            .activate_major(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
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
        let mut g = GuardStore::new();
        let err = r
            .activate_major(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                id,
                CapabilitySet::empty(),
            )
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
        let mut g = GuardStore::new();
        let err = r
            .activate_minor(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                id,
                CapabilitySet::empty(),
            )
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
    fn major_activation_emits_entered_and_stashes_guard() {
        let mock = MockMode::major("rust-mode");
        let act = mock.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        let events = r
            .activate_major(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                id,
                CapabilitySet::empty(),
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ModeEvent::MajorEntered { mode, .. } if mode == id));
        assert_eq!(act.load(Ordering::SeqCst), 1);
        assert_eq!(a.major(), Some(id));
        assert!(g.contains(buf(), id), "guard must be stashed");
    }

    #[test]
    fn major_swap_drops_prev_guard_then_activates_new() {
        let prev = MockMode::major("text-mode");
        let new = MockMode::major("rust-mode");
        let prev_deact = prev.deactivate_calls.clone();
        let new_act = new.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let prev_id = r.register(prev).unwrap();
        let new_id = r.register(new).unwrap();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        r.activate_major(
            &mut a,
            &mut g,
            &cfg(),
            &evts(),
            &svcs(),
            buf(),
            prev_id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let events = r
            .activate_major(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                new_id,
                CapabilitySet::empty(),
            )
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ModeEvent::MajorExiting { mode, .. } if mode == prev_id));
        assert!(matches!(events[1], ModeEvent::MajorEntered { mode, .. } if mode == new_id));
        // Previous Guard's Drop ran (= cleanup contract).
        assert_eq!(prev_deact.load(Ordering::SeqCst), 1);
        assert_eq!(new_act.load(Ordering::SeqCst), 1);
        assert_eq!(a.major(), Some(new_id));
        assert!(g.contains(buf(), new_id));
        assert!(!g.contains(buf(), prev_id));
    }

    #[test]
    fn major_reload_drops_then_reactivates() {
        let mock = MockMode::major("rust-mode");
        let act = mock.activate_calls.clone();
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        r.activate_major(
            &mut a,
            &mut g,
            &cfg(),
            &evts(),
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        r.activate_major(
            &mut a,
            &mut g,
            &cfg(),
            &evts(),
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
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
        let mut g = GuardStore::new();
        for id in [one, two, three] {
            r.activate_minor(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                id,
                CapabilitySet::empty(),
            )
            .unwrap();
        }
        assert_eq!(a.minors(), &[one, two, three]);
    }

    #[test]
    fn minor_re_activation_is_noop() {
        let mock = MockMode::minor("a-mode");
        let act = mock.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        r.activate_minor(
            &mut a,
            &mut g,
            &cfg(),
            &evts(),
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let events = r
            .activate_minor(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                id,
                CapabilitySet::empty(),
            )
            .unwrap();
        assert!(events.is_empty(), "double-activation should be no-op");
        assert_eq!(act.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn implies_auto_activates_dependency() {
        let mut r = ModeRegistry::new();
        let lnum = r.register(MockMode::minor("line-numbers-mode")).unwrap();
        let rlnum = r
            .register(MockMode::minor("relative-line-numbers-mode").implying(lnum))
            .unwrap();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        let events = r
            .activate_minor(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                rlnum,
                CapabilitySet::empty(),
            )
            .unwrap();
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
        let mut g = GuardStore::new();
        let err = r
            .activate_minor(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                id,
                CapabilitySet::empty(),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ModeActivationError::UnregisteredDependency { .. }
        ));
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
        let mut g = GuardStore::new();
        r.activate_minor(
            &mut a,
            &mut g,
            &cfg(),
            &evts(),
            &svcs(),
            buf(),
            two,
            CapabilitySet::empty(),
        )
        .unwrap();
        let err = r
            .activate_minor(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                one,
                CapabilitySet::empty(),
            )
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
        let mut r = ModeRegistry::new();
        let one_id = ModeId::new("a-mode");
        let _two = r
            .register(MockMode::minor("b-mode").conflicting_with(one_id))
            .unwrap();
        let one = r.register(MockMode::minor("a-mode")).unwrap();
        let two = ModeId::new("b-mode");
        assert_eq!(one, one_id);
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        r.activate_minor(
            &mut a,
            &mut g,
            &cfg(),
            &evts(),
            &svcs(),
            buf(),
            two,
            CapabilitySet::empty(),
        )
        .unwrap();
        let err = r
            .activate_minor(
                &mut a,
                &mut g,
                &cfg(),
                &evts(),
                &svcs(),
                buf(),
                one,
                CapabilitySet::empty(),
            )
            .unwrap_err();
        assert!(matches!(err, ModeActivationError::Conflict { .. }));
    }

    #[test]
    fn deactivate_minor_drops_guard_and_emits_event() {
        let mock = MockMode::minor("a-mode");
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        r.activate_minor(
            &mut a,
            &mut g,
            &cfg(),
            &evts(),
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let events = r.deactivate_minor(&mut a, &mut g, buf(), id).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ModeEvent::MinorDeactivated { mode, .. } if mode == id));
        assert_eq!(deact.load(Ordering::SeqCst), 1, "Guard::Drop ran");
        assert!(!a.has_minor(id));
        assert!(!g.contains(buf(), id));
    }

    #[test]
    fn deactivate_inactive_minor_is_noop() {
        let mut r = ModeRegistry::new();
        let id = r.register(MockMode::minor("a-mode")).unwrap();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        let events = r.deactivate_minor(&mut a, &mut g, buf(), id).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn deactivate_major_drops_guard_and_clears() {
        let mock = MockMode::major("rust-mode");
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        r.activate_major(
            &mut a,
            &mut g,
            &cfg(),
            &evts(),
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let events = r.deactivate_major(&mut a, &mut g, buf()).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ModeEvent::MajorExiting { mode, .. } if mode == id));
        assert_eq!(deact.load(Ordering::SeqCst), 1);
        assert_eq!(a.major(), None);
        assert!(!g.contains(buf(), id));
    }

    #[test]
    fn deactivate_major_when_none_active_is_noop() {
        let r = ModeRegistry::new();
        let mut a = ActiveModes::new();
        let mut g = GuardStore::new();
        let events = r.deactivate_major(&mut a, &mut g, buf()).unwrap();
        assert!(events.is_empty());
    }
}
