//! OM.A1 — the per-plugin actor bridge for agenda-source providers.
//!
//! The agenda analogue of `media_task.rs`, and deliberately its near-twin: a
//! dedicated async task owns the plugin's `Store<PluginState>` for life (the
//! Store is `!Sync`), an [`AgendaCall`] crosses an mpsc channel with a
//! `oneshot` reply, and the `Send + Sync` [`AgendaClient`] serialises calls
//! onto the single-consumer loop.
//!
//! **The serialisation is load-bearing here, not incidental.** `begin` drops
//! per-scan state and every following `scan` reads it, so the two must not
//! interleave with a second scan's calls. One actor, one queue, in order —
//! which is also why the scan walks files sequentially rather than fanning
//! out across the pool.
//!
//! This is the fifth near-copy of the picker / completion / decoration / media
//! actor. The rule-of-three note in `completion_task` is now well past earned;
//! generalising over the bindings type is worth doing the next time one of
//! them changes shape, and this slice deliberately did not take that on
//! mid-seam.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use wasmtime::Store;

use crate::agenda_host::bindings::AgendaSourcePlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};

/// The WIT `entry`, re-exported so the adapter and the loader name one type.
pub use crate::agenda_host::bindings::lattice::plugin_host::agenda_source::Entry;

type CallResult<T> = Result<T, PluginHostError>;

enum AgendaCall {
    /// `extensions()` — the file extensions this source wants offered.
    Extensions {
        reply: oneshot::Sender<CallResult<Vec<String>>>,
    },
    /// `begin()` — drop per-scan state.
    Begin {
        reply: oneshot::Sender<CallResult<()>>,
    },
    /// `scan(path, text)` — one file's agenda rows.
    Scan {
        path: String,
        text: String,
        reply: oneshot::Sender<CallResult<Result<Vec<Entry>, String>>>,
    },
}

/// The `Send + Sync` handle a caller holds. Cloning is cheap; every clone
/// talks to the same actor / `Store`, so calls serialise on the
/// single-consumer loop the `!Sync` `Store` requires — which is exactly what
/// `begin`-then-`scan` needs.
#[derive(Clone, Debug)]
pub struct AgendaClient {
    tx: mpsc::UnboundedSender<AgendaCall>,
    id: PluginId,
}

impl AgendaClient {
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `extensions()`. Once, at load.
    pub async fn extensions(&self) -> CallResult<Vec<String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(AgendaCall::Extensions { reply })
            .map_err(|_| PluginHostError::PluginGone { func: "extensions" })?;
        rx.await
            .map_err(|_| PluginHostError::PluginGone { func: "extensions" })?
    }

    /// Call the guest's `begin()` — the start of a scan.
    pub async fn begin(&self) -> CallResult<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(AgendaCall::Begin { reply })
            .map_err(|_| PluginHostError::PluginGone { func: "begin" })?;
        rx.await
            .map_err(|_| PluginHostError::PluginGone { func: "begin" })?
    }

    /// Call the guest's `scan(path, text)`.
    ///
    /// The outer result is the host surface (trap / gone / quarantined); the
    /// inner `Result<_, String>` is the guest's own WIT `result`. Either way
    /// the caller skips THIS FILE and continues — one bad file must not fail
    /// the agenda.
    pub async fn scan(&self, path: String, text: String) -> CallResult<Result<Vec<Entry>, String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(AgendaCall::Scan { path, text, reply })
            .map_err(|_| PluginHostError::PluginGone { func: "scan" })?;
        rx.await
            .map_err(|_| PluginHostError::PluginGone { func: "scan" })?
    }
}

/// The per-plugin actor: owns the `Store` + agenda bindings for the plugin's
/// life and serves calls off the channel until every [`AgendaClient`] drops.
pub struct AgendaActor {
    store: Store<PluginState>,
    bindings: AgendaSourcePlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<AgendaCall>,
    id: PluginId,
    /// Crash-quarantine: the first trap trips this, fires one `PluginCrashed`,
    /// and every later call returns `Quarantined`. A trap mid-scan therefore
    /// leaves the agenda showing what it collected rather than emptying it —
    /// partial-and-honest beats empty-and-silent (`org-mode.md` §8).
    quarantine: crate::Quarantine,
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl AgendaActor {
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
                AgendaCall::Extensions { reply } => {
                    let _ = reply.send(self.call_extensions().await);
                }
                AgendaCall::Begin { reply } => {
                    let _ = reply.send(self.call_begin().await);
                }
                AgendaCall::Scan { path, text, reply } => {
                    let _ = reply.send(self.call_scan(&path, &text).await);
                }
            }
        }
    }

    async fn call_extensions(&mut self) -> CallResult<Vec<String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "extensions" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self.bindings.call_extensions(&mut self.store).await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::AgendaSource,
            &mut self.quarantine,
            "extensions",
            start,
            result,
        )
    }

    async fn call_begin(&mut self) -> CallResult<()> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "begin" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self.bindings.call_begin(&mut self.store).await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::AgendaSource,
            &mut self.quarantine,
            "begin",
            start,
            result,
        )
    }

    /// **Fuel is re-armed per call**, and a scan calls this once per file.
    /// Arming once at instantiate — correct for a declare-once seam — would
    /// make the agenda work for the first stretch of a large project and then
    /// silently contribute nothing for the rest, which is precisely the cliff
    /// `WasmErrorParser::rearm` documents. `arm_store` above is that re-arm;
    /// it is not boilerplate.
    async fn call_scan(
        &mut self,
        path: &str,
        text: &str,
    ) -> CallResult<Result<Vec<Entry>, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "scan" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self.bindings.call_scan(&mut self.store, path, text).await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::AgendaSource,
            &mut self.quarantine,
            "scan",
            start,
            result,
        )
    }
}

impl PluginHost {
    /// Instantiate an `agenda-source-plugin` component under its capability
    /// grant and return the bridge. Grant / data-dir / WASI are identical to
    /// every other seam (shared `build_plugin_wasi` + `new_store`), and the
    /// actor is NOT spawned here — the lib owns no runtime.
    pub async fn spawn_agenda_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
    ) -> Result<(AgendaClient, AgendaActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "agenda plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings = AgendaSourcePlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);
        let (tx, rx) = mpsc::unbounded();
        let client = AgendaClient { tx, id };
        let actor = AgendaActor {
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
