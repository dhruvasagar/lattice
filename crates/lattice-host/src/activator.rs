//! M.2.b.2 (2026-06-01): `impl ModeActivator for Editor`.
//!
//! Synchronous activation surface for extension crates
//! (`lattice-multibuffer` today; future in-tree provider crates
//! shipped within it). Thin wrappers over the existing fat helpers
//! (`activate_major_for_buffer_kind`, `activate_mode_by_id`) —
//! both already run the full cascade (major → default minor → auto
//! minors → recompute options + completion → maybe-auto-LSP) and
//! return a `Vec<RendererSignal>`.
//!
//! The trait surface returns `()`, keeping `RendererSignal` out of
//! `lattice-mode`. Signals are enqueued into
//! `Editor::pending_renderer_signals`; the App's dispatch loop
//! drains via [`Editor::drain_pending_renderer_signals`] after the
//! extension-crate call frame returns.
//!
//! See `docs/dev/architecture/multibuffer-views.md` §3.7.

use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, BufferKind};
use lattice_mode::{ModeActivator, ModeId, ServiceRegistry};

use crate::editor::Editor;

impl ModeActivator for Editor {
    /// MR.6: the buffer a provider view is being opened over.
    ///
    /// The active *document* buffer, which is what the trigger was
    /// pressed in — the same id `transient_open_context` reports and the
    /// same one the chord dispatcher hands an action handler.
    fn active_buffer(&self) -> Option<BufferId> {
        Some(self.document_buffer_id)
    }

    fn activate_major_for_kind(&mut self, buffer: BufferId, kind: BufferKind) {
        let signals = self.activate_major_for_buffer_kind(buffer, kind);
        self.enqueue_renderer_signals(signals);
    }

    fn activate_minor_by_id(&mut self, buffer: BufferId, mode: ModeId) {
        let signals = self.activate_mode_by_id(buffer, mode);
        self.enqueue_renderer_signals(signals);
    }

    fn deactivate_minor_by_id(&mut self, buffer: BufferId, mode: ModeId) {
        let signals = self.deactivate_mode_by_id(buffer, mode);
        self.enqueue_renderer_signals(signals);
    }

    /// The real create-and-activate seam (`ensure_named_synthetic_document`
    /// inserts the buffer + runs the major's `on_activate` by id). This
    /// is why buffer creation lives here on the `&mut`-backed
    /// `ModeActivator` and not on the `&self` `BufferStore`.
    fn ensure_named_document(&mut self, name: &str, major: ModeId, flags: BufferFlags) -> BufferId {
        self.ensure_named_synthetic_document(name, major, flags)
    }

    fn services(&self) -> Arc<ServiceRegistry> {
        Arc::clone(&self.services)
    }

    /// Forward to the host's single write chokepoint, so a provider crate
    /// records what its buffer is about without depending on `lattice-host`.
    fn set_buffer_scope_dir(&mut self, buffer: BufferId, dir: std::path::PathBuf) {
        Editor::set_buffer_scope_dir(self, buffer, dir);
    }

    /// K.4.6 (2026-06-02): forward to the editor's
    /// `VirtualRowProviderRegistry`. The virtual-rows worker
    /// (`crate::virtual_rows_worker`) picks up new providers on
    /// its next wake; the registry itself is already shared
    /// (`Arc<VirtualRowProviderRegistry>`), so registration is
    /// observable to the worker without further plumbing.
    /// Returns `false` if a provider with the same `ProviderId`
    /// already exists in `buffer`'s scope — matches the
    /// underlying registry's no-replacement contract.
    fn register_virtual_row_provider(
        &mut self,
        buffer: BufferId,
        provider: Arc<dyn lattice_cells::VirtualRowProvider>,
    ) -> bool {
        self.virtual_row_providers.register(buffer, provider)
    }
}
