//! L4b (lsp-architecture.md §15): host-side [`lattice_lsp::modes::DiagnosticsQuery`]
//! impl backing `lsp-diagnostics-mode`'s mode-owned `gl` handler.
//!
//! The mode handler (in `lattice-lsp`) has only `ctx.buffer_id` +
//! `ctx.cursor` + the service registry — it cannot resolve a buffer to
//! its LSP URI (that map is editor-local). This impl closes that gap:
//! it reads the live published [`RenderState`] snapshot, resolves
//! `buffer_id → uri` through `buffers.uris`, and queries the
//! `DiagnosticsLayer` — all without exposing a host method or the URI
//! map to the mode. Registered once at boot under
//! [`lattice_lsp::modes::DiagnosticsQueryHandle`].

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::render_state::RenderState;

/// Reads a buffer's per-line diagnostics over the published render
/// state. Cheap: one `ArcSwap::load` + a `HashMap` lookup + the layer's
/// wait-free `diagnostics_on_line`.
pub(crate) struct HostDiagnosticsQuery {
    render_state: Arc<ArcSwap<RenderState>>,
}

impl HostDiagnosticsQuery {
    pub(crate) fn new(render_state: Arc<ArcSwap<RenderState>>) -> Self {
        Self { render_state }
    }
}

impl lattice_lsp::modes::DiagnosticsQuery for HostDiagnosticsQuery {
    fn on_line(
        &self,
        buffer_id: lattice_protocol::ids::BufferId,
        line: u32,
    ) -> Vec<lattice_lsp::Diagnostic> {
        let rs = self.render_state.load();
        let core_id = lattice_core::BufferId(buffer_id.raw() as u32);
        let Some(uri) = rs.buffers.uris.get(&core_id) else {
            return Vec::new();
        };
        rs.diagnostics.layer.diagnostics_on_line(uri, line)
    }
}
