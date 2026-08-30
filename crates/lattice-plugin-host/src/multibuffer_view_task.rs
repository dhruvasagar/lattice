//! MV.1 — the per-plugin actor bridge for multibuffer-view sources.
//!
//! The `picker_task.rs` shape, and deliberately so: a dedicated async task owns
//! the plugin's `Store<PluginState>` for life (the `Store` is `!Sync`), a
//! [`ViewCall`] crosses an mpsc channel with a `oneshot` reply, and the
//! `Send + Sync` [`MultibufferViewClient`] serializes calls onto the single-
//! consumer loop. [`PluginHost::spawn_multibuffer_view_source`] instantiates the
//! `multibuffer-view-plugin` world under the plugin's grant and returns
//! `(client, actor)`; the caller drives [`MultibufferViewActor::run`] on its
//! multi-thread runtime (the lib owns no runtime).
//!
//! This is the fourth actor of this shape (picker, completion, agenda, view).
//! The rule-of-three trigger to generalise the loop over the bindings type has
//! fired, and generalising it is its own refactor rather than a thing to attempt
//! inside the slice that adds the fourth — noted here so the next one does not
//! have to rediscover the count.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use wasmtime::Store;

use crate::multibuffer_view_host::bindings::MultibufferViewPlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};

pub use crate::lattice::plugin_host::types::{MultibufferViewResult, MultibufferViewSpec};

/// See `picker_task::CallResult`.
type CallResult<T> = Result<T, PluginHostError>;

/// A request sent from a [`MultibufferViewClient`] to its actor.
enum ViewCall {
    /// `register-multibuffer-views()` — drive the guest's registration export,
    /// then hand back every view it declared through the imported
    /// `register-multibuffer-view`.
    RegisterViews {
        reply: oneshot::Sender<CallResult<Vec<MultibufferViewSpec>>>,
    },
    /// `multibuffer-view-source.build(view, args)` — produce one view's
    /// excerpts. Replies the guest's `result<multibuffer-view-result, string>`
    /// (or a host trap).
    Build {
        view: String,
        args: Vec<String>,
        reply: oneshot::Sender<CallResult<Result<MultibufferViewResult, String>>>,
    },
}

/// The `Send + Sync` handle the provider holds. Cloning is cheap (an mpsc
/// `Sender` clone); every clone talks to the same actor / `Store`, so calls
/// serialize on the single-consumer loop — the guarantee the `!Sync` `Store`
/// needs. Dropping the last clone ends the actor loop (teardown).
#[derive(Clone)]
pub struct MultibufferViewClient {
    tx: mpsc::UnboundedSender<ViewCall>,
    id: PluginId,
}

impl MultibufferViewClient {
    /// The host-issued identity of the plugin behind this client.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Drive the guest's `register-multibuffer-views()` and collect every view
    /// it declared.
    ///
    /// An empty list is not an error: a plugin that provides the seam and
    /// declares nothing registers nothing, which is what it asked for.
    pub async fn register_views(&self) -> CallResult<Vec<MultibufferViewSpec>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(
            ViewCall::RegisterViews { reply },
            rx,
            "register-multibuffer-views",
        )
        .await
    }

    /// Call the guest's `build(view, args)`. The outer result is the host
    /// surface; the inner `Result<_, String>` is the guest's own WIT `result`,
    /// whose `Err` **declines** the view with the guest's message rather than
    /// opening an empty one.
    pub async fn build(
        &self,
        view: String,
        args: Vec<String>,
    ) -> CallResult<Result<MultibufferViewResult, String>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(ViewCall::Build { view, args, reply }, rx, "build")
            .await
    }

    /// Shared send-then-await-reply. A closed channel or a dropped reply sender
    /// both surface as [`PluginGone`](PluginHostError::PluginGone) — the caller
    /// stays live.
    async fn dispatch<T>(
        &self,
        call: ViewCall,
        rx: oneshot::Receiver<CallResult<T>>,
        func: &'static str,
    ) -> CallResult<T> {
        self.tx
            .unbounded_send(call)
            .map_err(|_| PluginHostError::PluginGone { func })?;
        rx.await.map_err(|_| PluginHostError::PluginGone { func })?
    }
}

/// The per-plugin actor: owns the `Store` + view bindings for the plugin's whole
/// life and serves calls until every client is dropped.
pub struct MultibufferViewActor {
    store: Store<PluginState>,
    bindings: MultibufferViewPlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<ViewCall>,
    id: PluginId,
    quarantine: crate::Quarantine,
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl MultibufferViewActor {
    /// The host-issued identity of this plugin.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// PO.2: attach the boundary tracer before spawning `run`.
    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
    }

    /// Drive the actor to completion. A trap does not end the loop — the
    /// `Store` survives a clean fuel/epoch trap, and quarantine handles the
    /// rest. The loop ends when the channel closes, dropping the `Store`.
    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                ViewCall::RegisterViews { reply } => {
                    let _ = reply.send(self.call_register_views().await);
                }
                ViewCall::Build { view, args, reply } => {
                    let _ = reply.send(self.call_build(&view, &args).await);
                }
            }
        }
    }

    /// Drive `register-multibuffer-views`, then drain what the guest declared.
    ///
    /// The drain reads `PluginState` AFTER the export returns — the
    /// `register-grammar` shape, because a guest registers by *calling*, so the
    /// specs do not exist until its body has run.
    async fn call_register_views(&mut self) -> CallResult<Vec<MultibufferViewSpec>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined {
                func: "register-multibuffer-views",
            });
        }
        arm_store(&mut self.store, self.budget)?;
        let __trace_start = std::time::Instant::now();
        let result = self
            .bindings
            .call_register_multibuffer_views(&mut self.store)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::MultibufferViewSource,
            &mut self.quarantine,
            "register-multibuffer-views",
            __trace_start,
            result,
        )?;
        Ok(std::mem::take(
            &mut self.store.data_mut().multibuffer_view_contributions.specs,
        ))
    }

    async fn call_build(
        &mut self,
        view: &str,
        args: &[String],
    ) -> CallResult<Result<MultibufferViewResult, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "build" });
        }
        arm_store(&mut self.store, self.budget)?;
        let __trace_start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_multibuffer_view_source()
            .call_build(&mut self.store, view, args)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::MultibufferViewSource,
            &mut self.quarantine,
            "build",
            __trace_start,
            result,
        )
    }
}

impl PluginHost {
    /// Instantiate a `multibuffer-view-plugin` component under its capability
    /// grant and return the bridge. Grant / data-dir / WASI are identical to
    /// `instantiate_plugin`; the actor is *not* spawned here (the lib owns no
    /// runtime). Mirror of [`spawn_picker_source`](Self::spawn_picker_source).
    pub async fn spawn_multibuffer_view_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
        config: Option<&Arc<lattice_config::ConfigRegistry>>,
    ) -> Result<(MultibufferViewClient, MultibufferViewActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "multibuffer-view plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            MultibufferViewPlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);
        // MV.1: the config registry. The SEVENTH seam to need this line, and six
        // of the previous ones shipped without it — each answering `none` to
        // `get-option` while looking perfectly wired. A view's contents very
        // often depend on an option (which directory, which filter), so it is
        // stamped here rather than waiting for the bug report.
        if let Some(registry) = config {
            store.data_mut().config_registry = Some(Arc::clone(registry));
        }
        let (tx, rx) = mpsc::unbounded();
        let client = MultibufferViewClient { tx, id };
        let actor = MultibufferViewActor {
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
