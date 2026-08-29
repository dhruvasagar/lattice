//! Plugin host — the WASM Component Model extension substrate (Phase 7).
//!
//! Design fragment: `docs/dev/architecture/plugin-host.md`. Slice plan:
//! `docs/dev/operations/slice-plans/plugin-host.md`. Spec: `design.md` §5.5.
//!
//! **PH7.1a — async runtime core.** The host now runs the canonical async
//! ABI (`design.md` §5.5, fragment §3): the engine has `async_support`, so a
//! plugin's lifecycle exports are `async` and a host call suspends the WASM
//! stack rather than pinning an OS thread. Each call runs under two hard
//! budgets — a **fuel** cap (total work) and an **epoch** deadline
//! (wall-clock) — and either, on exhaustion, traps *cleanly*: the offending
//! call returns a typed [`PluginHostError::Trap`], the [`Store`] is untouched
//! by any other plugin, and the host stays live. A background epoch-ticker
//! thread bumps the engine epoch so the wall-clock deadline actually fires.
//!
//! The lib owns no async runtime: methods are `async fn`, so the *caller*
//! (the editor's multi-thread pool — never the `current_thread` actor)
//! drives them. Running two plugins on two tasks runs them on two cores.
//!
//! **PH7.1b — module cache + lazy instantiation.** The AOT (Cranelift) compile
//! of a component is cached on disk (via wasmtime's own cache, keyed on bytes +
//! compiler config + target + wasmtime version), so a second launch reuses the
//! cached module instead of recompiling. Lazy instantiation is *structural*
//! here, not a new type: [`PluginHost::compile`] loads/caches a [`Component`]
//! without instantiating it; the [`Store`] and instance are created only by an
//! explicit [`PluginHost::instantiate`] call. When the contribution model lands
//! (PH7.3+), that call is what a plugin's *first contribution invocation* will
//! trigger.
//!
//! **PH7.2 — capability & security model.** Each plugin now instantiates
//! under a [`CapabilityGrant`] computed from its [`PluginManifest`] and
//! [`TrustTier`] (fragment §6). The grant is *enforced*, not advisory: each
//! [`Store`]'s WASI view is built with exactly its granted filesystem preopens
//! plus a private per-plugin data dir, so a plugin without an `fs:write` grant
//! cannot reach a path outside its data dir at the WASI layer (WASI has no
//! ambient authority). `net:http` / `proc:spawn` ride the grant as metadata for
//! the capability-gated `host-services` seam (PH7.3+); they are deliberately
//! *not* wired into the raw WASI view (see [`capability`]). The host also
//! issues each plugin a monotonic [`PluginId`] and stamps
//! [`SourceLayer::Plugin`] provenance from its own ground truth — a plugin
//! cannot forge provenance (`lattice_grammar::source` has no public
//! `SourceLocation` setter). See [`manifest`] and [`capability`].
//!
//! The end-to-end "a guest attempts a write and WASI denies it" proof lands at
//! PH7.4 with the real `wasm32-wasip2` `fuzzy-finder` (the guest toolchain PH7.0
//! deferred to that slice); PH7.2 proves the model at the host layer — grant
//! computation, the grant→preopen mapping, provenance issuance — with the
//! WASI-layer OS enforcement itself resting on wasmtime's tested guarantee.
//!
//! Still owned by later slices: every contribution seam (PH7.3+). The first
//! consumer of the `plugin` lifecycle world is the user's `init.rs`; the no-op
//! component the tests instantiate is the degenerate `init.rs`.

pub mod boundary;
pub mod boundary_app_effect;
pub mod boundary_context;
pub mod boundary_decoration;
pub mod boundary_effect;
pub mod boundary_event;
pub mod boundary_grammar;
pub mod boundary_picker;
pub mod buffer;
pub mod capability;
pub mod completion_host;
pub mod completion_source;
pub mod completion_task;
pub mod config_host;
// TC.2 — the sticky-context producer seam. Trio mirroring `decoration_*`:
// the bindgen world, the actor bridge, the native-trait adapter.
pub mod context_host;
pub mod context_source;
pub mod context_task;
/// CR.4: the `dashboard` guest→host section seam.
pub mod dashboard_host;
pub mod media_host;
pub mod media_source;
pub use crate::media_source::WasmMediaSource;
pub mod decoration_host;
pub mod decoration_source;
pub mod decoration_task;
// OM.A1: the `agenda-source` guest→host producer seam.
pub mod agenda_host;
pub mod agenda_source;
pub mod agenda_task;
pub use crate::agenda_source::{WasmAgendaSource, normalise_extensions};
// XF.4: the fs:write gate for guest-returned effects, applied at the
// boundary where the plugin's provenance is still known.
pub mod effect_authorizer;
pub use crate::effect_authorizer::EffectAuthorizer;
pub mod error_parser_host;
pub mod event_task;
pub mod events_host;
pub mod grammar_host;
pub mod grammar_trampoline;
/// CR.3: the `help` guest→host topic-registration seam.
pub mod help_host;
pub mod media_task;
// LG.3c: the `language` guest→host registration seam.
pub mod agenda_cache;
pub mod host_services;
pub mod keymap_host;
pub mod language_host;
pub mod manifest;
pub mod mode_host;
pub mod picker_host;
pub mod picker_source;
pub mod picker_task;
pub mod plugin_manager_host;
// OR.1: the durable, plugin-scoped byte store the `host-services` `store-*`
// calls act on. One per manifest id, shared across every seam's `Store`.
pub mod plugin_store;
pub mod teardown;
// TR.2b — the `transient-source` guest→host keyed-menu seam. Trio mirroring
// `picker_*`: the bindgen world, the actor bridge, the registry-builder adapter.
pub mod transient_host;
pub mod transient_source;
pub mod transient_task;
// TC.4 — the `theme` element-registration seam. Guest imports `register-element`
// and the host inserts into the SAME registry builtins live in.
pub mod theme_host;
pub mod trace;
pub mod trampoline;
pub mod tree_resource;
// OR.2: the `host-services.watch` / `unwatch` seam — a debounced directory
// watch whose batches are addressed to the plugin that armed them.
pub mod ui_host;
pub mod wake;
mod watch_host;

pub use boundary::WitBoundary;
pub use capability::{
    CapabilityGrant, FsGrant, GrantOutcome, PreopenSpec, TrustTier, build_wasi_ctx, grant,
};
pub use completion_source::WasmCompletionSource;
pub use completion_task::{CompletionActor, CompletionClient};
pub use context_source::WasmContextSource;
pub use context_task::{ContextActor, ContextClient};
pub use decoration_source::WasmDecorationSource;
pub use decoration_task::{DecorationActor, DecorationClient};
pub use manifest::{Capability, CapabilityParseError, ManifestError, PluginManifest, PluginSeam};
pub use picker_source::WasmPickerSource;
pub use picker_task::{PickerActor, PickerClient};
pub use teardown::{PluginTeardown, TeardownRegistries, TeardownReport};
pub use trace::{
    Direction, HotGate, PluginTracePushed, PluginTraceRecord, PluginTracer, PluginTracerHandle,
    TraceLevel, TraceOutcome,
};
pub use transient_source::{project_transient_context, spec_from_wit, transient_builder};
pub use transient_task::{TransientActor, TransientClient};
/// OC.2: the injected timer the wake seam sleeps on. The lib owns no runtime, so
/// the caller supplies this (the loader's is backed by `tokio::time::sleep`).
pub use wake::{Sleeper, SleeperHandle};
/// The compiled component — the return type of [`PluginHost::compile`],
/// re-exported so callers (the plugin loader) can name it without a direct
/// `wasmtime` dependency.
pub use wasmtime::component::Component;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use lattice_runtime::EventBus;

use lattice_grammar::SourceLayer;
use wasmtime::component::{HasSelf, Linker};
use wasmtime::{Cache, CacheConfig, Config, Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

// Host bindings for the `plugin` lifecycle world, async per the canonical
// ABI. The generated `Plugin` type carries async `call_activate` /
// `call_deactivate` and an async `instantiate_async`.
wasmtime::component::bindgen!({
    world: "plugin",
    path: "../../wit",
    // Generate async `call_activate` / `call_deactivate` + `instantiate_async`
    // for every export. (wasmtime 46 replaced the old top-level `async: true`
    // with this per-function form; async is always available on the engine,
    // so `Config::async_support` is a no-op and intentionally not called.)
    exports: { default: async },
    // NB: the `document` resource's host trait + `with`-mapping to
    // `DocumentResource` land at PH7.3d, not here. bindgen only lets a `with`
    // entry bind a resource that a *world function signature* references, and
    // no signature takes a `document` until the picker-source `init(ctx)` seam
    // (PH7.3d/PH7.4). `use buffer.{buffer-snapshot}` (in `plugin.wit`) emits the
    // owned record mirror this slice projects into; the resource backing
    // (`buffer::DocumentResource`) is ready to be `with`-mapped then.
});

/// Fuel granted for the (trivial) instantiation step, before per-call budgets
/// take over. Generous so instantiation never trips the fuel trap; a real
/// plugin's `activate` work is bounded by [`PluginBudget::fuel`] instead.
const INSTANTIATION_FUEL: u64 = 1_000_000_000;

/// How often the epoch-ticker thread bumps the engine epoch. The epoch
/// deadline is expressed in ticks, so this is the wall-clock granularity of
/// the deadline (≈1ms).
const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(1);

/// Per-call resource budget. Both limits are hard: whichever is hit first
/// traps the call cleanly.
#[derive(Debug, Clone, Copy)]
pub struct PluginBudget {
    /// Fuel (≈ units of work) a single lifecycle call may consume before it
    /// traps with [`TrapKind::Fuel`].
    pub fuel: u64,
    /// Epoch ticks (≈ milliseconds, see [`EPOCH_TICK_INTERVAL`]) a single
    /// call may run before it traps with [`TrapKind::Epoch`].
    pub epoch_deadline: u64,
}

impl Default for PluginBudget {
    fn default() -> Self {
        // Generous defaults: a well-behaved `activate` finishes far inside
        // these. PH7.5 tightens them into CI-gated budgets.
        Self {
            fuel: 1_000_000_000,
            epoch_deadline: 1_000,
        }
    }
}

impl PluginBudget {
    /// The Reflex-class budget for the **synchronous** grammar trampoline
    /// (PH7.7c; plugin-host.md §7 + audit F1). Grammar `apply` / `parse_args`
    /// run on the keystroke path (the PH7.7 fork), so — unlike the generous
    /// lifecycle/async [`default`](Self::default) (~1s epoch) — a plugin
    /// contribution must not stall the keystroke.
    ///
    /// **Fuel is the primary Reflex bound.** A grammar guest runs on the sync
    /// linker with no async host import, so it cannot *block* (no I/O to await) —
    /// it can only compute or spin, and both are fuel-bounded. `10M` fuel is
    /// ~one display frame of compute (Cranelift-compiled), ample for a real
    /// motion's arithmetic but a hard cap on a runaway loop; that is what keeps a
    /// plugin motion off the keystroke's critical path.
    ///
    /// **Epoch is a jitter-proof wall-clock *backstop*, not the tripwire.** The
    /// ticker granularity is 1ms ([`EPOCH_TICK_INTERVAL`]); a 2-tick deadline
    /// false-positives when the OS deschedules the dispatch thread mid-call
    /// (observed under a criterion warmup's millions of iterations). So the epoch
    /// is set generously (`50` ticks ≈ 50ms) — it never trips on scheduling
    /// jitter, and only catches the pathological case fuel somehow misses (near-
    /// impossible for a sync compute guest). A trap of either kind is caught by
    /// the trampoline → the contribution is a no-op with a warn
    /// ([`CommandError::Plugin`](lattice_grammar::CommandError::Plugin)), never a
    /// hang. Armed before every guest call — distinct from the lifecycle/producer
    /// budget by design (audit F1).
    pub fn grammar() -> Self {
        Self {
            fuel: 10_000_000,
            epoch_deadline: 50,
        }
    }

    /// The budget for a plugin **event handler** (`on-event`, PH7.8c; §7 "major-
    /// mode event handler"). Unlike grammar's Reflex budget, event delivery is
    /// **off the keystroke path** (async, on the plugin's own actor task), and a
    /// handler runs on the *async* linker — so it may legitimately `await` a
    /// capability-gated `host-services` call. A tight sub-frame epoch would
    /// false-trip such a suspend, so the **epoch is a generous backstop** (~1s,
    /// the lifecycle default) and **fuel is the primary bound**: `100M` ≈ ~10
    /// frames of compute, ample for a real hook (recolour a gutter, index a
    /// symbol) yet a hard cap on a runaway loop. A trap is caught per-delivery by
    /// the [`EventActor`](crate::event_task::EventActor) → the delivery is
    /// skipped with a warn, the plugin stays subscribed, other subscribers are
    /// untouched (§8). The marshalling+dispatch overhead itself is the CI-gated
    /// `< 250µs p99` row (PH7.8d), distinct from this runaway guard.
    pub fn event() -> Self {
        Self {
            fuel: 100_000_000,
            epoch_deadline: 1_000,
        }
    }

    /// The budget for a plugin **decoration producer** (`gutter-decorations`,
    /// PH7.9). Like the event budget, decoration production is **off the render
    /// path** (async, on the plugin's actor task, triggered by an edit / scroll /
    /// diagnostic change) and the producer runs on the async linker (it may
    /// `await` a `host-services` call — a git-gutter source reading the repo). So
    /// the **epoch is a generous ~1 s backstop** and **fuel is the primary bound**
    /// (`100M` ≈ ~10 frames of compute — ample for a real diff/annotate pass, a
    /// hard cap on a runaway). A trap is caught per-call by the
    /// [`DecorationActor`](crate::decoration_task::DecorationActor) → the trigger
    /// yields no decorations and the cached snapshot keeps its prior value (§8, no
    /// flicker). The §7 `< 50 µs p99` "segment update" gate is on the marshalling
    /// + dispatch overhead, distinct from this runaway guard.
    pub fn decoration() -> Self {
        Self {
            fuel: 100_000_000,
            epoch_deadline: 1_000,
        }
    }

    /// TC.2: budget for a `context-scopes` produce call. Deliberately the same
    /// shape as [`Self::decoration`] and for the same reason — both are async
    /// producers the host drives off the render path, so the guard is against a
    /// runaway, not against latency. Context is the more expensive of the two
    /// (a whole-buffer tree-sitter query rather than a per-line walk), and the
    /// budget is generous enough that a legitimate query on a large file
    /// finishes well inside it; `context.max-file-lines` is what bounds the
    /// intended work, this is what bounds the unintended.
    pub fn context() -> Self {
        Self {
            fuel: 100_000_000,
            epoch_deadline: 1_000,
        }
    }
}

/// Why a lifecycle call trapped. Fuel/epoch are the *expected* runaway-guard
/// outcomes; `Other` is any genuine wasm trap (unreachable, OOB, a guest
/// panic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    /// The call exhausted its fuel budget (a compute runaway).
    Fuel,
    /// The call ran past its epoch (wall-clock) deadline.
    Epoch,
    /// CG.4: the user pressed `<C-g>` while the call was running.
    ///
    /// Deliberately NOT a crash. Fuel/epoch/other mean the plugin
    /// misbehaved and should be quarantined; this means the user changed
    /// their mind, and quarantining a plugin for that would punish it for
    /// being interruptible.
    Cancelled,
    /// Any other wasm trap (unreachable, out-of-bounds, guest panic).
    Other,
}

impl TrapKind {
    /// A short, stable machine label (`"fuel"` / `"epoch"` / `"trap"`) for the
    /// [`lattice_protocol::Event::PluginCrashed`] payload and structured logs. Distinct from the
    /// [`Display`](std::fmt::Display) impl's human sentence: subscribers match on
    /// this without parsing prose, and it never changes with copy edits.
    pub fn label(self) -> &'static str {
        match self {
            TrapKind::Fuel => "fuel",
            TrapKind::Epoch => "epoch",
            TrapKind::Cancelled => "cancelled",
            TrapKind::Other => "trap",
        }
    }
}

impl std::fmt::Display for TrapKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TrapKind::Fuel => "out of fuel",
            TrapKind::Epoch => "epoch deadline exceeded",
            TrapKind::Cancelled => "cancelled by the user",
            TrapKind::Other => "wasm trap",
        };
        f.write_str(s)
    }
}

/// Typed error surface for the plugin host. No host path panics — every
/// failure mode is a value here (the four-artefact graceful-error clause).
/// `anyhow::Error` is wasmtime's error type; each variant carries it as
/// `#[source]`.
#[derive(Debug, thiserror::Error)]
pub enum PluginHostError {
    /// The wasmtime engine could not be built from the host config.
    #[error("failed to build the wasmtime engine")]
    Engine(#[source] anyhow::Error),

    /// The epoch-ticker background thread could not be spawned (the OS refused a
    /// thread). Surfaced rather than panicked — no host-construction path panics
    /// (the "every failure mode is a value" invariant).
    #[error("failed to spawn the epoch-ticker thread")]
    EpochTicker(#[source] std::io::Error),

    /// The component linker could not be populated with the WASI host
    /// functions (PH7.2). A host-setup failure, surfaced rather than panicked.
    #[error("failed to add WASI to the plugin component linker")]
    Linker(#[source] anyhow::Error),

    /// The on-disk module cache could not be initialised (bad directory,
    /// I/O error). A caching failure must never fail the host — callers may
    /// fall back to an uncached host — so this is surfaced, never panicked.
    #[error("failed to initialise the plugin module cache")]
    Cache(#[source] anyhow::Error),

    /// Component bytes were malformed or failed AOT compilation.
    #[error("failed to compile the plugin component")]
    Compile(#[source] anyhow::Error),

    /// The component compiled but could not be instantiated (a missing
    /// import, or fuel exhausted during the start function).
    #[error("failed to instantiate the plugin component")]
    Instantiate(#[source] anyhow::Error),

    /// A grammar plugin declared a malformed contribution spec (PH7.7c): an
    /// `arg-spec` / `latency-class` / `surface-form` that could not cross the
    /// boundary. Fails registration loudly (the spec is structurally wrong),
    /// unlike a *runtime* `apply` failure, which degrades gracefully to a no-op.
    #[error("plugin grammar spec is malformed: {0}")]
    GrammarSpec(String),

    /// A lifecycle export trapped. Fuel/epoch exhaustion and genuine wasm
    /// traps all land here as a value — the call is a no-op with a typed
    /// error and the host stays live.
    #[error("plugin lifecycle export `{func}` trapped: {kind}")]
    Trap {
        /// The export that trapped (`"activate"` / `"deactivate"`).
        func: &'static str,
        /// What caused the trap (fuel / epoch / other).
        kind: TrapKind,
        /// The underlying wasmtime trap.
        #[source]
        source: anyhow::Error,
    },

    /// A guest call was routed to a per-plugin actor task
    /// ([`picker_task`]) whose channel is closed — the task has ended and
    /// dropped its `Store` (teardown, or a fatal error unwound the loop). The
    /// call is a no-op with a typed error; the *caller* stays live. This is the
    /// bridge's graceful surface for "the plugin is gone", distinct from
    /// [`Trap`](Self::Trap) (the plugin ran but its call failed).
    #[error("plugin actor for `{func}` is no longer running")]
    PluginGone {
        /// The export the caller was trying to reach (`"spec"` / `"init"` /
        /// `"accept"`).
        func: &'static str,
    },

    /// The plugin instance is quarantined (PH7.12): a prior lifecycle / callback
    /// call trapped, tripping crash-quarantine, so this call short-circuits
    /// *before* re-entering the dead `Store`. Distinct from [`Trap`](Self::Trap)
    /// (the plugin ran and its call failed) and [`PluginGone`](Self::PluginGone)
    /// (the actor's channel is closed): the actor is still alive, but the instance
    /// is dead-until-reinstantiation. The `PluginCrashed` event already fired on
    /// the first trap; this variant is the quiet no-op every subsequent call
    /// returns until a reload (PH7.12b) mints a fresh instance.
    #[error("plugin is quarantined; `{func}` short-circuited after an earlier crash")]
    Quarantined {
        /// The export the caller was trying to reach.
        func: &'static str,
    },

    /// A value crossing the picker boundary could not be converted between its
    /// WIT mirror and the native type — a malformed record the guest returned
    /// (e.g. a non-UTF-8 path, §4.4), or a native value with no WIT
    /// representation. Carries the boundary layer's message. Never lossy: a
    /// conversion that cannot be represented is this typed error, not a silent
    /// drop.
    #[error("picker boundary conversion failed: {0}")]
    Boundary(String),
}

/// Re-arm a `Store`'s fuel + epoch budget before a call, so each call gets a
/// fresh allowance rather than sharing a running total. Shared by the
/// [`LoadedPlugin`] lifecycle path and the [`picker_task`] actor loop.
pub(crate) fn arm_store(
    store: &mut Store<PluginState>,
    budget: PluginBudget,
) -> Result<(), PluginHostError> {
    store
        .set_fuel(budget.fuel)
        .map_err(|e| PluginHostError::Instantiate(e.into()))?;

    // CG.4: refresh the token this call may be cancelled by, ONCE, here.
    // The registry is behind a mutex; the token is a plain atomic. Taking
    // the lock per call and polling the atomic per tick is the split
    // `foreground_cancel`'s own docs prescribe.
    //
    // Snapshotting also fixes the semantics: a call is cancelled by the
    // operation armed when it STARTED. A later `arm()` (a different user
    // action) must not reach back and kill a call already in flight.
    let state = store.data_mut();
    state.epoch_spent = 0;
    state.cancel_token = state.cancel.as_ref().and_then(|c| c.current_token());

    // The deadline is re-armed ONE TICK AT A TIME so the callback runs
    // every ~1ms and can poll the token. That moves enforcement of
    // `budget.epoch_deadline` in here — wasmtime counts to 1 repeatedly,
    // we count the total. Same ceiling, checked by us.
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(move |mut ctx| {
        let state = ctx.data_mut();
        if let Some(token) = &state.cancel_token
            && token.is_cancelled()
        {
            // Traps the guest. `classify_trap` maps this back to
            // `TrapKind::Cancelled` so the caller can tell "the user
            // stopped it" from "the plugin ran away".
            return Err(wasmtime::Error::msg(CANCELLED_TRAP));
        }
        state.epoch_spent += 1;
        if state.epoch_spent >= budget.epoch_deadline {
            // The pre-CG.4 outcome, produced by us rather than by
            // wasmtime's countdown: the call outran its time budget.
            return Err(wasmtime::Error::msg(EPOCH_TRAP));
        }
        Ok(wasmtime::UpdateDeadline::Continue(1))
    });
    Ok(())
}

/// Marker in the trap message for a call stopped by `<C-g>` (CG.4).
///
/// A string rather than a typed error because it has to survive
/// `wasmtime::Error`'s round-trip through the guest trap — the same
/// reason `classify_trap` matches on `wasmtime::Trap` variants rather
/// than carrying host types across.
pub(crate) const CANCELLED_TRAP: &str = "lattice: plugin call cancelled by the user";

/// Marker for a call that outran `budget.epoch_deadline` (CG.4 moved
/// this enforcement out of wasmtime's countdown and into our callback).
pub(crate) const EPOCH_TRAP: &str = "lattice: plugin call exceeded its time budget";

/// Classify a wasmtime call error into a [`TrapKind`] for reporting.
pub(crate) fn classify_trap(err: &wasmtime::Error) -> TrapKind {
    // CG.4: the epoch callback raises these two, so they arrive as plain
    // errors rather than `wasmtime::Trap` variants — checked FIRST,
    // because a callback-raised error may also carry a Trap in its
    // chain and `Interrupt` would otherwise swallow a cancellation.
    // `{:#}` renders the WHOLE anyhow chain. `to_string()` shows only the
    // outermost context, and wasmtime wraps a callback-raised error in
    // trap context — so the marker is never in the top line.
    let text = format!("{err:#}");
    if text.contains(CANCELLED_TRAP) {
        return TrapKind::Cancelled;
    }
    if text.contains(EPOCH_TRAP) {
        return TrapKind::Epoch;
    }
    match err.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => TrapKind::Fuel,
        Some(wasmtime::Trap::Interrupt) => TrapKind::Epoch,
        _ => TrapKind::Other,
    }
}

/// Per-instance crash-quarantine, shared by every repeated-call plugin surface
/// (the four actors — event / picker / decoration / completion — and the
/// grammar apply path). A component trap taints its instance irrecoverably:
/// wasmtime offers no rollback, so a `Store` that trapped once will keep
/// failing. The first trap trips quarantine here — [`lattice_protocol::Event::PluginCrashed`]
/// fires exactly once on the bus, an `info!` records the death — and every
/// later call short-circuits (via [`is_tripped`](Self::is_tripped)) *before*
/// re-entering the dead `Store`. This turns today's "tainted instance keeps
/// re-failing, each logged" behaviour into a clean one-shot crash signal plus a
/// silent no-op. Reload (PH7.12b) mints a fresh instance with a fresh,
/// untripped `Quarantine`.
///
/// Isolation is the guarantee: tripping touches only this instance's flag and
/// publishes one event; the actor, bus, every other plugin, and the editor are
/// untouched.
pub(crate) struct Quarantine {
    plugin: PluginId,
    bus: Arc<EventBus>,
    tripped: bool,
}

impl Quarantine {
    pub(crate) fn new(plugin: PluginId, bus: Arc<EventBus>) -> Self {
        Self {
            plugin,
            bus,
            tripped: false,
        }
    }

    /// Whether a prior trap has quarantined this instance. Callers short-circuit
    /// when this is `true` rather than re-entering the dead `Store`.
    pub(crate) fn is_tripped(&self) -> bool {
        self.tripped
    }

    /// Trip quarantine on a trap. Idempotent: the first call publishes exactly
    /// one [`lattice_protocol::Event::PluginCrashed`] and logs; a second call (a race, or a caller
    /// that forgot to check [`is_tripped`](Self::is_tripped)) is a silent no-op —
    /// the instance is already known dead and the event has already fired.
    pub(crate) fn trip(&mut self, func: &'static str, kind: TrapKind) {
        // CG.4: `<C-g>` is not a crash. Fuel / epoch / other mean the
        // plugin misbehaved and the instance is dead; cancellation means
        // the USER changed their mind, and the instance is perfectly
        // healthy. Quarantining here would punish a plugin for being
        // interruptible — press `<C-g>` on a slow picker and the plugin
        // would be dead until reload.
        if kind == TrapKind::Cancelled {
            tracing::debug!(
                plugin = self.plugin.0,
                func,
                "plugin call cancelled by the user; instance stays live"
            );
            return;
        }
        if self.tripped {
            return;
        }
        self.tripped = true;
        // info!, not warn!: a plugin dying is a one-shot, user-actionable event
        // (the notification / plugin-manager surface acts on it), not a
        // per-keystroke diagnostic. Later short-circuits are silent.
        tracing::info!(
            plugin = self.plugin.0,
            func,
            kind = kind.label(),
            "plugin quarantined after trap"
        );
        self.bus.publish(lattice_protocol::Event::PluginCrashed {
            plugin: self.plugin.0,
            func: func.to_string(),
            kind: kind.label().to_string(),
        });
    }
}

/// Map a raw wasmtime call result for a repeated-call actor export: on a trap,
/// trip `quarantine` (fires `PluginCrashed` once) and return a typed [`Trap`];
/// otherwise pass the value through. The paired short-circuit —
/// `if quarantine.is_tripped() { return Err(Quarantined { func }) }` at the top
/// of each export — keeps a dead instance from ever re-entering its `Store`.
/// Shared by the picker / decoration / completion actors (the event actor's
/// `deliver` is `()`-returning, so it trips inline).
///
/// [`Trap`]: PluginHostError::Trap
pub(crate) fn trip_and_map<T>(
    quarantine: &mut Quarantine,
    func: &'static str,
    result: wasmtime::Result<T>,
) -> Result<T, PluginHostError> {
    match result {
        Ok(v) => Ok(v),
        Err(source) => {
            let kind = classify_trap(&source);
            quarantine.trip(func, kind);
            Err(PluginHostError::Trap {
                func,
                kind,
                source: source.into(),
            })
        }
    }
}

/// PO.2 — the traced [`trip_and_map`]: map the guest result AND emit a boundary
/// [`PluginTraceRecord`](crate::trace::PluginTraceRecord) into `tracer` (when a
/// seam actor was wired with one). A successful call records at `Debug` — dropped
/// by the default `Info` gate, so there is no per-call noise unless the plugin is
/// raised to `debug`/`trace`; a trap records at `Error`, always kept. The seams
/// are async (off the actor thread), and emission is a cheap gated push, so this
/// stays off the editor hot path (design §4). `fuel_delta` is `0` for now — wall
/// time is the primary signal; fuel accounting is a later refinement.
pub(crate) fn trip_and_map_traced<T>(
    tracer: Option<&crate::trace::PluginTracerHandle>,
    plugin: u32,
    seam: PluginSeam,
    quarantine: &mut Quarantine,
    func: &'static str,
    start: std::time::Instant,
    result: wasmtime::Result<T>,
) -> Result<T, PluginHostError> {
    let micros = start.elapsed().as_micros() as u64;
    let mapped = trip_and_map(quarantine, func, result);
    if let Some(tracer) = tracer {
        use crate::trace::{Direction, PluginTraceRecord, TraceLevel, TraceOutcome};
        let (level, outcome) = match &mapped {
            Ok(_) => (
                TraceLevel::Debug,
                TraceOutcome::Ok {
                    micros,
                    fuel_delta: 0,
                },
            ),
            Err(PluginHostError::Trap { kind, .. }) => (
                TraceLevel::Error,
                TraceOutcome::Trap {
                    kind: kind.label().to_string(),
                    func: func.to_string(),
                },
            ),
            // A non-trap host error (e.g. a linker/encode failure) — surface it
            // at Warn without a trap classification.
            Err(_) => (
                TraceLevel::Warn,
                TraceOutcome::Ok {
                    micros,
                    fuel_delta: 0,
                },
            ),
        };
        tracer.trace(PluginTraceRecord {
            plugin,
            seam,
            direction: Direction::GuestExport,
            call: std::borrow::Cow::Borrowed(func),
            level,
            outcome,
            detail: None,
        });
    }
    mapped
}

#[cfg(test)]
mod trip_and_map_traced_tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;

    use super::*;
    use crate::trace::{PluginTracer, PluginTracerHandle, TraceLevel, TraceOutcome};

    fn tracer() -> PluginTracerHandle {
        // Trace-level default so per-call `Debug` records are kept in the test.
        Arc::new(PluginTracer::new(TraceLevel::Trace, 16))
    }

    fn quarantine() -> Quarantine {
        Quarantine::new(PluginId(7), Arc::new(lattice_runtime::EventBus::new()))
    }

    #[test]
    fn a_successful_call_records_a_debug_ok() {
        let t = tracer();
        let mut q = quarantine();
        let out: Result<u32, _> = trip_and_map_traced(
            Some(&t),
            7,
            PluginSeam::Grammar,
            &mut q,
            "apply-motion",
            std::time::Instant::now(),
            Ok(42u32),
        );
        assert_eq!(out.unwrap(), 42);
        let recs = t.snapshot_plugin(7);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].call, "apply-motion");
        assert_eq!(recs[0].level, TraceLevel::Debug);
        assert!(matches!(recs[0].outcome, TraceOutcome::Ok { .. }));
        assert!(matches!(
            recs[0].direction,
            crate::trace::Direction::GuestExport
        ));
    }

    #[test]
    fn a_trap_records_an_error_trap_and_trips_quarantine() {
        let t = tracer();
        let mut q = quarantine();
        let out: Result<u32, _> = trip_and_map_traced(
            Some(&t),
            7,
            PluginSeam::PickerSource,
            &mut q,
            "init",
            std::time::Instant::now(),
            Err(wasmtime::Error::msg("boom")),
        );
        assert!(out.is_err());
        assert!(q.is_tripped(), "a trap trips the quarantine");
        let recs = t.snapshot_plugin(7);
        assert_eq!(recs[0].level, TraceLevel::Error);
        assert!(
            matches!(&recs[0].outcome, TraceOutcome::Trap { func, kind } if func == "init" && kind == "trap"),
            "a generic error classifies as a `trap`-kind Trap outcome"
        );
    }

    #[test]
    fn no_tracer_is_a_no_op_but_still_maps() {
        let mut q = quarantine();
        // Below-the-gate + no tracer: the mapping still happens, nothing recorded.
        let out: Result<u32, _> = trip_and_map_traced(
            None,
            7,
            PluginSeam::Grammar,
            &mut q,
            "apply-motion",
            std::time::Instant::now(),
            Ok(1u32),
        );
        assert_eq!(out.unwrap(), 1);
    }
}

/// A background thread that bumps the engine epoch on a fixed interval, so
/// per-store epoch deadlines actually fire. Stopped and joined on drop.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn spawn(engine: &Engine, interval: Duration) -> Result<Self, PluginHostError> {
        let stop = Arc::new(AtomicBool::new(false));
        let engine = engine.clone(); // `Engine` is a cheap Arc-backed handle.
        let stop_flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("lattice-plugin-epoch".into())
            .spawn(move || {
                while !stop_flag.load(Ordering::Relaxed) {
                    std::thread::sleep(interval);
                    engine.increment_epoch();
                }
            })
            .map_err(PluginHostError::EpochTicker)?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            // The ticker only sleeps + bumps an epoch; the join is prompt.
            let _ = handle.join();
        }
    }
}

/// Per-`Store` host state. PH7.2 gives it the plugin's WASI view (built from
/// its [`CapabilityGrant`], §6) and the resource table WASI resources live in.
/// A plugin's `Store` reaches exactly the filesystem its grant preopened; the
/// [`WasiView`] impl is what wires the WASI host functions to this state.
///
/// Later slices grow this with the plugin's document-handle / callback
/// resource tables (PH7.3) — they share this same `ResourceTable`.
struct PluginState {
    /// The scoped WASI context (granted filesystem preopens + nothing else).
    wasi: WasiCtx,
    /// The resource table WASI (and, at PH7.3d, the `document` handle)
    /// resources live in.
    table: ResourceTable,
    /// The plugin's effective capability grant (PH7.2). Carried in the `Store`
    /// state so guest→host `host-services` calls (PH7.4b) can enforce it: those
    /// run host-side with full host authority, so the grant — not the WASI
    /// sandbox — is what bounds them.
    grant: CapabilityGrant,
    /// Grammar contributions the guest declares through the `grammar` register
    /// API during `register-grammar` (PH7.7b). The `grammar::Host` impl below
    /// records into it; the host drains it after the registration export returns
    /// and builds native `*Spec`s with trampoline `apply`s (PH7.7c). Empty for a
    /// plugin that registers no grammar (picker/completion plugins, the scaffold).
    grammar_contributions: grammar_host::GrammarContributions,
    /// Event subscriptions the guest declares through the `events` `subscribe`
    /// API during `register-events` (PH7.8b). The `events::Host` impl below
    /// records into it; the host drains it after the registration export returns
    /// and wires each subscription to the native `EventBus` (PH7.8c). Empty for a
    /// plugin that observes no events.
    event_subscriptions: events_host::EventContributions,
    /// The publish-side handle for the `register-event` / `emit-event`
    /// host-services (PH7.8b.2) — the plugin's identity (for the `plugin:<id>`
    /// provenance) plus the bus it emits onto. `Some` on the two paths that are
    /// handed a bus: [`PluginHost::spawn_event_plugin`] (PH7.8b.2) and
    /// [`PluginHost::instantiate_grammar_plugin`] (OC.1 — a grammar action
    /// bridging a chord to the plugin's own async side). `None` elsewhere, in
    /// which case an `emit-event` is a warn + drop (the honest "no bus wired
    /// here" degradation).
    event_emit: Option<EventEmitCtx>,
    /// The `ConfigRegistry` a config plugin registers options into / reads via the
    /// `config` seam (PH7.10). `Some` only for a plugin spawned onto a registry
    /// ([`PluginHost::spawn_config_plugin`]); `None` otherwise, in which case a
    /// `register-option` returns `false` and `get-option` returns `none` (the
    /// honest "no registry wired" degradation — the host isn't boot-wired yet).
    config_registry: Option<Arc<lattice_config::ConfigRegistry>>,
    /// TC.4: the theme registry a `theme` plugin's `register-element` inserts
    /// into. Wired before `register-theme-elements` runs; `None` for every
    /// other world (the call then logs and registers nothing).
    theme_registry: Option<lattice_theme::ThemeRegistryHandle>,
    /// TC.4: namespaced element names this plugin registered — the teardown
    /// tokens, mirroring `config_contributions`.
    theme_contributions: Vec<String>,
    /// CR.4: section specs this plugin declared through
    /// `dashboard.register-section`, drained by `spawn_dashboard_sections`
    /// after the export returns. The host then instantiates one live guest
    /// per spec — a section is a function, not data.
    dashboard_contributions: Vec<dashboard_host::DashboardSectionSpec>,
    /// CR.3: topics this plugin declared through `help.register-topic`,
    /// drained by `spawn_help_plugin` after the export returns. Plain data —
    /// the loader turns them into `HelpTopic`s, so this crate never depends
    /// on `lattice-help`.
    help_contributions: Vec<help_host::HelpTopicSpec>,
    /// LG.3c: languages the guest declared, drained by the loader.
    language_contributions: Vec<language_host::LanguageSpec>,
    /// The plugin's manifest id (e.g. `"auto-pair"`). Set by every spawn/
    /// instantiate path from the manifest. Used to **auto-namespace** the
    /// plugin's config options — a `register-option("style")` registers
    /// `auto-pair.style`, and `get`/`set-option` resolve the plugin's own
    /// namespace first (falling back to the raw name so core options stay
    /// readable). `None` in the minimal test constructor (no prefixing then).
    plugin_name: Option<String>,
    /// Names of options this plugin registered via `register-option` (PH7.10),
    /// recorded so the host can report them after `register-options` returns (and
    /// as the teardown seam PH7.12 will unregister). Empty for a non-config plugin.
    config_contributions: Vec<String>,
    /// Mode declarations the guest makes through `register-mode` during
    /// `register-modes` (PH7.11a). The `modes::Host` impl records into it; the
    /// host drains it after the registration export returns and registers each
    /// into the `ModeRegistry` (`spawn_mode_plugin`). Empty for a non-mode plugin.
    mode_contributions: mode_host::ModeContributions,
    /// The keymap handle + command-registry snapshot a keymap plugin binds user
    /// keybindings against via the `keymap` seam (PL8.D.1). `Some` only for a
    /// plugin spawned via [`PluginHost::spawn_keymap_plugin`]; `None` otherwise,
    /// in which case a `register-binding` returns `false` (the honest "no keymap
    /// wired" degradation).
    keymap_ctx: Option<keymap_host::KeymapBindCtx>,
    /// The user keybindings this plugin bound via `register-binding` (PL8.D.1),
    /// recorded as teardown tokens so the loader unbinds the `KeymapLayer::User`
    /// entries on unload (PL8.D.2). Empty for a non-keymap plugin.
    keymap_contributions: Vec<keymap_host::KeymapBindingToken>,
    /// PO.5: the guest `logging` seam's routing target — the plugin id + tracer.
    /// `Some` once an async instantiate/spawn path stamps it ([`LogCtx`]); `None`
    /// for the sync grammar guest and any pre-tracer path (a `log` debug-drops).
    log_ctx: Option<LogCtx>,
    /// PR.6: what the guest `project` seam resolves through. `None` in a
    /// harness that wired no resolver; the seam then answers `none`.
    project: Option<ProjectCtx>,
    /// CG.4: the foreground-cancel registry, stamped once per store.
    /// `arm_store` takes the lock once per guest call to refresh
    /// [`Self::cancel_token`]; the epoch callback never touches it.
    cancel: Option<lattice_mode::ForegroundCancelHandle>,
    /// CG.4: the token the epoch callback polls — a plain atomic, read
    /// once per millisecond inside a running guest call. Refreshed by
    /// `arm_store`, so each call is cancelled by the operation that was
    /// armed when it STARTED, not by one armed after it finished.
    cancel_token: Option<lattice_protocol::CancellationToken>,
    /// CG.4: epoch ticks this call has consumed.
    ///
    /// The time budget moved in here when the deadline callback landed:
    /// the deadline is now re-armed one tick at a time so the token can
    /// be polled, so `budget.epoch_deadline` is enforced by counting
    /// rather than by wasmtime's own countdown. Reset per call.
    epoch_spent: u64,
    /// OC.3 / ML.6: what the `ui` seam's modeline calls act on — the element
    /// registry plus the bus content updates publish onto. `Some` on the async
    /// spawn paths that are handed a modeline; `None` on the sync grammar store.
    ///
    /// That `None` is the guarantee, not an oversight. The plan for OC.3 said
    /// the seam would be "wired on the async linker only", which does not
    /// survive the Component Model — a plugin's import set is fixed for the
    /// whole component and the same artefact instantiates against the grammar
    /// linker too, so an import missing there fails the WHOLE plugin (TC.6 /
    /// CR.3 / LG.3c / OM.11, and org has been broken exactly this way once).
    /// So `ui` is linked on both and the modeline is kept off the keystroke path
    /// here instead, where it is testable.
    ui: Option<ui_host::UiCtx>,
    /// OC.2: the plugin's armed periodic wakes. `Some` only on a store whose
    /// actor can fire them — [`PluginHost::spawn_event_plugin`], and only when a
    /// [`SleeperHandle`] was installed. `None` everywhere else, in which case
    /// `wake-every` answers `0` and logs; a guest that treats a `0` as armed
    /// simply never hears back, which is a visible nothing rather than a
    /// plausible wrong answer.
    ///
    /// Notably `None` on the sync grammar store: `events` is not on the grammar
    /// linker, so the seam is unreachable from the keystroke path structurally,
    /// and this field is the second, redundant answer to the same question.
    wake: Option<wake::WakeCtx>,
    /// OR.1: the plugin's durable byte store, shared with every other seam
    /// instance of the SAME manifest id. `None` for a store built without a
    /// manifest name (the degenerate `instantiate` path and internal harnesses)
    /// — there is no id to scope a store to, so `store-get` answers `none` and
    /// `store-put` says why.
    ///
    /// Stamped for every store here rather than per-spawn-path, on the
    /// `project` / `ui` reasoning: the handle needs no plugin id allocation and
    /// nothing to wait for, so no path can forget it. Forgetting it is exactly
    /// the failure this seam exists to prevent — a picker seam holding a store
    /// the grammar seam cannot see is the drift, not a degradation of it.
    store: Option<plugin_store::PluginStoreHandle>,
    /// OR.2: the directory watches this guest armed, keyed by the path it named.
    ///
    /// **On the `Store`, not in a host-side registry**, and that is the whole
    /// teardown story: a watch's lifetime is exactly this plugin instance's, so
    /// dropping the `Store` — on unload, on quarantine, on the event actor's
    /// channel closing — stops every watch with no bookkeeping anyone can
    /// forget to write. `unwatch` is then a `remove` on this map.
    watches: std::collections::HashMap<PathBuf, watch_host::Watch>,
    /// PM.7: plugins this guest declared via `plugin-manager.require` during
    /// `register-plugins`. Recorded here, drained by
    /// [`PluginHost::spawn_plugin_manager_plugin`] after the export returns —
    /// the host resolves/builds/loads them off-thread, never inside the guest
    /// call. Empty for every world that does not import `plugin-manager`.
    require_contributions: plugin_manager_host::RequireContributions,
}

/// The bus-publish handle a plugin needs to emit custom events (PH7.8b.2). Set
/// on [`PluginState`] by [`PluginHost::spawn_event_plugin`] once the plugin's
/// identity is allocated. The subscribe side already threads the concrete
/// [`EventBus`] into the host ([`event_task`](crate::event_task)), so the emit
/// side stores it directly rather than behind a closure indirection.
struct EventEmitCtx {
    /// The host-issued identity — the `plugin:<id>` provenance stamped on every
    /// event this plugin declares via `register-event`.
    plugin_id: PluginId,
    /// The bus `emit-event` publishes `Event::Plugin` onto.
    bus: Arc<EventBus>,
}

/// PO.5: what the guest `logging` seam needs to route a `log` call — the plugin's
/// host-issued id (keys the trace record) + the shared tracer. Set on
/// [`PluginState`] by each async instantiate/spawn path
/// ([`PluginHost::log_ctx_for`]); the `EventEmitCtx` analog for Layer 2.
struct LogCtx {
    /// The host-issued id stamped on every `PluginTraceRecord` from this plugin.
    plugin: u32,
    /// The sink the guest's log lines land in (the same tracer as the boundary
    /// trace, so the two interleave in `*plugin-trace*`).
    tracer: crate::trace::PluginTracerHandle,
}

impl WasiView for PluginState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Host impl of the `host-services` guest→host seam (PH7.4b, §5). The generated
/// `Host::walk` returns the WIT `result<list<string>, string>` directly as
/// `Result<Vec<String>, String>` — a sync host func that cannot trap, so bindgen
/// omits the outer `wasmtime::Result`. Walk logic + the capability gate live in
/// [`host_services::walk_within_grant`]; the impl just forwards with the Store's
/// grant.
impl crate::lattice::plugin_host::host_services::Host for PluginState {
    fn walk(&mut self, root: String) -> Result<Vec<String>, String> {
        host_services::walk_within_grant(&self.grant, &root)
    }

    /// OC.5a `read-file`: the read that works on every seam, including the
    /// synchronous grammar dispatch path where the guest's own WASI view
    /// panics rather than reads. Gate + logic in
    /// [`host_services::read_within_grant`].
    fn read_file(&mut self, path: String) -> Result<String, String> {
        host_services::read_within_grant(&self.grant, &path)
    }

    /// `register-event` (PH7.8b.2): declare a plugin-defined event into the
    /// runtime registry under this plugin's `plugin:<id>` provenance. Returns
    /// `false` on a built-in-shadow (the registry refuses it) OR when no emit
    /// context is wired (a plugin not spawned onto a bus — degrade gracefully to
    /// "not registered" rather than panic; the host isn't boot-wired yet).
    fn register_event(&mut self, name: String, doc: String) -> bool {
        match &self.event_emit {
            Some(ctx) => host_services::register_plugin_event(ctx.plugin_id, &name, &doc),
            None => {
                tracing::warn!(
                    event = %name,
                    "register-event ignored: plugin has no event bus wired"
                );
                false
            }
        }
    }

    /// `emit-event` (PH7.8b.2): publish a plugin-defined event on the bus. A
    /// plugin with no emit context wired (not spawned onto a bus) degrades to a
    /// warn + drop — never a panic (the four-artefact graceful-failure clause).
    fn emit_event(&mut self, name: String, payload: Vec<u8>) {
        match &self.event_emit {
            Some(ctx) => host_services::emit_plugin_event(&ctx.bus, name, payload),
            None => {
                tracing::warn!(
                    event = %name,
                    "emit-event dropped: plugin has no event bus wired"
                );
            }
        }
    }

    /// OC.4 `local-utc-offset-seconds`: the host's current offset from UTC.
    /// Ungated, and resolved per call so DST is correct — see
    /// [`host_services::local_utc_offset_seconds`].
    fn local_utc_offset_seconds(&mut self) -> i32 {
        host_services::local_utc_offset_seconds()
    }

    /// OR.3 `new-uuid`: a fresh uppercase v4 id, mintable from every seam —
    /// including the synchronous grammar linker, which is the only reason it is
    /// host-side. See [`host_services::new_uuid`].
    fn new_uuid(&mut self) -> Result<String, String> {
        host_services::new_uuid()
    }

    /// OR.2 `watch`. Gated on the same `fs:read` grant `walk` and `read-file`
    /// check — a watch reveals filesystem activity, so it is the same
    /// authorization question, answered by the same function.
    ///
    /// Requires an event bus on this seam, because a watch with nowhere to
    /// deliver is not a degraded watch, it is a thread that runs forever and
    /// tells nobody. The refusal names that, so a plugin author arming one from
    /// the grammar seam learns why it is silent rather than assuming the
    /// watcher is broken.
    fn watch(&mut self, path: String) -> Result<(), String> {
        let Some(ctx) = &self.event_emit else {
            tracing::warn!(
                path = %path,
                "watch refused: plugin has no event bus wired on this seam"
            );
            return Err(format!(
                "fs watch denied: '{path}' — this seam has no event bus, so a watch could \
                 never be delivered (arm it from the plugin's events seam)"
            ));
        };
        let key = PathBuf::from(&path);
        if self.watches.contains_key(&key) {
            // Idempotent: re-arming would leave the first watch orphaned and
            // double every batch.
            return Ok(());
        }
        let watch =
            watch_host::watch_within_grant(&self.grant, ctx.bus.clone(), ctx.plugin_id.0, &path)?;
        self.watches.insert(key, watch);
        Ok(())
    }

    /// OR.2 `unwatch`. Dropping the entry stops the OS watch and ends its
    /// coalescing thread. Unwatching what is not watched is `ok` — a disarm is
    /// idempotent, because the alternative is a guest that must track host
    /// state to avoid an error.
    fn unwatch(&mut self, path: String) -> Result<(), String> {
        self.watches.remove(&PathBuf::from(&path));
        Ok(())
    }

    /// OR.1 `store-put`. Gated on `state:write`; `err` names which of the three
    /// refusals happened, because "the put failed" sends a plugin author
    /// nowhere.
    fn store_put(&mut self, key: String, value: Vec<u8>) -> Result<(), String> {
        match self.writable_store()? {
            Some(mut store) => store.put(&key, value),
            None => Err(
                "store put failed: this plugin has no store (loaded without a manifest id)".into(),
            ),
        }
    }

    /// OR.1 `store-get`. `none` for every absent case — nothing stored, no
    /// grant, no store — because a reader for whom absence is ordinary cannot
    /// act differently on any of them.
    fn store_get(&mut self, key: String) -> Option<Vec<u8>> {
        self.readable_store()?.get(&key)
    }

    /// OR.1 `store-delete`. Deleting what is not there is `ok`.
    fn store_delete(&mut self, key: String) -> Result<(), String> {
        match self.writable_store()? {
            Some(mut store) => store.delete(&key),
            None => Err(
                "store delete failed: this plugin has no store (loaded without a manifest id)"
                    .into(),
            ),
        }
    }

    /// OR.1 `store-keys`. Empty for an ungranted or storeless plugin.
    fn store_keys(&mut self, prefix: String) -> Vec<String> {
        self.readable_store()
            .map(|s| s.keys(&prefix))
            .unwrap_or_default()
    }

    /// OR.1 `store-generation`. `0` for an ungranted or storeless plugin — a
    /// number that never moves, so a reader polling it simply never rebuilds
    /// rather than rebuilding forever.
    fn store_generation(&mut self) -> u64 {
        self.readable_store().map(|s| s.generation()).unwrap_or(0)
    }
}

impl PluginState {
    /// The store for a READ (`store-get` / `store-keys` / `store-generation`),
    /// or `None` when this plugin has none or was not granted `state:write`.
    ///
    /// Reads are gated on the same grant as writes. A plugin that may not
    /// persist has nothing of its own to read back, and letting it read would
    /// only expose what an earlier, granted version of itself had left there.
    fn readable_store(&self) -> Option<std::sync::MutexGuard<'_, plugin_store::PluginStore>> {
        if !self.grant.state_write {
            return None;
        }
        // Poisoning is recovered rather than propagated: a panic in another
        // thread's guest call must not turn every later store read into a
        // panic on the keystroke path. The bytes are unaffected — the map is
        // only ever mutated through methods that cannot leave it half-updated.
        Some(
            self.store
                .as_ref()?
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// The store for a WRITE. `Err` when the grant is missing (named, so the
    /// author fixes their manifest); `Ok(None)` when there is no store at all.
    #[allow(clippy::type_complexity)]
    fn writable_store(
        &self,
    ) -> Result<Option<std::sync::MutexGuard<'_, plugin_store::PluginStore>>, String> {
        if !self.grant.state_write {
            // info!: user-actionable (a plugin was denied persistence), the
            // level `walk` / `read-file` denials use.
            tracing::info!("host-services store denied: the plugin has no `state:write` grant");
            return Err(
                "store denied: this plugin did not request the `state:write` capability".into(),
            );
        }
        Ok(self
            .store
            .as_ref()
            .map(|s| s.lock().unwrap_or_else(|poisoned| poisoned.into_inner())))
    }
}

/// Host impl of the `ui` guest→host contribution seam (OC.3 / ML.6, §5). Three
/// sync, non-trapping functions — the `host-services` shape — over a plugin's
/// own modeline element. Every id is namespaced with the plugin's name before it
/// reaches the registry, so one plugin cannot address another's element or a
/// built-in. Logic + the namespacing live in [`ui_host`] so they are testable
/// without a `Store`; this impl is the wiring and the absent-context arm.
impl crate::lattice::plugin_host::ui::Host for PluginState {
    fn register_segment(
        &mut self,
        id: String,
        zone: crate::lattice::plugin_host::types::UiZone,
        priority: i32,
    ) -> bool {
        let id = ui_host::namespaced_id(self.plugin_name.as_deref(), &id);
        let Some(ctx) = self.ui.as_ref() else {
            tracing::warn!(
                element = %id,
                "register-segment ignored: no modeline wired on this seam"
            );
            return false;
        };
        match ui_host::register_segment(ctx, id.clone(), zone, priority) {
            // No teardown token is recorded here: unload reverses this plugin's
            // elements by NAMESPACE (`PluginTeardown::modeline_namespace`), so a
            // segment registered later in the plugin's life is covered too.
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(element = %id, %error, "register-segment refused");
                false
            }
        }
    }

    fn emit_segment(&mut self, id: String, text: String) {
        let id = ui_host::namespaced_id(self.plugin_name.as_deref(), &id);
        match self.ui.as_ref() {
            Some(ctx) => ui_host::emit_segment(ctx, id, text),
            None => {
                tracing::warn!(
                    element = %id,
                    "emit-segment dropped: no modeline wired on this seam"
                );
            }
        }
    }

    fn clear_segment(&mut self, id: String) {
        let id = ui_host::namespaced_id(self.plugin_name.as_deref(), &id);
        if let Some(ctx) = self.ui.as_ref() {
            ui_host::clear_segment(ctx, id);
        }
    }
}

/// Host impl of the `plugin-manager` guest→host seam (PM.7).
///
/// `require` **records and returns**. It performs no resolution, no clone, no
/// build and no load — see the module docs for why that split is load-bearing
/// rather than merely tidy.
///
/// A rejected spec returns `false` instead of trapping: one bad entry in a
/// user's `init.rs` must not take the whole config down, which is the same
/// graceful-degradation clause every other seam here follows.
impl crate::plugin_manager_host::bindings::lattice::plugin_host::plugin_manager::Host
    for PluginState
{
    fn require(
        &mut self,
        spec: crate::plugin_manager_host::bindings::lattice::plugin_host::plugin_manager::PluginSpec,
    ) -> bool {
        use crate::plugin_manager_host::bindings::lattice::plugin_host::plugin_manager::PluginSource as WitSource;
        if !plugin_manager_host::is_safe_plugin_name(&spec.name) {
            tracing::warn!(
                name = %spec.name,
                "require ignored: plugin name is not a single safe path component"
            );
            return false;
        }
        let source = match spec.source {
            WitSource::Local(path) => plugin_manager_host::RequiredSource::Local(path),
            WitSource::Git(g) => plugin_manager_host::RequiredSource::Git {
                url: g.url,
                rev: g.rev,
            },
            WitSource::Prebuilt(url) => plugin_manager_host::RequiredSource::Prebuilt { url },
        };
        tracing::debug!(name = %spec.name, "require recorded");
        self.require_contributions
            .record(plugin_manager_host::RequiredPlugin {
                name: spec.name,
                source,
                enable_mode: spec.enable_mode,
                pinned: spec.pinned,
            });
        true
    }
}

/// PR.6: what the guest `project` seam needs to answer.
///
/// Both halves are required: the resolver turns a path into a project, and
/// the buffer store turns a `buffer` id into that path. Bundled so
/// `PluginState` carries one `Option` rather than two that could disagree
/// about whether the seam is wired.
#[derive(Clone)]
pub(crate) struct ProjectCtx {
    pub(crate) resolver: lattice_core::ProjectResolverHandle,
    pub(crate) buffers: lattice_mode::BufferStoreHandle,
}

/// PR.6: host impl of the `project` guest→host seam.
///
/// Design: `docs/dev/architecture/project-resolution.md` §6. The host answers,
/// the guest asks — core never depends on a plugin being alive.
///
/// Both funcs return `option`, unlike the total native
/// `ProjectResolver::for_path`, and the two `none`s mean different things:
/// `root-for-buffer` returns it for an id the host does not know (untrusted
/// input from the guest), `root-for-path` only when no resolver is wired at all
/// (a stripped harness). A buffer that *exists* always resolves — one with no
/// path on disk reports the working directory with `kind = pwd`.
///
/// Sync host funcs. Resolution can walk the filesystem on a cache miss, but it
/// runs on the plugin's own store and task — never the UI or actor thread.
impl crate::lattice::plugin_host::project::Host for PluginState {
    fn root_for_buffer(
        &mut self,
        buffer: u64,
    ) -> Option<crate::lattice::plugin_host::project::ProjectInfo> {
        let ctx = self.project.as_ref()?;
        // A guest-supplied id is untrusted: `path_for` returning `None` is
        // ambiguous between "no such buffer" and "buffer has no path", so ask
        // the store whether it knows the buffer at all first. `name_for`
        // answers for every registered buffer, pathless ones included.
        let id = lattice_core::BufferId(buffer as u32);
        ctx.buffers.name_for(id)?;
        let path = ctx.buffers.path_for(id);
        Some(to_wire(match path {
            Some(path) => ctx.resolver.for_path(&path),
            // Pathless (scratch, terminal): an empty relative path resolves
            // against the resolver's own pwd, so the answer stays consistent
            // with `:cd` rather than reading the process cwd.
            None => ctx.resolver.for_path(std::path::Path::new("")),
        }))
    }

    fn root_for_path(
        &mut self,
        path: String,
    ) -> Option<crate::lattice::plugin_host::project::ProjectInfo> {
        let ctx = self.project.as_ref()?;
        Some(to_wire(ctx.resolver.for_path(std::path::Path::new(&path))))
    }
}

/// Native [`lattice_core::Project`] → the wire record.
fn to_wire(p: lattice_core::Project) -> crate::lattice::plugin_host::project::ProjectInfo {
    use crate::lattice::plugin_host::project as wit;
    let (kind, marker) = match &p.kind {
        lattice_core::ProjectKind::Marker(m) => (wit::ProjectKind::Marker, m.clone()),
        // Empty rather than a sentinel word: a guest checking `kind` has the
        // answer already, and a guest rendering `marker` should show nothing
        // rather than the string "pwd".
        lattice_core::ProjectKind::Pwd => (wit::ProjectKind::Pwd, String::new()),
    };
    wit::ProjectInfo {
        root: p.root.display().to_string(),
        kind,
        marker,
    }
}

/// Host impl of the `logging` guest→host seam (PO.5, Layer 2). Routes each guest
/// `log(level, context, message)` into the plugin's tracer as a
/// [`Direction::HostImport`](crate::trace::Direction::HostImport) record with
/// `seam = logging`, so the guest's own narrative interleaves with the boundary
/// trace in `*plugin-trace*`. The `context` becomes the record's `call` (the
/// guest's category) and the `message` its `detail`; `tracer.trace` gates it by
/// the plugin's level, exactly like a boundary record. A plugin with no `log_ctx`
/// wired (the sync grammar guest, or a pre-tracer path) degrades to a debug-drop —
/// never a panic (the graceful-failure clause). Sync host func (only a ring push),
/// so bindgen omits the outer `wasmtime::Result`.
impl crate::lattice::plugin_host::logging::Host for PluginState {
    fn log(
        &mut self,
        level: crate::lattice::plugin_host::logging::Level,
        context: String,
        message: String,
    ) {
        use crate::trace::{Direction, PluginTraceRecord, TraceOutcome};
        let Some(ctx) = &self.log_ctx else {
            tracing::debug!(%context, %message, "plugin log dropped: no tracer wired");
            return;
        };
        ctx.tracer.trace(PluginTraceRecord {
            plugin: ctx.plugin,
            seam: PluginSeam::Logging,
            direction: Direction::HostImport,
            // The guest's chosen category rides in `call`; the formatter renders
            // `logging <context>: <message>` for Layer-2 records.
            call: std::borrow::Cow::Owned(context),
            level: map_log_level(level),
            // Logging carries no timing/fuel — the outcome is a nominal Ok; the
            // formatter shows the message, not the outcome, for logging records.
            outcome: TraceOutcome::Ok {
                micros: 0,
                fuel_delta: 0,
            },
            detail: Some(message),
        });
    }
}

/// Map a `wasi:logging`-shaped [`Level`](crate::lattice::plugin_host::logging::Level)
/// to a host [`TraceLevel`](crate::trace::TraceLevel). `critical` folds into
/// `Error` (the tracer has no separate critical tier).
fn map_log_level(level: crate::lattice::plugin_host::logging::Level) -> crate::trace::TraceLevel {
    use crate::lattice::plugin_host::logging::Level;
    use crate::trace::TraceLevel;
    match level {
        Level::Trace => TraceLevel::Trace,
        Level::Debug => TraceLevel::Debug,
        Level::Info => TraceLevel::Info,
        Level::Warn => TraceLevel::Warn,
        Level::Error | Level::Critical => TraceLevel::Error,
    }
}

/// Host impl of the `grammar` guest→host register API (PH7.7b, §4.1). The guest
/// calls these (from its `register-grammar` export) to contribute vim grammar;
/// each records the declaration into the Store's [`grammar_host::GrammarContributions`]
/// so the host can drain it after registration and build native `*Spec`s with
/// trampoline `apply`s (PH7.7c). Sync + infallible (they only push — recording
/// cannot trap; name collisions / registry errors surface at drain time, not
/// here), so bindgen omits the outer `wasmtime::Result`. The bodies forward to
/// the accumulator (the `host_services` `walk` shape).
impl crate::grammar_host::bindings::lattice::plugin_host::grammar::Host for PluginState {
    fn register_motion(
        &mut self,
        name: String,
        doc: String,
        spec: crate::lattice::plugin_host::types::MotionSpec,
        callback: u32,
    ) {
        self.grammar_contributions
            .record_motion(name, doc, spec, callback);
    }

    fn register_operator(
        &mut self,
        name: String,
        doc: String,
        spec: crate::lattice::plugin_host::types::OperatorSpec,
        callback: u32,
    ) {
        self.grammar_contributions
            .record_operator(name, doc, spec, callback);
    }

    fn register_text_object(
        &mut self,
        name: String,
        doc: String,
        spec: crate::lattice::plugin_host::types::TextObjectSpec,
        callback: u32,
    ) {
        self.grammar_contributions
            .record_text_object(name, doc, spec, callback);
    }

    fn register_action(
        &mut self,
        name: String,
        doc: String,
        spec: crate::lattice::plugin_host::types::ActionSpec,
        callback: u32,
    ) {
        self.grammar_contributions
            .record_action(name, doc, spec, callback);
    }

    fn register_ex_command(
        &mut self,
        name: String,
        doc: String,
        spec: crate::lattice::plugin_host::types::ExCommandSpec,
        parse_callback: u32,
        apply_callback: u32,
    ) {
        self.grammar_contributions.record_ex_command(
            name,
            doc,
            spec,
            parse_callback,
            apply_callback,
        );
    }
}

/// Host impl of the `buffer` `document` resource (AP.0.1). A grammar action
/// receives a `borrow<document>`; each method here resolves the borrowed handle
/// out of the Store's `ResourceTable` to the [`DocumentResource`](crate::buffer::DocumentResource)
/// the trampoline minted and forwards to its (already unit-tested) reader. Bulk
/// rope text never crosses — `get-text-range` slices only the requested span.
/// The interface-level `buffer` host trait — no free functions (the interface is
/// only the `document` resource + the `buffer-snapshot` record), so it is a
/// marker `add_to_linker` requires alongside [`HostDocument`].
impl crate::grammar_host::bindings::lattice::plugin_host::buffer::Host for PluginState {}

impl crate::grammar_host::bindings::lattice::plugin_host::buffer::HostDocument for PluginState {
    fn get_text_range(
        &mut self,
        self_: wasmtime::component::Resource<crate::buffer::DocumentResource>,
        r: crate::lattice::plugin_host::types::Range,
    ) -> Result<String, String> {
        let native = lattice_protocol::position::Range::from_wit(r)?;
        let doc = self
            .table
            .get(&self_)
            .map_err(|e| format!("document handle: {e}"))?;
        doc.get_text_range(native)
    }

    fn line_count(
        &mut self,
        self_: wasmtime::component::Resource<crate::buffer::DocumentResource>,
    ) -> u32 {
        self.table.get(&self_).map(|d| d.line_count()).unwrap_or(0)
    }

    fn byte_len(
        &mut self,
        self_: wasmtime::component::Resource<crate::buffer::DocumentResource>,
    ) -> u64 {
        self.table.get(&self_).map(|d| d.byte_len()).unwrap_or(0)
    }

    fn line(
        &mut self,
        self_: wasmtime::component::Resource<crate::buffer::DocumentResource>,
        n: u32,
    ) -> Option<String> {
        self.table.get(&self_).ok().and_then(|d| d.line_at(n))
    }

    fn path(
        &mut self,
        self_: wasmtime::component::Resource<crate::buffer::DocumentResource>,
    ) -> Option<String> {
        self.table.get(&self_).ok().and_then(|d| d.path())
    }

    fn drop(
        &mut self,
        rep: wasmtime::component::Resource<crate::buffer::DocumentResource>,
    ) -> wasmtime::Result<()> {
        // The host lends `borrow<document>` handles and reclaims the owned table
        // entry itself in the trampoline, so a guest never owns one to drop; this
        // fires only if that invariant changes. Delete defensively, ignore a
        // missing entry (already reclaimed) — never a trap on teardown.
        let _ = self.table.delete(rep);
        Ok(())
    }
}

// TS.1: the `tree-sitter` interface `Host` — the marker bindgen requires
// alongside `HostTreeSnapshot` / `HostNode` (the `buffer::Host` shape), plus
// OT.2's one free function.
impl crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::Host for PluginState {
    /// OT.2: parse an off-buffer file and mint a `tree-snapshot` for it.
    ///
    /// Every failure is `None`, never a trap — see the WIT for why the caller
    /// cannot usefully tell them apart. Each arm logs at the level its cause
    /// deserves: a denied grant is user-actionable (`info!`, matching
    /// `read-file`'s and `walk`'s level), everything else is diagnostic
    /// (`debug!`) because a project walk hitting unparseable files is the
    /// ordinary case, not a problem, and `info!` here would flood
    /// `*messages*` on a refile over a large tree.
    fn parse_file(
        &mut self,
        path: String,
    ) -> Option<wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>> {
        // The capability gate first: cheapest, and the one that is a policy
        // answer rather than a fact about the file.
        if !self
            .grant
            .editor
            .contains(lattice_mode::CapabilitySet::TREE_SITTER)
        {
            tracing::info!(
                %path,
                "parse-file denied: plugin lacks the tree-sitter capability"
            );
            return None;
        }
        // Then the fs grant — the SAME check `read-file` makes, so a plugin
        // cannot reach further with a parse than it can with a read.
        let source = match host_services::read_within_grant(&self.grant, &path) {
            Ok(text) => text,
            Err(error) => {
                tracing::debug!(%path, %error, "parse-file: read failed");
                return None;
            }
        };
        // Resolve the language exactly as a buffer's would — native arms first,
        // then the plugin registry — so a plugin gets a tree for its own
        // registered extension for the same reason the editor would.
        let lang = lattice_syntax::Lang::detect_from_path(Some(std::path::Path::new(&path)));
        let mut syntax = match lattice_syntax::Syntax::for_language(lang) {
            Ok(Some(syntax)) => syntax,
            // `Plain`, or a language whose grammar is not loadable here.
            Ok(None) => {
                tracing::debug!(%path, ?lang, "parse-file: no grammar for this extension");
                return None;
            }
            Err(error) => {
                tracing::debug!(%path, ?lang, %error, "parse-file: grammar load failed");
                return None;
            }
        };
        syntax.parse(&source);
        let snapshot = std::sync::Arc::new(syntax.snapshot_owned());
        // A snapshot can exist with no tree behind it. Minting a handle whose
        // every method answers nothing would be worse than `none`, which says
        // so — the same filter the buffer path applies.
        if snapshot.tree().is_none() {
            tracing::debug!(%path, ?lang, "parse-file: parsed to no tree");
            return None;
        }
        match self
            .table
            .push(crate::tree_resource::TreeSnapshotResource::new(snapshot))
        {
            Ok(resource) => Some(resource),
            Err(error) => {
                tracing::debug!(%path, %error, "parse-file: resource table push failed");
                None
            }
        }
    }
}

impl crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::HostTreeSnapshot
    for PluginState
{
    fn root(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>,
    ) -> wasmtime::component::Resource<crate::tree_resource::NodeResource> {
        let node = self
            .table
            .get(&self_)
            .expect("tree-snapshot handle live for the call")
            .root();
        self.table.push(node).expect("node resource table push")
    }

    fn node_at(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>,
        pos: crate::lattice::plugin_host::types::Position,
    ) -> Option<wasmtime::component::Resource<crate::tree_resource::NodeResource>> {
        let pos = lattice_protocol::position::Position::from_wit(pos).ok()?;
        let node = self.table.get(&self_).ok()?.node_at(pos)?;
        self.table.push(node).ok()
    }

    fn enclosing(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>,
        pos: crate::lattice::plugin_host::types::Position,
        kinds: Vec<String>,
    ) -> Option<wasmtime::component::Resource<crate::tree_resource::NodeResource>> {
        let pos = lattice_protocol::position::Position::from_wit(pos).ok()?;
        let node = self.table.get(&self_).ok()?.enclosing(pos, &kinds)?;
        self.table.push(node).ok()
    }

    fn language(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>,
    ) -> String {
        self.table
            .get(&self_)
            .map(|ts| ts.language())
            .unwrap_or_default()
    }

    fn compile_query(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>,
        source: String,
    ) -> Result<wasmtime::component::Resource<crate::tree_resource::QueryResource>, String> {
        let query = self
            .table
            .get(&self_)
            .map_err(|e| format!("tree-snapshot handle: {e}"))?
            .compile_query(&source)?;
        self.table
            .push(query)
            .map_err(|e| format!("query resource push: {e}"))
    }

    fn run_query(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>,
        q: wasmtime::component::Resource<crate::tree_resource::QueryResource>,
        within: Option<crate::lattice::plugin_host::types::Range>,
    ) -> Vec<crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::Capture> {
        let within = within.and_then(|r| lattice_protocol::position::Range::from_wit(r).ok());
        // Collect the (owned) NodeResource captures while borrowing the table,
        // then release the borrows and push each into the table (a mutable
        // borrow) as an owned `node` handle the guest receives + drops.
        let results = {
            let (Ok(ts), Ok(qr)) = (self.table.get(&self_), self.table.get(&q)) else {
                return Vec::new();
            };
            ts.run_query(qr, within)
        };
        results
            .into_iter()
            .filter_map(|(name, node)| {
                let node = self.table.push(node).ok()?;
                Some(
                    crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::Capture {
                        name,
                        node,
                    },
                )
            })
            .collect()
    }

    fn run_query_ranges(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>,
        q: wasmtime::component::Resource<crate::tree_resource::QueryResource>,
        within: Option<crate::lattice::plugin_host::types::Range>,
    ) -> Vec<crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::CaptureRange> {
        let within = within.and_then(|r| lattice_protocol::position::Range::from_wit(r).ok());
        // No second pass over the table: a range is a value, so unlike
        // `run_query` this never needs a mutable table borrow to mint handles.
        let (Ok(ts), Ok(qr)) = (self.table.get(&self_), self.table.get(&q)) else {
            return Vec::new();
        };
        ts.run_query_ranges(qr, within)
            .into_iter()
            // A range that will not convert is dropped rather than trapping:
            // one unrepresentable capture must not fail the whole query.
            .filter_map(|(name, match_index, range)| {
                Some(
                    crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::CaptureRange {
                        name,
                        match_index,
                        range: range.to_wit().ok()?,
                    },
                )
            })
            .collect()
    }

    fn drop(
        &mut self,
        rep: wasmtime::component::Resource<crate::tree_resource::TreeSnapshotResource>,
    ) -> wasmtime::Result<()> {
        // The trampoline lends `borrow<tree-snapshot>` and reclaims the owned
        // entry itself (the `document` discipline); a guest never owns one. Delete
        // defensively, ignore a missing entry — never a trap on teardown.
        let _ = self.table.delete(rep);
        Ok(())
    }
}

impl crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::HostNode for PluginState {
    fn kind(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> String {
        self.table.get(&self_).map(|n| n.kind()).unwrap_or_default()
    }

    fn is_named(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> bool {
        self.table
            .get(&self_)
            .map(|n| n.is_named())
            .unwrap_or(false)
    }

    fn is_error(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> bool {
        self.table
            .get(&self_)
            .map(|n| n.is_error())
            .unwrap_or(false)
    }

    fn byte_range(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> crate::lattice::plugin_host::types::Range {
        let r = self.table.get(&self_).map(|n| n.byte_range()).unwrap_or(
            lattice_protocol::position::Range {
                start: lattice_protocol::position::Position { line: 0, byte: 0 },
                end: lattice_protocol::position::Position { line: 0, byte: 0 },
            },
        );
        crate::lattice::plugin_host::types::Range {
            start: crate::lattice::plugin_host::types::Position {
                line: r.start.line,
                byte: r.start.byte,
            },
            end: crate::lattice::plugin_host::types::Position {
                line: r.end.line,
                byte: r.end.byte,
            },
        }
    }

    fn parent(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> Option<wasmtime::component::Resource<crate::tree_resource::NodeResource>> {
        let node = self.table.get(&self_).ok()?.parent()?;
        self.table.push(node).ok()
    }

    fn named_child_count(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> u32 {
        self.table
            .get(&self_)
            .map(|n| n.named_child_count())
            .unwrap_or(0)
    }

    fn named_child(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
        index: u32,
    ) -> Option<wasmtime::component::Resource<crate::tree_resource::NodeResource>> {
        let node = self.table.get(&self_).ok()?.named_child(index)?;
        self.table.push(node).ok()
    }

    fn child_by_field(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
        name: String,
    ) -> Option<wasmtime::component::Resource<crate::tree_resource::NodeResource>> {
        let node = self.table.get(&self_).ok()?.child_by_field(&name)?;
        self.table.push(node).ok()
    }

    fn next_named_sibling(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> Option<wasmtime::component::Resource<crate::tree_resource::NodeResource>> {
        let node = self.table.get(&self_).ok()?.next_named_sibling()?;
        self.table.push(node).ok()
    }

    fn prev_named_sibling(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> Option<wasmtime::component::Resource<crate::tree_resource::NodeResource>> {
        let node = self.table.get(&self_).ok()?.prev_named_sibling()?;
        self.table.push(node).ok()
    }

    fn walk(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> wasmtime::component::Resource<crate::tree_resource::CursorResource> {
        let cursor = self
            .table
            .get(&self_)
            .expect("node handle live for the call")
            .walk();
        self.table.push(cursor).expect("cursor resource table push")
    }

    fn drop(
        &mut self,
        rep: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) -> wasmtime::Result<()> {
        // Nodes ARE owned by the guest (unlike the lent snapshot handle); the
        // guest's generated RAII wrapper drops them when they leave scope. Delete
        // the table entry, ignoring a missing one — never a trap.
        let _ = self.table.delete(rep);
        Ok(())
    }
}

// TS.2: the `query` resource is opaque — no methods beyond the implicit drop.
impl crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::HostQuery for PluginState {
    fn drop(
        &mut self,
        rep: wasmtime::component::Resource<crate::tree_resource::QueryResource>,
    ) -> wasmtime::Result<()> {
        // Guest-owned (returned by `compile-query`); dropped by the guest's RAII
        // wrapper. Delete defensively, ignore a missing entry — never a trap.
        let _ = self.table.delete(rep);
        Ok(())
    }
}

impl crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::HostTreeCursor
    for PluginState
{
    fn current_node(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::CursorResource>,
    ) -> wasmtime::component::Resource<crate::tree_resource::NodeResource> {
        let node = self
            .table
            .get(&self_)
            .expect("cursor handle live for the call")
            .current_node();
        self.table.push(node).expect("node resource table push")
    }

    fn current_field(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::CursorResource>,
    ) -> Option<String> {
        self.table.get(&self_).ok().and_then(|c| c.current_field())
    }

    fn goto_first_named_child(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::CursorResource>,
    ) -> bool {
        self.table
            .get_mut(&self_)
            .map(|c| c.goto_first_named_child())
            .unwrap_or(false)
    }

    fn goto_next_named_sibling(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::CursorResource>,
    ) -> bool {
        self.table
            .get_mut(&self_)
            .map(|c| c.goto_next_named_sibling())
            .unwrap_or(false)
    }

    fn goto_parent(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::CursorResource>,
    ) -> bool {
        self.table
            .get_mut(&self_)
            .map(|c| c.goto_parent())
            .unwrap_or(false)
    }

    fn reset(
        &mut self,
        self_: wasmtime::component::Resource<crate::tree_resource::CursorResource>,
        n: wasmtime::component::Resource<crate::tree_resource::NodeResource>,
    ) {
        // Read the target node's path (releasing that borrow) before mutating the
        // cursor — the two live in the same table.
        let Some(path) = self.table.get(&n).ok().map(|nr| nr.path().to_vec()) else {
            return;
        };
        if let Ok(cursor) = self.table.get_mut(&self_) {
            cursor.reset_to_path(path);
        }
    }

    fn drop(
        &mut self,
        rep: wasmtime::component::Resource<crate::tree_resource::CursorResource>,
    ) -> wasmtime::Result<()> {
        let _ = self.table.delete(rep);
        Ok(())
    }
}

/// Host impl of the `events` guest→host subscription API (PH7.8b, §5). The guest
/// calls `subscribe` (from its `register-events` export) to observe editor
/// events; each records the `(filter, handler)` pair into the Store's
/// [`events_host::EventContributions`] so the host can drain it after
/// registration and wire each subscription to the native `EventBus` (PH7.8c).
/// Sync + infallible (it only pushes — recording cannot trap), so bindgen omits
/// the outer `wasmtime::Result` (the `grammar` / `host_services` shape). The
/// filter stays WIT-typed here and projects to native at wire time.
impl crate::events_host::bindings::lattice::plugin_host::events::Host for PluginState {
    fn subscribe(&mut self, filter: crate::lattice::plugin_host::types::EventFilter, handler: u32) {
        self.event_subscriptions.record(filter, handler);
    }

    /// OC.2 `wake-every`: arm a periodic wake, answered by `on-wake(id)` on this
    /// plugin's own actor task. Returns `0` when no wake context is wired — a
    /// store whose actor cannot fire one, or a host with no [`SleeperHandle`]
    /// installed. Degrading rather than trapping is the four-artefact
    /// graceful-failure clause; the `warn!` is what makes the nothing visible.
    fn wake_every(&mut self, ms: u32) -> u32 {
        let plugin = self.event_emit.as_ref().map_or(0, |c| c.plugin_id.0);
        match self.wake.as_mut() {
            Some(ctx) => ctx.arm(plugin, ms),
            None => {
                tracing::warn!(ms, "wake-every refused: no wake mechanism on this seam");
                0
            }
        }
    }

    /// OC.2 `cancel-wake`: disarm. Idempotent by contract, including on a store
    /// with no wake context at all.
    fn cancel_wake(&mut self, id: u32) {
        let plugin = self.event_emit.as_ref().map_or(0, |c| c.plugin_id.0);
        if let Some(ctx) = self.wake.as_mut() {
            ctx.cancel(plugin, id);
        }
    }
}

/// Host impl of the `config` guest→host option seam (PH7.10, §5). The guest calls
/// `register-option` (from its `register-options` export) to declare options and
/// `get-option` to read any option's current string value. Both are sync +
/// non-trapping, so bindgen omits the outer `wasmtime::Result` (the
/// `host-services` `walk` shape). `register-option` maps the WIT `option-type` to
/// a native `OptionType` and registers into the SAME `ConfigRegistry` core
/// options use ([`config_host::register_plugin_option`]); a plugin with no
/// registry wired degrades to `false` / `none` (never a panic — the
/// four-artefact graceful-failure clause).
impl crate::theme_host::bindings::lattice::plugin_host::theme::Host for PluginState {
    fn register_element(
        &mut self,
        name: String,
        doc: String,
        default: crate::theme_host::bindings::lattice::plugin_host::theme::StyleSpec,
    ) -> Result<(), String> {
        let Some(registry) = self.theme_registry.clone() else {
            // Graceful, not a trap: a harness with no theme service wired is a
            // test shape, not a plugin error. The element simply does not
            // exist and rows fall back to their default style.
            tracing::warn!(
                element = %name,
                "register-element ignored: plugin has no theme registry wired"
            );
            return Ok(());
        };
        // Auto-namespaced by manifest id, exactly like `register-option`, so a
        // plugin cannot squat a bare name or shadow a builtin. An internal
        // caller with no manifest id gets no prefix.
        let Some(plugin_id) = self.plugin_name.clone() else {
            return Err("register-element requires a plugin identity".to_string());
        };
        let spec = crate::theme_host::style_spec_from_wit(default);
        let full =
            crate::theme_host::register_plugin_element(&*registry, &plugin_id, &name, &doc, spec);
        self.theme_contributions.push(full);
        Ok(())
    }

    /// TK.5: an override for an element this plugin owns, above the theme.
    ///
    /// `register-element` supplies a default, which sits BELOW the active
    /// theme — so a plugin could not express "the user configured this and it
    /// must win" without this.
    fn set_element_override(
        &mut self,
        name: String,
        style: crate::theme_host::bindings::lattice::plugin_host::theme::StyleSpec,
    ) -> Result<(), String> {
        let Some(registry) = self.theme_registry.clone() else {
            // Same graceful shape as `register_element`: a harness with no
            // theme service is a test shape, not a plugin error.
            tracing::warn!(
                element = %name,
                "set-element-override ignored: plugin has no theme registry wired"
            );
            return Ok(());
        };
        let Some(plugin_id) = self.plugin_name.clone() else {
            return Err("set-element-override requires a plugin identity".to_string());
        };
        let spec = crate::theme_host::style_spec_from_wit(style);
        crate::theme_host::set_plugin_element_override(&*registry, &plugin_id, &name, spec)
    }
}

/// CR.3: the `help` seam's single host func.
///
/// Validation and namespacing live in [`help_host`] as free functions so they
/// are unit-testable without a `Store` (the `config_host` / `theme_host`
/// precedent). This body is the thin part: derive the namespace from the
/// manifest id the HOST holds — never from anything the guest passed — and
/// record.
///
/// A rejection is an `Err` back to the guest, not a trap. One malformed page
/// costs itself; the plugin's other pages still register, and the load still
/// succeeds. That matches how every other declaration seam here treats bad
/// guest data.
impl crate::help_host::bindings::lattice::plugin_host::help::Host for PluginState {
    fn register_topic(
        &mut self,
        name: String,
        summary: String,
        body: String,
        related_commands: Vec<String>,
    ) -> Result<(), String> {
        let plugin_id = self.plugin_name.clone().unwrap_or_default();
        match crate::help_host::validate_topic(&plugin_id, &name, &summary, &body, related_commands)
        {
            Ok(spec) => {
                self.help_contributions.push(spec);
                Ok(())
            }
            Err(err) => {
                tracing::warn!(topic = %name, %err, "register-topic rejected");
                Err(err)
            }
        }
    }
}

/// LG.3c: the `language` seam's declaration host func.
///
/// Records the guest's bytes and query sources; compiling the grammar is the
/// loader's job, after this store is gone (see `language_host`). A rejection
/// is an `Err` back to the guest, not a trap — one malformed language costs
/// itself, the plugin's others still register, and the load still succeeds.
impl crate::language_host::bindings::lattice::plugin_host::language::Host for PluginState {
    fn register_language(
        &mut self,
        spec: crate::language_host::bindings::lattice::plugin_host::language::LanguageSpec,
    ) -> Result<(), String> {
        let declared = spec.name.clone();
        match crate::language_host::validate_language(spec) {
            Ok(spec) => {
                self.language_contributions.push(spec);
                Ok(())
            }
            Err(err) => {
                tracing::warn!(language = %declared, %err, "register-language rejected");
                Err(err)
            }
        }
    }
}

/// CR.4: the `dashboard` seam's declaration host func.
///
/// Records only — the host instantiates the live per-section guests after the
/// export returns. Section ids are NOT namespaced (see `dashboard.wit`):
/// replacing a built-in section by id is a supported capability, so the
/// namespace that defuses `help`'s collisions would defeat this seam's.
impl crate::dashboard_host::bindings::lattice::plugin_host::dashboard::Host for PluginState {
    fn register_section(
        &mut self,
        id: String,
        order: i32,
        default_enabled: bool,
    ) -> Result<(), String> {
        match crate::dashboard_host::validate_section(&id, order, default_enabled) {
            Ok(spec) => {
                self.dashboard_contributions.push(spec);
                Ok(())
            }
            Err(err) => {
                tracing::warn!(section = %id, %err, "register-section rejected");
                Err(err)
            }
        }
    }
}

impl crate::config_host::bindings::lattice::plugin_host::config::Host for PluginState {
    fn register_option(
        &mut self,
        name: String,
        ty: crate::config_host::bindings::lattice::plugin_host::config::OptionType,
        default: String,
        doc: String,
    ) -> bool {
        use crate::config_host::PluginOptionKind;
        use crate::config_host::bindings::lattice::plugin_host::config::OptionType as WitOptionType;
        let kind = match ty {
            WitOptionType::Boolean => PluginOptionKind::Boolean,
            WitOptionType::Integer => PluginOptionKind::Integer,
            WitOptionType::String => PluginOptionKind::String,
        };
        // Auto-namespace the plugin's OWN option by its manifest id: a plugin
        // registering `style` contributes `auto-pair.style`, so plugins can't
        // collide in the global option namespace. No prefix for an internal
        // caller with no manifest id.
        let full = match &self.plugin_name {
            Some(id) => format!("{id}.{name}"),
            None => name,
        };
        // The `&self.config_registry` borrow ends with the match (the result is a
        // plain `bool`), so the `config_contributions` push below doesn't overlap.
        let registered = match &self.config_registry {
            Some(registry) => {
                config_host::register_plugin_option(registry, &full, kind, &default, &doc)
            }
            None => {
                tracing::warn!(
                    option = %full,
                    "register-option ignored: plugin has no config registry wired"
                );
                false
            }
        };
        if registered {
            self.config_contributions.push(full);
        }
        registered
    }

    fn get_option(&mut self, name: String) -> Option<String> {
        let registry = self.config_registry.as_ref()?;
        // The plugin's OWN namespace first (`style` → `auto-pair.style`), then the
        // raw name — so a plugin reads its options with short names AND can still
        // read a core option (`tabstop`) that isn't in its namespace.
        if let Some(id) = &self.plugin_name
            && let Some(opt) = registry.lookup(&format!("{id}.{name}"))
        {
            return Some(opt.get_formatted());
        }
        registry.lookup(&name).map(|opt| opt.get_formatted())
    }

    /// `set-option` (CI.7): override an existing option's value via the SAME
    /// `parse_and_set_command` path `:set name=value` uses (coerce + validate +
    /// publish `OptionChanged`). `false` on an unknown option / invalid value /
    /// no registry — a logged no-op, never a trap.
    fn set_option(&mut self, name: String, value: String) -> bool {
        let Some(registry) = self.config_registry.as_ref() else {
            tracing::warn!(
                option = %name,
                "set-option ignored: plugin has no config registry wired"
            );
            return false;
        };
        // The plugin's OWN option if it exists in its namespace (`style` →
        // `auto-pair.style`), else the raw name — so a plugin sets its own option
        // with a short name AND can set a core option by its full name.
        let target = self
            .plugin_name
            .as_ref()
            .map(|id| format!("{id}.{name}"))
            .filter(|full| registry.lookup(full).is_some())
            .unwrap_or(name);
        match registry.parse_and_set_command(&format!("{target}={value}")) {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(
                    option = %target,
                    %value,
                    %err,
                    "set-option failed (unknown option or invalid value)"
                );
                false
            }
        }
    }
}

/// Host impl of the `modes` guest→host mode-declaration seam (PH7.11a, §5). The
/// guest calls `register-mode` (from its `register-modes` export) to declare a
/// minor mode; each records the declaration into the Store's
/// [`mode_host::ModeContributions`] so the host can drain it after registration
/// and register each into a `&mut ModeRegistry` (`spawn_mode_plugin`).
/// Sync + infallible (it only pushes — recording cannot trap; the register
/// outcome surfaces at drain time), so bindgen omits the outer `wasmtime::Result`
/// (the `grammar` / `config` shape). The WIT declaration projects to the native
/// [`mode_host::PluginModeDecl`] here (kind / policy / capability flags → native).
impl crate::keymap_host::bindings::lattice::plugin_host::keymap::Host for PluginState {
    fn register_binding(
        &mut self,
        binding_mode: crate::keymap_host::bindings::lattice::plugin_host::keymap::BindingMode,
        chord: String,
        command: String,
    ) -> bool {
        // Clone the wired context out first so the immutable borrow ends before
        // the `keymap_contributions` push (the `config_host` register-option
        // borrow-split precedent).
        let Some(ctx) = self.keymap_ctx.as_ref() else {
            tracing::warn!(
                chord,
                command,
                "register-binding skipped: no keymap wired (degraded — host not spawned onto a keymap)"
            );
            return false;
        };
        let keymap = ctx.keymap.clone();
        let commands = std::sync::Arc::clone(&ctx.commands);
        let plugin_id = ctx.plugin_id.0;
        let mode = crate::keymap_host::project_binding_mode(binding_mode);

        let bound = crate::keymap_host::bind_user_keybinding(
            &keymap, &commands, plugin_id, mode, &chord, &command,
        );
        if bound {
            self.keymap_contributions
                .push(crate::keymap_host::KeymapBindingToken { mode, chord });
        }
        bound
    }
}

impl crate::mode_host::bindings::lattice::plugin_host::modes::Host for PluginState {
    fn register_mode(
        &mut self,
        decl: crate::mode_host::bindings::lattice::plugin_host::modes::ModeDeclaration,
    ) {
        use crate::mode_host::bindings::lattice::plugin_host::modes::{
            ActivationPolicy as WitPolicy, BindingMode as WitBindingMode,
            ModeCapabilities as WitCaps, ModeKind as WitKind,
            OverridePriority as WitOverridePriority,
        };
        use crate::mode_host::{
            PluginKeymapBinding, PluginModeDecl, PluginModeKind, PluginModeOverride,
        };
        use lattice_keymap::BindingMode;
        use lattice_mode::{ActivationPolicy, CapabilitySet, ModeId, OverridePriority};

        let kind = match decl.kind {
            WitKind::Major => PluginModeKind::Major,
            WitKind::Minor => PluginModeKind::Minor,
        };
        let policy = match decl.activation_policy {
            WitPolicy::Manual => ActivationPolicy::Manual,
            WitPolicy::Global => ActivationPolicy::Global,
            WitPolicy::Universal => ActivationPolicy::Universal,
            WitPolicy::Majors(ids) => {
                ActivationPolicy::Majors(ids.iter().map(|m| ModeId::new(m)).collect())
            }
        };
        // WIT `flags` project to the native bitflags one bit at a time (the
        // generated flags type is distinct from `CapabilitySet`).
        let mut caps = CapabilitySet::empty();
        caps.set(
            CapabilitySet::BUFFER_URI,
            decl.capabilities.contains(WitCaps::BUFFER_URI),
        );
        caps.set(CapabilitySet::LSP, decl.capabilities.contains(WitCaps::LSP));
        caps.set(
            CapabilitySet::TREE_SITTER,
            decl.capabilities.contains(WitCaps::TREE_SITTER),
        );
        caps.set(
            CapabilitySet::FOLDS,
            decl.capabilities.contains(WitCaps::FOLDS),
        );
        caps.set(
            CapabilitySet::WRITABLE,
            decl.capabilities.contains(WitCaps::WRITABLE),
        );
        caps.set(
            CapabilitySet::DIAGNOSTICS,
            decl.capabilities.contains(WitCaps::DIAGNOSTICS),
        );

        // Project the keymap bindings (PH7.11b): WIT `binding-mode` → native
        // `BindingMode`; chord + command names cross as strings (resolved against
        // the `CommandRegistry` at bind time in `spawn_mode_plugin`).
        let keymap = decl
            .keymap
            .into_iter()
            .map(|b| PluginKeymapBinding {
                mode: match b.binding_mode {
                    WitBindingMode::Normal => BindingMode::Normal,
                    WitBindingMode::Insert => BindingMode::Insert,
                    WitBindingMode::Visual => BindingMode::Visual,
                    WitBindingMode::Select => BindingMode::Select,
                    WitBindingMode::Replace => BindingMode::Replace,
                    WitBindingMode::Command => BindingMode::Command,
                    WitBindingMode::Search => BindingMode::Search,
                },
                chord: b.chord,
                command: b.command,
            })
            .collect();

        // MO.1: option overrides. Name and value cross as strings and are
        // resolved against the `ConfigRegistry` at drain — the same shape the
        // keymap's command names take, and for the same reason: the registry
        // they must agree with is native, and a declaration is only data.
        let options = decl
            .options
            .into_iter()
            .map(|o| PluginModeOverride {
                name: o.name,
                value: o.value,
                priority: match o.priority {
                    WitOverridePriority::Low => OverridePriority::Low,
                    WitOverridePriority::Normal => OverridePriority::Normal,
                    WitOverridePriority::High => OverridePriority::High,
                },
            })
            .collect();

        self.mode_contributions.record(PluginModeDecl {
            id: decl.id,
            kind,
            policy,
            caps,
            keymap,
            // OM.2: crosses as a plain string; `register_plugin_mode` decides
            // whether it means anything (majors only) and the registry
            // indexes it.
            target_language: decl.target_language,
            options,
        });
    }

    /// `enable-mode` (CI.4): request the Editor enable a minor mode globally. The
    /// guest can't reach the activator, so this publishes
    /// `Event::ModeEnablementRequested` onto the bus; the Editor flips the
    /// registry + re-activates open buffers. Degrades to a `warn` when no bus is
    /// wired (the "not boot-wired yet" case, like `emit-event`), never a trap.
    fn enable_mode(&mut self, id: String) {
        self.request_mode_enablement(id, true);
    }

    /// `disable-mode` (CI.4): the inverse of [`enable_mode`](Self::enable_mode).
    fn disable_mode(&mut self, id: String) {
        self.request_mode_enablement(id, false);
    }
}

impl PluginState {
    /// Shared body of `enable-mode` / `disable-mode` (CI.4): publish the
    /// host-internal enablement request onto the plugin's bus (the same
    /// `event_emit` handle `emit-event` uses).
    fn request_mode_enablement(&mut self, mode: String, enabled: bool) {
        match &self.event_emit {
            Some(ctx) => ctx
                .bus
                .publish(lattice_protocol::Event::ModeEnablementRequested { mode, enabled }),
            None => tracing::warn!(
                %mode,
                enabled,
                "enable-mode/disable-mode dropped: no bus wired (plugin not spawned onto a bus)"
            ),
        }
    }
}

/// A host-issued plugin identity. Monotonic, allocated by the [`PluginHost`] at
/// instantiation — never supplied by the guest. It is the `u32` inside
/// [`SourceLayer::Plugin`], so every contribution a plugin registers (PH7.3+)
/// traces back to a provenance the guest cannot forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginId(pub u32);

/// The default on-disk module-cache directory,
/// `<user-cache>/lattice/plugin-cache/` (XDG on Linux, Application Support on
/// macOS, LocalAppData on Windows). Falls back to the temp dir if no user
/// cache dir can be resolved.
fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lattice")
        .join("plugin-cache")
}

/// The default per-plugin data-dir base, `<user-data>/lattice/plugins/`
/// (`${XDG_DATA_HOME}` on Linux, Application Support on macOS, LocalAppData on
/// Windows). Each plugin's private dir is `<base>/<plugin-id>/data/` (fragment
/// §6). Falls back to the temp dir if no user data dir can be resolved.
fn default_data_dir_base() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lattice")
        .join("plugins")
}

/// The wasmtime engine, the (import-free) component linker, the on-disk module
/// cache, and the epoch ticker. One host per editor process; construct it once
/// (the engine owns Cranelift).
pub struct PluginHost {
    engine: Engine,
    linker: Linker<PluginState>,
    // A second linker for the SYNCHRONOUS grammar seam (PH7.7c). The shared
    // `linker` wires WASI *async* (picker/completion suspend at host calls); the
    // grammar trampoline calls the guest *synchronously* on the dispatch thread,
    // so its guest must never reach an async host import. This linker wires WASI
    // *sync* (`add_to_linker_sync`) + the sync `grammar` register import, so a
    // grammar guest's sync `instantiate` + `apply` calls have no async import to
    // invoke — the sync path is correct by construction, not by luck. Same
    // engine (shared AOT cache), just a different import table.
    grammar_linker: Linker<PluginState>,
    // A clone of the cache handed to the engine config; kept so callers can
    // read hit/miss stats. `Cache` is a cheap Arc-backed handle.
    cache: Cache,
    // Base dir under which each plugin's private data dir is created:
    // `<data_dir_base>/<plugin-id>/data/` (PH7.2, fragment §6).
    data_dir_base: PathBuf,
    // OR.1: one durable byte store per MANIFEST ID, handed to every
    // `PluginState` built for that id.
    //
    // Keyed by id rather than by instance, and that is the slice's whole point
    // rather than a caching detail. The picker seam, the grammar seam and the
    // event seam are separate `wasmtime::Store`s with separate memory; a store
    // per instance would mean N copies drifting, with find-node offering a node
    // `<CR>` cannot open and nothing reporting an error. Sharing the handle
    // makes a write on one seam visible to a read on another immediately —
    // before any flush, and without a reload.
    stores: Mutex<std::collections::HashMap<String, plugin_store::PluginStoreHandle>>,
    // Monotonic source of host-issued `PluginId`s. `&self` methods allocate,
    // so this is atomic.
    next_id: AtomicU32,
    // PO.5: the boundary tracer, so each instantiate/spawn path can stamp a
    // plugin's `PluginState.log_ctx` (the guest `logging` seam routes into it).
    // Set once by the loader (`set_tracer`) after it builds the tracer — the host
    // is constructed first (`install.rs`), so `OnceLock` gives set-once storage
    // through the shared `Arc<PluginHost>` without a constructor reorder. `None`
    // (unset) → a guest `log` degrades to a debug-drop, like `event_emit`.
    tracer: std::sync::OnceLock<crate::trace::PluginTracerHandle>,
    // PR.6: what the guest `project` seam answers from. Set once by boot
    // (`set_project_context`) after the resolver is registered; the host is
    // constructed first, so `OnceLock` gives set-once storage through the
    // shared `Arc<PluginHost>` without a constructor reorder — the `tracer`
    // precedent above. Unset → `root-for-*` returns `none`, which a real
    // editor never does and a stripped harness always does.
    project: std::sync::OnceLock<ProjectCtx>,
    // CG.4: the foreground-cancel registry `<C-g>` fires. Set once by
    // boot, like `tracer` / `project`; unset → guest calls run to their
    // budget and cannot be interrupted early, which is the pre-CG.4
    // behaviour.
    cancel: std::sync::OnceLock<lattice_mode::ForegroundCancelHandle>,
    // OC.2: the timer the wake seam sleeps on. Injected rather than owned
    // because this crate deliberately has no runtime — `tokio` is a
    // dev-dependency and `futures` was chosen over `tokio::sync` to keep it
    // that way. Set once by boot, like `tracer` / `project` / `cancel`; unset
    // → `wake-every` answers `0`, which is what every harness that never wires
    // one gets.
    sleeper: std::sync::OnceLock<wake::SleeperHandle>,
    // OC.3 / ML.6: what the `ui` seam acts on — the modeline element registry
    // and the bus content updates publish onto. Both halves are required (a
    // registry with no bus registers descriptors nothing ever repaints), so
    // they are set together like `project`'s resolver + buffer store. Unset →
    // `register-segment` returns false and `emit-segment` warns and drops.
    ui: std::sync::OnceLock<ui_host::UiCtx>,
    // Dropped last; keeps the ticker alive for the host's lifetime and stops
    // it on drop.
    _epoch_ticker: EpochTicker,
}

impl PluginHost {
    /// Build a host with the default module-cache directory
    /// ([`default_cache_dir`]) and per-plugin data-dir base
    /// ([`default_data_dir_base`]). This is the production constructor.
    pub fn new() -> Result<Self, PluginHostError> {
        Self::with_dirs(default_cache_dir(), default_data_dir_base())
    }

    /// Build a host caching compiled components under `cache_dir`, with the
    /// default per-plugin data-dir base. Kept as the narrow constructor the
    /// cache tests already use.
    pub fn with_cache_dir(cache_dir: impl Into<PathBuf>) -> Result<Self, PluginHostError> {
        Self::with_dirs(cache_dir, default_data_dir_base())
    }

    /// Build a host with an explicit module-cache directory *and* per-plugin
    /// data-dir base. Capability tests point both at per-test tempdirs so the
    /// data-dir mounts and provenance are hermetic.
    ///
    /// The AOT (Cranelift) compile of a component is cached on disk by
    /// wasmtime, keyed on the component bytes, the compiler configuration, the
    /// target, and the wasmtime version — so a **second launch reuses the
    /// cached module** instead of recompiling (design.md §15 Q17). wasmtime
    /// owns the keying and invalidation; the host owns only the location.
    ///
    /// The linker is populated with the WASI (preview2) host functions once
    /// here; each plugin's *view* onto them is scoped per-`Store` from its
    /// grant (PH7.2). Components that import no WASI (the hand-written
    /// lifecycle fixtures) instantiate fine against the populated linker.
    pub fn with_dirs(
        cache_dir: impl Into<PathBuf>,
        data_dir_base: impl Into<PathBuf>,
    ) -> Result<Self, PluginHostError> {
        let cache_dir = cache_dir.into();
        std::fs::create_dir_all(&cache_dir).map_err(|e| PluginHostError::Cache(e.into()))?;
        let mut cache_config = CacheConfig::new();
        cache_config.with_directory(cache_dir);
        let cache = Cache::new(cache_config).map_err(|e| PluginHostError::Cache(e.into()))?;

        let mut config = Config::new();
        // Async is always available on the engine in wasmtime 46; the generated
        // exports are async (see the `bindgen!` above). Fuel + epoch give the
        // two hard per-call budgets.
        config.consume_fuel(true);
        config.epoch_interruption(true);
        // Transparent AOT artifact cache: `Component::new` (in `compile`) skips
        // recompilation on a cache hit.
        config.cache(Some(cache.clone()));

        let engine = Engine::new(&config).map_err(|e| PluginHostError::Engine(e.into()))?;
        let mut linker = Linker::new(&engine);
        // Wire the WASI host functions to `PluginState`'s `WasiView`. Async to
        // match the canonical ABI — a WASI host call suspends the guest stack,
        // never pins the caller's thread.
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|e| PluginHostError::Linker(e.into()))?;
        // The `host-services` guest→host seam (PH7.4b). Sync host funcs are fine
        // in the async linker; `walk` is bounded, so it does not need to suspend.
        crate::lattice::plugin_host::host_services::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // PM.7: the `plugin-manager` (`require`) seam. A sync host func — it
        // only records into `PluginState` — and inert for every world that
        // does not import `plugin-manager`, which is all of them but the
        // config/init world.
        crate::plugin_manager_host::bindings::lattice::plugin_host::plugin_manager::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // The `events` guest→host subscription seam (PH7.8b). Wired into the
        // ASYNC linker — the events-plugin world's `on-event` delivery is async
        // (off the keystroke path), unlike the sync grammar seam. `subscribe` is
        // a sync host func (it only records into `PluginState`) and is inert for
        // worlds that don't import `events` (picker/completion/the scaffold).
        crate::events_host::bindings::lattice::plugin_host::events::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // The `config` guest→host option seam (PH7.10). Sync host funcs
        // (`register-option` / `get-option` only touch the `ConfigRegistry`),
        // inert for worlds that don't import `config`.
        crate::config_host::bindings::lattice::plugin_host::config::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // TC.4: the `theme` guest->host element-declaration seam. Sync host
        // func (`register-element` only touches the theme registry), inert for
        // worlds that don't import `theme`.
        crate::theme_host::bindings::lattice::plugin_host::theme::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // CR.3: the `help` guest→host topic-registration seam. Sync host func
        // (`register-topic` only records into `PluginState`), inert for worlds
        // that don't import `help`.
        crate::help_host::bindings::lattice::plugin_host::help::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // LG.3c: the `language` seam. Sync host func (`register-language`
        // only records into `PluginState`), inert for worlds that don't
        // import `language`.
        crate::language_host::bindings::lattice::plugin_host::language::add_to_linker::<
            _,
            HasSelf<_>,
        >(&mut linker, |state: &mut PluginState| state)
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // CR.4: the `dashboard` declaration seam. `register-section` only
        // records into `PluginState`, so it is sync and safe on either
        // linker; inert for worlds that don't import `dashboard`.
        crate::dashboard_host::bindings::lattice::plugin_host::dashboard::add_to_linker::<
            _,
            HasSelf<_>,
        >(&mut linker, |state: &mut PluginState| state)
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // The `modes` guest→host mode-declaration seam (PH7.11a). Sync host func
        // (`register-mode` only records into `PluginState`), inert for worlds that
        // don't import `modes`.
        crate::mode_host::bindings::lattice::plugin_host::modes::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // The `keymap` guest→host binding-registration seam (PL8.D.1). Sync host
        // func (`register-binding` resolves + binds into `KeymapLayer::User`,
        // recording a teardown token — no await), inert for worlds that don't
        // import `keymap`. Async linker: registration is off the keystroke path
        // (binding *resolution* stays native).
        crate::keymap_host::bindings::lattice::plugin_host::keymap::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // The `logging` guest→host seam (PO.5, Layer 2). Sync host func (`log`
        // only pushes into the `PluginTracer` ring), wired into the ASYNC linker —
        // logging is off the keystroke path (NOT in the sync grammar linker, so a
        // grammar guest can never reach it). Inert for worlds that don't import
        // `logging`. Shared across the async-seam worlds via the bindgen
        // `with`-reuse of this one generated module.
        crate::lattice::plugin_host::logging::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // PR.6: the `project` guest→host seam. Sync host funcs (a cached
        // directory walk), wired here beside `logging` and inert for worlds
        // that don't import it. Shared across the async-seam worlds via the
        // bindgen `with`-reuse of this one generated module.
        crate::lattice::plugin_host::project::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // OC.3 / ML.6: the `ui` modeline-contribution seam. Sync host funcs (a
        // registry write and a bus publish), wired here beside `logging` /
        // `project` and inert for worlds that don't import it.
        crate::lattice::plugin_host::ui::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // Multi-seam support (AP.1 spike): a single plugin `.wasm` may `provide`
        // grammar AND async seams (auto-pair: grammar + modes + config). Its
        // import set is fixed, so the async linker (used for the mode/config
        // drains) must ALSO satisfy the grammar + buffer imports. Both are sync
        // host funcs (register-* only record; `document` reads are table lookups),
        // so they are inert here for the async-only worlds and correct for a
        // combined component. Symmetric to grammar-linker below.
        crate::grammar_host::bindings::lattice::plugin_host::grammar::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        crate::grammar_host::bindings::lattice::plugin_host::buffer::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // TS.1: the `tree-sitter` `tree-snapshot` / `node` resources, on the
        // async linker too (superset symmetry with `buffer`) — a combined plugin
        // instantiated here for its async drains still imports `tree-sitter` via
        // `apply-action`. The reads are sync host-table lookups; inert for worlds
        // that don't import it.
        crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::add_to_linker::<
            _,
            HasSelf<_>,
        >(&mut linker, |state: &mut PluginState| state)
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // The grammar seam's SYNC linker (PH7.7b/c). The `grammar` register API
        // (`register-*`) is guest→host sync host funcs (they only record into
        // `PluginState`). It is wired into a SECOND linker whose WASI is `sync`
        // — so a grammar guest instantiated here has no async host import, and
        // the trampoline's synchronous `apply` calls on the dispatch thread are
        // correct by construction (the PH7.7 fork). Same `engine`, so the AOT
        // cache is shared; only the import table differs from the async `linker`.
        let mut grammar_linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut grammar_linker)
            .map_err(|e| PluginHostError::Linker(e.into()))?;
        crate::grammar_host::bindings::lattice::plugin_host::grammar::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // AP.0.1: the `buffer` `document` resource, so a grammar action's
        // `borrow<document>` param resolves. On the SYNC grammar linker only —
        // the reads are synchronous host-table lookups (no I/O), correct for the
        // dispatch-thread trampoline.
        crate::grammar_host::bindings::lattice::plugin_host::buffer::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // TS.1: the `tree-sitter` resources on the SYNC grammar linker, so a
        // grammar action's `option<borrow<tree-snapshot>>` param resolves. Reads
        // are synchronous host-table lookups + parse-free tree walks (no I/O, no
        // parse — the tree is already there), correct for the dispatch-thread
        // trampoline.
        crate::grammar_host::bindings::lattice::plugin_host::tree_sitter::add_to_linker::<
            _,
            HasSelf<_>,
        >(&mut grammar_linker, |state: &mut PluginState| state)
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // Multi-seam support (AP.1 spike): a combined plugin instantiated here for
        // its GRAMMAR drain still imports its `modes` / `config` seams, so the sync
        // grammar linker must satisfy them too. Both `register-mode` /
        // `register-option` / `get-option` are sync host funcs that only record
        // into `PluginState` (never called on the apply-action hot path — only
        // during the registration exports, which run on the async drains), so they
        // are safe here and inert for grammar-only guests. `logging` is
        // deliberately NOT added — the combined `auto-pair` world omits it, so the
        // "no logging reachable from the grammar hot path" invariant holds.
        crate::config_host::bindings::lattice::plugin_host::config::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // TC.6: a multi-seam component that provides BOTH `grammar` and `theme`
        // (treesitter-context) is instantiated against this sync linker for its
        // grammar seam, and instantiation must satisfy EVERY import the world
        // declares — not only the ones that seam uses. Registering an element
        // touches the theme registry and nothing else, so it is safe on the
        // sync path; leaving it out simply made the whole component fail to
        // load, which is how this was found.
        crate::theme_host::bindings::lattice::plugin_host::theme::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // CR.3, for the TC.6 reason above: a multi-seam component providing
        // BOTH `grammar` and `help` is instantiated against this sync linker
        // for its grammar seam, and instantiation must satisfy EVERY import
        // the world declares, not only the ones that seam uses. Recording a
        // topic touches `PluginState` and nothing else, so it is safe here;
        // leaving it out would make the whole component fail to load.
        crate::help_host::bindings::lattice::plugin_host::help::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // LG.3c, for the same TC.6 reason: a multi-seam component providing
        // BOTH `grammar` and `language` instantiates against this sync linker
        // for its grammar seam, and instantiation must satisfy EVERY import
        // the world declares. Recording a language touches `PluginState` and
        // nothing else, so it is safe here.
        crate::language_host::bindings::lattice::plugin_host::language::add_to_linker::<
            _,
            HasSelf<_>,
        >(&mut grammar_linker, |state: &mut PluginState| state)
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // CR.4: and on the sync linker, which is the one `dashboard-plugin`
        // actually instantiates against — `render-section` runs inside the
        // compositor and must not suspend.
        crate::dashboard_host::bindings::lattice::plugin_host::dashboard::add_to_linker::<
            _,
            HasSelf<_>,
        >(&mut grammar_linker, |state: &mut PluginState| state)
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        crate::mode_host::bindings::lattice::plugin_host::modes::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // OM.11, for the same TC.6 reason: org provides BOTH `grammar` and
        // `picker-source` (refile's target list), and a picker source needs
        // `walk`. Instantiation must satisfy every import the world declares,
        // so without this the whole component fails to load on the grammar
        // seam — which is how this was found, and what the `multiseam` fixture
        // now pins (it imports `host-services` for exactly this reason).
        //
        // `walk` and `read-file` are sync, bounded host funcs. What this does
        // NOT wire is `logging`, still deliberately absent, so "no logging
        // reachable from the grammar hot path" stays structural.
        //
        // **This comment used to claim the guest could already read files here
        // through WASI, and that was wrong.** `add_to_linker_sync` does give the
        // guest a filesystem view, but `wasmtime-wasi`'s sync filesystem shim
        // blocks on a runtime internally, and this thread is already inside one
        // — so a grammar action calling `std::fs::read_to_string` panics with
        // "Cannot start a runtime from within a runtime" rather than reading
        // anything. Found by org's capture trying exactly that (OC.5a).
        //
        // That is why `read-file` exists on `host-services`: a host-side read,
        // capability-gated like `walk`, is the only read reachable from this
        // seam. Async seams (picker / completion, on the async `linker` above)
        // are unaffected and use WASI directly — org's picker source does.
        crate::lattice::plugin_host::host_services::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // OC.3 / ML.6, for the same TC.6 reason, and the plan for that slice
        // predicted otherwise — it said `ui` would be "wired on the async linker
        // only, so the modeline is structurally unreachable from the keystroke
        // path". That mechanism does not survive the Component Model. A
        // component's import set is fixed for the whole artefact, and org — the
        // plugin this seam exists for — provides `grammar` too, so an import
        // absent here fails the WHOLE plugin rather than one seam. Org has
        // already been broken in exactly this way once, by a single
        // `logging::log` call (see its world comment).
        //
        // The guarantee the plan wanted is kept one layer in instead:
        // `instantiate_grammar_plugin` clears `PluginState::ui`, so a grammar
        // action's `emit-segment` finds no context and warns + drops. That is
        // the `config` / `theme` / `keymap` shape, and unlike a linker omission
        // it is testable — `a_grammar_action_cannot_reach_the_modeline` is the
        // test.
        crate::lattice::plugin_host::ui::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        // OC.6, and the fifth instance of the same TC.6 reason. Org bridges a
        // chord to its own async side — the clock's session, wake and modeline
        // all live on its event actor (design D6) — so its component imports
        // `events`, and a component's import set is fixed for the whole
        // artefact. Without this the grammar seam of that same `.wasm` fails to
        // instantiate and org loses its entire keymap, not just its clock.
        //
        // All three functions are safe here. `subscribe` only records into
        // `PluginState`. `wake-every` finds `wake: None` on a grammar store —
        // that field is filled in by `spawn_event_plugin` alone — so it answers
        // `0` and warns, which is what keeps the periodic wake off the keystroke
        // path now that the import has to be reachable from it. `cancel-wake` on
        // a store with no wake context is a no-op by contract.
        crate::events_host::bindings::lattice::plugin_host::events::add_to_linker::<_, HasSelf<_>>(
            &mut grammar_linker,
            |state: &mut PluginState| state,
        )
        .map_err(|e| PluginHostError::Linker(e.into()))?;
        let epoch_ticker = EpochTicker::spawn(&engine, EPOCH_TICK_INTERVAL)?;

        Ok(Self {
            engine,
            linker,
            grammar_linker,
            cache,
            data_dir_base: data_dir_base.into(),
            next_id: AtomicU32::new(0),
            tracer: std::sync::OnceLock::new(),
            project: std::sync::OnceLock::new(),
            cancel: std::sync::OnceLock::new(),
            sleeper: std::sync::OnceLock::new(),
            ui: std::sync::OnceLock::new(),
            stores: Mutex::new(std::collections::HashMap::new()),
            _epoch_ticker: epoch_ticker,
        })
    }

    /// OT.3b: where a plugin's private data lives — `<base>/<id>/data/`, the
    /// same directory `build_plugin_wasi` grants it.
    ///
    /// Exposed so a seam adapter can persist across restarts beside the
    /// plugin's own data (the agenda result cache), and so uninstalling a
    /// plugin removes what it cached. `None` for an id that is not a safe
    /// directory name — the same refusal `build_plugin_wasi` makes, rather
    /// than a second opinion about it.
    pub fn plugin_data_dir(&self, plugin_id: &str) -> Option<PathBuf> {
        crate::manifest::is_safe_plugin_id(plugin_id)
            .then(|| self.data_dir_base.join(plugin_id).join("data"))
    }

    /// OR.1: the shared byte store for `plugin_id`, opened on first use and
    /// reused for every later seam instance of the same id.
    ///
    /// `None` for an id that is not a safe directory name — the same refusal
    /// [`plugin_data_dir`](Self::plugin_data_dir) makes, rather than a second
    /// opinion about it. Such a plugin gets no data mount either, so a store
    /// would have nowhere to live.
    fn store_for(&self, plugin_id: &str) -> Option<plugin_store::PluginStoreHandle> {
        let dir = self.plugin_data_dir(plugin_id)?;
        let mut stores = self
            .stores
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = stores.get(plugin_id) {
            return Some(existing.clone());
        }
        // The dir is created by `build_plugin_wasi` on the manifest paths, but
        // not on every path that names a plugin, so create it here too rather
        // than depending on an ordering the compiler cannot check. A failure
        // degrades at flush time (logged, dropped), never here.
        let _ = std::fs::create_dir_all(&dir);
        let handle: plugin_store::PluginStoreHandle =
            Arc::new(Mutex::new(plugin_store::PluginStore::open(&dir)));
        stores.insert(plugin_id.to_string(), handle.clone());
        Some(handle)
    }

    /// OR.1: read one key out of a plugin's store, host-side.
    ///
    /// The store is the guest's schema and the host never interprets it — this
    /// hands back the same opaque bytes `store-get` would, without
    /// instantiating anything. It exists because a *reader* of a plugin's index
    /// may be host-side (a multibuffer provider) and because a test that asked
    /// the writing guest what it wrote would pass against a per-instance store,
    /// which is the exact drift the store exists to prevent.
    ///
    /// `None` for an unknown plugin id, an unsafe one, or a key with nothing
    /// under it — the three are indistinguishable to a caller, and all three
    /// mean "build it".
    pub fn plugin_store_get(&self, plugin_id: &str, key: &str) -> Option<Vec<u8>> {
        let store = self.store_for(plugin_id)?;
        let guard = store.lock().unwrap_or_else(|p| p.into_inner());
        guard.get(key)
    }

    /// Install the boundary tracer (PO.5) — the loader calls this once, after it
    /// builds the tracer, so every subsequent instantiate/spawn stamps the
    /// plugin's `log_ctx` and the guest `logging` seam routes into the ring.
    /// Idempotent: a second call is ignored (the tracer is set once per host).
    pub fn set_tracer(&self, tracer: crate::trace::PluginTracerHandle) {
        let _ = self.tracer.set(tracer);
    }

    /// Build the [`LogCtx`] for a freshly-allocated `id` — `Some` once a tracer is
    /// installed ([`set_tracer`](Self::set_tracer)), else `None` (a guest `log`
    /// then degrades to a debug-drop). Stamped onto the store in each async
    /// instantiate/spawn path.
    fn log_ctx_for(&self, id: PluginId) -> Option<LogCtx> {
        self.tracer.get().map(|tracer| LogCtx {
            plugin: id.0,
            tracer: tracer.clone(),
        })
    }

    /// PR.6: hand the host what the guest `project` seam answers from.
    ///
    /// Called once at boot after the resolver is registered. Idempotent —
    /// a second call is ignored, like [`set_tracer`](Self::set_tracer).
    pub fn set_project_context(
        &self,
        resolver: lattice_core::ProjectResolverHandle,
        buffers: lattice_mode::BufferStoreHandle,
    ) {
        let _ = self.project.set(ProjectCtx { resolver, buffers });
    }

    /// CG.4: hand the host the foreground-cancel registry, so a running
    /// guest call can be interrupted by `<C-g>`.
    ///
    /// Idempotent — a second call is ignored, like [`set_tracer`](Self::set_tracer).
    pub fn set_foreground_cancel(&self, cancel: lattice_mode::ForegroundCancelHandle) {
        let _ = self.cancel.set(cancel);
    }

    /// OC.2: hand the host the timer the `wake-every` seam sleeps on.
    ///
    /// This crate owns no runtime, so it cannot construct one — the caller that
    /// spawns the actors is the one that has an executor to sleep on, and it
    /// supplies the [`Sleeper`] here. Unset leaves `wake-every` answering `0`.
    ///
    /// Idempotent — a second call is ignored, like [`set_tracer`](Self::set_tracer).
    pub fn set_sleeper(&self, sleeper: wake::SleeperHandle) {
        let _ = self.sleeper.set(sleeper);
    }

    /// OC.3 / ML.6: hand the host what a plugin's `ui` modeline calls act on.
    ///
    /// Both halves at once, on the `set_project_context` reasoning: a registry
    /// with no bus would let a plugin register a descriptor and push content
    /// that nothing ever repaints — half-wired, and half-wired is the failure
    /// mode this seam is most likely to have (`plugin-gates-hand-guests-
    /// throwaway-contexts`). Unset leaves `register-segment` returning `false`.
    ///
    /// Idempotent — a second call is ignored, like [`set_tracer`](Self::set_tracer).
    pub fn set_modeline(&self, modeline: lattice_mode::ModelineServiceHandle, bus: Arc<EventBus>) {
        let _ = self.ui.set(ui_host::UiCtx { modeline, bus });
    }

    /// Allocate the next host-issued [`PluginId`]. Monotonic and unique for the
    /// host's lifetime; the guest never influences it.
    fn alloc_id(&self) -> PluginId {
        PluginId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Number of module-cache hits so far (a compiled artifact was reused from
    /// disk instead of recompiled). Exposed for tests and future observability.
    pub fn cache_hits(&self) -> usize {
        self.cache.cache_hits()
    }

    /// Number of module-cache misses so far (a component was compiled and its
    /// artifact written to the cache).
    pub fn cache_misses(&self) -> usize {
        self.cache.cache_misses()
    }

    /// Compile component bytes (AOT via Cranelift) into a reusable
    /// [`Component`]. Malformed / non-component input returns
    /// [`PluginHostError::Compile`] — no panic. Compilation is synchronous
    /// regardless of the async engine.
    pub fn compile(&self, bytes: &[u8]) -> Result<Component, PluginHostError> {
        Component::new(&self.engine, bytes).map_err(|e| PluginHostError::Compile(e.into()))
    }

    /// Instantiate a compiled component into a live [`LoadedPlugin`] with the
    /// default per-call budget and **no capability grant** (an empty WASI view
    /// — zero filesystem access, no data dir). This is the degenerate load; a
    /// real plugin uses [`instantiate_plugin`](Self::instantiate_plugin) with a
    /// manifest.
    pub async fn instantiate(
        &self,
        component: &Component,
    ) -> Result<LoadedPlugin, PluginHostError> {
        self.instantiate_with_budget(component, PluginBudget::default())
            .await
    }

    /// Instantiate a compiled component with an explicit per-call budget and
    /// **no capability grant** (empty WASI view). See [`instantiate`](Self::instantiate).
    pub async fn instantiate_with_budget(
        &self,
        component: &Component,
        budget: PluginBudget,
    ) -> Result<LoadedPlugin, PluginHostError> {
        // No grant, no data dir: the guest reaches no filesystem at all — and a
        // host-services `walk` from such a plugin is denied (empty grant).
        let wasi = WasiCtxBuilder::new().build();
        let (mut store, bindings) = self
            .instantiate_inner(component, wasi, CapabilityGrant::default(), budget, None)
            .await?;
        // PO.5: stamp the logging route once the id is allocated, so a `plugin`-
        // world guest's `log` reaches the tracer.
        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);
        Ok(LoadedPlugin {
            store,
            bindings,
            budget,
            id,
            grant: CapabilityGrant::default(),
            denied: Vec::new(),
            data_dir: None,
        })
    }

    /// Instantiate a plugin **under its capability grant** (PH7.2, fragment §6).
    ///
    /// The grant is computed from `manifest` + `tier`; a private data dir
    /// (`<data-base>/<manifest.id>/data/`) is created and mounted writable, and
    /// each granted `fs:*` prefix is preopened at its own path. The resulting
    /// [`Store`]'s WASI view reaches **exactly** the granted filesystem and
    /// nothing else — a plugin without an `fs:write` grant cannot write outside
    /// its data dir at the WASI layer. Requested capabilities the tier withheld
    /// (e.g. `proc:spawn` for a user-installed plugin) are surfaced on
    /// [`LoadedPlugin::denied_capabilities`] so the host can notify the user;
    /// the load still succeeds (graceful degradation).
    ///
    /// Each instantiation gets its own `Store` (the isolation boundary) and a
    /// fresh host-issued [`PluginId`]. Instantiation runs with generous
    /// [`INSTANTIATION_FUEL`]; the tighter [`PluginBudget`] applies per call.
    pub async fn instantiate_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
    ) -> Result<LoadedPlugin, PluginHostError> {
        let (wasi, outcome, data_dir) = self.build_plugin_wasi(manifest, tier);
        let (mut store, bindings) = self
            .instantiate_inner(
                component,
                wasi,
                outcome.grant.clone(),
                budget,
                Some(&manifest.id),
            )
            .await?;
        // PO.5: stamp the logging route once the id is allocated.
        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);
        Ok(LoadedPlugin {
            store,
            bindings,
            budget,
            id,
            grant: outcome.grant,
            denied: outcome.denied,
            data_dir: Some(data_dir),
        })
    }

    /// Shared instantiation core: build the `Store` around `wasi`, arm the
    /// instantiation fuel/epoch, and instantiate the component against the
    /// WASI-populated linker.
    async fn instantiate_inner(
        &self,
        component: &Component,
        wasi: WasiCtx,
        grant: CapabilityGrant,
        budget: PluginBudget,
        name: Option<&str>,
    ) -> Result<(Store<PluginState>, Plugin), PluginHostError> {
        let mut store = self.new_store(wasi, grant, budget, name)?;
        let bindings = Plugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        Ok((store, bindings))
    }

    /// Build a per-plugin `Store` around `wasi` + `grant` and arm it with the
    /// generous [`INSTANTIATION_FUEL`] + `budget`'s epoch (the start function's
    /// allowance; per-call arming via [`arm_store`] happens before each export).
    /// Shared by the lifecycle-world instantiation and the picker actor spawn
    /// (`picker_task`) so both build the same scoped `Store`.
    fn new_store(
        &self,
        wasi: WasiCtx,
        grant: CapabilityGrant,
        budget: PluginBudget,
        // The plugin's manifest id — drives config-option auto-namespacing. `None`
        // only for internal callers with no manifest (they register no options).
        name: Option<&str>,
    ) -> Result<Store<PluginState>, PluginHostError> {
        let state = PluginState {
            wasi,
            table: ResourceTable::new(),
            grant,
            grammar_contributions: grammar_host::GrammarContributions::default(),
            event_subscriptions: events_host::EventContributions::default(),
            // Set by `spawn_event_plugin` once the plugin's id is allocated; a
            // plugin not spawned onto a bus cannot emit (warn + drop).
            event_emit: None,
            // Set by `spawn_config_plugin`; a plugin not spawned onto a registry
            // cannot register/read options (warn + false / none).
            config_registry: None,
            // From the manifest id; drives config-option auto-namespacing.
            plugin_name: name.map(str::to_string),
            config_contributions: Vec::new(),
            theme_registry: None,
            theme_contributions: Vec::new(),
            help_contributions: Vec::new(),
            language_contributions: Vec::new(),
            dashboard_contributions: Vec::new(),
            // Drained by `spawn_mode_plugin` into the `ModeRegistry` after
            // `register-modes` returns (PH7.11a).
            mode_contributions: mode_host::ModeContributions::default(),
            // Set by `spawn_keymap_plugin`; a plugin not spawned onto a keymap
            // cannot bind (register-binding → false).
            keymap_ctx: None,
            keymap_contributions: Vec::new(),
            // Stamped by each async instantiate/spawn path (`log_ctx_for`) once
            // the id is allocated; `None` here + for the sync grammar guest (which
            // never imports `logging`) → a guest `log` is a debug-drop.
            log_ctx: None,
            // PR.6: stamped for every store below, not per-spawn-path like
            // `log_ctx` — resolution needs no plugin id, so there is nothing
            // to wait for and no path that can forget it.
            project: self.project.get().cloned(),
            cancel: self.cancel.get().cloned(),
            cancel_token: None,
            epoch_spent: 0,
            // OC.3: stamped for every store here rather than per-spawn-path, on
            // the `project` reasoning — there is nothing to wait for, so no path
            // can forget it. The one store that must NOT have it, the sync
            // grammar store, clears it explicitly in
            // `instantiate_grammar_plugin`, which is a line a reader can find.
            ui: self.ui.get().cloned(),
            // OC.2: only `spawn_event_plugin` fills this in — it is the one path
            // whose actor has somewhere to deliver an `on-wake`.
            wake: None,
            // OR.1: stamped for every store here, on the `project` / `ui`
            // reasoning — it needs no allocated plugin id, so there is nothing
            // to wait for and no spawn path that can forget it. A seam holding
            // a store its sibling seams cannot see IS the drift this exists to
            // prevent, so the wiring must not be per-path.
            store: name.and_then(|id| self.store_for(id)),
            // OR.2: empty until the guest arms one; dropped with this `Store`.
            watches: std::collections::HashMap::new(),
            require_contributions: Default::default(),
        };
        let mut store = Store::new(&self.engine, state);
        store
            .set_fuel(INSTANTIATION_FUEL)
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        // Default epoch behaviour is to trap on deadline; arm it generously
        // for instantiation (per-call arming happens before each export).
        store.set_epoch_deadline(budget.epoch_deadline);
        Ok(store)
    }

    /// Compute a plugin's grant from `manifest` + `tier`, create its private
    /// data dir, and build the scoped WASI view — the front half of
    /// [`instantiate_plugin`](Self::instantiate_plugin), factored out so the
    /// picker actor spawn (`picker_task`) instantiates under the identical
    /// capability model. A data dir that cannot be created degrades to "no data
    /// mount" (a `warn!`, never a failed load — fragment §6).
    fn build_plugin_wasi(
        &self,
        manifest: &PluginManifest,
        tier: TrustTier,
    ) -> (WasiCtx, GrantOutcome, PathBuf) {
        let outcome = grant(manifest, tier);
        // SECURITY (isolation, defense-in-depth): the id is validated at parse
        // (`from_toml_str` rejects a path-escaping id), but a programmatic
        // `PluginManifest::new` bypasses that. Re-check HERE — the true security
        // boundary — before joining the id into a WRITABLE mount path: an unsafe
        // id degrades to NO data mount (an empty scoped WASI view), never an
        // out-of-sandbox writable dir. A crafted `/etc/cron.d` / `../../.ssh` id
        // can therefore never relocate the mount, grant or no grant.
        let data_dir = if crate::manifest::is_safe_plugin_id(&manifest.id) {
            let dir = self.data_dir_base.join(&manifest.id).join("data");
            if let Err(err) = std::fs::create_dir_all(&dir) {
                tracing::warn!(
                    path = %dir.display(),
                    error = %err,
                    "plugin data dir create failed; the data mount is degraded"
                );
            }
            dir
        } else {
            tracing::error!(
                id = %manifest.id,
                "plugin id is not a safe path component; refusing the data mount (no /data)"
            );
            // A path inside the base that is NOT preopened (no create, no mount):
            // build_wasi_ctx only preopens dirs it is told to; an absent data dir
            // means the guest simply has no /data. Return the (unmounted) intended
            // path for the caller's bookkeeping without ever creating/mounting it.
            self.data_dir_base.join("__invalid__").join("data")
        };
        let wasi = build_wasi_ctx(&outcome.grant, &data_dir);
        (wasi, outcome, data_dir)
    }
}

/// A live plugin instance: its `Store` (holding the scoped WASI view), the
/// lifecycle bindings, the per-call budget, its host-issued identity and
/// effective grant. Dropping it tears the `Store` down (the reload/teardown
/// seam PH7.12 formalises).
pub struct LoadedPlugin {
    store: Store<PluginState>,
    bindings: Plugin,
    budget: PluginBudget,
    id: PluginId,
    grant: CapabilityGrant,
    denied: Vec<Capability>,
    data_dir: Option<PathBuf>,
}

impl LoadedPlugin {
    /// The host-issued identity for this instance. The guest never influences
    /// it; it is the numeric id inside this plugin's provenance.
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// The provenance layer the host stamps for every contribution this plugin
    /// registers (PH7.3+). Host-issued from [`PluginId`], so a plugin cannot
    /// forge a builtin/user provenance — the acid test of §6.
    pub fn source_layer(&self) -> SourceLayer {
        SourceLayer::Plugin(self.id.0)
    }

    /// The effective capability grant this plugin runs under.
    pub fn grant(&self) -> &CapabilityGrant {
        &self.grant
    }

    /// Requested capabilities the trust tier withheld (e.g. `proc:spawn` for a
    /// user-installed plugin). The host turns these into a "loaded with reduced
    /// function" notification; the plugin still loaded.
    pub fn denied_capabilities(&self) -> &[Capability] {
        &self.denied
    }

    /// The plugin's private data dir, if it was instantiated with a grant
    /// ([`PluginHost::instantiate_plugin`]). `None` for the degenerate
    /// no-grant load.
    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    /// Re-arm the fuel + epoch budget before a lifecycle call, so each call
    /// gets a fresh allowance rather than sharing a running total.
    fn arm_budget(&mut self) -> Result<(), PluginHostError> {
        arm_store(&mut self.store, self.budget)
    }

    /// Call the component's `activate` export. For `init.rs` this runs the
    /// user's configuration; for the no-op component it returns immediately.
    /// Fuel/epoch exhaustion or any wasm trap surfaces as
    /// [`PluginHostError::Trap`] and leaves the host live.
    pub async fn activate(&mut self) -> Result<(), PluginHostError> {
        self.arm_budget()?;
        self.bindings
            .call_activate(&mut self.store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "activate",
                kind: classify_trap(&source),
                source: source.into(),
            })
    }

    /// Call the component's `deactivate` export (teardown / reload).
    pub async fn deactivate(&mut self) -> Result<(), PluginHostError> {
        self.arm_budget()?;
        self.bindings
            .call_deactivate(&mut self.store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "deactivate",
                kind: classify_trap(&source),
                source: source.into(),
            })
    }
}
