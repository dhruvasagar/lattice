//! TC.2 — the `WasmContextSource` adapter (the async-produce path).
//!
//! Wraps a context plugin's [`ContextClient`] bridge and exposes the **native**
//! [`AsyncContextSource`](lattice_mode::AsyncContextSource) the host's registry
//! holds — the same trait-object indirection the completion / picker /
//! decoration seams use, so `lattice-host` and the renderers never name this
//! crate.
//!
//! The host drives this when a reparse completes, stamps the result with the
//! parse version, and caches it. Nothing on the keystroke path calls it: the
//! per-pane resolution that runs at cursor rate reads the *cache*
//! (`lattice_cells::context::resolve_context`), never a producer.

use std::sync::Arc;

use lattice_cells::context::ContextScope;
use lattice_syntax::SyntaxSnapshot;

use crate::WitBoundary;
use crate::boundary_context::project_context_request;
use crate::{ContextClient, PluginId};

/// An async context-scope producer over a plugin's [`ContextClient`]. Cheap to
/// clone (the client is an mpsc `Sender` clone); every clone talks to the same
/// actor / `Store`.
#[derive(Clone, Debug)]
pub struct WasmContextSource {
    client: ContextClient,
}

impl lattice_mode::AsyncContextSource for WasmContextSource {
    fn source_id(&self) -> u64 {
        self.plugin_id().0 as u64
    }

    fn produce(
        &self,
        buffer_id: u64,
        path: Option<std::path::PathBuf>,
        line_count: u32,
        syntax: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> lattice_mode::ContextFuture<'_> {
        // Downcast the type-erased snapshot here, at the one place that knows
        // both sides — `lattice-mode` must not depend on `lattice-syntax` (the
        // `ActionContext::syntax` precedent). A snapshot of some other type is
        // indistinguishable from "no parse" and degrades to an empty result
        // rather than an error: it is a host wiring mistake, not a plugin
        // failure, and blanking the strip would misattribute it.
        let tree = syntax.and_then(|any| any.downcast::<SyntaxSnapshot>().ok());
        Box::pin(async move {
            self.context_scopes(buffer_id, path.as_deref(), line_count, tree)
                .await
        })
    }
}

impl WasmContextSource {
    /// Build the adapter over a client bridge. (No `connect`/`spec` round-trip
    /// like completion — a context provider has no id/doc metadata; it is a pure
    /// producer keyed by the plugin that owns it.)
    pub fn new(client: ContextClient) -> Self {
        Self { client }
    }

    /// The host-issued id of the plugin behind this source.
    pub fn plugin_id(&self) -> PluginId {
        self.client.id()
    }

    /// Produce the structural context scopes for a buffer — the async producer
    /// the host calls OFF the render path when a reparse lands.
    ///
    /// Graceful (§8, no blanking): the outer host error (trap / plugin-gone) and
    /// the inner guest WIT `err` both collapse to the `String` the caller logs —
    /// on an `Err` the caller keeps the buffer's *prior* cached scopes rather
    /// than clearing them, so a failed refresh never reads as the feature
    /// breaking. A scope that fails to cross fails the whole batch as a typed
    /// error, never a silent drop.
    pub async fn context_scopes(
        &self,
        buffer_id: u64,
        path: Option<&std::path::Path>,
        line_count: u32,
        tree: Option<Arc<SyntaxSnapshot>>,
    ) -> Result<Vec<ContextScope>, String> {
        let req = project_context_request(buffer_id, path, line_count);
        let wit = match self.client.produce(req, tree).await {
            Ok(inner) => inner?,
            Err(host_err) => return Err(format!("context plugin: {host_err}")),
        };
        wit.into_iter().map(ContextScope::from_wit).collect()
    }
}
