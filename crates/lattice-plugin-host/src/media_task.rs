//! IM.6b — the per-plugin actor bridge for inline-media providers.
//!
//! The media analogue of `decoration_task.rs`, and deliberately its near-twin:
//! a dedicated async task owns the plugin's `Store<PluginState>` for life (the
//! Store is `!Sync`), a [`MediaCall`] crosses an mpsc channel with a `oneshot`
//! reply, and the `Send + Sync` [`MediaClient`] serialises calls onto the
//! single-consumer loop.
//!
//! A **producer**, host-called OFF the render path: the host calls `produce` on
//! a trigger (buffer opened, edited), caches the result, and the renderer reads
//! the cache. WASM on the tick would be a paramount-#1 violation.
//!
//! Generalising the picker / completion / decoration / media actors over their
//! bindings type is still deferred — this is the fourth near-copy, so the
//! rule-of-three note in `completion_task` has now been earned and the
//! generalisation is worth doing the next time one of them changes shape.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use wasmtime::Store;

use crate::media_host::bindings::MediaPlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};

// The `with:`-mapped mirrors — the SAME Rust types the boundary round-trips.
pub use crate::lattice::plugin_host::types::{DecorationContext, MediaBlock, MediaFit};

type CallResult<T> = Result<T, PluginHostError>;

enum MediaCall {
    /// `media.media-blocks(ctx)` — produce the buffer's inline media blocks.
    Produce {
        ctx: Box<DecorationContext>,
        reply: oneshot::Sender<CallResult<Result<Vec<MediaBlock>, String>>>,
    },
}

/// The `Send + Sync` handle a caller holds. Cloning is cheap; every clone talks
/// to the same actor / `Store`, so calls serialise on the single-consumer loop
/// the `!Sync` `Store` requires.
#[derive(Clone, Debug)]
pub struct MediaClient {
    tx: mpsc::UnboundedSender<MediaCall>,
    id: PluginId,
}

impl MediaClient {
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `media-blocks(ctx)`.
    ///
    /// The outer result is the host surface (trap / gone / quarantined); the
    /// inner `Result<_, String>` is the guest's own WIT `result`. An `Err`
    /// string means the provider produced nothing this trigger — it is logged
    /// and the buffer KEEPS its prior blocks, so a transient failure mid-edit
    /// does not make every image in the document blink out.
    pub async fn produce(
        &self,
        ctx: DecorationContext,
    ) -> CallResult<Result<Vec<MediaBlock>, String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(MediaCall::Produce {
                ctx: Box::new(ctx),
                reply,
            })
            .map_err(|_| PluginHostError::PluginGone {
                func: "media-blocks",
            })?;
        rx.await.map_err(|_| PluginHostError::PluginGone {
            func: "media-blocks",
        })?
    }
}

/// The per-plugin actor: owns the `Store` + media bindings for the plugin's
/// life and serves calls off the channel until every [`MediaClient`] drops.
pub struct MediaActor {
    store: Store<PluginState>,
    bindings: MediaPlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<MediaCall>,
    id: PluginId,
    /// Crash-quarantine: the first `media-blocks` trap trips this, fires one
    /// `PluginCrashed`, and every later call returns `Quarantined`.
    quarantine: crate::Quarantine,
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl MediaActor {
    pub fn id(&self) -> PluginId {
        self.id
    }

    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
    }

    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                MediaCall::Produce { ctx, reply } => {
                    let _ = reply.send(self.call_produce(&ctx).await);
                }
            }
        }
    }

    async fn call_produce(
        &mut self,
        ctx: &DecorationContext,
    ) -> CallResult<Result<Vec<MediaBlock>, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined {
                func: "media-blocks",
            });
        }
        arm_store(&mut self.store, self.budget)?;
        let __trace_start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_media()
            .call_media_blocks(&mut self.store, ctx)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::Media,
            &mut self.quarantine,
            "media-blocks",
            __trace_start,
            result,
        )
    }
}

impl PluginHost {
    /// Instantiate a `media-plugin` component under its capability grant and
    /// return the bridge. Grant / data-dir / WASI are identical to every other
    /// seam (shared `build_plugin_wasi` + `new_store`), and the actor is NOT
    /// spawned here — the lib owns no runtime.
    pub async fn spawn_media_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
    ) -> Result<(MediaClient, MediaActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "media plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings = MediaPlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);
        let (tx, rx) = mpsc::unbounded();
        let client = MediaClient { tx, id };
        let actor = MediaActor {
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
