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

use lattice_core::{BufferId, BufferKind};
use lattice_mode::{ModeActivator, ModeId, ServiceRegistry};

use crate::editor::Editor;

impl ModeActivator for Editor {
    fn activate_major_for_kind(&mut self, buffer: BufferId, kind: BufferKind) {
        let signals = self.activate_major_for_buffer_kind(buffer, kind);
        self.enqueue_renderer_signals(signals);
    }

    fn activate_minor_by_id(&mut self, buffer: BufferId, mode: ModeId) {
        let signals = self.activate_mode_by_id(buffer, mode);
        self.enqueue_renderer_signals(signals);
    }

    fn services(&self) -> Arc<ServiceRegistry> {
        Arc::clone(&self.services)
    }
}
