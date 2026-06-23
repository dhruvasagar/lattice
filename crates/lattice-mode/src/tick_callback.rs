//! IDE-protocol I1.1: tick-callback registry — the one generic host
//! primitive the Claude Code IDE peer needs.
//!
//! A mode registers a per-tick drain closure; the host runs every
//! registered closure once per editor tick (inside `run_tick_pending`)
//! and applies the `Effect`s they return through the existing effect
//! pipeline. This *generalizes* the host's existing hardcoded per-tick
//! drains (`option_change_rx`, `lsp_log_event_rx`, …) — those are the
//! smell this primitive replaces for new subsystems: rather than adding
//! an `Editor::drain_<x>` method + an `Option<Receiver>` field per
//! subsystem, a mode owns its channel and registers a closure that
//! drains it.
//!
//! Per `feedback_mode_owns_its_surface`: the drain *body* lives in the
//! mode's owning crate (it closes over the mode's own receiver), not in
//! `lattice-host`. The host's role is the generic run-loop + the
//! effect-apply pipeline.
//!
//! ## Shape
//!
//! Unlike [`ActionHandlerRegistry`](crate::action_handler_registry::ActionHandlerRegistry)
//! — whose `Fn` handlers are wait-free behind `arc-swap` — a tick
//! callback is `FnMut`: it mutates captured state (a channel receiver)
//! on every run. So the registry stores the closures behind a
//! `Mutex<Vec<…>>` rather than an `ArcSwap`. Registration is rare (per
//! mode activation / deactivation); `run_all` is the per-tick hot path,
//! but it runs on the editor (actor) thread only — the lock is
//! uncontended in practice.
//!
//! ## Lifecycle
//!
//! [`register`](TickCallbackRegistry::register) returns a
//! [`TickCallbackRegistration`] RAII token. A mode's `Guard` carries the
//! token; dropping the Guard on deactivation drops the token, which
//! removes the closure — so a stopped mode contributes no per-tick work.

use std::sync::Arc;
use std::sync::Mutex;

use lattice_grammar::effect::Effect;

/// A per-tick drain closure. Returns the `Effect`s the host should apply
/// this tick (empty when there was nothing pending). `FnMut` because the
/// canonical body drains a channel receiver, which needs `&mut`.
///
/// `Send` so the registry (held behind an `Arc` shared with mode
/// activation paths) is `Send`; the closure is only ever *invoked* on
/// the editor thread inside [`TickCallbackRegistry::run_all`].
pub type TickCallback = Box<dyn FnMut() -> Vec<Effect> + Send + 'static>;

/// Typed handle for `ServiceRegistry` lookup. Boot registers a fresh
/// `TickCallbackRegistry` under this alias; modes pull it from
/// `on_activate` via `ctx.service::<TickCallbackRegistryHandle>()` (or
/// receive it directly at registration) and add their drain.
///
/// Per `feedback_servicesregistry_arc_typeid`: register and lookup MUST
/// use the same `T` for the TypeId hash to match. This alias guarantees
/// the convention.
pub type TickCallbackRegistryHandle = Arc<TickCallbackRegistry>;

/// Internal mutable state: a monotonic id counter plus the live
/// callbacks. Kept in one `Mutex` so `register` is a single critical
/// section.
struct Inner {
    next_id: u64,
    callbacks: Vec<(u64, TickCallback)>,
}

/// Registry of mode-contributed per-tick drain closures.
///
/// Stored behind an `Arc` and shared by reference: the host calls
/// [`run_all`](Self::run_all) once per tick; modes
/// [`register`](Self::register) during `Mode::on_activate` and
/// unregister via the returned [`TickCallbackRegistration`] token's
/// `Drop`.
pub struct TickCallbackRegistry {
    inner: Mutex<Inner>,
}

impl TickCallbackRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_id: 0,
                callbacks: Vec::new(),
            }),
        }
    }

    /// Register a per-tick drain closure. Returns an RAII registration
    /// token; dropping it removes the closure. Modes accumulate the
    /// token in their `Guard` so deactivation drops it and the per-tick
    /// drain stops naturally.
    pub fn register(self: &Arc<Self>, callback: TickCallback) -> TickCallbackRegistration {
        let id = {
            // Poison recovery: a panicking callback while the lock is
            // held (see `run_all`) would poison the Mutex; we never want
            // a single bad drain to wedge every other mode's drain, so
            // recover the inner state rather than propagate the panic.
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let id = g.next_id;
            g.next_id += 1;
            g.callbacks.push((id, callback));
            id
        };
        TickCallbackRegistration {
            registry: Arc::clone(self),
            id,
        }
    }

    /// Run every registered callback once, in registration order, and
    /// return the concatenated `Effect`s for the host to apply. Called
    /// once per editor tick from `Editor::run_tick_pending`.
    ///
    /// Holds the lock for the duration: a callback MUST NOT call
    /// `register` / drop a [`TickCallbackRegistration`] on *this same*
    /// registry from inside its body (it would deadlock). Drain
    /// closures only ever touch their own channel + return effects, so
    /// this is a documented non-constraint in practice.
    pub fn run_all(&self) -> Vec<Effect> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut effects = Vec::new();
        for (_id, cb) in g.callbacks.iter_mut() {
            effects.extend(cb());
        }
        effects
    }

    /// Direct removal by id. Normally called via
    /// [`TickCallbackRegistration::drop`].
    fn unregister(&self, id: u64) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.callbacks.retain(|(cid, _)| *cid != id);
    }

    /// Number of currently registered callbacks. Test affordance.
    #[doc(hidden)]
    pub fn registered_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .callbacks
            .len()
    }
}

impl Default for TickCallbackRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TickCallbackRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TickCallbackRegistry")
            .field("registered_count", &self.registered_count())
            .finish_non_exhaustive()
    }
}

/// RAII registration token. Dropping it removes the callback. Modes
/// aggregate the token into their `Mode::Guard`; when the Guard drops on
/// deactivation, the callback is removed and the mode contributes no
/// further per-tick work.
///
/// `Send + 'static` so it fits the `Mode::Guard: Send + 'static` bound.
pub struct TickCallbackRegistration {
    registry: Arc<TickCallbackRegistry>,
    id: u64,
}

impl Drop for TickCallbackRegistration {
    fn drop(&mut self) {
        self.registry.unregister(self.id);
    }
}

impl std::fmt::Debug for TickCallbackRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TickCallbackRegistration")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn run_all_on_empty_registry_returns_no_effects() {
        let r = Arc::new(TickCallbackRegistry::new());
        assert!(r.run_all().is_empty());
    }

    #[test]
    fn registered_callback_runs_and_returns_effects() {
        let r = Arc::new(TickCallbackRegistry::new());
        let _reg = r.register(Box::new(|| vec![Effect::None]));
        let effects = r.run_all();
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::None));
    }

    #[test]
    fn callback_is_fnmut_and_observes_state_across_ticks() {
        // The canonical body mutates captured state every run (a
        // receiver drain). Prove `FnMut` semantics: a captured counter
        // increments on each `run_all`.
        let r = Arc::new(TickCallbackRegistry::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_in = Arc::clone(&runs);
        let _reg = r.register(Box::new(move || {
            runs_in.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        }));
        r.run_all();
        r.run_all();
        r.run_all();
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn multiple_callbacks_all_run_in_registration_order() {
        let r = Arc::new(TickCallbackRegistry::new());
        let _a = r.register(Box::new(|| vec![Effect::None]));
        let _b = r.register(Box::new(|| vec![Effect::None, Effect::None]));
        let effects = r.run_all();
        // a's one + b's two, concatenated.
        assert_eq!(effects.len(), 3);
    }

    #[test]
    fn drop_registration_stops_the_callback() {
        let r = Arc::new(TickCallbackRegistry::new());
        let reg = r.register(Box::new(|| vec![Effect::None]));
        assert_eq!(r.registered_count(), 1);
        assert_eq!(r.run_all().len(), 1);
        drop(reg);
        assert_eq!(r.registered_count(), 0);
        assert!(r.run_all().is_empty());
    }

    #[test]
    fn registrations_drop_independently() {
        let r = Arc::new(TickCallbackRegistry::new());
        let reg_a = r.register(Box::new(|| vec![Effect::None]));
        let _reg_b = r.register(Box::new(|| vec![Effect::None]));
        assert_eq!(r.registered_count(), 2);
        drop(reg_a);
        assert_eq!(r.registered_count(), 1);
        // The surviving callback still runs.
        assert_eq!(r.run_all().len(), 1);
    }

    #[test]
    fn registration_is_send_static() {
        fn assert_send_static<T: Send + 'static>() {}
        assert_send_static::<TickCallbackRegistration>();
    }

    #[test]
    fn registry_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TickCallbackRegistry>();
    }
}
