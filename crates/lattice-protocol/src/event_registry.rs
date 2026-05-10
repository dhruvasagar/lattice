// `linkme`'s distributed-slice expansion uses `#[link_section]`
// declarations, which the workspace's `unsafe_code = "deny"`
// lint flags. Same shape `lattice-config::core_options` uses --
// scope-limited opt-in for the macro expansion site.
#![allow(unsafe_code)]

//! Typed-event surface (mode-architecture §5.10 follow-up).
//!
//! The legacy [`crate::Event`] enum is the closed catalogue of
//! editor-core events (DocumentOpened, DocumentChanged, etc.).
//! Feature crates (`lattice-lsp`, `lattice-completion`, future
//! plugins) need to declare and own *their* events without
//! editing the central enum -- the same ownership model
//! `lattice-mode` uses for `Mode` declarations.
//!
//! This module adds:
//!
//! - [`Event`] trait -- marker every concrete event type
//!   implements (`Any + Debug + Send + Sync + 'static`).
//! - [`EventTypeId`] -- thin wrapper over `std::any::TypeId`
//!   keyed against an interned name; the registry surfaces the
//!   name for introspection (`:describe-events`).
//! - [`EventDescriptor`] -- per-event metadata (name, doc,
//!   source crate). Aggregated process-wide via
//!   [`EVENT_DESCRIPTORS`] (a `linkme` distributed slice; same
//!   mechanism `lattice-config` uses for typed options).
//! - [`register_event!`] macro -- single declaration site that
//!   pushes the descriptor and wires `Event::TYPE_ID`.
//!
//! The runtime's [`lattice_runtime::EventBus`] (M.5.3.a follow-
//! up) accepts both shapes: legacy enum publishes via
//! `publish` / `subscribe`, typed events via
//! `publish_typed::<T>` / `subscribe_typed::<T>`. Built-in
//! events stay on the legacy path until a future cleanup slice
//! migrates them; new events (LSP and beyond) declare via the
//! typed path.

use std::any::TypeId;
use std::fmt::Debug;

/// Marker trait every concrete event type implements. Required
/// supertraits give the bus enough to box, store, and
/// downcast: `Any` for the cast, `Send + Sync + 'static` for
/// cross-thread shipping, `Debug` for diagnostic logs.
pub trait Event: std::any::Any + Debug + Send + Sync + 'static {
    /// The type-id under which this event is registered.
    /// Implementors typically delegate to a `static` to avoid
    /// re-allocating the metadata on every call. The macro
    /// [`register_event!`] generates this for you.
    fn event_type_id(&self) -> EventTypeId;
}

/// Stable identifier for an event type. Pairs Rust's
/// `std::any::TypeId` (the bus's downcast key) with a string
/// name (what `:describe-events` prints). Hash / Eq use the
/// `TypeId` only -- two registrations with the same struct but
/// different names would collide on the bus side, which is the
/// behaviour we want (the macro panics at registration on a
/// duplicate).
#[derive(Debug, Clone, Copy)]
pub struct EventTypeId {
    type_id: TypeId,
    name: &'static str,
}

impl EventTypeId {
    pub const fn new(type_id: TypeId, name: &'static str) -> Self {
        Self { type_id, name }
    }

    pub fn of<T: 'static>(name: &'static str) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            name,
        }
    }

    pub fn rust_type_id(&self) -> TypeId {
        self.type_id
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl PartialEq for EventTypeId {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id
    }
}

impl Eq for EventTypeId {}

impl std::hash::Hash for EventTypeId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.type_id.hash(state);
    }
}

/// Per-event metadata aggregated into [`EVENT_DESCRIPTORS`].
/// `:describe-events` walks this slice; subscriber tooling
/// (future "wait-for-event" debuggers, plugin host) can also
/// enumerate it.
///
/// `name` is the user-facing identifier (`"lsp.buffer-attached"`,
/// `"document.changed"`); convention: lowercase,
/// dot-separated, namespaced by feature.
#[derive(Debug, Clone, Copy)]
pub struct EventDescriptor {
    pub name: &'static str,
    pub doc: &'static str,
    pub source_crate: &'static str,
    /// Returns the `TypeId` of the concrete event type. Stored
    /// as a fn pointer (rather than the `TypeId` directly)
    /// because `TypeId::of::<T>()` is non-const today; the
    /// distributed-slice entries call this once at registry
    /// build time.
    pub type_id: fn() -> TypeId,
}

/// Process-wide distributed slice every `register_event!` call
/// pushes into. `linkme` aggregates entries across crates at
/// link time -- same mechanism `lattice-config`'s typed-option
/// registry uses.
#[linkme::distributed_slice]
pub static EVENT_DESCRIPTORS: [EventDescriptor];

/// Walk every registered event descriptor. Order is
/// link-determined (not sorted); callers that need a stable
/// presentation should sort by `name` themselves.
pub fn registered_events() -> impl Iterator<Item = &'static EventDescriptor> {
    EVENT_DESCRIPTORS.iter()
}

/// Look up a descriptor by exact name. Returns `None` when no
/// event is registered under that name.
pub fn descriptor_by_name(name: &str) -> Option<&'static EventDescriptor> {
    EVENT_DESCRIPTORS.iter().find(|d| d.name == name)
}

/// Look up a descriptor by `TypeId`. Used by the bus to format
/// "unknown subscriber for event X" diagnostics and by
/// `:describe-events` when invoked off a publisher's
/// `event_type_id()`.
pub fn descriptor_by_type_id(type_id: TypeId) -> Option<&'static EventDescriptor> {
    EVENT_DESCRIPTORS.iter().find(|d| (d.type_id)() == type_id)
}

/// Declare and register an event type. Generates:
///
/// 1. An impl of [`Event`] for `$ty` returning the registered
///    [`EventTypeId`].
/// 2. A `linkme`-aggregated [`EventDescriptor`] entry pushed
///    into [`EVENT_DESCRIPTORS`].
///
/// Conventions:
/// - `$name` is the user-facing identifier
///   (`"lsp.buffer-attached"`); lowercase, dot-separated,
///   namespaced by feature.
/// - `$doc` is the short summary `:describe-events` prints.
/// - `$source_crate` records who owns the declaration; useful
///   for `:describe-events --by-crate` and plugin host
///   tooling.
///
/// Example:
/// ```ignore
/// pub struct LspBufferAttached { pub id: BufferId, pub path: Option<PathBuf> }
/// register_event!(
///     LspBufferAttached,
///     "lsp.buffer-attached",
///     "Fired after lsp-mode activates on a buffer.",
///     "lattice-lsp",
/// );
/// ```
///
/// The macro can't be invoked from within a `cfg(test)` module
/// alone -- `linkme` requires the slice entry at the top level
/// of a binary's link graph. For tests that need a registered
/// event, declare it at module scope.
///
/// **Caller crates must add `linkme` as a direct dependency**
/// (proc-macro attribute paths can't route through re-exports).
/// Same constraint `lattice-config` callers face.
#[macro_export]
macro_rules! register_event {
    ($ty:ty, $name:literal, $doc:literal, $source_crate:literal $(,)?) => {
        impl $crate::event_registry::Event for $ty {
            fn event_type_id(&self) -> $crate::event_registry::EventTypeId {
                $crate::event_registry::EventTypeId::of::<$ty>($name)
            }
        }

        const _: () = {
            #[linkme::distributed_slice($crate::event_registry::EVENT_DESCRIPTORS)]
            static DESCRIPTOR: $crate::event_registry::EventDescriptor =
                $crate::event_registry::EventDescriptor {
                    name: $name,
                    doc: $doc,
                    source_crate: $source_crate,
                    type_id: || std::any::TypeId::of::<$ty>(),
                };
        };
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // Declare a test event at module scope (linkme requires
    // top-level for the slice entry to land in the link graph).
    #[derive(Debug, Clone)]
    pub struct TestEvent {
        pub _n: u32,
    }

    register_event!(
        TestEvent,
        "test.event",
        "Test event used to validate the registry surface.",
        "lattice-protocol-tests",
    );

    #[test]
    fn registered_event_appears_in_descriptors_slice() {
        let found = registered_events().any(|d| d.name == "test.event");
        assert!(found, "test.event should appear in EVENT_DESCRIPTORS");
    }

    #[test]
    fn descriptor_by_name_finds_test_event() {
        let d = descriptor_by_name("test.event").expect("registered");
        assert_eq!(d.name, "test.event");
        assert_eq!(d.source_crate, "lattice-protocol-tests");
    }

    #[test]
    fn descriptor_by_type_id_round_trips() {
        let tid = std::any::TypeId::of::<TestEvent>();
        let d = descriptor_by_type_id(tid).expect("registered");
        assert_eq!(d.name, "test.event");
    }

    #[test]
    fn event_trait_returns_registered_type_id() {
        let e = TestEvent { _n: 42 };
        let etid = e.event_type_id();
        assert_eq!(etid.name(), "test.event");
        assert_eq!(etid.rust_type_id(), std::any::TypeId::of::<TestEvent>());
    }
}
