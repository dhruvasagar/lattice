//! Internal typed events published by `lattice-host` subsystems.
//!
//! These are typed-bus events (M.5.3.b pattern: concrete structs,
//! `publish_typed` / `subscribe_typed`) rather than `Event` enum
//! variants. They implement `lattice_protocol::event_registry::Event`
//! manually (no `linkme` descriptor) because they are internal
//! implementation signals, not user-visible autocmd events.

/// Fired by the syntax-handle callback after every snapshot publish
/// (edit-shift + final reparse). Subscribers fire `cells_wake` so
/// the cells worker rebuilds with fresh syntax without waiting for
/// the next keystroke.
///
/// Published from the `on_publish` closure passed to
/// `SyntaxHandle::seeded_with_runtime` in `editor_boot.rs` and the
/// `do_edit` path in `dispatch.rs`.
#[derive(Debug, Clone)]
pub(crate) struct SyntaxReparsed;

/// Fired by the actor after every `publish_render_state` triggered
/// by the `async_landed` arm (async completions: syntax reparsed,
/// excerpts appended, etc.). The subscriber in `editor_boot.rs`
/// fires `cells_wake` after receiving this event, guaranteeing that
/// the cells worker reads the freshly-written `PaneCellsInputs`
/// rather than racing against the actor's ArcSwap store.
///
/// Same pattern as `SyntaxReparsed` — event bus as the ordering
/// seam between the actor OS thread and the cells-worker task.
#[derive(Debug, Clone)]
pub(crate) struct AsyncRenderStatePublished;

impl lattice_protocol::event_registry::Event for SyntaxReparsed {
    fn event_type_id(&self) -> lattice_protocol::event_registry::EventTypeId {
        lattice_protocol::event_registry::EventTypeId::of::<Self>("syntax.reparsed")
    }
}

impl lattice_protocol::event_registry::Event for AsyncRenderStatePublished {
    fn event_type_id(&self) -> lattice_protocol::event_registry::EventTypeId {
        lattice_protocol::event_registry::EventTypeId::of::<Self>("render_state.async_published")
    }
}
