//! Boot-composition: the `BootContext` — the host's generic-primitive surface.
//!
//! `editor_boot.rs` was a ~1700-line god-function where every async subsystem
//! hand-wired the same six things: mode registration, command registration,
//! service registration, an `async_landed` wake, a per-tick drain, and a
//! deferred install for late handles. `BootContext` is that surface made
//! explicit — the typed bundle a subsystem's `install(boot)` receives, exposing
//! the easy-to-get-wrong operations as *primitives that cannot be wired without
//! their safety property*.
//!
//! Design fragment: `docs/dev/architecture/boot-composition.md`.
//! Slice plan: `docs/dev/operations/slice-plans/boot-composition.md`.
//!
//! ## Status
//!
//! - **BC.1** ✅ — the skeleton + the two wake-robustness primitives below.
//! - **BC.3a** ✅ — `editor_boot::Editor::boot` now builds a `BootContext` in a
//!   Phase-A block and routes **all** command / mode / service registration
//!   through it (`commands_mut` / `modes_mut` / `register_service`), freezing
//!   each registry into the `Arc` the `Editor` literal seats. The `render_state`
//!   cell / `BufferStore` / `DiagnosticsQueryHandle` are fields here (built in
//!   Phase A); the §5 "forwardable cell" worry did not match the code (both are
//!   default-init / early-seeded Arc-shared cells — see the design fragment §5
//!   re-assessment), so the hoist preserved Arc identity by moving `let`
//!   bindings, never reconstructing.
//! - **BC.3b+** 🚧 — per-subsystem `install(boot)` migrations (claude-code
//!   first), collapsing each subsystem's scattered wiring into one call.
//!
//! ## Wake-robustness primitives
//!
//! - [`BootContext::inbound`] — the bundled inbound primitive. A channel
//!   whose `send` wakes `async_landed` (the wake is inside the sender, so it
//!   is structurally impossible to forget) and whose items are drained
//!   per-tick via the tick-callback registry through a handler. Generalizes
//!   the I3 `ClaudeCodeInboundBus` and LSP's hand-rolled inbound buses. (Not
//!   yet consumed by `editor_boot` — BC.3b is its first caller.)
//! - [`BootContext::wake_on_event`] — subscribe a typed event and wake
//!   `async_landed` whenever one is published. Generalizes the
//!   `MultibufferExcerptsReady` / L1c `wake_on` forwarder tasks.
//! - [`BootContext::tick_callback`] — register a raw per-tick drain (the I1
//!   registry), retaining the RAII token for boot lifetime.

use std::sync::Arc;

use std::any::Any;

use lattice_grammar::CommandRegistry;
use lattice_grammar::effect::Effect;
use lattice_grammar::registry::OperatorId;
use lattice_keymap::KeymapLayer;
use lattice_mode::idle_gate::{IdleGateHandle, IdleGateHandler, IdleGateRegistryHandle};
use lattice_mode::inbound::{InboundBus, make_inbound, make_inbound_raw};
use lattice_mode::tick_callback::{
    TickCallback, TickCallbackRegistration, TickCallbackRegistryHandle,
};
use lattice_mode::{
    BufferStoreHandle, ModeRegistry, OperatorChordWirerHandle, ServiceRegistry, SubsystemBoot,
};
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
/// subsystem registers its modes / commands / services through `boot` — the
/// [`SubsystemBoot`] surface ([`modes_mut`](SubsystemBoot::modes_mut) /
/// [`commands_mut`](SubsystemBoot::commands_mut) /
/// [`register_service`](SubsystemBoot::register_service)); host-native
/// **builtins** (the native vim grammar, ex-commands, foundation / language /
/// oil / file-tree / snippet / tutor / buffer-kind modes, host actions,
/// mode-toggle commands, syntax text objects) register through the SAME seam.
/// **BC.final finding (2026-06-25):** the `*_mut` accessors are NOT transitional
/// — those builtins are host-native (not subsystems, never installed), so the
/// accessors are the *permanent* builtin-registration seam, kept by design. The
/// mode-ownership acid test ("a new SUBSYSTEM touches boot in one place")
/// governs the Phase-B `install` list, not the host's own builtins. The
/// registries are held
/// behind `Option` and *taken* on [`freeze_command_registry`](Self::freeze_command_registry)
/// / [`freeze_mode_registry`](Self::freeze_mode_registry) /
/// [`freeze_service_registry`](Self::freeze_service_registry). The freeze order
/// `editor_boot` uses: the `ModeRegistry` first (right after the mode-
/// registration block — its `Arc` is needed by `register_mode_toggle_commands`,
/// which borrows `&mut CommandRegistry` + `&ModeRegistry` at once and so cannot
/// hold both through `boot`; freezing modes first hands back an
/// `Arc<ModeRegistry>` that derefs to `&ModeRegistry`), then the
/// `CommandRegistry` mid-boot (its `Arc` feeds the picker registry + document
/// handles), then the `ServiceRegistry` last. Freezing only wraps + takes the
/// registry; the populated data is unchanged, so the order is behaviour-neutral.
/// Registering into an already-frozen registry is a boot-sequencing bug and
/// panics with a clear message.
pub struct BootContext {
    event_bus: Arc<EventBus>,
    tick_callbacks: TickCallbackRegistryHandle,
    async_landed: Arc<Notify>,
    runtime_handle: Handle,
    /// Boot-lifetime tick-callback registration tokens (held so the drains
    /// they represent are not unregistered the instant `inbound` /
    /// `tick_callback` returns).
    registrations: Vec<TickCallbackRegistration>,
    /// BC.3a — the generic buffer-store handle (over the Phase-A
    /// `BufferRegistry`); exposed via [`SubsystemBoot::buffer_store`] and
    /// consumed by the claude-code (and future) read tools. (The LSP
    /// diagnostics handle is NOT a field — subsystems reach it via the generic
    /// [`SubsystemBoot::service`] lookup, keeping the trait free of lattice-lsp
    /// types; the host registers it as a Phase-A service.)
    buffer_store: BufferStoreHandle,
    /// WK.3 — the idle-gate registry: subsystem-armed deadlines the actor
    /// sleeps to. Shared with the `Editor` (which hands it to the actor loop),
    /// so a gate armed by a subsystem's handler reaches the actor's `select!`.
    idle_gates: IdleGateRegistryHandle,
    /// BC.3a — owned registries, `None` once frozen (taken by `freeze_*`).
    command_registry: Option<CommandRegistry>,
    mode_registry: Option<ModeRegistry>,
    service_registry: Option<ServiceRegistry>,
    /// LL.9 — the operator binder, set once the keymap exists
    /// ([`set_operator_binder`](Self::set_operator_binder)).
    operator_binder: Option<OperatorChordWirerHandle>,
    /// LL.9 — `bind_operator` calls made before the binder existed (every
    /// install runs ahead of the keymap), flushed in order by
    /// `set_operator_binder`.
    pending_operator_bindings: Vec<PendingOperatorBinding>,
}

/// One `bind_operator` request awaiting the keymap.
struct PendingOperatorBinding {
    layer: KeymapLayer,
    /// As written: `<leader>` is expanded, and the chord parsed, by the
    /// binder, once the keymap knows the leader.
    prefix: String,
    op: OperatorId,
    post_motion_char: bool,
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
        idle_gates: IdleGateRegistryHandle,
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
            idle_gates,
            command_registry: Some(command_registry),
            mode_registry: Some(mode_registry),
            service_registry: Some(service_registry),
            operator_binder: None,
            pending_operator_bindings: Vec::new(),
        }
    }

    /// The editor's off-keystroke wake handle (host-only lifecycle accessor;
    /// the subsystem install surface is the [`SubsystemBoot`] impl below).
    pub fn async_landed(&self) -> &Arc<Notify> {
        &self.async_landed
    }

    /// BC.8d: a *host-drained* inbound bus — the wake-baked sender PLUS the raw
    /// receiver, with no per-tick handler. For server-initiated work whose apply
    /// is irreducibly `&mut Editor` (LSP `workspace/applyEdit`): the host seats
    /// the receiver on the `Editor` and drains it from `run_tick_pending`, while
    /// `send` still wakes the editor off-keystroke (the wake lives in the sender
    /// — can't be forgotten). Keeps the irreducible apply host-side without an
    /// internal-pump `Effect`. Inherent (not on [`SubsystemBoot`]): only the
    /// host's own Phase-A wiring uses it; no subsystem `install` does.
    pub fn inbound_raw<T>(&self) -> (InboundBus<T>, tokio::sync::mpsc::UnboundedReceiver<T>)
    where
        T: Send + 'static,
    {
        make_inbound_raw::<T>(Arc::clone(&self.async_landed))
    }

    /// The shared tick-callback registry (run once per tick by the host).
    pub fn tick_callbacks(&self) -> &TickCallbackRegistryHandle {
        &self.tick_callbacks
    }

    /// WK.3: the shared idle-gate registry. The host seats this on the
    /// `Editor` so the actor loop can point its pinned sleep at
    /// `earliest()` and fire the due gates when it elapses.
    pub fn idle_gates(&self) -> &IdleGateRegistryHandle {
        &self.idle_gates
    }

    /// Freeze the command registry into its shared runtime-mutable handle
    /// (`ArcSwap`) and take it out of the context. Called mid-boot, after all
    /// command registration, before the handle is consumed (picker registry,
    /// document handles). Subsequent `commands_mut` panics.
    ///
    /// PL8.B / B3b: `ArcSwap` (not a bare `Arc`) so the plugin loader can
    /// RCU-register a runtime grammar contribution; the dispatch path — the
    /// per-buffer actor and every host-side ex-command / completion read —
    /// snapshots it wait-free. Mirrors [`Self::freeze_mode_registry`].
    pub fn freeze_command_registry(&mut self) -> lattice_grammar::CommandRegistryHandle {
        Arc::new(arc_swap::ArcSwap::from_pointee(
            self.command_registry
                .take()
                .expect("command registry already frozen"),
        ))
    }

    /// Freeze the mode registry into its shared runtime-mutable handle
    /// (`ArcSwap`) and take it out. Called after `register_mode_toggle_commands`.
    /// Subsequent `modes_mut` panics. `ArcSwap` (not a bare `Arc`) so the plugin
    /// loader can RCU-register a runtime mode; reads snapshot it wait-free.
    pub fn freeze_mode_registry(&mut self) -> lattice_mode::ModeRegistryHandle {
        Arc::new(arc_swap::ArcSwap::from_pointee(
            self.mode_registry
                .take()
                .expect("mode registry already frozen"),
        ))
    }

    /// LL.9 — hand over the binder that writes operator chords into the live
    /// keymap, and flush every [`SubsystemBoot::bind_operator`] made before it
    /// existed. Called by `editor_boot` as soon as the keymap handle does;
    /// later `bind_operator` calls bind immediately.
    pub fn set_operator_binder(&mut self, binder: OperatorChordWirerHandle) {
        for p in std::mem::take(&mut self.pending_operator_bindings) {
            Self::bind_now(&binder, &p);
        }
        self.operator_binder = Some(binder);
    }

    fn bind_now(binder: &OperatorChordWirerHandle, p: &PendingOperatorBinding) {
        let expanded = binder.expand_leader(&p.prefix);
        let prefix = match lattice_protocol::parse_chord_sequence(&expanded) {
            Ok(chords) if !chords.is_empty() => chords,
            outcome => {
                tracing::error!(
                    prefix = %p.prefix,
                    ?outcome,
                    "bind_operator: chord does not parse, operator left unbound"
                );
                return;
            }
        };
        // The linewise form repeats the prefix's last key (`gaa`, `yy`).
        binder.bind_in(
            p.layer,
            lattice_grammar::SourceLocation::synthetic("subsystem-operator"),
            &prefix,
            p.op,
            prefix.last().cloned(),
            p.post_motion_char,
        );
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

/// BC.3b: the subsystem install surface. Subsystems wire against this trait
/// (defined in `lattice-mode`, below them) instead of the concrete
/// `BootContext` (in `lattice-host`, above them — which would cycle). Host-only
/// lifecycle (`new`, `freeze_*`, `into_registrations`, the
/// `async_landed`/`tick_callbacks` accessors) stays inherent above; only the
/// generic install operations live here. The LSP diagnostics handle is reached
/// via [`service`](SubsystemBoot::service), so this surface names no
/// `lattice-lsp` type.
impl SubsystemBoot for BootContext {
    fn commands_mut(&mut self) -> &mut CommandRegistry {
        self.command_registry
            .as_mut()
            .expect("command registry already frozen (registered after freeze_command_registry)")
    }

    fn modes_mut(&mut self) -> &mut ModeRegistry {
        self.mode_registry
            .as_mut()
            .expect("mode registry already frozen (registered after freeze_mode_registry)")
    }

    fn services_mut(&mut self) -> &mut ServiceRegistry {
        self.service_registry
            .as_mut()
            .expect("service registry already frozen (registered after freeze_service_registry)")
    }

    fn register_service<T: Any + Send + Sync>(&mut self, service: T) {
        self.services_mut().register(service);
    }

    fn service<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.service_registry.as_ref().and_then(|r| r.get::<T>())
    }

    fn inbound<T, H>(&mut self, handler: H) -> InboundBus<T>
    where
        T: Send + 'static,
        H: FnMut(T) -> Vec<Effect> + Send + 'static,
    {
        let (bus, drain) = make_inbound::<T, H>(Arc::clone(&self.async_landed), handler);
        let reg = self.tick_callbacks.register(drain);
        self.registrations.push(reg);
        bus
    }

    fn wake_on_event<E>(&self)
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

    fn tick_callback(&mut self, callback: TickCallback) {
        let reg = self.tick_callbacks.register(callback);
        self.registrations.push(reg);
    }

    fn idle_gate(&mut self, name: &'static str, handler: IdleGateHandler) -> IdleGateHandle {
        self.idle_gates.register(name, handler)
    }

    fn bind_operator(&mut self, layer: KeymapLayer, prefix: &str, op: OperatorId) {
        if prefix.is_empty() {
            tracing::error!("bind_operator: empty chord, operator left unbound");
            return;
        }
        // A bind after the freeze is a programming error, but not worth a panic
        // at boot: log it and leave the operator unbound, as a bad chord does.
        let Some(registry) = self.command_registry.as_ref() else {
            tracing::error!(prefix = ?prefix, "bind_operator: called after the command registry froze, operator left unbound");
            return;
        };
        let post_motion_char = registry.operator_post_motion_char(op.0);
        let pending = PendingOperatorBinding {
            layer,
            prefix: prefix.to_string(),
            op,
            post_motion_char,
        };
        match &self.operator_binder {
            Some(binder) => Self::bind_now(binder, &pending),
            None => self.pending_operator_bindings.push(pending),
        }
    }

    fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }

    fn runtime_handle(&self) -> &Handle {
        &self.runtime_handle
    }

    fn buffer_store(&self) -> &BufferStoreHandle {
        &self.buffer_store
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::buffer_registry::BufferRegistry;
    use lattice_mode::tick_callback::TickCallbackRegistry;
    use lattice_protocol::chord::KeyChord;
    use std::sync::Mutex;
    use std::time::Duration;

    fn ctx() -> BootContext {
        let buffer_store: BufferStoreHandle =
            BufferStoreHandle::new(Arc::new(BufferRegistry::new()));
        BootContext::new(
            Arc::new(EventBus::new()),
            Arc::new(TickCallbackRegistry::new()),
            Arc::new(Notify::new()),
            Handle::current(),
            buffer_store,
            Arc::new(lattice_mode::idle_gate::IdleGateRegistry::new()),
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
        assert_eq!(
            *seen.lock().unwrap(),
            vec![1, 2],
            "handler saw items in order"
        );
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

    /// MG.18d — magit's post-refresh cursor must reach the screen with
    /// NO keypress.
    ///
    /// The mechanism is easy to get subtly wrong: a bare
    /// `tick_callback` would look identical in code review and pass any
    /// test that dispatches an action first, because `run_tick_pending`
    /// is also reached from `App::apply`'s tail. It would then sit
    /// until the user pressed something — "staging works but the cursor
    /// only catches up when I touch a key".
    ///
    /// So this asserts the wake specifically: send on magit's cursor
    /// bus, and `async_landed` (the actor's off-keystroke arm) must
    /// fire, with the drain producing the targeted cursor move.
    #[tokio::test]
    async fn magits_cursor_bus_wakes_the_editor_without_a_keypress() {
        use lattice_magit::cursor_restore::{CursorBusHandle, CursorRequest};
        use lattice_protocol::position::Position;

        let mut ctx = ctx();
        lattice_magit::install(&mut ctx);

        let bus = ctx
            .service::<CursorBusHandle>()
            .expect("magit installs a cursor bus");
        let target = lattice_core::BufferId(7);
        (*bus)
            .send(CursorRequest {
                buffer: target,
                position: Position::new(4, 0),
            })
            .map_err(|_| "send")
            .expect("bus accepts the request");

        let woke =
            tokio::time::timeout(Duration::from_millis(200), ctx.async_landed().notified()).await;
        assert!(
            woke.is_ok(),
            "the send must wake the editor — otherwise the cursor waits for a keypress"
        );

        let effects = ctx.tick_callbacks().run_all();
        let moved = effects.iter().any(|e| {
            matches!(
                e,
                Effect::CursorMoveIn { target: t, position }
                    if *t == target && *position == Position::new(4, 0)
            )
        });
        assert!(
            moved,
            "the drain must yield a buffer-targeted cursor move, got {effects:?}"
        );
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
        assert!(
            woke.is_ok(),
            "publishing a subscribed event must wake the editor"
        );
    }

    // ── LL.9: bind_operator ───────────────────────────────────────────────

    type Bound = Vec<(KeymapLayer, Vec<KeyChord>, Option<KeyChord>, bool)>;

    #[derive(Default)]
    struct RecordingBinder(Mutex<Bound>);

    impl lattice_mode::OperatorChordWirer for RecordingBinder {
        fn wire(
            &self,
            _: OperatorId,
            _: &str,
            _: Option<char>,
            _: lattice_mode::ModeId,
            _: lattice_mode::PluginOrigin<'_>,
            _: bool,
        ) -> Result<(), String> {
            Ok(())
        }

        fn bind_in(
            &self,
            layer: KeymapLayer,
            _: lattice_grammar::SourceLocation,
            prefix: &[KeyChord],
            _: OperatorId,
            doubled: Option<KeyChord>,
            post_motion_char: bool,
        ) {
            self.0
                .lock()
                .unwrap()
                .push((layer, prefix.to_vec(), doubled, post_motion_char));
        }
    }

    fn probe_op(boot: &mut BootContext, post_motion_char: bool) -> OperatorId {
        use lattice_grammar::registry::OperatorSpec;
        boot.commands_mut().register_operator(
            "operator:ll9-boot-probe",
            "",
            OperatorSpec {
                repeatable: false,
                mutates: false,
                args_schema: vec![],
                blockwise_per_row: false,
                post_motion_char,
                apply: Arc::new(|_| Ok(Effect::None)),
            },
        )
    }

    fn layer() -> KeymapLayer {
        KeymapLayer::MinorMode(lattice_mode::ModeId::new("ll9-boot-mode"))
    }

    /// Every install runs before the keymap exists, so a bind made then must
    /// be held and flushed, in order, with the operator's own
    /// `post_motion_char` and the prefix's last key as the doubled form.
    #[tokio::test]
    async fn bind_operator_before_the_binder_is_held_then_flushed() {
        let mut boot = ctx();
        let op = probe_op(&mut boot, true);
        boot.bind_operator(layer(), "gX", op);
        boot.bind_operator(layer(), "<Space>a", op);

        let binder = Arc::new(RecordingBinder::default());
        assert!(binder.0.lock().unwrap().is_empty(), "nothing bound yet");
        boot.set_operator_binder(binder.clone());

        let bound = binder.0.lock().unwrap();
        assert_eq!(bound.len(), 2, "both flushed");
        assert_eq!(bound[0].1, vec![KeyChord::char('g'), KeyChord::char('X')]);
        assert_eq!(bound[0].2, Some(KeyChord::char('X')), "doubled = last key");
        assert!(bound[0].3, "post_motion_char read off the operator");
        assert_eq!(bound[0].0, layer());
        assert_eq!(bound[1].1.len(), 2, "`<Space>a` parses to two chords");
    }

    #[tokio::test]
    async fn bind_operator_after_the_binder_binds_immediately() {
        let mut boot = ctx();
        let op = probe_op(&mut boot, false);
        let binder = Arc::new(RecordingBinder::default());
        boot.set_operator_binder(binder.clone());
        boot.bind_operator(layer(), "gX", op);
        let bound = binder.0.lock().unwrap();
        assert_eq!(bound.len(), 1);
        assert!(!bound[0].3);
    }

    /// LL.10b: `<leader>` is the binder's to expand, at bind time, as the
    /// keymap expands it for its own chords; a subsystem writes `<leader>aa`.
    #[tokio::test]
    async fn bind_operator_expands_the_leader_through_the_binder() {
        #[derive(Default)]
        struct CommaLeader(RecordingBinder);
        impl lattice_mode::OperatorChordWirer for CommaLeader {
            fn wire(
                &self,
                _: OperatorId,
                _: &str,
                _: Option<char>,
                _: lattice_mode::ModeId,
                _: lattice_mode::PluginOrigin<'_>,
                _: bool,
            ) -> Result<(), String> {
                Ok(())
            }
            fn bind_in(
                &self,
                layer: KeymapLayer,
                source: lattice_grammar::SourceLocation,
                prefix: &[KeyChord],
                op: OperatorId,
                doubled: Option<KeyChord>,
                post_motion_char: bool,
            ) {
                self.0
                    .bind_in(layer, source, prefix, op, doubled, post_motion_char);
            }
            fn expand_leader(&self, chord: &str) -> String {
                chord.replace("<leader>", ",")
            }
        }
        let mut boot = ctx();
        let op = probe_op(&mut boot, false);
        let binder = Arc::new(CommaLeader::default());
        boot.set_operator_binder(binder.clone());
        boot.bind_operator(layer(), "<leader>aa", op);
        let bound = binder.0.0.lock().unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(
            bound[0].1,
            vec![
                KeyChord::char(','),
                KeyChord::char('a'),
                KeyChord::char('a')
            ]
        );
        assert_eq!(bound[0].2, Some(KeyChord::char('a')));
    }

    /// A chord that does not parse is logged and skipped, not a failed boot
    /// and not a half-bound prefix.
    #[tokio::test]
    async fn bind_operator_with_an_unparseable_chord_binds_nothing() {
        let mut boot = ctx();
        let op = probe_op(&mut boot, false);
        let binder = Arc::new(RecordingBinder::default());
        boot.set_operator_binder(binder.clone());
        boot.bind_operator(layer(), "<NotAKey", op);
        boot.bind_operator(layer(), "", op);
        assert!(binder.0.lock().unwrap().is_empty());
    }
}
