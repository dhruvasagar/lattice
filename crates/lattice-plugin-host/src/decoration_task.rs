//! PH7.9b — the per-plugin actor bridge for decoration providers.
//!
//! The decoration analogue of `completion_task.rs`: a dedicated async task owns
//! the plugin's `Store<PluginState>` for life (the Store is `!Sync`), a
//! `DecorationCall` crosses an mpsc channel with a `oneshot` reply, and the
//! `Send + Sync` [`DecorationClient`] serializes calls onto the single-consumer
//! loop. `PluginHost::spawn_decoration_source` instantiates the
//! `decorations-plugin` world under the plugin's grant and returns
//! `(DecorationClient, DecorationActor)`; the caller drives
//! [`DecorationActor::run`] on its multi-thread runtime (the lib owns no runtime).
//!
//! Like completion (PH7.6), this is a **producer**, host-called OFF the render
//! path — the host calls `produce` on a trigger (edit / scroll / diagnostic
//! change), caches the result, and the renderer reads the cache (never WASM on
//! the tick, paramount #1). The picker / completion / decoration actors are
//! near-identical request/reply bridges; generalising the loop over the bindings
//! type is deferred until a real need (the `completion_task` rule-of-three note).

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use wasmtime::Store;

use crate::decoration_host::bindings::DecorationsPlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};

// The decoration WIT records the bridge's public API traffics in — the
// `with:`-mapped `types` mirrors (`decoration_host.rs`), i.e. the SAME Rust types
// `WitBoundary` round-trips; the native↔WIT conversion is the caller's job
// (`boundary_decoration.rs`). Re-exported `pub` (they appear in the
// `DecorationClient` method signatures).
pub use crate::lattice::plugin_host::types::{DecorationContext, GutterDecoration};

/// See `completion_task::CallResult`.
type CallResult<T> = Result<T, PluginHostError>;

/// A request sent from a [`DecorationClient`] to its [`DecorationActor`].
enum DecorationCall {
    /// `decorations.gutter-decorations(ctx)` — produce the per-line gutter
    /// decorations for a buffer. Replies the guest's `result<list<gutter-
    /// decoration>, string>` (or a host trap).
    Produce {
        ctx: Box<DecorationContext>,
        reply: oneshot::Sender<CallResult<Result<Vec<GutterDecoration>, String>>>,
    },
}

/// The `Send + Sync` handle a caller holds. Cloning is cheap (an mpsc `Sender`
/// clone); every clone talks to the same actor / `Store`, so calls serialize on
/// the single-consumer loop the `!Sync` `Store` needs.
#[derive(Clone, Debug)]
pub struct DecorationClient {
    tx: mpsc::UnboundedSender<DecorationCall>,
    id: PluginId,
}

impl DecorationClient {
    /// The host-issued identity of the plugin behind this client.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `gutter-decorations(ctx)`. The outer result is the host
    /// surface; the inner `Result<_, String>` is the guest's own WIT `result` (an
    /// `Err` string is a provider that produced nothing for this trigger — logged,
    /// the cached snapshot keeps its prior value).
    pub async fn produce(
        &self,
        ctx: DecorationContext,
    ) -> CallResult<Result<Vec<GutterDecoration>, String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(DecorationCall::Produce {
                ctx: Box::new(ctx),
                reply,
            })
            .map_err(|_| PluginHostError::PluginGone {
                func: "gutter-decorations",
            })?;
        rx.await.map_err(|_| PluginHostError::PluginGone {
            func: "gutter-decorations",
        })?
    }
}

/// The per-plugin actor: owns the `Store` + decoration bindings for the plugin's
/// life and serves calls off the channel until every [`DecorationClient`] drops.
pub struct DecorationActor {
    store: Store<PluginState>,
    bindings: DecorationsPlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<DecorationCall>,
    id: PluginId,
    /// Crash-quarantine (PH7.12): the first `gutter-decorations` trap trips this,
    /// fires one `PluginCrashed`, and every later call returns `Quarantined`.
    quarantine: crate::Quarantine,
    /// PO.2: the boundary tracer, wired by the loader via with_tracer; None in tests / pre-wire.
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl DecorationActor {
    /// The host-issued identity of this plugin.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// PO.2: attach the boundary tracer (the loader calls this before spawning
    /// run()). Off the hot path — the seam is async.
    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
    }

    /// Drive the actor to completion — see `completion_task::CompletionActor::run`.
    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                DecorationCall::Produce { ctx, reply } => {
                    let _ = reply.send(self.call_produce(&ctx).await);
                }
            }
        }
    }

    async fn call_produce(
        &mut self,
        ctx: &DecorationContext,
    ) -> CallResult<Result<Vec<GutterDecoration>, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined {
                func: "gutter-decorations",
            });
        }
        arm_store(&mut self.store, self.budget)?;
        let __trace_start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_decorations()
            .call_gutter_decorations(&mut self.store, ctx)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::Decorations,
            &mut self.quarantine,
            "gutter-decorations",
            __trace_start,
            result,
        )
    }
}

impl PluginHost {
    /// Instantiate a `decorations-plugin` component under its capability grant and
    /// return the bridge: a `Send + Sync` [`DecorationClient`] plus the
    /// [`DecorationActor`] the caller drives. Grant / data-dir / WASI are identical
    /// to `instantiate_plugin` (shared `build_plugin_wasi` + `new_store`), and the
    /// actor is *not* spawned here (the lib owns no runtime). Mirror of
    /// [`spawn_completion_source`](Self::spawn_completion_source).
    pub async fn spawn_decoration_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
    ) -> Result<(DecorationClient, DecorationActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "decoration plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget)?;
        let bindings = DecorationsPlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        // PO.5: route this plugin's `logging` calls into the tracer (Layer 2).
        store.data_mut().log_ctx = self.log_ctx_for(id);
        let (tx, rx) = mpsc::unbounded();
        let client = DecorationClient { tx, id };
        let actor = DecorationActor {
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
