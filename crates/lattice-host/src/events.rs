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

impl lattice_protocol::event_registry::Event for SyntaxReparsed {
    fn event_type_id(&self) -> lattice_protocol::event_registry::EventTypeId {
        lattice_protocol::event_registry::EventTypeId::of::<Self>("syntax.reparsed")
    }
}
