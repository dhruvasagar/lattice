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

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use wasmtime::Store;

use crate::picker_host::bindings::PickerSourcePlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};
/// OR.5b: the NATIVE spec. `register-picker-source` converts each spec at the
/// host-import call, so the actor hands back specs that have already crossed —
/// converting again here would be doing the same work twice and giving the
/// second attempt a chance to disagree.
use lattice_picker::source::PickerSourceSpec as NativePickerSourceSpec;

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
    /// OR.5b: `register-picker-sources()` — drive the guest's registration
    /// export, then hand back every spec it declared through the imported
    /// `register-picker-source`.
    ///
    /// Replaces `spec()`. The difference is the slice: a component used to BE
    /// one source and now DECLARES N, so registration is a call the guest makes
    /// rather than a value the host reads.
    RegisterSources {
        reply: oneshot::Sender<CallResult<Vec<NativePickerSourceSpec>>>,
    },
    /// `picker-source.init(source, ctx, args)` — build the candidate set for
    /// ONE of this plugin's sources. Replies the guest's
    /// `result<list<candidate-pair>, string>` (or a host trap).
    Init {
        source: String,
        ctx: Box<PickerContext>,
        args: Vec<String>,
        reply: oneshot::Sender<CallResult<Result<Vec<CandidatePair>, String>>>,
    },
    /// `picker-source.accept(source, ctx, routing)` — resolve a chosen routing
    /// token. Replies the guest's `result<picker-accept-outcome, string>`.
    Accept {
        source: String,
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

    /// OR.5b: drive the guest's `register-picker-sources()` and collect every
    /// source it declared. Returns a typed host error
    /// ([`PluginGone`](PluginHostError::PluginGone) if the actor has ended,
    /// [`Trap`](PluginHostError::Trap) on fuel/epoch/wasm).
    ///
    /// An empty list is not an error: a plugin that provides the seam and
    /// declares nothing registers nothing, which is what it asked for.
    pub async fn register_sources(&self) -> CallResult<Vec<NativePickerSourceSpec>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(
            PickerCall::RegisterSources { reply },
            rx,
            "register-picker-sources",
        )
        .await
    }

    /// Call the guest's `init(source, ctx, args)`. The outer result is the host
    /// surface; the inner `Result<_, String>` is the guest's own WIT `result`
    /// (an `Err` string is a source that declined to produce candidates).
    pub async fn init(
        &self,
        source: String,
        ctx: PickerContext,
        args: Vec<String>,
    ) -> CallResult<Result<Vec<CandidatePair>, String>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(
            PickerCall::Init {
                source,
                ctx: Box::new(ctx),
                args,
                reply,
            },
            rx,
            "init",
        )
        .await
    }

    /// Call the guest's `accept(source, ctx, routing)` — translate the user's
    /// chosen routing token into a typed outcome the host applies.
    pub async fn accept(
        &self,
        source: String,
        ctx: PickerContext,
        routing: RoutingPayload,
    ) -> CallResult<Result<PickerAcceptOutcome, String>> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(
            PickerCall::Accept {
                source,
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
    /// Crash-quarantine (PH7.12): the first export trap trips this, fires one
    /// `PluginCrashed`, and every later call returns `Quarantined` without
    /// re-entering the dead `Store`.
    quarantine: crate::Quarantine,
    /// PO.2: the boundary tracer, wired by the loader via with_tracer; None in tests / pre-wire.
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl PickerActor {
    /// The host-issued identity of this plugin (matches its [`PickerClient::id`]).
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// PO.2: attach the boundary tracer (the loader calls this before spawning
    /// run()). Off the hot path — the seam is async.
    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
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
                PickerCall::RegisterSources { reply } => {
                    let _ = reply.send(self.call_register_sources().await);
                }
                PickerCall::Init {
                    source,
                    ctx,
                    args,
                    reply,
                } => {
                    let _ = reply.send(self.call_init(&source, &ctx, &args).await);
                }
                PickerCall::Accept {
                    source,
                    ctx,
                    routing,
                    reply,
                } => {
                    let _ = reply.send(self.call_accept(&source, &ctx, &routing).await);
                }
            }
        }
    }

    /// OR.5b: drive `register-picker-sources`, then drain what the guest
    /// declared through the imported `register-picker-source`.
    ///
    /// The drain reads `PluginState` AFTER the export returns, which is the
    /// `register-grammar` shape — a guest registers by calling, so the specs do
    /// not exist until its body has run.
    async fn call_register_sources(&mut self) -> CallResult<Vec<NativePickerSourceSpec>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined {
                func: "register-picker-sources",
            });
        }
        arm_store(&mut self.store, self.budget)?;
        let __trace_start = std::time::Instant::now();
        let result = self
            .bindings
            .call_register_picker_sources(&mut self.store)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::PickerSource,
            &mut self.quarantine,
            "register-picker-sources",
            __trace_start,
            result,
        )?;
        Ok(self.store.data_mut().picker_contributions.take())
    }

    async fn call_init(
        &mut self,
        source: &str,
        ctx: &PickerContext,
        args: &[String],
    ) -> CallResult<Result<Vec<CandidatePair>, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "init" });
        }
        arm_store(&mut self.store, self.budget)?;
        let __trace_start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_picker_source()
            .call_init(&mut self.store, source, ctx, args)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::PickerSource,
            &mut self.quarantine,
            "init",
            __trace_start,
            result,
        )
    }

    async fn call_accept(
        &mut self,
        source: &str,
        ctx: &PickerContext,
        routing: &RoutingPayload,
    ) -> CallResult<Result<PickerAcceptOutcome, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "accept" });
        }
        arm_store(&mut self.store, self.budget)?;
        let __trace_start = std::time::Instant::now();
        let result = self
            .bindings
            .lattice_plugin_host_picker_source()
            .call_accept(&mut self.store, source, ctx, routing)
            .await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::PickerSource,
            &mut self.quarantine,
            "accept",
            __trace_start,
            result,
        )
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
        bus: &Arc<EventBus>,
        config: Option<&Arc<lattice_config::ConfigRegistry>>,
    ) -> Result<(PickerClient, PickerActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "picker plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings = PickerSourcePlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        // PO.5: route this plugin's `logging` calls into the tracer (Layer 2).
        store.data_mut().log_ctx = self.log_ctx_for(id);
        // OR.6: the config registry, so a source can read the options that
        // decide what it offers.
        //
        // Its absence was not a gap in the abstract — org-roam's find-node reads
        // `org.roam-directory` to decide whether it is configured at all, and
        // without a registry on THIS store `get-option` answered `none` and the
        // picker reported "roam is not configured" for a corpus it had just
        // indexed. The seam was wired end to end and answered nothing, which is
        // the failure `spawn_event_plugin` / `spawn_context_plugin` /
        // `spawn_transient_plugin` each already carry this line to prevent.
        if let Some(registry) = config {
            store.data_mut().config_registry = Some(Arc::clone(registry));
        }
        let (tx, rx) = mpsc::unbounded();
        let client = PickerClient { tx, id };
        let actor = PickerActor {
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
