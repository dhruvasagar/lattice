//! `ModeRegistry`: register modes, look them up, drive
//! activation / deactivation against a per-buffer `ActiveModes`
//! and an App-owned [`GuardStoreHandle`].
//!
//! M-async.2: activation is **spawn-based**. The sync prefix
//! validates the request and mutates `active_modes`; the
//! lifecycle future (`on_activate_dyn`) is spawned on the shared
//! runtime via [`lattice_runtime::spawn_task`]. The spawned task
//! awaits the future, stashes the Guard in the
//! [`GuardStoreHandle`], and publishes a typed [`ModeEvent`]
//! (`MajorEntered` / `MinorActivated` on success,
//! `ModeActivationFailed` on lifecycle error).
//!
//! Deactivation stays synchronous (Drop is sync): the
//! dispatcher locks the store, removes the Guard, drops it. The
//! `MajorExiting` / `MinorDeactivated` event publishes before
//! the drop fires.
//!
//! Validation order before spawning:
//! 1. Mode is registered (`ModeActivationError::NotRegistered`).
//! 2. Mode kind matches the call (`WrongKind`).
//! 3. Buffer satisfies required capabilities
//!    (`MissingCapability`).
//! 4. No conflict with already-active modes (`Conflict`).
//! 5. All `implies` dependencies are registered
//!    (`UnregisteredDependency`).
//!
//! Lifecycle errors (from `on_activate.await`) become
//! `ModeActivationFailed` events on the bus; the spawned task
//! never returns them to the caller (the caller already
//! returned `Ok(())`).
//!
//! M-async.3 follow-up: an App-side subscriber rolls back
//! `active_modes` on `ModeActivationFailed`. M-async.2 leaves
//! the mutation in place even on lifecycle error (the previous
//! `Vec<ModeEvent>` return type leaked the same way).

use std::collections::HashMap;
use std::sync::Arc;

use lattice_protocol::ids::BufferId;

use crate::active::ActiveModes;
use crate::capability::CapabilitySet;
use crate::context::ModeContext;
use crate::error::ModeActivationError;
use crate::event::ModeEvent;
use crate::guards::GuardStoreHandle;
use crate::mode::{DynMode, Mode, ModeId, ModeKind};

/// Why a registration failed.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RegistrationError {
    #[error("mode `{0}` is already registered")]
    Duplicate(ModeId),
}

/// Mode registry. Owns the catalogue of registered modes
/// (`Arc<dyn DynMode>`) and drives activation / deactivation.
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

    /// Register a mode. Same id twice is a `Duplicate` error.
    pub fn register<M: Mode>(&mut self, mode: M) -> Result<ModeId, RegistrationError> {
        let id = <M as Mode>::id(&mode);
        if self.modes.contains_key(&id) {
            return Err(RegistrationError::Duplicate(id));
        }
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

    /// Iterate every registered mode's `(id, kind)`.
    pub fn iter_meta(&self) -> impl Iterator<Item = (ModeId, ModeKind)> + '_ {
        self.modes.iter().map(|(id, mode)| (*id, mode.kind()))
    }

    pub fn len(&self) -> usize {
        self.modes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modes.is_empty()
    }

    /// Activate a major mode on `buffer`. Synchronous prefix:
    /// validate + mutate `active_modes`. The lifecycle future
    /// runs on a tokio worker via
    /// [`lattice_runtime::spawn_task`]; on resolve the spawned
    /// task stashes the Guard in `guards` and publishes
    /// `MajorEntered` (or `ModeActivationFailed` if `on_activate`
    /// returned `Err`).
    ///
    /// If a different major is currently active, it is
    /// deactivated synchronously first (Drop runs, `MajorExiting`
    /// publishes). Idempotent: reactivating the current major
    /// triggers a *reload* (deactivate then re-activate).
    ///
    /// Implied minors are spawned recursively after the major's
    /// sync prefix (M-async.3 makes the cascade await the
    /// parent's future before scheduling children; M-async.2
    /// schedules them in parallel).
    #[allow(clippy::too_many_arguments)]
    pub fn activate_major(
        &self,
        active: &mut ActiveModes,
        guards: &GuardStoreHandle,
        config: &Arc<lattice_config::ConfigRegistry>,
        events: &Arc<lattice_runtime::EventBus>,
        services: &Arc<crate::services::ServiceRegistry>,
        buffer: BufferId,
        mode: ModeId,
        caps: CapabilitySet,
    ) -> Result<(), ModeActivationError> {
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

        // Tear down current major (if any). `MajorExiting`
        // publishes BEFORE the Guard drops.
        if let Some(prev_id) = active.major() {
            events.publish_typed(ModeEvent::MajorExiting {
                buffer,
                mode: prev_id,
            });
            // Drop the previous Guard. Box<dyn Any + Send> goes
            // out of scope here; original Guard's Drop fires.
            let _ = guards.remove(buffer, prev_id);
            active.set_major(None);
        }

        // Sync prefix: mutate active_modes BEFORE spawning so
        // `App::active_modes.has_major(mode)` is `true` the
        // moment this call returns. The Guard isn't yet
        // stashed; lifecycle errors leave `active_modes`
        // mutated (M-async.3 rolls back via subscriber).
        active.set_major(Some(mode));

        // Spawn the lifecycle future. Note: cascade through
        // `implies()` is initiated synchronously here. M-async.2
        // schedules implied children in parallel with the
        // parent's future; M-async.3 sequences them after the
        // parent resolves.
        self.spawn_lifecycle(
            entry.clone(),
            guards.clone(),
            events.clone(),
            ctx_for(buffer, mode, config, events, services),
            buffer,
            mode,
            ModeKind::Major,
        );

        // Implied minors spawn alongside.
        for &dep in entry.implies() {
            if !self.is_registered(dep) {
                return Err(ModeActivationError::UnregisteredDependency { mode, dep });
            }
            if active.has_minor(dep) {
                continue;
            }
            self.activate_minor_inner(active, guards, config, events, services, buffer, dep, caps)?;
        }

        Ok(())
    }

    /// Activate a minor mode on `buffer`. Same sync-prefix-then-
    /// spawn shape as `activate_major`.
    #[allow(clippy::too_many_arguments)]
    pub fn activate_minor(
        &self,
        active: &mut ActiveModes,
        guards: &GuardStoreHandle,
        config: &Arc<lattice_config::ConfigRegistry>,
        events: &Arc<lattice_runtime::EventBus>,
        services: &Arc<crate::services::ServiceRegistry>,
        buffer: BufferId,
        mode: ModeId,
        caps: CapabilitySet,
    ) -> Result<(), ModeActivationError> {
        self.activate_minor_inner(active, guards, config, events, services, buffer, mode, caps)
    }

    #[allow(clippy::too_many_arguments)]
    fn activate_minor_inner(
        &self,
        active: &mut ActiveModes,
        guards: &GuardStoreHandle,
        config: &Arc<lattice_config::ConfigRegistry>,
        events: &Arc<lattice_runtime::EventBus>,
        services: &Arc<crate::services::ServiceRegistry>,
        buffer: BufferId,
        mode: ModeId,
        caps: CapabilitySet,
    ) -> Result<(), ModeActivationError> {
        let entry = self
            .modes
            .get(&mode)
            .ok_or(ModeActivationError::NotRegistered(mode))?;
        if entry.kind() != ModeKind::Minor {
            return Err(ModeActivationError::WrongKind { mode });
        }
        if active.has_minor(mode) {
            return Ok(());
        }
        let missing = entry.required_capabilities() - caps;
        if !missing.is_empty() {
            return Err(ModeActivationError::MissingCapability { mode, missing });
        }
        // Conflict checks.
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

        // Sync prefix: append to active_modes BEFORE spawning.
        active.push_minor(mode);

        self.spawn_lifecycle(
            entry.clone(),
            guards.clone(),
            events.clone(),
            ctx_for(buffer, mode, config, events, services),
            buffer,
            mode,
            ModeKind::Minor,
        );

        // Recurse into implied minors.
        for &dep in entry.implies() {
            if !self.is_registered(dep) {
                return Err(ModeActivationError::UnregisteredDependency { mode, dep });
            }
            if active.has_minor(dep) {
                continue;
            }
            self.activate_minor_inner(active, guards, config, events, services, buffer, dep, caps)?;
        }

        Ok(())
    }

    /// Deactivate a minor mode. Synchronous: locks `guards`,
    /// removes + drops the Guard, publishes
    /// `MinorDeactivated`. Idempotent.
    ///
    /// `MinorDeactivated` publishes BEFORE the Guard drops so
    /// subscribers can inspect the state about to be torn down.
    pub fn deactivate_minor(
        &self,
        active: &mut ActiveModes,
        guards: &GuardStoreHandle,
        events: &Arc<lattice_runtime::EventBus>,
        buffer: BufferId,
        mode: ModeId,
    ) -> Result<(), ModeActivationError> {
        if !active.has_minor(mode) {
            return Ok(());
        }
        let entry = self
            .modes
            .get(&mode)
            .ok_or(ModeActivationError::NotRegistered(mode))?;
        let implies: Vec<ModeId> = entry.implies().to_vec();
        events.publish_typed(ModeEvent::MinorDeactivated { buffer, mode });
        // Drop the Guard. Box<dyn Any + Send> goes out of scope
        // *after* the lock releases (the `let _ = ...` binding
        // owns it briefly).
        let _ = guards.remove(buffer, mode);
        active.remove_minor(mode);
        // Cascade-deactivate every implied minor that's still
        // active.
        for &dep in &implies {
            if !active.has_minor(dep) {
                continue;
            }
            self.deactivate_minor(active, guards, events, buffer, dep)?;
        }
        Ok(())
    }

    /// Deactivate the active major mode (if any). Synchronous.
    pub fn deactivate_major(
        &self,
        active: &mut ActiveModes,
        guards: &GuardStoreHandle,
        events: &Arc<lattice_runtime::EventBus>,
        buffer: BufferId,
    ) -> Result<(), ModeActivationError> {
        let Some(mode) = active.major() else {
            return Ok(());
        };
        let _ = self
            .modes
            .get(&mode)
            .ok_or(ModeActivationError::NotRegistered(mode))?;
        events.publish_typed(ModeEvent::MajorExiting { buffer, mode });
        let _ = guards.remove(buffer, mode);
        active.set_major(None);
        Ok(())
    }

    /// Spawn the lifecycle future for `entry` on the shared
    /// runtime. The spawned task awaits the future, stashes the
    /// Guard on success, and publishes a typed `ModeEvent`.
    fn spawn_lifecycle(
        &self,
        entry: Arc<dyn DynMode>,
        guards: GuardStoreHandle,
        events: Arc<lattice_runtime::EventBus>,
        ctx: ModeContext,
        buffer: BufferId,
        mode: ModeId,
        kind: ModeKind,
    ) {
        lattice_runtime::spawn_task(async move {
            match entry.on_activate_dyn(ctx).await {
                Ok(guard) => {
                    guards.insert(buffer, mode, guard);
                    let evt = match kind {
                        ModeKind::Major => ModeEvent::MajorEntered { buffer, mode },
                        ModeKind::Minor => ModeEvent::MinorActivated { buffer, mode },
                    };
                    events.publish_typed(evt);
                }
                Err(err) => {
                    events.publish_typed(ModeEvent::activation_failed(buffer, mode, &err));
                }
            }
        });
    }
}

/// Build a `ModeContext` for a `(buffer, mode)` activation.
/// Inline because the dispatcher constructs one per spawned
/// task; the registry-method body would otherwise repeat the
/// same 5-arg constructor call.
fn ctx_for(
    buffer: BufferId,
    mode: ModeId,
    config: &Arc<lattice_config::ConfigRegistry>,
    events: &Arc<lattice_runtime::EventBus>,
    services: &Arc<crate::services::ServiceRegistry>,
) -> ModeContext {
    ModeContext::new(
        buffer,
        mode,
        config.clone(),
        events.clone(),
        services.clone(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::mode::LifecycleFuture;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::mpsc::UnboundedReceiver;

    /// Test mode with a typed Guard that records drop count.
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

    /// Subscribe to `ModeEvent` on `bus`; return the receiver.
    fn subscribe_mode_events(bus: &lattice_runtime::EventBus) -> UnboundedReceiver<ModeEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        bus.subscribe_typed::<ModeEvent>(tx);
        rx
    }

    /// Drain the spawned lifecycle task: yield to the runtime
    /// until the task completes (recv() on the bus channel returns).
    async fn await_event(rx: &mut UnboundedReceiver<ModeEvent>) -> ModeEvent {
        rx.recv()
            .await
            .expect("bus channel should deliver the event")
    }

    #[tokio::test]
    async fn register_and_lookup() {
        let mut r = ModeRegistry::new();
        let id = r.register(MockMode::major("rust-mode")).unwrap();
        assert!(r.is_registered(id));
        assert_eq!(r.get(id).unwrap().kind(), ModeKind::Major);
        assert_eq!(r.len(), 1);
    }

    #[tokio::test]
    async fn duplicate_registration_fails() {
        let mut r = ModeRegistry::new();
        r.register(MockMode::major("rust-mode")).unwrap();
        let err = r.register(MockMode::major("rust-mode")).unwrap_err();
        assert!(matches!(err, RegistrationError::Duplicate(_)));
    }

    #[tokio::test]
    async fn activate_unregistered_major_fails() {
        let r = ModeRegistry::new();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let err = r
            .activate_major(
                &mut a,
                &g,
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

    #[tokio::test]
    async fn activate_wrong_kind_fails() {
        let mut r = ModeRegistry::new();
        let id = r.register(MockMode::minor("read-only-mode")).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let err = r
            .activate_major(
                &mut a,
                &g,
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

    #[tokio::test]
    async fn missing_capability_blocks_activation() {
        let mut r = ModeRegistry::new();
        let id = r
            .register(MockMode::minor("lsp-mode").requires(CapabilitySet::LSP))
            .unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let err = r
            .activate_minor(
                &mut a,
                &g,
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

    #[tokio::test]
    async fn major_activation_publishes_entered_and_stashes_guard() {
        let mock = MockMode::major("rust-mode");
        let act = mock.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.activate_major(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        // Sync prefix mutated `active_modes` immediately.
        assert_eq!(a.major(), Some(id));
        // Lifecycle task publishes MajorEntered when on_activate
        // resolves.
        let evt = await_event(&mut rx).await;
        assert!(matches!(evt, ModeEvent::MajorEntered { mode, .. } if mode == id));
        assert_eq!(act.load(Ordering::SeqCst), 1);
        assert!(g.contains(buf(), id), "Guard stashed by spawned task");
    }

    #[tokio::test]
    async fn major_swap_publishes_exiting_then_entered() {
        let prev = MockMode::major("text-mode");
        let new = MockMode::major("rust-mode");
        let prev_deact = prev.deactivate_calls.clone();
        let new_act = new.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let prev_id = r.register(prev).unwrap();
        let new_id = r.register(new).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        // First major: drain the MajorEntered.
        r.activate_major(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            prev_id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let evt = await_event(&mut rx).await;
        assert!(matches!(evt, ModeEvent::MajorEntered { mode, .. } if mode == prev_id));
        // Swap.
        r.activate_major(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            new_id,
            CapabilitySet::empty(),
        )
        .unwrap();
        // MajorExiting fires synchronously; MajorEntered after
        // the spawned task resolves.
        let exiting = await_event(&mut rx).await;
        assert!(matches!(exiting, ModeEvent::MajorExiting { mode, .. } if mode == prev_id));
        let entered = await_event(&mut rx).await;
        assert!(matches!(entered, ModeEvent::MajorEntered { mode, .. } if mode == new_id));
        // Previous Guard's Drop ran (Drop = cleanup contract).
        assert_eq!(prev_deact.load(Ordering::SeqCst), 1);
        assert_eq!(new_act.load(Ordering::SeqCst), 1);
        assert_eq!(a.major(), Some(new_id));
        assert!(g.contains(buf(), new_id));
        assert!(!g.contains(buf(), prev_id));
    }

    #[tokio::test]
    async fn major_reload_drops_then_reactivates() {
        let mock = MockMode::major("rust-mode");
        let act = mock.activate_calls.clone();
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.activate_major(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let _entered = await_event(&mut rx).await;
        r.activate_major(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let _exiting = await_event(&mut rx).await;
        let _re_entered = await_event(&mut rx).await;
        assert_eq!(act.load(Ordering::SeqCst), 2);
        assert_eq!(deact.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn minor_activation_appends_in_order() {
        let mut r = ModeRegistry::new();
        let one = r.register(MockMode::minor("a-mode")).unwrap();
        let two = r.register(MockMode::minor("b-mode")).unwrap();
        let three = r.register(MockMode::minor("c-mode")).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        for id in [one, two, three] {
            r.activate_minor(
                &mut a,
                &g,
                &cfg(),
                &bus,
                &svcs(),
                buf(),
                id,
                CapabilitySet::empty(),
            )
            .unwrap();
            let _activated = await_event(&mut rx).await;
        }
        assert_eq!(a.minors(), &[one, two, three]);
    }

    #[tokio::test]
    async fn minor_re_activation_is_noop() {
        let mock = MockMode::minor("a-mode");
        let act = mock.activate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.activate_minor(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let _first = await_event(&mut rx).await;
        // Second call: idempotent no-op; on_activate not called again.
        r.activate_minor(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        // Give the runtime a tick to confirm no second event fires.
        tokio::task::yield_now().await;
        assert!(
            rx.try_recv().is_err(),
            "double-activation should not publish a second event"
        );
        assert_eq!(act.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn implies_auto_activates_dependency() {
        let mut r = ModeRegistry::new();
        let lnum = r.register(MockMode::minor("line-numbers-mode")).unwrap();
        let rlnum = r
            .register(MockMode::minor("relative-line-numbers-mode").implying(lnum))
            .unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.activate_minor(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            rlnum,
            CapabilitySet::empty(),
        )
        .unwrap();
        // Two MinorActivated events arrive (parent + implied
        // dep). M-async.2 spawns them in parallel; order is
        // schedule-dependent. Drain both and assert the set.
        let mut got: Vec<ModeId> = Vec::new();
        for _ in 0..2 {
            let evt = await_event(&mut rx).await;
            match evt {
                ModeEvent::MinorActivated { mode, .. } => got.push(mode),
                other => panic!("expected MinorActivated, got {other:?}"),
            }
        }
        got.sort_by_key(|id| id.as_str().to_string());
        let mut want = vec![rlnum, lnum];
        want.sort_by_key(|id| id.as_str().to_string());
        assert_eq!(got, want);
        assert!(a.has_minor(rlnum));
        assert!(a.has_minor(lnum));
    }

    #[tokio::test]
    async fn implies_unregistered_dependency_fails() {
        let mut r = ModeRegistry::new();
        let phantom = ModeId::new("ghost-mode");
        let id = r
            .register(MockMode::minor("thing-mode").implying(phantom))
            .unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let err = r
            .activate_minor(
                &mut a,
                &g,
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

    #[tokio::test]
    async fn conflict_blocks_activation() {
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
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.activate_minor(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            two,
            CapabilitySet::empty(),
        )
        .unwrap();
        let _two_activated = await_event(&mut rx).await;
        let err = r
            .activate_minor(
                &mut a,
                &g,
                &cfg(),
                &bus,
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

    #[tokio::test]
    async fn deactivate_minor_drops_guard_and_publishes_event() {
        let mock = MockMode::minor("a-mode");
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.activate_minor(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let _activated = await_event(&mut rx).await;
        r.deactivate_minor(&mut a, &g, &bus, buf(), id).unwrap();
        let evt = await_event(&mut rx).await;
        assert!(matches!(evt, ModeEvent::MinorDeactivated { mode, .. } if mode == id));
        assert_eq!(deact.load(Ordering::SeqCst), 1, "Guard::Drop ran");
        assert!(!a.has_minor(id));
        assert!(!g.contains(buf(), id));
    }

    #[tokio::test]
    async fn deactivate_inactive_minor_is_noop() {
        let mut r = ModeRegistry::new();
        let id = r.register(MockMode::minor("a-mode")).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.deactivate_minor(&mut a, &g, &bus, buf(), id).unwrap();
        tokio::task::yield_now().await;
        assert!(
            rx.try_recv().is_err(),
            "no-op deactivate should not publish"
        );
    }

    #[tokio::test]
    async fn deactivate_major_drops_guard_and_clears() {
        let mock = MockMode::major("rust-mode");
        let deact = mock.deactivate_calls.clone();
        let mut r = ModeRegistry::new();
        let id = r.register(mock).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.activate_major(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let _entered = await_event(&mut rx).await;
        r.deactivate_major(&mut a, &g, &bus, buf()).unwrap();
        let evt = await_event(&mut rx).await;
        assert!(matches!(evt, ModeEvent::MajorExiting { mode, .. } if mode == id));
        assert_eq!(deact.load(Ordering::SeqCst), 1);
        assert_eq!(a.major(), None);
        assert!(!g.contains(buf(), id));
    }

    #[tokio::test]
    async fn deactivate_major_when_none_active_is_noop() {
        let r = ModeRegistry::new();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.deactivate_major(&mut a, &g, &bus, buf()).unwrap();
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
    }

    /// Lifecycle that returns `Err`: dispatcher publishes
    /// `ModeActivationFailed` instead of `MinorActivated`.
    /// `active_modes` was already mutated by the sync prefix --
    /// M-async.3 rolls it back via subscriber.
    #[tokio::test]
    async fn lifecycle_err_publishes_activation_failed() {
        struct FailingMode {
            id: ModeId,
        }
        impl Mode for FailingMode {
            type Guard = ();
            fn id(&self) -> ModeId {
                self.id
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                let id = self.id;
                Box::pin(async move { Err(ModeActivationError::NotRegistered(id)) })
            }
        }
        let mut r = ModeRegistry::new();
        let id = r
            .register(FailingMode {
                id: ModeId::new("fail-mode"),
            })
            .unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);
        r.activate_minor(
            &mut a,
            &g,
            &cfg(),
            &bus,
            &svcs(),
            buf(),
            id,
            CapabilitySet::empty(),
        )
        .unwrap();
        let evt = await_event(&mut rx).await;
        match evt {
            ModeEvent::ModeActivationFailed {
                mode, reason, ..
            } => {
                assert_eq!(mode, id);
                assert!(reason.contains("not registered"));
            }
            other => panic!("expected ModeActivationFailed, got {other:?}"),
        }
        assert!(
            !g.contains(buf(), id),
            "no Guard stashed on lifecycle error"
        );
    }
}
