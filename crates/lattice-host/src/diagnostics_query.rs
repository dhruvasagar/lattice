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

    /// IDE-protocol I2.0: all diagnostics for a file `uri` (string form).
    /// Parses the uri and reads the layer's `diagnostics_for`; both are
    /// wait-free over the published render state, so this is safe to call
    /// off the editor thread (the Claude Code IDE peer's WS task does).
    fn for_uri(&self, uri: &str) -> Vec<lattice_lsp::Diagnostic> {
        let Ok(parsed) = lattice_lsp::uri_from_str(uri) else {
            return Vec::new();
        };
        self.render_state
            .load()
            .diagnostics
            .layer
            .diagnostics_for(&parsed)
    }

    /// IDE-protocol I2.0: every uri (string form) with diagnostics.
    fn uris_with_diagnostics(&self) -> Vec<String> {
        self.render_state
            .load()
            .diagnostics
            .layer
            .iter_uris()
            .into_iter()
            .map(|u| u.as_str().to_string())
            .collect()
    }
}
