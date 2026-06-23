//! Boot-composition BC.1: the `BootContext` skeleton.
//!
//! `editor_boot.rs` is a ~1700-line god-function where every async subsystem
//! hand-wires the same six things: mode registration, command registration,
//! service registration, an `async_landed` wake, a per-tick drain, and a
//! deferred install for late handles. `BootContext` is the host's
//! generic-primitive surface made explicit — the typed bundle a subsystem's
//! future `install(boot)` receives, exposing the easy-to-get-wrong operations
//! as *primitives that cannot be wired without their safety property*.
//!
//! Design fragment: `docs/dev/architecture/boot-composition.md`.
//! Slice plan: `docs/dev/operations/slice-plans/boot-composition.md`.
//!
//! ## BC.1 scope (this slice)
//!
//! Additive only — `editor_boot.rs` does not yet construct or use a
//! `BootContext`; that is BC.3. This slice defines the bundle plus the two
//! wake-robustness primitives and pins their behaviour:
//!
//! - [`BootContext::inbound`] — the bundled inbound primitive. A channel
//!   whose `send` wakes `async_landed` (the wake is inside the sender, so it
//!   is structurally impossible to forget) and whose items are drained
//!   per-tick via the tick-callback registry through a handler. Generalizes
//!   the I3 `ClaudeCodeInboundBus` and LSP's hand-rolled inbound buses.
//! - [`BootContext::wake_on_event`] — subscribe a typed event and wake
//!   `async_landed` whenever one is published. Generalizes the
//!   `MultibufferExcerptsReady` / L1c `wake_on` forwarder tasks.
//! - [`BootContext::tick_callback`] — register a raw per-tick drain (the I1
//!   registry), retaining the RAII token for boot lifetime.
//!
//! ## Deferred to BC.3 (intentionally NOT here)
//!
//! The full bundle in the design fragment also lists the `render_state` cell,
//! the renderer's `BufferStore` handle, and the `DiagnosticsQueryHandle`.
//! Those are **not** fields here yet: their correct representation is a
//! *forwardable cell* (`Arc<ArcSwap<…>>` / `Arc::default()`) created in Phase
//! A and *seated* when the renderer wiring runs (the §5 crux). Modeling them
//! as eager fields in BC.1 would bake the wrong shape; they join at BC.3 with
//! the forwardable-cell semantics. The mode / command / service registration
//! helpers likewise land when boot actually constructs the context (BC.3),
//! against the live registries — not stubbed here.

use std::sync::Arc;

use std::any::Any;

use lattice_grammar::CommandRegistry;
use lattice_grammar::effect::Effect;
use lattice_lsp::modes::DiagnosticsQueryHandle;
use lattice_mode::inbound::{InboundBus, make_inbound};
use lattice_mode::tick_callback::{
    TickCallback, TickCallbackRegistration, TickCallbackRegistryHandle,
};
use lattice_mode::{BufferStoreHandle, ModeRegistry, ServiceRegistry};
use lattice_protocol::event_registry::Event as TypedEvent;
use lattice_runtime::EventBus;
use tokio::runtime::Handle;
use tokio::sync::{Notify, mpsc};

/// The host's generic-primitive surface, handed to per-subsystem wiring.
///
/// Holds shared handles by `Arc` (cheap to clone) plus the boot-lifetime
/// tick-callback registration tokens, so drains registered via
/// [`inbound`](Self::inbound) / [`tick_callback`](Self::tick_callback)
/// outlive the construction phase. The tokens move into the `Editor` (program
/// lifetime) via [`into_registrations`](Self::into_registrations); dropping the
/// `BootContext` without taking them drops the drains.
///
/// ## BC.3a — registry ownership (decision 2-b)
///
/// `BootContext` **owns** the three registries during the build phase. A
/// subsystem registers its modes / commands / services through `boot`
/// ([`modes_mut`](Self::modes_mut) / [`commands_mut`](Self::commands_mut) /
/// [`register_service`](Self::register_service)); host-internal `editor_boot`
/// code uses the same seam until every subsystem has migrated (BC.final removes
/// the `*_mut` accessors once nothing inline remains). The registries are held
/// behind `Option` and *taken* on [`freeze_command_registry`](Self::freeze_command_registry)
/// / [`freeze_mode_registry`](Self::freeze_mode_registry) /
/// [`freeze_service_registry`](Self::freeze_service_registry): the
/// `CommandRegistry` freezes mid-boot (its `Arc` feeds the picker registry +
/// document handles), the `ModeRegistry` after `register_mode_toggle_commands`,
/// the `ServiceRegistry` last. Registering into an already-frozen registry is a
/// boot-sequencing bug and panics with a clear message.
pub struct BootContext {
    event_bus: Arc<EventBus>,
    tick_callbacks: TickCallbackRegistryHandle,
    async_landed: Arc<Notify>,
    runtime_handle: Handle,
    /// Boot-lifetime tick-callback registration tokens (held so the drains
    /// they represent are not unregistered the instant `inbound` /
    /// `tick_callback` returns).
    registrations: Vec<TickCallbackRegistration>,
    /// BC.3a — read-tool handles derived from the Phase-A `BufferRegistry` /
    /// `render_state` cell; consumed by the claude-code (and future) read tools.
    buffer_store: BufferStoreHandle,
    diagnostics: DiagnosticsQueryHandle,
    /// BC.3a — owned registries, `None` once frozen (taken by `freeze_*`).
    command_registry: Option<CommandRegistry>,
    mode_registry: Option<ModeRegistry>,
    service_registry: Option<ServiceRegistry>,
}

impl BootContext {
    /// Bundle the host primitives + the registries `editor_boot` will populate
    /// through this context. The three registries are passed in empty (fresh
    /// `*::new()`); `editor_boot` and the per-subsystem installs register into
    /// them via `boot` and `freeze_*` them at the right points.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_bus: Arc<EventBus>,
        tick_callbacks: TickCallbackRegistryHandle,
        async_landed: Arc<Notify>,
        runtime_handle: Handle,
        buffer_store: BufferStoreHandle,
        diagnostics: DiagnosticsQueryHandle,
        command_registry: CommandRegistry,
        mode_registry: ModeRegistry,
        service_registry: ServiceRegistry,
    ) -> Self {
        Self {
            event_bus,
            tick_callbacks,
            async_landed,
            runtime_handle,
            registrations: Vec::new(),
            buffer_store,
            diagnostics,
            command_registry: Some(command_registry),
            mode_registry: Some(mode_registry),
            service_registry: Some(service_registry),
        }
    }

    /// The bundled **inbound** primitive (`boot-composition.md` §3).
    ///
    /// Creates a channel whose [`InboundBus::send`] wakes `async_landed` —
    /// the wake is inside the sender, structurally impossible to forget — and
    /// registers a per-tick drain that runs each pending item through
    /// `handler` (validate → map to an existing [`Effect`]) and returns the
    /// effects for the host to apply. The RAII registration token is retained
    /// for boot lifetime. Returns the bus for the off-thread producer.
    pub fn inbound<T, H>(&mut self, handler: H) -> InboundBus<T>
    where
        T: Send + 'static,
        H: FnMut(T) -> Vec<Effect> + Send + 'static,
    {
        let (bus, drain) = make_inbound::<T, H>(Arc::clone(&self.async_landed), handler);
        let reg = self.tick_callbacks.register(drain);
        self.registrations.push(reg);
        bus
    }

    /// Subscribe a typed event and **wake `async_landed`** whenever one is
    /// published — generalizes the hand-rolled `wake_on` forwarder tasks. The
    /// forwarder runs on the shared runtime and lives for the program (it
    /// ends only when the event bus drops the subscription).
    pub fn wake_on_event<E>(&self)
    where
        E: TypedEvent + Clone,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<E>();
        self.event_bus.subscribe_typed(tx);
        let wake = Arc::clone(&self.async_landed);
        self.runtime_handle.spawn(async move {
            while rx.recv().await.is_some() {
                wake.notify_one();
            }
        });
    }

    /// Register a raw per-tick drain closure (the I1 registry), retaining the
    /// RAII token for boot lifetime. Used for drains that are not channel
    /// inbound buses (e.g. a state-poll closure).
    pub fn tick_callback(&mut self, callback: TickCallback) {
        let reg = self.tick_callbacks.register(callback);
        self.registrations.push(reg);
    }

    /// The editor's off-keystroke wake handle.
    pub fn async_landed(&self) -> &Arc<Notify> {
        &self.async_landed
    }

    /// The shared tick-callback registry (run once per tick by the host).
    pub fn tick_callbacks(&self) -> &TickCallbackRegistryHandle {
        &self.tick_callbacks
    }

    /// The typed event bus.
    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }

    /// The shared runtime handle (off-thread spawns).
    pub fn runtime_handle(&self) -> &Handle {
        &self.runtime_handle
    }

    // ── BC.3a: registry ownership (decision 2-b) ───────────────────────────

    /// Mutable access to the command registry — the seam command-registration
    /// runs through (subsystem `register_*_ex_commands(boot.commands_mut(), …)`
    /// + host-internal builtins). Panics if the registry is already frozen.
    pub fn commands_mut(&mut self) -> &mut CommandRegistry {
        self.command_registry
            .as_mut()
            .expect("command registry already frozen (registered after freeze_command_registry)")
    }

    /// Mutable access to the mode registry — the seam mode-registration runs
    /// through (subsystem `register_*_modes(boot.modes_mut())`). Panics if the
    /// registry is already frozen.
    pub fn modes_mut(&mut self) -> &mut ModeRegistry {
        self.mode_registry
            .as_mut()
            .expect("mode registry already frozen (registered after freeze_mode_registry)")
    }

    /// Mutable access to the service registry — for bulk / multi-step service
    /// wiring. Most callers want [`register_service`](Self::register_service).
    /// Panics if the registry is already frozen.
    pub fn services_mut(&mut self) -> &mut ServiceRegistry {
        self.service_registry
            .as_mut()
            .expect("service registry already frozen (registered after freeze_service_registry)")
    }

    /// Register a single service handle under its `TypeId` (the common case).
    /// Per the `ServiceRegistry` Arc/TypeId rule, register and look up with the
    /// same `T`.
    pub fn register_service<T: Any + Send + Sync>(&mut self, service: T) {
        self.services_mut().register(service);
    }

    /// The read-tool buffer-store handle (Phase-A `BufferRegistry`).
    pub fn buffer_store(&self) -> &BufferStoreHandle {
        &self.buffer_store
    }

    /// The read-tool diagnostics-query handle (over the Phase-A render-state cell).
    pub fn diagnostics(&self) -> &DiagnosticsQueryHandle {
        &self.diagnostics
    }

    /// Freeze the command registry into its shared `Arc` and take it out of the
    /// context. Called mid-boot, after all command registration, before the
    /// `Arc` is consumed (picker registry, document handles). Subsequent
    /// `commands_mut` panics.
    pub fn freeze_command_registry(&mut self) -> Arc<CommandRegistry> {
        Arc::new(
            self.command_registry
                .take()
                .expect("command registry already frozen"),
        )
    }

    /// Freeze the mode registry into its shared `Arc` and take it out. Called
    /// after `register_mode_toggle_commands`. Subsequent `modes_mut` panics.
    pub fn freeze_mode_registry(&mut self) -> Arc<ModeRegistry> {
        Arc::new(
            self.mode_registry
                .take()
                .expect("mode registry already frozen"),
        )
    }

    /// Freeze the service registry into its shared `Arc` and take it out.
    /// Called last, after the services block. Subsequent `services_mut` panics.
    pub fn freeze_service_registry(&mut self) -> Arc<ServiceRegistry> {
        Arc::new(
            self.service_registry
                .take()
                .expect("service registry already frozen"),
        )
    }

    /// Take the boot-lifetime tick-callback registration tokens. BC.3 calls
    /// this to move them into the `Editor` so the drains live for the program
    /// rather than being dropped when the `BootContext` is dropped.
    pub fn into_registrations(self) -> Vec<TickCallbackRegistration> {
        self.registrations
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::buffer_registry::BufferRegistry;
    use crate::diagnostics_query::HostDiagnosticsQuery;
    use crate::render_state::RenderState;
    use lattice_mode::tick_callback::TickCallbackRegistry;
    use std::sync::Mutex;
    use std::time::Duration;

    fn ctx() -> BootContext {
        let render_state = Arc::new(arc_swap::ArcSwap::from_pointee(RenderState::default()));
        let buffer_store: BufferStoreHandle =
            BufferStoreHandle::new(Arc::new(BufferRegistry::new()));
        let diagnostics: DiagnosticsQueryHandle = Arc::new(HostDiagnosticsQuery::new(render_state));
        BootContext::new(
            Arc::new(EventBus::new()),
            Arc::new(TickCallbackRegistry::new()),
            Arc::new(Notify::new()),
            Handle::current(),
            buffer_store,
            diagnostics,
            CommandRegistry::new(),
            ModeRegistry::new(),
            ServiceRegistry::new(),
        )
    }

    #[tokio::test]
    async fn registries_register_then_freeze_into_arcs() {
        let mut ctx = ctx();
        // A service registered through the context survives into the frozen Arc.
        ctx.register_service::<u64>(42);
        // The command / mode registries are reachable + mutable pre-freeze.
        let _ = ctx.commands_mut();
        let _ = ctx.modes_mut();

        let services = ctx.freeze_service_registry();
        assert_eq!(
            services.get::<u64>().as_deref(),
            Some(&42),
            "registered service is present in the frozen registry"
        );
        let _commands = ctx.freeze_command_registry();
        let _modes = ctx.freeze_mode_registry();
    }

    #[tokio::test]
    #[should_panic(expected = "service registry already frozen")]
    async fn register_after_freeze_panics() {
        let mut ctx = ctx();
        let _ = ctx.freeze_service_registry();
        // Registering after the freeze is a boot-sequencing bug.
        ctx.register_service::<u64>(7);
    }

    #[tokio::test]
    async fn inbound_drains_via_tick_registry_and_send_wakes() {
        let mut ctx = ctx();
        let seen = Arc::new(Mutex::new(Vec::<u32>::new()));
        let seen_in = Arc::clone(&seen);
        let bus = ctx.inbound::<u32, _>(move |n| {
            seen_in.lock().unwrap().push(n);
            vec![Effect::None]
        });

        // The drain is registered on the shared registry, token retained.
        assert_eq!(ctx.tick_callbacks().registered_count(), 1);

        bus.send(1).unwrap();
        bus.send(2).unwrap();

        // `send` woke the editor off-keystroke.
        let woke =
            tokio::time::timeout(Duration::from_millis(200), ctx.async_landed().notified()).await;
        assert!(woke.is_ok(), "inbound send must wake the editor");

        // The host's per-tick run drains both items through the handler.
        let effects = ctx.tick_callbacks().run_all();
        assert_eq!(effects.len(), 2, "both pending items drained as effects");
        assert_eq!(*seen.lock().unwrap(), vec![1, 2], "handler saw items in order");
    }

    #[tokio::test]
    async fn tick_callback_registration_is_retained() {
        let mut ctx = ctx();
        ctx.tick_callback(Box::new(|| vec![Effect::None]));
        // Token lives in the context, so the drain survives past the call.
        assert_eq!(ctx.tick_callbacks().registered_count(), 1);
        assert_eq!(ctx.tick_callbacks().run_all().len(), 1);
    }

    #[tokio::test]
    async fn into_registrations_hands_off_the_tokens() {
        let mut ctx = ctx();
        ctx.tick_callback(Box::new(|| vec![Effect::None]));
        let registry = Arc::clone(ctx.tick_callbacks());
        let tokens = ctx.into_registrations();
        assert_eq!(tokens.len(), 1, "one retained registration handed off");
        // While the caller holds the tokens, the drain stays registered.
        assert_eq!(registry.registered_count(), 1);
        // Dropping them (BC.3 hands them to the Editor; here we just drop)
        // unregisters the drain — proving the tokens are the lifetime anchor.
        drop(tokens);
        assert_eq!(registry.registered_count(), 0);
    }

    #[tokio::test]
    async fn wake_on_event_fires_the_wake() {
        // `LspInlayHintRefresh` is just a convenient concrete `TypedEvent +
        // Clone` fixture; the mechanism under test is generic.
        use lattice_lsp::LspInlayHintRefresh;

        let ctx = ctx();
        ctx.wake_on_event::<LspInlayHintRefresh>();
        ctx.event_bus().publish_typed(LspInlayHintRefresh {
            server_id: Arc::from("test-server"),
        });

        let woke =
            tokio::time::timeout(Duration::from_millis(200), ctx.async_landed().notified()).await;
        assert!(woke.is_ok(), "publishing a subscribed event must wake the editor");
    }
}
