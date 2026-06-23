//! `SubsystemBoot` — the capability surface a subsystem wires against at boot.
//!
//! Boot-composition BC.3b. Every subsystem (the Claude Code IDE peer, LSP,
//! multibuffer, terminal, …) self-installs through one crate-owned entry point
//!
//! ```ignore
//! pub fn install(boot: &mut impl SubsystemBoot) { … }
//! ```
//!
//! that does *all* of its wiring — modes, commands, services, the off-keystroke
//! inbound bus, event wakes — against the generic primitives this trait exposes.
//! The host (`editor_boot`) then has a single Phase-B *install list*: one line
//! per subsystem. Adding a subsystem touches the host in exactly that one place
//! and zero host internals (no `Editor::` method, no host `Action` variant) —
//! the mode-ownership acid test, and the property that keeps host churn flat as
//! the mode count grows into the hundreds.
//!
//! ## Why a trait (not the concrete context)
//!
//! The concrete bundle (`lattice_host::boot_context::BootContext`) lives in
//! `lattice-host`, which depends on every subsystem crate — so a subsystem
//! crate cannot name it without a dependency cycle. `SubsystemBoot` lives here
//! in `lattice-mode` (below every subsystem crate), exposes only the generic
//! install surface, and is implemented by `BootContext`. Subsystems depend on
//! the capability, not the host. Host-only lifecycle (registry freezing,
//! tick-token hand-off, the LSP-specific diagnostics handle) stays inherent on
//! `BootContext` and never leaks into this surface — the LSP diagnostics handle,
//! for instance, is reached through the generic [`service`](SubsystemBoot::service)
//! lookup, so this trait never names a `lattice-lsp` type.
//!
//! The generic methods make the trait non-object-safe; installs take
//! `&mut impl SubsystemBoot` (static dispatch), so object safety is not needed.

use std::any::Any;
use std::sync::Arc;

use lattice_grammar::CommandRegistry;
use lattice_grammar::effect::Effect;
use lattice_protocol::event_registry::Event as TypedEvent;
use lattice_runtime::EventBus;
use tokio::runtime::Handle;

use crate::inbound::InboundBus;
use crate::tick_callback::TickCallback;
use crate::{BufferStoreHandle, ModeRegistry, ServiceRegistry};

/// The generic-primitive surface a subsystem's `install(boot)` wires against.
///
/// Implemented by the host's `BootContext`. See the module docs for the
/// install pattern and why this is a trait rather than the concrete bundle.
pub trait SubsystemBoot {
    /// Mutable access to the command registry — register ex-commands,
    /// operators, motions, text objects. Valid until the host freezes the
    /// registry (after the install list); calling it post-freeze panics.
    fn commands_mut(&mut self) -> &mut CommandRegistry;

    /// Mutable access to the mode registry — register the subsystem's major /
    /// minor modes. Valid until the host freezes the registry.
    fn modes_mut(&mut self) -> &mut ModeRegistry;

    /// Mutable access to the service registry — for bulk / multi-step service
    /// wiring. Most callers want [`register_service`](Self::register_service).
    fn services_mut(&mut self) -> &mut ServiceRegistry;

    /// Register a single service handle under its `TypeId`. Per the
    /// `ServiceRegistry` Arc/TypeId rule, register and look up with the same
    /// `T` (register `Arc<X>` ⇒ look up `Arc<X>`).
    fn register_service<T: Any + Send + Sync>(&mut self, service: T);

    /// Look up a service by type — the seam a subsystem uses to reach a
    /// *generic* handle another layer owns (e.g. the LSP `DiagnosticsQueryHandle`
    /// the host registered in Phase A) without this trait naming that type.
    /// Returns `Arc<T>`; for an `Arc<X>` registration this is `Arc<Arc<X>>`,
    /// so unwrap one layer (`(*h).clone()`).
    fn service<T: Any + Send + Sync>(&self) -> Option<Arc<T>>;

    /// The bundled **inbound** primitive: a channel whose `send` wakes the
    /// editor off-keystroke (the wake is inside the sender — structurally
    /// impossible to forget) and whose items are drained per-tick, each run
    /// through `handler` (validate → map to an [`Effect`] → resolve any
    /// oneshot). Returns the sender for the off-thread producer; the drain's
    /// registration is retained for the editor's lifetime by the host.
    fn inbound<T, H>(&mut self, handler: H) -> InboundBus<T>
    where
        T: Send + 'static,
        H: FnMut(T) -> Vec<Effect> + Send + 'static;

    /// Subscribe a typed event and **wake the editor** whenever one is
    /// published — the off-keystroke-repaint primitive for event-driven work.
    fn wake_on_event<E>(&self)
    where
        E: TypedEvent + Clone;

    /// Register a raw per-tick drain closure (for drains that are not channel
    /// inbound buses, e.g. a state-poll). The registration is retained for the
    /// editor's lifetime by the host.
    fn tick_callback(&mut self, callback: TickCallback);

    /// The typed event bus (subscribe / publish).
    fn event_bus(&self) -> &Arc<EventBus>;

    /// The shared async runtime handle (off-thread spawns).
    fn runtime_handle(&self) -> &Handle;

    /// The generic buffer-store handle — the uniform buffer-access substrate
    /// (read a buffer's text / path / id by `BufferId`).
    fn buffer_store(&self) -> &BufferStoreHandle;
}
