//! PH7.9c — the `WasmDecorationSource` adapter (the async-produce path).
//!
//! Wraps a decoration plugin's [`DecorationClient`] bridge and exposes a
//! **native**-typed producer the host polls off the render path. Like
//! `WasmCompletionSource` (PH7.6), this is NOT an `Arc<dyn>` impl of the sync
//! `Mode::gutter_decorations` trait — that trait is read *per frame* by the
//! renderer, and a WASM mode can't satisfy it inline (per-frame WASM =
//! paramount-#1 violation). Instead the host calls this producer on a trigger
//! (edit / scroll / diagnostic change), caches the returned
//! `Vec<GutterDecoration>` per buffer, and the renderer reads the cache. This
//! adapter is the host-facing "decoration source" a boot-wired `Editor` will poll
//! (the renderer-reads-the-cache wiring is the Phase-8 boot-wiring step).

use lattice_mode::GutterDecoration;

use crate::WitBoundary;
use crate::boundary_decoration::project_decoration_context;
use crate::{DecorationClient, PluginId};

/// An async gutter-decoration producer over a plugin's [`DecorationClient`].
/// Cheap to clone (the client is an mpsc `Sender` clone); every clone talks to
/// the same actor / `Store`.
#[derive(Clone)]
pub struct WasmDecorationSource {
    client: DecorationClient,
}

impl WasmDecorationSource {
    /// Build the adapter over a client bridge. (No `connect`/`spec` round-trip
    /// like completion — a decoration provider has no id/doc metadata; it is a
    /// pure producer keyed by the mode that owns it.)
    pub fn new(client: DecorationClient) -> Self {
        Self { client }
    }

    /// The host-issued id of the plugin behind this source.
    pub fn plugin_id(&self) -> PluginId {
        self.client.id()
    }

    /// Produce the per-line gutter decorations for a buffer — the async producer
    /// the host calls OFF the render path. Projects the owned
    /// [`decoration-context`](crate::decoration_task::DecorationContext) from the
    /// buffer metadata, calls the guest, and converts the result to native
    /// [`GutterDecoration`]s.
    ///
    /// Graceful (§8, no flicker): the outer host error (trap / plugin-gone) and
    /// the inner guest WIT `err` both collapse to the `String` the caller logs —
    /// on an `Err`, the caller keeps the buffer's *prior* cached snapshot rather
    /// than clearing it, so decoration cues never blink mid-refresh. A candidate
    /// that fails to cross (malformed record) fails the whole batch as a typed
    /// error, never a silent drop.
    pub async fn gutter_decorations(
        &self,
        buffer_id: u64,
        path: Option<&std::path::Path>,
        line_count: u32,
    ) -> Result<Vec<GutterDecoration>, String> {
        let ctx = project_decoration_context(buffer_id, path, line_count);
        let wit = match self.client.produce(ctx).await {
            Ok(inner) => inner?,
            Err(host_err) => return Err(format!("decoration plugin: {host_err}")),
        };
        wit.into_iter().map(GutterDecoration::from_wit).collect()
    }
}
