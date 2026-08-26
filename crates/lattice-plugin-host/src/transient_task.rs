//! TR.2b — the per-plugin actor bridge for transient-source providers.
//!
//! The transient analogue of `picker_task.rs`, and deliberately its near-twin:
//! a dedicated async task owns the plugin's `Store<PluginState>` for life (the
//! `Store` is `!Sync`), a [`TransientCall`] crosses an mpsc channel with a
//! `oneshot` reply, and the `Send + Sync` [`TransientClient`] serialises calls
//! onto the single-consumer loop.
//!
//! Two exports, and they are called at very different rates: `id()` once at
//! load, to key the registry entry; `build(ctx)` once per menu open. Neither is
//! on a hot path, which is why the design fragment can afford to call the guest
//! per open rather than caching a spec that would go stale the moment the user
//! moved to a different buffer.
//!
//! Fuel is re-armed **per call** (`arm_store` inside each `call_*`). Arming
//! once at instantiate would be correct for a declare-once seam and wrong here:
//! `build` is called for the life of the editor, so the menu would work for the
//! first stretch of a session and then silently stop opening.
//!
//! This is the Nth near-copy of the picker / completion / decoration / media /
//! agenda actor; the rule-of-three note in `completion_task` still stands, and
//! this slice deliberately did not take the generalisation on mid-seam.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use wasmtime::Store;

use crate::transient_host::bindings::TransientSourcePlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};

// The WIT records the bridge traffics in — the `with:`-mapped `types` mirrors,
// i.e. the SAME Rust types `WitBoundary` round-trips. Re-exported `pub` (they
// appear in `TransientClient`'s signatures) so callers get a clean
// `transient_task::…` path instead of reaching into `crate::lattice::…`.
pub use crate::lattice::plugin_host::types::{
    TransientContext, TransientGroup, TransientItem, TransientItemKind, TransientSpec,
};

type CallResult<T> = Result<T, PluginHostError>;

/// A request from a [`TransientClient`] to its [`TransientActor`].
enum TransientCall {
    /// `transient-source.id()` — the menu's registry name. No WIT `result`;
    /// the reply is the name or a host-side trap.
    Id {
        reply: oneshot::Sender<CallResult<String>>,
    },
    /// `transient-source.build(ctx)` — the menu for this open. Replies the
    /// guest's `result<transient-spec, string>` (or a host trap).
    Build {
        ctx: Box<TransientContext>,
        reply: oneshot::Sender<CallResult<Result<TransientSpec, String>>>,
    },
}

/// The `Send + Sync` handle the host adapter holds. Cloning is cheap (an mpsc
/// `Sender` clone); every clone talks to the same actor / `Store`, so calls
/// serialise on the single-consumer loop the `!Sync` `Store` requires.
/// Dropping the last clone ends the actor loop — the teardown seam.
#[derive(Clone, Debug)]
pub struct TransientClient {
    tx: mpsc::UnboundedSender<TransientCall>,
    id: PluginId,
}

impl TransientClient {
    /// The host-issued identity of the plugin behind this client.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `id()` — the name the menu registers under. Once, at
    /// load.
    pub async fn menu_id(&self) -> CallResult<String> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(TransientCall::Id { reply }, rx, "id").await
    }

    /// Call the guest's `build(ctx)`.
    ///
    /// The outer result is the host surface (trap / gone / quarantined); the
    /// inner `Result<_, String>` is the guest's own WIT `result`. Both mean the
    /// same thing to the caller — the menu does not open and the user is told
    /// why — but they are kept distinct so the echo can say which.
    pub async fn build(&self, ctx: TransientContext) -> CallResult<Result<TransientSpec, String>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(
            TransientCall::Build {
                ctx: Box::new(ctx),
                reply,
            },
            rx,
            "build",
        )
        .await
    }

    /// Shared send-then-await-reply. A closed channel (send fails) or a dropped
    /// reply sender (the actor unwound mid-call) both surface as
    /// [`PluginGone`](PluginHostError::PluginGone) — the caller stays live.
    async fn dispatch<T>(
        &self,
        call: TransientCall,
        rx: oneshot::Receiver<CallResult<T>>,
        func: &'static str,
    ) -> CallResult<T> {
        self.tx
            .unbounded_send(call)
            .map_err(|_| PluginHostError::PluginGone { func })?;
        rx.await.map_err(|_| PluginHostError::PluginGone { func })?
    }
}

/// The per-plugin actor: owns the `Store` + transient bindings for the plugin's
/// whole life and serves calls off the channel until every
/// [`TransientClient`] drops.
pub struct TransientActor {
    store: Store<PluginState>,
    bindings: TransientSourcePlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<TransientCall>,
    id: PluginId,
    /// Crash-quarantine: the first export trap trips this, fires one
    /// `PluginCrashed`, and every later call returns `Quarantined` without
    /// re-entering the dead `Store`. A trapped builder means the menu stops
    /// opening and says so — never a half-built menu.
    quarantine: crate::Quarantine,
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl TransientActor {
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Attach the boundary tracer (the loader calls this before spawning
    /// `run()`). Off the hot path — the seam is async.
    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
    }

    /// Drive the actor to completion. The loop ends when the channel closes
    /// (all clients dropped), dropping the `Store` — the teardown seam.
    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                TransientCall::Id { reply } => {
                    let _ = reply.send(self.call_id().await);
                }
                TransientCall::Build { ctx, reply } => {
                    let _ = reply.send(self.call_build(&ctx).await);
                }
            }
        }
    }

    async fn call_id(&mut self) -> CallResult<String> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "id" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_transient_source()
            .call_id(&mut self.store)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::TransientSource,
            &mut self.quarantine,
            "id",
            start,
            result,
        )
    }

    async fn call_build(
        &mut self,
        ctx: &TransientContext,
    ) -> CallResult<Result<TransientSpec, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "build" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_transient_source()
            .call_build(&mut self.store, ctx)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::TransientSource,
            &mut self.quarantine,
            "build",
            start,
            result,
        )
    }
}

impl PluginHost {
    /// Instantiate a `transient-source-plugin` component under its capability
    /// grant and return the bridge: a `Send + Sync` [`TransientClient`] plus the
    /// [`TransientActor`] the caller drives. Grant / data-dir / WASI are
    /// identical to every other seam (shared `build_plugin_wasi` +
    /// `new_store`), and the actor is NOT spawned here — the lib owns no
    /// runtime.
    pub async fn spawn_transient_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
    ) -> Result<(TransientClient, TransientActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "transient plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            TransientSourcePlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);
        let (tx, rx) = mpsc::unbounded();
        let client = TransientClient { tx, id };
        let actor = TransientActor {
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
