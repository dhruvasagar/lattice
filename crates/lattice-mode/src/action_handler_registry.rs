//! M.10.1: action-handler registry — mode-contributed closures
//! per `CommandId`.
//!
//! Per `feedback_mode_owns_its_surface` (CLAUDE.md standing
//! rules, sharpened 2026-06-02) + `mode-architecture.md` §5.3,
//! a mode owns BOTH the chord choice AND the action handler
//! body. The keymap layer is contributed by `Mode::keymap()`;
//! the handler closure is registered into this substrate from
//! `Mode::on_activate`. The host's chord-resolved-action
//! dispatcher consults this registry; the handler returns an
//! `Effect` the host applies through the existing pipeline.
//!
//! Half-migration failure mode (the M.10 audit caught this):
//! keymap layer in the mode but `Editor::do_<provider>_action`
//! body in `lattice-host::dispatch`. This substrate exists so
//! the handler body lives in the mode's owning crate, not the
//! host.
//!
//! ## Shape
//!
//! Wait-free lookup via `arc-swap`. Register / unregister
//! perform a copy-on-write via `ArcSwap::rcu` — O(N) per
//! lifecycle event but registers are rare (per mode
//! activation / deactivation, not per keystroke). Lookups
//! happen per chord-dispatch — one `Arc` load + one
//! `HashMap::get`.
//!
//! ## Lifecycle
//!
//! `register` returns an [`ActionHandlerRegistration`] RAII
//! token. The mode's `Guard` carries one token per registered
//! action; dropping the Guard drops the tokens, each of which
//! unregisters its `CommandId`. Re-activation re-registers
//! fresh closures.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_grammar::effect::Effect;
use lattice_protocol::ids::{BufferId, CommandId};
use lattice_protocol::position::Position;
use lattice_runtime::EventBus;

use crate::services::ServiceRegistry;

/// Read-only context handed to a mode-contributed action
/// handler closure. Carries the bare minimum the host can
/// supply on every invocation: the active buffer + cursor and
/// shared subsystem registries the handler may need.
///
/// Borrows from App-owned state for the duration of the
/// action dispatch — handlers do NOT outlive their context.
pub struct ActionContext<'a> {
    /// Active buffer at the moment the chord fired.
    pub buffer_id: BufferId,
    /// Active document cursor at the moment the chord fired.
    pub cursor: Position,
    /// Typed service registry. Handlers look up subsystem
    /// handles they need (`MultibufferRegistryHandle`,
    /// `ProjectSearchServiceHandle`, etc.) via
    /// `ctx.services.get::<Foo>()`.
    pub services: &'a ServiceRegistry,
    /// Typed event bus. Handlers publish events that other
    /// subsystems subscribe to (e.g.
    /// `ProjectSearchRefreshed`).
    pub events: &'a EventBus,
}

/// Action handler closure shape. Returns an [`Effect`] when
/// the handler wants the host to apply state mutation
/// (open a file, change selection, etc.); `None` for
/// fire-and-forget handlers (logging, publishing an event).
///
/// Closures are `Send + Sync` so the registry can be cloned
/// freely across threads; they take a borrowed
/// [`ActionContext`] for zero-copy access to live state.
pub type ActionHandler = Arc<
    dyn Fn(&ActionContext<'_>) -> Option<Effect> + Send + Sync + 'static,
>;

/// M.10.1.b (2026-06-03): typed handle for `ServiceRegistry`
/// lookup. Boot registers a fresh `ActionHandlerRegistry` under
/// this alias; modes pull it from `on_activate` via
/// `ctx.service::<ActionHandlerRegistryHandle>()`.
///
/// Per `feedback_servicesregistry_arc_typeid`: register and
/// lookup MUST use the same `T` for the TypeId hash to match.
/// This alias guarantees the convention.
pub type ActionHandlerRegistryHandle = Arc<ActionHandlerRegistry>;

/// Wait-free registry of mode-contributed action handlers.
///
/// Stored behind an `Arc` and shared by reference: the host's
/// chord dispatcher reads via [`lookup`](Self::lookup);
/// modes register via [`register`](Self::register) during
/// `Mode::on_activate` and unregister via the returned
/// [`ActionHandlerRegistration`] token's `Drop` impl.
pub struct ActionHandlerRegistry {
    handlers: ArcSwap<HashMap<CommandId, ActionHandler>>,
}

impl ActionHandlerRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            handlers: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    /// Register an action handler closure for `action_id`.
    /// Returns an RAII registration token; the token's `Drop`
    /// impl unregisters the handler. Modes accumulate tokens
    /// in their `Guard` so deactivation drops them and the
    /// chord falls through to "unhandled" naturally.
    ///
    /// If a handler is already registered for `action_id`,
    /// the new closure replaces it. (Modes shouldn't collide
    /// on `CommandId` in normal usage — IDs are per-action
    /// and modes register distinct sets — but the
    /// last-write-wins semantics keeps the substrate
    /// behavior well-defined.)
    pub fn register(
        self: &Arc<Self>,
        action_id: CommandId,
        handler: ActionHandler,
    ) -> ActionHandlerRegistration {
        self.handlers.rcu(|map| {
            let mut next = (**map).clone();
            next.insert(action_id, handler.clone());
            next
        });
        ActionHandlerRegistration {
            registry: Arc::clone(self),
            action_id,
        }
    }

    /// Wait-free lookup. Returns a cloned `Arc` of the
    /// handler closure (cheap — `Arc::clone` is one atomic
    /// increment). The host's chord dispatcher calls this
    /// once per chord resolution; calling the returned
    /// handler is a direct `Fn` invocation.
    pub fn lookup(&self, action_id: CommandId) -> Option<ActionHandler> {
        self.handlers.load().get(&action_id).cloned()
    }

    /// Direct unregister. Normally called via
    /// [`ActionHandlerRegistration::drop`]; exposed for
    /// callers that need explicit lifetime control (e.g.
    /// tests).
    fn unregister(&self, action_id: CommandId) {
        self.handlers.rcu(|map| {
            let mut next = (**map).clone();
            next.remove(&action_id);
            next
        });
    }

    /// Number of currently registered handlers. Test
    /// affordance — production code shouldn't need this.
    #[doc(hidden)]
    pub fn registered_count(&self) -> usize {
        self.handlers.load().len()
    }
}

impl Default for ActionHandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ActionHandlerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionHandlerRegistry")
            .field("registered_count", &self.registered_count())
            .finish_non_exhaustive()
    }
}

/// RAII registration token. Dropping it unregisters the
/// handler. Modes typically aggregate these into their
/// `Mode::Guard`; when the Guard drops on deactivation, every
/// token drops, every handler unregisters.
///
/// `Send + 'static` so it fits the `Mode::Guard: Send +
/// 'static` bound.
pub struct ActionHandlerRegistration {
    registry: Arc<ActionHandlerRegistry>,
    action_id: CommandId,
}

impl ActionHandlerRegistration {
    /// The `CommandId` this registration is bound to.
    /// Test affordance.
    #[doc(hidden)]
    pub fn action_id(&self) -> CommandId {
        self.action_id
    }
}

impl Drop for ActionHandlerRegistration {
    fn drop(&mut self) {
        self.registry.unregister(self.action_id);
    }
}

impl std::fmt::Debug for ActionHandlerRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionHandlerRegistration")
            .field("action_id", &self.action_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn cid(raw: u64) -> CommandId {
        CommandId::new(raw)
    }

    fn handler_returning_none() -> ActionHandler {
        Arc::new(|_ctx: &ActionContext<'_>| None)
    }

    #[test]
    fn register_then_lookup_returns_handler() {
        let r = Arc::new(ActionHandlerRegistry::new());
        let id = cid(1);
        let _reg = r.register(id, handler_returning_none());
        assert!(r.lookup(id).is_some());
    }

    #[test]
    fn missing_lookup_returns_none() {
        let r = Arc::new(ActionHandlerRegistry::new());
        assert!(r.lookup(cid(42)).is_none());
    }

    #[test]
    fn drop_registration_unregisters_handler() {
        let r = Arc::new(ActionHandlerRegistry::new());
        let id = cid(7);
        let reg = r.register(id, handler_returning_none());
        assert_eq!(r.registered_count(), 1);
        drop(reg);
        assert_eq!(r.registered_count(), 0);
        assert!(r.lookup(id).is_none());
    }

    #[test]
    fn multiple_handlers_coexist_and_drop_independently() {
        let r = Arc::new(ActionHandlerRegistry::new());
        let reg_a = r.register(cid(1), handler_returning_none());
        let reg_b = r.register(cid(2), handler_returning_none());
        let _reg_c = r.register(cid(3), handler_returning_none());
        assert_eq!(r.registered_count(), 3);
        drop(reg_a);
        assert_eq!(r.registered_count(), 2);
        assert!(r.lookup(cid(1)).is_none());
        assert!(r.lookup(cid(2)).is_some());
        assert!(r.lookup(cid(3)).is_some());
        drop(reg_b);
        assert_eq!(r.registered_count(), 1);
    }

    #[test]
    fn second_register_for_same_id_replaces_previous() {
        let r = Arc::new(ActionHandlerRegistry::new());
        let id = cid(10);

        // First handler always returns None; second returns
        // a sentinel Effect we can detect.
        let _reg_first = r.register(id, handler_returning_none());
        let _reg_second = r.register(
            id,
            Arc::new(|_ctx: &ActionContext<'_>| Some(Effect::None)),
        );

        let h = r.lookup(id).unwrap();
        let services = ServiceRegistry::new();
        let events = EventBus::new();
        let ctx = ActionContext {
            buffer_id: BufferId::new(0),
            cursor: Position::ZERO,
            services: &services,
            events: &events,
        };
        let effect = h(&ctx);
        assert!(matches!(effect, Some(Effect::None)));
    }

    #[test]
    fn registration_is_send_static() {
        fn assert_send_static<T: Send + 'static>() {}
        assert_send_static::<ActionHandlerRegistration>();
    }

    #[test]
    fn registry_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ActionHandlerRegistry>();
        assert_send_sync::<ActionHandler>();
    }

    #[test]
    fn lookup_after_drop_chain_is_consistent() {
        // Stress: register / drop interleavings preserve
        // exact membership semantics.
        let r = Arc::new(ActionHandlerRegistry::new());
        let regs: Vec<_> = (0..16)
            .map(|i| r.register(cid(i), handler_returning_none()))
            .collect();
        assert_eq!(r.registered_count(), 16);
        // Drop every other registration.
        let (keep, drop_these): (Vec<_>, Vec<_>) =
            regs.into_iter().enumerate().partition(|(i, _)| i % 2 == 0);
        drop(drop_these);
        assert_eq!(r.registered_count(), 8);
        for (i, _) in &keep {
            assert!(r.lookup(cid(*i as u64)).is_some());
        }
        for i in (0..16).filter(|i| i % 2 != 0) {
            assert!(r.lookup(cid(i)).is_none());
        }
    }
}
