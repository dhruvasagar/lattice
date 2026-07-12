//! PH7.4c.1b — the per-plugin actor task + call protocol (the bridge).
//!
//! Design fragment: `docs/dev/architecture/plugin-host.md` §3 (the
//! Store-per-plugin, task-per-Store model) and §4.3 (host owns the async
//! plumbing). Slice plan: `slice-plans/plugin-host.md` PH7.4c.1b.
//!
//! ## The problem this solves
//!
//! A picker source's exports (`spec`/`init`/`accept`, `picker_host.rs`) are
//! **async, single-threaded, and fuel-bounded**: they run against a
//! `wasmtime::Store<PluginState>`, which is `!Sync` and must not be touched
//! concurrently. But the host wraps a source as `Arc<dyn PickerSourceGenerator>`
//! (PH7.4c.2) — a `Send + Sync` trait object the picker registry calls from
//! anywhere. Something has to bridge a `Send + Sync` caller to a `!Sync`,
//! single-threaded, async guest.
//!
//! ## The shape (locked with Dhruva, design option A)
//!
//! Each plugin runs as **one dedicated async task** ([`PickerActor`]) that owns
//! its `Store` for its whole life. The task loops over an mpsc channel; each
//! [`PickerCall`] carries the call's inputs plus a `oneshot` reply sender. The
//! `Send + Sync` [`PickerClient`] holds the mpsc `Sender`: `init`/`accept`/`spec`
//! send a request and await the oneshot reply. The `Store` is never locked and
//! never leaves the task, so it stays single-threaded by construction; per-call
//! fuel/epoch is armed inside the loop, before each guest call.
//!
//! This is chosen over `Arc<async_mutex<Store>>` (a lock held across `.await`,
//! and a runtime dependency baked into the lib) because it keeps the `Store`
//! genuinely single-threaded and reuses cleanly for every future guest-backed
//! `Arc<dyn>` adapter (completion, grammar, modes). See the fragment's
//! heuristic-#1 note.
//!
//! ## Runtime ownership
//!
//! The lib owns no async runtime. [`PluginHost::spawn_picker_source`] returns
//! the `(PickerClient, PickerActor)` pair; the **caller** drives the actor by
//! spawning [`PickerActor::run`] on its own multi-thread runtime (never the
//! `current_thread` editor actor — paramount #4 + the no-UI-thread-work rule).
//! The channels are `futures::channel`, runtime-agnostic on purpose.

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use wasmtime::Store;

use crate::picker_host::bindings::PickerSourcePlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store, classify_trap,
};

// The picker WIT records the bridge's public API traffics in. They are the
// `with:`-mapped `types` mirrors (`picker_host.rs`) plus the picker-interface
// `candidate-pair`, i.e. the SAME Rust types `WitBoundary` round-trips — the
// boundary conversion (native ↔ these) is PH7.4c.2's job, so this bridge speaks
// purely in WIT types and stays conversion-free. Re-exported `pub` (they appear
// in the `PickerClient` method signatures) so callers get a clean
// `picker_task::…` path instead of reaching into `crate::lattice::…`.
pub use crate::lattice::plugin_host::types::{
    ActiveBufferSnapshot, PickerAcceptOutcome, PickerContext, PickerSourceSpec, Position,
    RoutingPayload,
};
pub use crate::picker_host::bindings::exports::lattice::plugin_host::picker_source::CandidatePair;

/// The result of a picker guest call routed through the actor. The outer
/// [`PluginHostError`] is the *host-side* failure surface — a
/// [`Trap`](PluginHostError::Trap) (fuel/epoch/wasm) or
/// [`PluginGone`](PluginHostError::PluginGone) (the actor ended). The inner
/// `Result<T, String>` is the guest's own typed WIT `result` — an `init` that
/// declined, an `accept` that could not resolve its routing token. `spec` has
/// no WIT `result`, so its call type is just `Result<PickerSourceSpec, _>`.
type CallResult<T> = Result<T, PluginHostError>;

/// A request sent from a [`PickerClient`] to its [`PickerActor`]. Each variant
/// carries the guest inputs plus the `oneshot` the actor replies on. The large
/// [`PickerContext`] projection is boxed so the enum stays small.
enum PickerCall {
    /// `picker-source.spec()` — the source's declared metadata. No WIT
    /// `result`; the reply is the spec or a host-side trap.
    Spec {
        reply: oneshot::Sender<CallResult<PickerSourceSpec>>,
    },
    /// `picker-source.init(ctx, args)` — build the candidate set. Replies the
    /// guest's `result<list<candidate-pair>, string>` (or a host trap).
    Init {
        ctx: Box<PickerContext>,
        args: Vec<String>,
        reply: oneshot::Sender<CallResult<Result<Vec<CandidatePair>, String>>>,
    },
    /// `picker-source.accept(ctx, routing)` — resolve a chosen routing token.
    /// Replies the guest's `result<picker-accept-outcome, string>`.
    Accept {
        ctx: Box<PickerContext>,
        routing: RoutingPayload,
        reply: oneshot::Sender<CallResult<Result<PickerAcceptOutcome, String>>>,
    },
}

/// The `Send + Sync` handle a host adapter (PH7.4c.2) holds. Cloning it is cheap
/// (an mpsc `Sender` clone); every clone talks to the same actor / `Store`, so
/// calls are serialized by the single-consumer loop — the guarantee the `!Sync`
/// `Store` needs. Dropping the last clone ends the actor loop (teardown).
#[derive(Clone)]
pub struct PickerClient {
    tx: mpsc::UnboundedSender<PickerCall>,
    id: PluginId,
}

impl PickerClient {
    /// The host-issued identity of the plugin behind this client — the `u32`
    /// inside its `SourceLayer::Plugin(id)` provenance.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `spec()`. Returns the source's declared metadata, or a
    /// typed host error ([`PluginGone`](PluginHostError::PluginGone) if the
    /// actor has ended, [`Trap`](PluginHostError::Trap) on fuel/epoch/wasm).
    pub async fn spec(&self) -> CallResult<PickerSourceSpec> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(PickerCall::Spec { reply }, rx, "spec").await
    }

    /// Call the guest's `init(ctx, args)`. The outer result is the host surface;
    /// the inner `Result<_, String>` is the guest's own WIT `result` (an `Err`
    /// string is a source that declined to produce candidates).
    pub async fn init(
        &self,
        ctx: PickerContext,
        args: Vec<String>,
    ) -> CallResult<Result<Vec<CandidatePair>, String>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(
            PickerCall::Init {
                ctx: Box::new(ctx),
                args,
                reply,
            },
            rx,
            "init",
        )
        .await
    }

    /// Call the guest's `accept(ctx, routing)` — translate the user's chosen
    /// routing token into a typed outcome the host applies.
    pub async fn accept(
        &self,
        ctx: PickerContext,
        routing: RoutingPayload,
    ) -> CallResult<Result<PickerAcceptOutcome, String>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(
            PickerCall::Accept {
                ctx: Box::new(ctx),
                routing,
                reply,
            },
            rx,
            "accept",
        )
        .await
    }

    /// Shared send-then-await-reply. A closed channel (send fails) or a dropped
    /// reply sender (the actor unwound mid-call) both surface as
    /// [`PluginGone`](PluginHostError::PluginGone) — the caller stays live.
    async fn dispatch<T>(
        &self,
        call: PickerCall,
        rx: oneshot::Receiver<CallResult<T>>,
        func: &'static str,
    ) -> CallResult<T> {
        self.tx
            .unbounded_send(call)
            .map_err(|_| PluginHostError::PluginGone { func })?;
        rx.await.map_err(|_| PluginHostError::PluginGone { func })?
    }
}

/// The per-plugin actor: owns the `Store` + picker bindings for the plugin's
/// whole life and serves calls off the channel until every [`PickerClient`] is
/// dropped. Construct via [`PluginHost::spawn_picker_source`]; drive by spawning
/// [`run`](Self::run) on a multi-thread runtime.
pub struct PickerActor {
    store: Store<PluginState>,
    bindings: PickerSourcePlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<PickerCall>,
    id: PluginId,
}

impl PickerActor {
    /// The host-issued identity of this plugin (matches its [`PickerClient::id`]).
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Drive the actor to completion. Serves each [`PickerCall`] in arrival
    /// order — arming the per-call fuel/epoch budget, calling the guest export,
    /// mapping a trap to a typed error — and replies on the call's `oneshot`.
    /// A trap does **not** end the loop: the `Store` survives a clean fuel/epoch
    /// trap, so the source stays callable (full crash-quarantine is PH7.12). The
    /// loop ends when the channel closes (all clients dropped), dropping the
    /// `Store` — the teardown seam. If a caller has already dropped its reply
    /// receiver, the send is a no-op (the call was abandoned).
    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                PickerCall::Spec { reply } => {
                    let _ = reply.send(self.call_spec().await);
                }
                PickerCall::Init { ctx, args, reply } => {
                    let _ = reply.send(self.call_init(&ctx, &args).await);
                }
                PickerCall::Accept {
                    ctx,
                    routing,
                    reply,
                } => {
                    let _ = reply.send(self.call_accept(&ctx, &routing).await);
                }
            }
        }
    }

    async fn call_spec(&mut self) -> CallResult<PickerSourceSpec> {
        arm_store(&mut self.store, self.budget)?;
        self.bindings
            .lattice_plugin_host_picker_source()
            .call_spec(&mut self.store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "spec",
                kind: classify_trap(&source),
                source: source.into(),
            })
    }

    async fn call_init(
        &mut self,
        ctx: &PickerContext,
        args: &[String],
    ) -> CallResult<Result<Vec<CandidatePair>, String>> {
        arm_store(&mut self.store, self.budget)?;
        self.bindings
            .lattice_plugin_host_picker_source()
            .call_init(&mut self.store, ctx, args)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "init",
                kind: classify_trap(&source),
                source: source.into(),
            })
    }

    async fn call_accept(
        &mut self,
        ctx: &PickerContext,
        routing: &RoutingPayload,
    ) -> CallResult<Result<PickerAcceptOutcome, String>> {
        arm_store(&mut self.store, self.budget)?;
        self.bindings
            .lattice_plugin_host_picker_source()
            .call_accept(&mut self.store, ctx, routing)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "accept",
                kind: classify_trap(&source),
                source: source.into(),
            })
    }
}

impl PluginHost {
    /// Instantiate a `picker-source-plugin` component under its capability grant
    /// and return the bridge: a `Send + Sync` [`PickerClient`] plus the
    /// [`PickerActor`] the caller drives (spawn [`PickerActor::run`] on a
    /// multi-thread runtime). Grant computation, the private data dir, and the
    /// scoped WASI view are identical to
    /// [`instantiate_plugin`](Self::instantiate_plugin) (via `build_plugin_wasi`)
    /// — a picker plugin is sandboxed exactly like a lifecycle plugin.
    ///
    /// The actor is *not* spawned here (the lib owns no runtime). Until the
    /// caller drives it, calls on the client simply queue on the channel.
    ///
    /// Denied capabilities (a tier-withheld request) are logged; surfacing them
    /// to the user rides the registration path (PH7.4c.2), which is the only
    /// consumer that needs them.
    pub async fn spawn_picker_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
    ) -> Result<(PickerClient, PickerActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "picker plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget)?;
        let bindings = PickerSourcePlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        let (tx, rx) = mpsc::unbounded();
        let client = PickerClient { tx, id };
        let actor = PickerActor {
            store,
            bindings,
            budget,
            rx,
            id,
        };
        Ok((client, actor))
    }
}
