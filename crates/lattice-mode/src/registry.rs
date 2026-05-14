//! `ModeRegistry`: register modes, look them up, drive
//! activation / deactivation against a per-buffer `ActiveModes`
//! and an App-owned [`GuardStoreHandle`].
//!
//! M-async.3: activation is **spawn-based with sequential
//! cascade**. The sync prefix walks the requested mode + its
//! `implies()` tree, validates each, mutates `active_modes` for
//! each, and builds an ordered cascade plan
//! (`Vec<CascadeStep>`). One task is spawned that walks the
//! plan in DFS order, awaiting each step's `on_activate.await`
//! before moving to the next. This guarantees a parent's
//! lifecycle resolves before its implied children's begin --
//! no sub-mode reads parent's not-yet-written state.
//!
//! On lifecycle error the spawned task publishes
//! `ModeActivationFailed` for the failing step, then publishes
//! `ModeActivationFailed { reason: "cascade aborted by X" }`
//! for every remaining (unrun) step. An App-side subscriber
//! (`drain_mode_lifecycle_events`) rolls back `active_modes` /
//! `mode_guards` on each.
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
    /// validate the major + its `implies()` tree, mutate
    /// `active_modes` for the whole tree, bump epochs + build a
    /// cascade plan. Then one task is spawned that walks the
    /// plan in DFS order, awaiting each step's
    /// `on_activate.await` before the next.
    ///
    /// If a different major is currently active, it is
    /// deactivated synchronously first (Drop runs, `MajorExiting`
    /// publishes). Idempotent: reactivating the current major
    /// triggers a *reload* (deactivate then re-activate).
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
            let _ = guards.remove(buffer, prev_id);
            active.set_major(None);
        }

        // Sync prefix: mutate active_modes BEFORE building the
        // cascade plan so `App::active_modes.has_major(mode)`
        // is `true` the moment this call returns.
        active.set_major(Some(mode));

        // Build the cascade plan: root major + implied minors
        // in DFS order. Each step bumps the epoch + records
        // the new value so the spawn task can validate before
        // stashing. Validation errors short-circuit the build;
        // partial active_modes mutation rolls back on error.
        let mut plan: Vec<CascadeStep> = vec![CascadeStep {
            entry: entry.clone(),
            mode,
            kind: ModeKind::Major,
            epoch: guards.bump_epoch(buffer, mode),
        }];
        if let Err(e) = self.record_implies_cascade(active, &mut plan, &entry, mode, caps, buffer, guards) {
            active.set_major(None);
            for step in plan.iter().skip(1) {
                active.remove_minor(step.mode);
            }
            return Err(e);
        }

        self.spawn_cascade(plan, guards.clone(), events.clone(), config, events, services, buffer);
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
        let mut plan: Vec<CascadeStep> = Vec::new();
        self.validate_and_record_minor(active, &mut plan, buffer, mode, caps, guards)?;
        // `plan` is empty when the minor was already active (no
        // -op): skip the spawn entirely.
        if !plan.is_empty() {
            self.spawn_cascade(
                plan,
                guards.clone(),
                events.clone(),
                config,
                events,
                services,
                buffer,
            );
        }
        Ok(())
    }

    /// Validate `mode` (a minor) against the active set, mutate
    /// `active` to include it, bump its epoch, and push the
    /// resulting [`CascadeStep`] onto `plan`. Recursively
    /// records implied minors. On error, rolls back the
    /// `active` mutations + plan pushes performed for THIS
    /// call (callers responsible for unwinding their own
    /// pushes).
    #[allow(clippy::too_many_arguments)]
    fn validate_and_record_minor(
        &self,
        active: &mut ActiveModes,
        plan: &mut Vec<CascadeStep>,
        buffer: BufferId,
        mode: ModeId,
        caps: CapabilitySet,
        guards: &GuardStoreHandle,
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

        active.push_minor(mode);
        plan.push(CascadeStep {
            entry: entry.clone(),
            mode,
            kind: ModeKind::Minor,
            epoch: guards.bump_epoch(buffer, mode),
        });

        if let Err(e) = self.record_implies_cascade(active, plan, &entry, mode, caps, buffer, guards) {
            active.remove_minor(mode);
            if let Some(pos) = plan.iter().position(|s| s.mode == mode) {
                for step in plan.drain(pos..) {
                    if step.mode != mode {
                        active.remove_minor(step.mode);
                    }
                }
            }
            return Err(e);
        }

        Ok(())
    }

    /// Walk `entry.implies()`, validating + recording each as
    /// a cascade step. Shared between `activate_major` and the
    /// minor recursion.
    #[allow(clippy::too_many_arguments)]
    fn record_implies_cascade(
        &self,
        active: &mut ActiveModes,
        plan: &mut Vec<CascadeStep>,
        entry: &Arc<dyn DynMode>,
        mode: ModeId,
        caps: CapabilitySet,
        buffer: BufferId,
        guards: &GuardStoreHandle,
    ) -> Result<(), ModeActivationError> {
        for &dep in entry.implies() {
            if !self.is_registered(dep) {
                return Err(ModeActivationError::UnregisteredDependency { mode, dep });
            }
            if active.has_minor(dep) {
                continue;
            }
            self.validate_and_record_minor(active, plan, buffer, dep, caps, guards)?;
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

    /// Drive `plan` DFS. Builds the cascade future, polls it
    /// once synchronously with a no-op waker, and only
    /// `spawn_task`s the remaining work if the future is still
    /// Pending.
    ///
    /// **Why try-sync-then-spawn:** today's modes (markers with
    /// `Guard = ()`, plus the hand-written LSP modes) finish
    /// `on_activate` without any `.await` -- the future returns
    /// `Ready` on first poll. Letting the App thread drive the
    /// cascade synchronously when nothing yields keeps
    /// `App::activate_mode_by_id` ⇒ `App::deactivate_mode_by_id`
    /// (rapid toggle, e.g. tests + future plugin scripting)
    /// race-free: the Guard is in the store before deactivate
    /// runs. When a mode is rewritten to `.await` real I/O
    /// (e.g. the LSP initialize handshake), the first
    /// `Pending` yield trips the spawn path and the
    /// remainder runs on the runtime -- the App thread stops
    /// blocking, paramount goal #4 still honoured.
    ///
    /// On success, stashes the Guard + publishes the
    /// lifecycle event. On failure, publishes
    /// `ModeActivationFailed` for the failing step plus a
    /// synthetic "cascade aborted" failure for every
    /// remaining unrun step, so the App's rollback subscriber
    /// can clean up `active_modes` for the whole subtree.
    #[allow(clippy::too_many_arguments)]
    fn spawn_cascade(
        &self,
        plan: Vec<CascadeStep>,
        guards: GuardStoreHandle,
        events_for_task: Arc<lattice_runtime::EventBus>,
        config: &Arc<lattice_config::ConfigRegistry>,
        events: &Arc<lattice_runtime::EventBus>,
        services: &Arc<crate::services::ServiceRegistry>,
        buffer: BufferId,
    ) {
        let config = config.clone();
        let events_ctx = events.clone();
        let services = services.clone();
        let cascade_fut = async move {
            for (i, step) in plan.iter().enumerate() {
                let ctx = ModeContext::new(
                    buffer,
                    step.mode,
                    config.clone(),
                    events_ctx.clone(),
                    services.clone(),
                );
                match step.entry.on_activate_dyn(ctx).await {
                    Ok(guard) => {
                        // try_insert validates that no
                        // deactivate (or later activate)
                        // arrived during the await. On stale,
                        // returns Err(guard) -- we drop the
                        // Box here (which fires the original
                        // Guard type's Drop for out-of-band
                        // cleanup) and skip the success event.
                        match guards.try_insert(buffer, step.mode, step.epoch, guard) {
                            Ok(()) => {
                                let evt = match step.kind {
                                    ModeKind::Major => ModeEvent::MajorEntered {
                                        buffer,
                                        mode: step.mode,
                                    },
                                    ModeKind::Minor => ModeEvent::MinorActivated {
                                        buffer,
                                        mode: step.mode,
                                    },
                                };
                                events_for_task.publish_typed(evt);
                            }
                            Err(stale_guard) => {
                                // Drop here. The Box goes out
                                // of scope on the next line;
                                // the original Guard's Drop
                                // fires (publishes
                                // LspBufferDetached, restores
                                // foldmethod, etc.).
                                drop(stale_guard);
                                // No event published: the
                                // deactivate/re-activate that
                                // bumped the epoch already
                                // published its own
                                // MinorDeactivated /
                                // MajorExiting.
                            }
                        }
                    }
                    Err(err) => {
                        events_for_task
                            .publish_typed(ModeEvent::activation_failed(buffer, step.mode, &err));
                        let trigger = step.mode;
                        for remaining in &plan[i + 1..] {
                            events_for_task.publish_typed(ModeEvent::ModeActivationFailed {
                                buffer,
                                mode: remaining.mode,
                                reason: format!("cascade aborted by {trigger}"),
                            });
                        }
                        return;
                    }
                }
            }
        };

        // Try-sync-then-spawn. Poll once with a no-op waker
        // (the future may register interest with it; standard
        // Rust futures re-register on every poll, so handing
        // the same `fut` to tokio when Pending is sound).
        let mut fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
            Box::pin(cascade_fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(()) => {
                // Cascade finished entirely on this thread; no
                // spawn needed. Guards are in the store, events
                // are on the bus. Rapid `deactivate` is now
                // race-free.
            }
            std::task::Poll::Pending => {
                lattice_runtime::spawn_task(fut);
            }
        }
    }
}

/// One step in a cascade plan. Built synchronously by the sync
/// prefix; consumed by the spawned task that awaits each step's
/// `on_activate.await` in order.
///
/// `epoch` is the value [`GuardStoreHandle::bump_epoch`]
/// returned when the sync prefix queued this step. The spawn
/// task passes it back to [`GuardStoreHandle::try_insert`] on
/// completion; mismatch means a deactivate (or a later
/// re-activate) bumped the epoch meanwhile, so the Guard is
/// stale and gets dropped instead of stashed.
struct CascadeStep {
    entry: Arc<dyn DynMode>,
    mode: ModeId,
    kind: ModeKind,
    epoch: u64,
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
        // M-async.3: sequential cascade. Parent's
        // `on_activate.await` resolves BEFORE the implied
        // child's begins, so events arrive in DFS order:
        // parent first, then child.
        let evt = await_event(&mut rx).await;
        assert!(
            matches!(evt, ModeEvent::MinorActivated { mode, .. } if mode == rlnum),
            "parent (rlnum) should activate first; got {evt:?}",
        );
        let evt = await_event(&mut rx).await;
        assert!(
            matches!(evt, ModeEvent::MinorActivated { mode, .. } if mode == lnum),
            "implied child (lnum) should activate second; got {evt:?}",
        );
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

    /// M-async.3 cascade abort: parent's `on_activate` returns
    /// `Err` → publishes `ModeActivationFailed` for the parent
    /// *and* a synthetic "cascade aborted by parent"
    /// `ModeActivationFailed` for each unrun implied child.
    /// Children never spawn; their Guards never stash; the
    /// App's rollback subscriber sees one event per unrun mode
    /// and clears `active_modes` for the whole subtree.
    #[tokio::test]
    async fn cascade_abort_publishes_synthetic_failures_for_unrun_steps() {
        struct FailingParent {
            id: ModeId,
            implies: Vec<ModeId>,
        }
        impl Mode for FailingParent {
            type Guard = ();
            fn id(&self) -> ModeId {
                self.id
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn implies(&self) -> &[ModeId] {
                &self.implies
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                let id = self.id;
                Box::pin(async move { Err(ModeActivationError::NotRegistered(id)) })
            }
        }
        let mut r = ModeRegistry::new();
        let child_id = r.register(MockMode::minor("child-mode")).unwrap();
        let parent_id = ModeId::new("parent-mode");
        r.register(FailingParent {
            id: parent_id,
            implies: vec![child_id],
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
            parent_id,
            CapabilitySet::empty(),
        )
        .unwrap();
        // Sync prefix mutated active_modes for parent + child.
        assert!(a.has_minor(parent_id));
        assert!(a.has_minor(child_id));
        // First event: parent's real failure.
        let evt = await_event(&mut rx).await;
        match evt {
            ModeEvent::ModeActivationFailed { mode, reason, .. } => {
                assert_eq!(mode, parent_id);
                assert!(
                    !reason.starts_with("cascade aborted"),
                    "parent's reason should be the real error, not the synthetic prefix; got {reason:?}",
                );
            }
            other => panic!("expected parent's ModeActivationFailed first, got {other:?}"),
        }
        // Second event: child's synthetic "cascade aborted".
        let evt = await_event(&mut rx).await;
        match evt {
            ModeEvent::ModeActivationFailed { mode, reason, .. } => {
                assert_eq!(mode, child_id);
                assert!(
                    reason.contains("cascade aborted"),
                    "child's reason should announce cascade abort; got {reason:?}",
                );
                assert!(reason.contains(parent_id.as_str()));
            }
            other => panic!("expected child's synthetic ModeActivationFailed, got {other:?}"),
        }
        // No Guards stashed -- neither parent nor child ran to
        // completion.
        assert!(!g.contains(buf(), parent_id));
        assert!(!g.contains(buf(), child_id));
    }

    /// M-async.4: a mode whose `on_activate` truly `.await`s
    /// (yields Pending on first poll) trips the
    /// try-sync-then-spawn driver into the spawn path. If a
    /// deactivate arrives before the spawn completes, the
    /// epoch bump in `remove` invalidates the in-flight spawn's
    /// captured epoch; its later `try_insert` fails the match
    /// and the Guard drops on the spawn side instead of
    /// stashing into a logically-inactive store slot.
    ///
    /// This test pins that contract: rapid `activate →
    /// deactivate` against an `.await`ing mode produces no
    /// leaked Guard, and the stale Guard's `Drop` still fires
    /// (out-of-band; the dispatcher relies on Drop for
    /// cleanup correctness).
    #[tokio::test]
    async fn rapid_deactivate_during_pending_activate_drops_guard_on_spawn_side() {
        use tokio::sync::oneshot;

        /// Guard whose Drop bumps an atomic counter. The test
        /// asserts the counter increments even when the spawn
        /// detects a stale epoch.
        struct DropTrackingGuard {
            counter: StdArc<AtomicU32>,
        }
        impl Drop for DropTrackingGuard {
            fn drop(&mut self) {
                self.counter.fetch_add(1, Ordering::SeqCst);
            }
        }

        /// Mode whose `on_activate` `.await`s a oneshot before
        /// returning the Guard. Lets the test interleave: spawn
        /// task is parked, deactivate runs, then we release
        /// the oneshot and watch the spawn task try to insert.
        struct GatedMode {
            id: ModeId,
            gate_rx: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
            drop_counter: StdArc<AtomicU32>,
        }
        impl Mode for GatedMode {
            type Guard = DropTrackingGuard;
            fn id(&self) -> ModeId {
                self.id
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
                let rx = self
                    .gate_rx
                    .lock()
                    .unwrap()
                    .take()
                    .expect("oneshot consumed twice");
                let counter = self.drop_counter.clone();
                Box::pin(async move {
                    // Yield Pending until the test releases the gate.
                    let _ = rx.await;
                    Ok(DropTrackingGuard { counter })
                })
            }
        }

        let drop_counter = StdArc::new(AtomicU32::new(0));
        let (gate_tx, gate_rx) = oneshot::channel::<()>();
        let mode = GatedMode {
            id: ModeId::new("gated-mode"),
            gate_rx: std::sync::Mutex::new(Some(gate_rx)),
            drop_counter: drop_counter.clone(),
        };
        let mut r = ModeRegistry::new();
        let id = r.register(mode).unwrap();
        let mut a = ActiveModes::new();
        let g = GuardStoreHandle::new();
        let bus = evts();
        let mut rx = subscribe_mode_events(&bus);

        // Activate -- sync prefix mutates active_modes + bumps
        // epoch; first poll yields Pending (oneshot not
        // released) so the driver spawns the rest.
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
        assert!(a.has_minor(id));
        assert!(!g.contains(buf(), id), "Guard not stashed yet (spawn pending)");

        // Deactivate immediately. Synchronous: bumps epoch
        // (invalidating in-flight spawn), removes from
        // active_modes, publishes MinorDeactivated.
        // guards.remove returns None (Guard wasn't in store
        // yet) so no Drop fires here.
        r.deactivate_minor(&mut a, &g, &bus, buf(), id).unwrap();
        assert!(!a.has_minor(id));
        // MinorDeactivated published from the sync deactivate.
        let evt = await_event(&mut rx).await;
        assert!(
            matches!(evt, ModeEvent::MinorDeactivated { mode, .. } if mode == id),
            "deactivate should publish MinorDeactivated synchronously",
        );
        assert_eq!(
            drop_counter.load(Ordering::SeqCst),
            0,
            "Guard hasn't been constructed yet -- spawn is parked on oneshot",
        );

        // Release the spawn's `.await`. It now resolves +
        // tries to try_insert. Epoch mismatch (we bumped via
        // deactivate's remove) → returns Err(guard) → Guard
        // dropped on the spawn side → DropTrackingGuard::drop
        // fires.
        gate_tx.send(()).unwrap();
        // Wait for the spawn task to observe the wake + run
        // its drop. Tokio multi-thread runtime is shared
        // across tests so contention can stretch wall-time;
        // poll-and-sleep with a generous budget rather than
        // a tight yield loop.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while drop_counter.load(Ordering::SeqCst) == 0 {
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            drop_counter.load(Ordering::SeqCst),
            1,
            "stale Guard's Drop should have fired on the spawn side",
        );
        assert!(
            !g.contains(buf(), id),
            "Guard must NOT be stashed in the store -- the deactivate already happened",
        );
        // No spurious MinorActivated event for the activation
        // that ended up stale.
        assert!(
            rx.try_recv().is_err(),
            "stale activation should not publish MinorActivated",
        );
    }
}
