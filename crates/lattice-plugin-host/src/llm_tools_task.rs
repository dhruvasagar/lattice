//! AT.5 — the per-plugin actor bridge for `llm-tools` plugins.
//!
//! The same shape as `transient_task.rs`: a dedicated async task owns the
//! plugin's `Store<PluginState>` for life (the `Store` is `!Sync`), an
//! [`LlmToolsCall`] crosses an unbounded channel with a `oneshot` reply, and
//! the `Send + Sync` [`LlmToolsClient`] serialises calls onto the
//! single-consumer loop. The caller is the assistant's request task, on
//! whatever thread the llm runtime runs it; neither side blocks the other.
//!
//! The client carries no llm types. lattice-ai owns what a tool may do (its
//! permission row, the review of its proposals); the plugin loader adapts this
//! client to lattice-ai's `ToolPlugin` trait. So this crate does not compile
//! the llm layer, and lattice-ai does not compile wasmtime.
//!
//! **A call cannot write.** The store is built from the plugin's grant with
//! every filesystem prefix made read-only, so the `fs:write` a plugin holds
//! for its other seams does not reach a tool call: `host-services.delete-file`
//! is refused and the WASI preopens are read-only. A plugin that wants to
//! change a file returns a proposal, which the host reviews and applies.
//! The plugin's private `/data` directory stays writable; it is the plugin's
//! own state, not the user's files.
//!
//! Fuel is re-armed **per call**: `call` is reached for the life of the
//! editor, so arming once at instantiate would make the tools work for the
//! first stretch of a session and then silently stop.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use wasmtime::Store;

use crate::capability::build_wasi_ctx;
use crate::llm_tools_host::bindings::LlmToolsPlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};

// The `with:`-mapped `types` mirrors, re-exported because they appear in
// `LlmToolsClient`'s signatures.
pub use crate::lattice::plugin_host::types::{
    LlmProfile, LlmTool, LlmToolKind, LlmToolResult, ProposedEdit,
};

type CallResult<T> = Result<T, PluginHostError>;

/// A request from an [`LlmToolsClient`] to its [`LlmToolsActor`].
enum LlmToolsCall {
    /// `llm-tools.tools()` — the tools offered. Once, at registration.
    Tools {
        reply: oneshot::Sender<CallResult<Vec<LlmTool>>>,
    },
    /// `llm-tools.call(name, args)` — one model tool call.
    Call {
        name: String,
        args: String,
        reply: oneshot::Sender<CallResult<Result<LlmToolResult, String>>>,
    },
    /// `llm-tools.profiles()` — the profiles contributed. Once, at
    /// registration.
    Profiles {
        reply: oneshot::Sender<CallResult<Vec<LlmProfile>>>,
    },
}

/// The `Send + Sync` handle the loader's adapter holds. Cloning is cheap (an
/// mpsc `Sender` clone); every clone talks to the same actor and `Store`.
/// Dropping the last clone ends the actor loop, which is the teardown seam.
#[derive(Clone, Debug)]
pub struct LlmToolsClient {
    tx: mpsc::UnboundedSender<LlmToolsCall>,
    id: PluginId,
}

impl LlmToolsClient {
    /// The host-issued identity of the plugin behind this client.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `tools()`.
    pub async fn tools(&self) -> CallResult<Vec<LlmTool>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(LlmToolsCall::Tools { reply }, rx, "tools")
            .await
    }

    /// Call the guest's `call(name, args)`.
    ///
    /// The outer result is the host surface (trap, gone, quarantined); the
    /// inner `Result<_, String>` is the guest's own WIT `result`. To the model
    /// both are the tool failing, but they are kept apart so the message can
    /// say which.
    pub async fn call(
        &self,
        name: String,
        args: String,
    ) -> CallResult<Result<LlmToolResult, String>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(LlmToolsCall::Call { name, args, reply }, rx, "call")
            .await
    }

    /// Call the guest's `profiles()`.
    pub async fn profiles(&self) -> CallResult<Vec<LlmProfile>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(LlmToolsCall::Profiles { reply }, rx, "profiles")
            .await
    }

    /// Send, then await the reply. A closed channel or a dropped reply sender
    /// (the actor unwound mid-call) both surface as
    /// [`PluginGone`](PluginHostError::PluginGone); the caller stays live.
    async fn dispatch<T>(
        &self,
        call: LlmToolsCall,
        rx: oneshot::Receiver<CallResult<T>>,
        func: &'static str,
    ) -> CallResult<T> {
        self.tx
            .unbounded_send(call)
            .map_err(|_| PluginHostError::PluginGone { func })?;
        rx.await.map_err(|_| PluginHostError::PluginGone { func })?
    }
}

/// The per-plugin actor: owns the `Store` and bindings for the plugin's whole
/// life and serves calls until every [`LlmToolsClient`] drops.
pub struct LlmToolsActor {
    store: Store<PluginState>,
    bindings: LlmToolsPlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<LlmToolsCall>,
    id: PluginId,
    /// Crash quarantine: the first trap trips this and fires one
    /// `PluginCrashed`; every later call returns `Quarantined` without
    /// re-entering the dead `Store`, and the model is told the tool failed.
    quarantine: crate::Quarantine,
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl LlmToolsActor {
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Attach the boundary tracer (the loader calls this before spawning
    /// `run()`).
    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
    }

    /// Drive the actor to completion. The loop ends when the channel closes
    /// (all clients dropped), dropping the `Store`.
    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                LlmToolsCall::Tools { reply } => {
                    let _ = reply.send(self.call_tools().await);
                }
                LlmToolsCall::Call { name, args, reply } => {
                    let _ = reply.send(self.call_call(&name, &args).await);
                }
                LlmToolsCall::Profiles { reply } => {
                    let _ = reply.send(self.call_profiles().await);
                }
            }
        }
    }

    async fn call_tools(&mut self) -> CallResult<Vec<LlmTool>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "tools" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_llm_tools()
            .call_tools(&mut self.store)
            .await;
        self.settle("tools", start, result)
    }

    async fn call_call(
        &mut self,
        name: &str,
        args: &str,
    ) -> CallResult<Result<LlmToolResult, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "call" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_llm_tools()
            .call_call(&mut self.store, name, args)
            .await;
        self.settle("call", start, result)
    }

    async fn call_profiles(&mut self) -> CallResult<Vec<LlmProfile>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "profiles" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_llm_tools()
            .call_profiles(&mut self.store)
            .await;
        self.settle("profiles", start, result)
    }

    fn settle<T>(
        &mut self,
        func: &'static str,
        start: std::time::Instant,
        result: wasmtime::Result<T>,
    ) -> CallResult<T> {
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::LlmTools,
            &mut self.quarantine,
            func,
            start,
            result,
        )
    }
}

impl PluginHost {
    /// Instantiate an `llm-tools-plugin` component and return the bridge: a
    /// `Send + Sync` [`LlmToolsClient`] plus the [`LlmToolsActor`] the caller
    /// drives. The actor is NOT spawned here; the lib owns no runtime.
    ///
    /// The grant is the plugin's own with every filesystem prefix made
    /// read-only (see the module note). Network and process grants are kept:
    /// a plugin whose tools need them may hold them, and the assistant asks
    /// before each call of such a plugin's tools.
    pub async fn spawn_llm_tools(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
        config: Option<&Arc<lattice_config::ConfigRegistry>>,
    ) -> Result<(LlmToolsClient, LlmToolsActor), PluginHostError> {
        let (_, mut outcome, data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "llm-tools plugin loaded with a withheld capability (reduced function)"
            );
        }
        for fs in &mut outcome.grant.fs {
            fs.write = false;
        }
        let wasi = build_wasi_ctx(&outcome.grant, &data_dir);
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings = LlmToolsPlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);
        // A tool reads its plugin's own options (org's agenda files).
        if let Some(registry) = config {
            store.data_mut().config_registry = Some(Arc::clone(registry));
        }
        let (tx, rx) = mpsc::unbounded();
        let client = LlmToolsClient { tx, id };
        let actor = LlmToolsActor {
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
