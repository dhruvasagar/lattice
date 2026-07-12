//! PH7.6 — the per-plugin actor bridge for completion sources.
//!
//! The completion analogue of `picker_task.rs`: a dedicated async task owns the
//! plugin's `Store<PluginState>` for life (the Store is `!Sync`), a
//! `CompletionCall` crosses an mpsc channel with a `oneshot` reply, and the
//! `Send + Sync` [`CompletionClient`] serializes calls onto the single-consumer
//! loop. `PluginHost::spawn_completion_source` instantiates the
//! `completion-source-plugin` world under the plugin's grant and returns
//! `(CompletionClient, CompletionActor)`; the caller drives
//! [`CompletionActor::run`] on its multi-thread runtime (the lib owns no
//! runtime).
//!
//! The picker and completion actors are near-identical; a third guest-backed
//! `Arc<dyn>` adapter (grammar, PH7.7) is the rule-of-three trigger to generalise
//! the loop over the bindings type. Until then this duplicates the ~50-line
//! shape and reuses the shared `arm_store` / `new_store` / `build_plugin_wasi`
//! primitives.

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use wasmtime::Store;

use crate::completion_host::bindings::CompletionSourcePlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store, classify_trap,
};

// The completion WIT records the bridge's public API traffics in — the
// `with:`-mapped `types` mirrors (`completion_host.rs`), i.e. the SAME Rust types
// `WitBoundary` round-trips; the native↔WIT conversion is the adapter's job
// (`picker_source.rs` precedent). Re-exported `pub` (they appear in the
// `CompletionClient` method signatures).
pub use crate::lattice::plugin_host::types::{CompletionSourceSpec, GenerateContext, RawCandidate};

/// See `picker_task::CallResult`.
type CallResult<T> = Result<T, PluginHostError>;

/// A request sent from a [`CompletionClient`] to its [`CompletionActor`].
enum CompletionCall {
    /// `completion-source.spec()` — the source's `(id, doc)` identity. No WIT
    /// `result`; the reply is the spec or a host-side trap.
    Spec {
        reply: oneshot::Sender<CallResult<CompletionSourceSpec>>,
    },
    /// `completion-source.generate(ctx)` — produce raw candidates. Replies the
    /// guest's `result<list<raw-candidate>, string>` (or a host trap).
    Generate {
        ctx: Box<GenerateContext>,
        reply: oneshot::Sender<CallResult<Result<Vec<RawCandidate>, String>>>,
    },
}

/// The `Send + Sync` handle the [`WasmCompletionSource`](crate::WasmCompletionSource)
/// adapter holds. Cloning is cheap (an mpsc `Sender` clone); every clone talks to
/// the same actor / `Store`, so calls serialize on the single-consumer loop.
#[derive(Clone)]
pub struct CompletionClient {
    tx: mpsc::UnboundedSender<CompletionCall>,
    id: PluginId,
}

impl CompletionClient {
    /// The host-issued identity of the plugin behind this client.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `spec()`.
    pub async fn spec(&self) -> CallResult<CompletionSourceSpec> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(CompletionCall::Spec { reply }, rx, "spec")
            .await
    }

    /// Call the guest's `generate(ctx)`. The outer result is the host surface;
    /// the inner `Result<_, String>` is the guest's own WIT `result` (an `Err`
    /// string is a source that produced no rows — logged, echoed as empty).
    pub async fn generate(
        &self,
        ctx: GenerateContext,
    ) -> CallResult<Result<Vec<RawCandidate>, String>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(
            CompletionCall::Generate {
                ctx: Box::new(ctx),
                reply,
            },
            rx,
            "generate",
        )
        .await
    }

    async fn dispatch<T>(
        &self,
        call: CompletionCall,
        rx: oneshot::Receiver<CallResult<T>>,
        func: &'static str,
    ) -> CallResult<T> {
        self.tx
            .unbounded_send(call)
            .map_err(|_| PluginHostError::PluginGone { func })?;
        rx.await.map_err(|_| PluginHostError::PluginGone { func })?
    }
}

/// The per-plugin actor: owns the `Store` + completion bindings for the plugin's
/// life and serves calls off the channel until every [`CompletionClient`] drops.
pub struct CompletionActor {
    store: Store<PluginState>,
    bindings: CompletionSourcePlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<CompletionCall>,
    id: PluginId,
}

impl CompletionActor {
    /// The host-issued identity of this plugin.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Drive the actor to completion — see `picker_task::PickerActor::run`.
    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                CompletionCall::Spec { reply } => {
                    let _ = reply.send(self.call_spec().await);
                }
                CompletionCall::Generate { ctx, reply } => {
                    let _ = reply.send(self.call_generate(&ctx).await);
                }
            }
        }
    }

    async fn call_spec(&mut self) -> CallResult<CompletionSourceSpec> {
        arm_store(&mut self.store, self.budget)?;
        self.bindings
            .lattice_plugin_host_completion_source()
            .call_spec(&mut self.store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "spec",
                kind: classify_trap(&source),
                source: source.into(),
            })
    }

    async fn call_generate(
        &mut self,
        ctx: &GenerateContext,
    ) -> CallResult<Result<Vec<RawCandidate>, String>> {
        arm_store(&mut self.store, self.budget)?;
        self.bindings
            .lattice_plugin_host_completion_source()
            .call_generate(&mut self.store, ctx)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "generate",
                kind: classify_trap(&source),
                source: source.into(),
            })
    }
}

impl PluginHost {
    /// Instantiate a `completion-source-plugin` component under its capability
    /// grant and return the bridge: a `Send + Sync` [`CompletionClient`] plus the
    /// [`CompletionActor`] the caller drives. Grant / data-dir / WASI are
    /// identical to `instantiate_plugin` (shared `build_plugin_wasi` +
    /// `new_store`), and the actor is *not* spawned here (the lib owns no
    /// runtime). Mirror of [`spawn_picker_source`](Self::spawn_picker_source).
    pub async fn spawn_completion_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
    ) -> Result<(CompletionClient, CompletionActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "completion plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget)?;
        let bindings =
            CompletionSourcePlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        let (tx, rx) = mpsc::unbounded();
        let client = CompletionClient { tx, id };
        let actor = CompletionActor {
            store,
            bindings,
            budget,
            rx,
            id,
        };
        Ok((client, actor))
    }
}
