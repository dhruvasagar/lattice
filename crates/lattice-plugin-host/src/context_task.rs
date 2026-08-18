//! TC.2 — the per-plugin actor bridge for sticky-context providers.
//!
//! The context analogue of `decoration_task.rs`: a dedicated async task owns the
//! plugin's `Store<PluginState>` for life (the Store is `!Sync`), a
//! [`ContextCall`] crosses an mpsc channel with a `oneshot` reply, and the
//! `Send + Sync` [`ContextClient`] serializes calls onto the single-consumer
//! loop. `PluginHost::spawn_context_source` instantiates the `context-plugin`
//! world under the plugin's grant and returns `(ContextClient, ContextActor)`;
//! the caller drives [`ContextActor::run`] on its multi-thread runtime (the lib
//! owns no runtime).
//!
//! **The tree crosses as a call-scoped borrow.** Unlike the other async seams,
//! `context-scopes` takes `option<borrow<tree-snapshot>>`. The actor pushes an
//! owned `TreeSnapshotResource` into the store's table, lends a non-owning
//! borrow to the guest, and reclaims the owned entry after the call — the
//! `grammar_trampoline` pattern (TS.1), which is what keeps the
//! `tree-sitter` capability meaning "the tree you were handed" rather than "any
//! buffer's tree, any time". The owned entry lives across the guest's
//! suspension; the host owns it throughout, and any `node` the guest derives is
//! guest-owned and dropped before it returns.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use lattice_syntax::SyntaxSnapshot;
use wasmtime::Store;
use wasmtime::component::Resource;

use crate::context_host::bindings::ContextPlugin;
use crate::tree_resource::TreeSnapshotResource;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};

// The context WIT records the bridge's public API traffics in — the
// `with:`-mapped `types` mirrors (`context_host.rs`), i.e. the SAME Rust types
// `WitBoundary` round-trips; the native↔WIT conversion is the caller's job
// (`boundary_context.rs`).
pub use crate::lattice::plugin_host::types::{ContextRequest, ContextScope};

/// See `completion_task::CallResult`.
type CallResult<T> = Result<T, PluginHostError>;

/// A request sent from a [`ContextClient`] to its [`ContextActor`].
enum ContextCall {
    /// `context.context-scopes(req, tree)` — produce the structural scopes for a
    /// buffer. Replies the guest's `result<list<context-scope>, string>` (or a
    /// host trap).
    Produce {
        req: Box<ContextRequest>,
        /// The buffer's parse snapshot, or `None` when it has no tree. Crosses
        /// to the guest as a call-scoped borrow.
        tree: Option<Arc<SyntaxSnapshot>>,
        reply: oneshot::Sender<CallResult<Result<Vec<ContextScope>, String>>>,
    },
}

/// The `Send + Sync` handle a caller holds. Cloning is cheap (an mpsc `Sender`
/// clone); every clone talks to the same actor / `Store`, so calls serialize on
/// the single-consumer loop the `!Sync` `Store` needs.
#[derive(Clone, Debug)]
pub struct ContextClient {
    tx: mpsc::UnboundedSender<ContextCall>,
    id: PluginId,
}

impl ContextClient {
    /// The host-issued identity of the plugin behind this client.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `context-scopes(req, tree)`. The outer result is the
    /// host surface; the inner `Result<_, String>` is the guest's own WIT
    /// `result` (an `Err` string means this refresh produced nothing — logged,
    /// and the caller KEEPS the buffer's prior cached scopes).
    pub async fn produce(
        &self,
        req: ContextRequest,
        tree: Option<Arc<SyntaxSnapshot>>,
    ) -> CallResult<Result<Vec<ContextScope>, String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(ContextCall::Produce {
                req: Box::new(req),
                tree,
                reply,
            })
            .map_err(|_| PluginHostError::PluginGone {
                func: "context-scopes",
            })?;
        rx.await.map_err(|_| PluginHostError::PluginGone {
            func: "context-scopes",
        })?
    }
}

/// The per-plugin actor: owns the `Store` + context bindings for the plugin's
/// life and serves calls off the channel until every [`ContextClient`] drops.
pub struct ContextActor {
    store: Store<PluginState>,
    bindings: ContextPlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<ContextCall>,
    id: PluginId,
    /// Crash-quarantine (PH7.12): the first `context-scopes` trap trips this,
    /// fires one `PluginCrashed`, and every later call returns `Quarantined`.
    quarantine: crate::Quarantine,
    /// PO.2: the boundary tracer, wired by the loader via `with_tracer`; `None`
    /// in tests / pre-wire.
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl ContextActor {
    /// The host-issued identity of this plugin.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// PO.2: attach the boundary tracer (the loader calls this before spawning
    /// `run()`). Off the hot path — the seam is async.
    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
    }

    /// Drive the actor to completion — see `completion_task::CompletionActor::run`.
    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                ContextCall::Produce { req, tree, reply } => {
                    let _ = reply.send(self.call_produce(&req, tree.as_ref()).await);
                }
            }
        }
    }

    async fn call_produce(
        &mut self,
        req: &ContextRequest,
        tree: Option<&Arc<SyntaxSnapshot>>,
    ) -> CallResult<Result<Vec<ContextScope>, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined {
                func: "context-scopes",
            });
        }
        arm_store(&mut self.store, self.budget)?;

        // Lend the snapshot as a borrow: push an owned entry, hand the guest a
        // non-owning handle, reclaim after the call. Only mint one when the
        // buffer actually HAS a parse — otherwise the guest gets `none` and is
        // expected to return an empty list (a normal state, not an error).
        let owned_tree = match tree {
            Some(snap) if snap.tree().is_some() => Some(
                self.store
                    .data_mut()
                    .table
                    .push(TreeSnapshotResource::new(snap.clone()))
                    .map_err(|e| PluginHostError::Instantiate(e.into()))?,
            ),
            _ => None,
        };
        let tree_borrow = owned_tree.as_ref().map(|o| Resource::new_borrow(o.rep()));

        let __trace_start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_context()
            .call_context_scopes(&mut self.store, req, tree_borrow)
            .await;

        // Reclaim the owned entry whether the call succeeded, erred, or trapped
        // — a trapped guest must not leak a table entry into the next call.
        if let Some(owned_tree) = owned_tree {
            let _ = self.store.data_mut().table.delete(owned_tree);
        }

        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::Context,
            &mut self.quarantine,
            "context-scopes",
            __trace_start,
            result,
        )
    }
}

impl PluginHost {
    /// Instantiate a `context-plugin` component under its capability grant and
    /// return the bridge: a `Send + Sync` [`ContextClient`] plus the
    /// [`ContextActor`] the caller drives. Grant / data-dir / WASI are identical
    /// to `instantiate_plugin` (shared `build_plugin_wasi` + `new_store`), and
    /// the actor is *not* spawned here (the lib owns no runtime). Mirror of
    /// [`spawn_decoration_source`](Self::spawn_decoration_source).
    pub async fn spawn_context_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
        config: Option<&Arc<lattice_config::ConfigRegistry>>,
    ) -> Result<(ContextClient, ContextActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "context plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings = ContextPlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        // PO.5: route this plugin's `logging` calls into the tracer (Layer 2).
        store.data_mut().log_ctx = self.log_ctx_for(id);
        // The producer reads its OWN options through `get-option`
        // (`max-file-lines`, `disabled-languages`). Without the registry on
        // this store every such read returns `None` and the guest silently
        // falls back to its compiled defaults — so the options resolve in
        // `:customize`, report a value to `:set …?`, and change nothing.
        // `spawn_config_plugin` wires this for the config seam; the context
        // seam runs in its own store and needs it too.
        if let Some(registry) = config {
            store.data_mut().config_registry = Some(Arc::clone(registry));
        }
        let (tx, rx) = mpsc::unbounded();
        let client = ContextClient { tx, id };
        let actor = ContextActor {
            store,
            bindings,
            budget,
            rx,
            id,
            quarantine: crate::Quarantine::new(id, Arc::clone(bus)),
            tracer: None,
        };
        Ok((client, actor))
    }
}
