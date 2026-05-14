//! `ModeContext`: the handle passed to
//! [`crate::Mode::on_activate`].
//!
//! Owned (`Send + 'static`) so the dispatcher can hand it to a
//! `tokio::spawn`ed future without lifetime gymnastics. Hooks
//! that need to mutate options (e.g. `lsp-folding-mode` swapping
//! `foldmethod=lsp` on activate) reach the typed-options
//! registry through [`ModeContext::config`]. Hooks that need to
//! publish typed events use [`ModeContext::events`]. Hooks that
//! need a subsystem handle (LSP supervisor, BufferStore) call
//! [`ModeContext::service`].
//!
//! Why no `BufferLocals` access:
//!
//! Mode-private state is owned by the [`Mode::Guard`](crate::Mode::Guard)
//! returned from `on_activate`. The Guard's `Drop` impl performs
//! cleanup. App-managed buffer-locals (icons, syntax handles,
//! folds, ...) live in an App-owned map and are written through
//! App-side code paths, not through `ctx`.
//!
//! Why owned handles (not `&Arc<T>`):
//!
//! The context is constructed once per activation and moved into
//! the lifecycle future. The future captures it across `await`
//! points, so every field must be owned and `Send + 'static`.
//! `Arc<T>` is cheap to clone -- the dispatcher does one
//! `Arc::clone` per activation when building the context.

use std::any::Any;
use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_protocol::ids::BufferId;
use lattice_runtime::EventBus;

use crate::mode::ModeId;
use crate::services::ServiceRegistry;

/// Lifecycle context. Carries the current activation's
/// metadata + cheap-clone handles to the system registries the
/// mode may need.
///
/// **Owned, `Send + 'static`**: the dispatcher constructs one
/// per activation, moves it into the lifecycle future, and
/// drops it when the future resolves.
pub struct ModeContext {
    buffer_id: BufferId,
    current_mode: ModeId,
    config: Arc<ConfigRegistry>,
    events: Arc<EventBus>,
    services: Arc<ServiceRegistry>,
}

impl ModeContext {
    /// Construct a new context. Public for tests that drive
    /// mode lifecycle directly; production callers go through
    /// the registry's activation methods which build the
    /// context internally.
    pub fn new(
        buffer_id: BufferId,
        current_mode: ModeId,
        config: Arc<ConfigRegistry>,
        events: Arc<EventBus>,
        services: Arc<ServiceRegistry>,
    ) -> Self {
        Self {
            buffer_id,
            current_mode,
            config,
            events,
            services,
        }
    }

    /// Typed service lookup. Used by modes that need access to
    /// subsystem handles (e.g. `LspMode` retrieves
    /// `LspSupervisorHandle`; `LspLogMode` retrieves
    /// `BufferStoreHandle` to synthesize its `*lsp*` buffer).
    /// Returns `None` when no service of type `T` is registered
    /// -- modes should fail gracefully (log / echo) rather than
    /// panic, since tests may run a mode without wiring the
    /// service.
    pub fn service<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.services.get::<T>()
    }

    /// Shared typed-options registry. Mutations propagate
    /// through the registry's `OptionChanged` event stream the
    /// same way `:set` does, so subscribers fire automatically.
    pub fn config(&self) -> &ConfigRegistry {
        &self.config
    }

    /// Cheap-clone handle to the config registry. Use when the
    /// mode needs to stash the registry inside its Guard for
    /// later mutation (e.g. restoring an option in `Drop`).
    pub fn config_handle(&self) -> Arc<ConfigRegistry> {
        self.config.clone()
    }

    /// Shared typed event bus.
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// Cheap-clone handle to the event bus. Use when the mode
    /// needs to stash the bus inside its Guard for later
    /// publish / unsubscribe (e.g. unsubscribing a subscription
    /// ID in `Drop`).
    pub fn events_handle(&self) -> Arc<EventBus> {
        self.events.clone()
    }

    /// Buffer the activation is operating on.
    pub fn buffer_id(&self) -> BufferId {
        self.buffer_id
    }

    /// The mode whose lifecycle hook is currently running.
    pub fn current_mode(&self) -> ModeId {
        self.current_mode
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn ctx() -> ModeContext {
        ModeContext::new(
            BufferId::new(1),
            ModeId::new("a-mode"),
            Arc::new(ConfigRegistry::new()),
            Arc::new(EventBus::new()),
            Arc::new(ServiceRegistry::new()),
        )
    }

    #[test]
    fn buffer_id_and_current_mode_round_trip() {
        let c = ctx();
        assert_eq!(c.buffer_id(), BufferId::new(1));
        assert_eq!(c.current_mode().as_str(), "a-mode");
    }

    #[test]
    fn handles_are_cheap_to_clone() {
        let c = ctx();
        let h1 = c.events_handle();
        let h2 = c.events_handle();
        // Both Arc clones point at the same bus.
        assert!(Arc::ptr_eq(&h1, &h2));
    }

    /// The context is `Send + 'static` so a spawned future can
    /// hold it across `.await` points.
    #[test]
    fn ctx_is_send_static() {
        fn assert_send_static<T: Send + 'static>() {}
        assert_send_static::<ModeContext>();
    }
}
