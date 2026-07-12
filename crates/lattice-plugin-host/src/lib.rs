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
pub mod boundary_effect;
pub mod boundary_grammar;
pub mod boundary_picker;
pub mod buffer;
pub mod capability;
pub mod host_services;
pub mod completion_host;
pub mod completion_source;
pub mod completion_task;
pub mod grammar_host;
pub mod grammar_trampoline;
pub mod manifest;
pub mod picker_host;
pub mod picker_source;
pub mod picker_task;
pub mod trampoline;

pub use boundary::WitBoundary;
pub use capability::{
    CapabilityGrant, FsGrant, GrantOutcome, PreopenSpec, TrustTier, build_wasi_ctx, grant,
};
pub use completion_source::WasmCompletionSource;
pub use completion_task::{CompletionActor, CompletionClient};
pub use manifest::{Capability, CapabilityParseError, ManifestError, PluginManifest};
pub use picker_source::WasmPickerSource;
pub use picker_task::{PickerActor, PickerClient};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use lattice_grammar::SourceLayer;
use wasmtime::component::{Component, HasSelf, Linker};
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
    /// Any other wasm trap (unreachable, out-of-bounds, guest panic).
    Other,
}

impl std::fmt::Display for TrapKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TrapKind::Fuel => "out of fuel",
            TrapKind::Epoch => "epoch deadline exceeded",
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
    store.set_epoch_deadline(budget.epoch_deadline);
    Ok(())
}

/// Classify a wasmtime call error into a [`TrapKind`] for reporting.
pub(crate) fn classify_trap(err: &wasmtime::Error) -> TrapKind {
    match err.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => TrapKind::Fuel,
        Some(wasmtime::Trap::Interrupt) => TrapKind::Epoch,
        _ => TrapKind::Other,
    }
}

/// A background thread that bumps the engine epoch on a fixed interval, so
/// per-store epoch deadlines actually fire. Stopped and joined on drop.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn spawn(engine: &Engine, interval: Duration) -> Self {
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
            .expect("spawning the epoch-ticker thread");
        Self {
            stop,
            handle: Some(handle),
        }
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
    // Monotonic source of host-issued `PluginId`s. `&self` methods allocate,
    // so this is atomic.
    next_id: AtomicU32,
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
        let epoch_ticker = EpochTicker::spawn(&engine, EPOCH_TICK_INTERVAL);

        Ok(Self {
            engine,
            linker,
            grammar_linker,
            cache,
            data_dir_base: data_dir_base.into(),
            next_id: AtomicU32::new(0),
            _epoch_ticker: epoch_ticker,
        })
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
        let (store, bindings) = self
            .instantiate_inner(component, wasi, CapabilityGrant::default(), budget)
            .await?;
        Ok(LoadedPlugin {
            store,
            bindings,
            budget,
            id: self.alloc_id(),
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
        let (store, bindings) = self
            .instantiate_inner(component, wasi, outcome.grant.clone(), budget)
            .await?;
        Ok(LoadedPlugin {
            store,
            bindings,
            budget,
            id: self.alloc_id(),
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
    ) -> Result<(Store<PluginState>, Plugin), PluginHostError> {
        let mut store = self.new_store(wasi, grant, budget)?;
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
    ) -> Result<Store<PluginState>, PluginHostError> {
        let state = PluginState {
            wasi,
            table: ResourceTable::new(),
            grant,
            grammar_contributions: grammar_host::GrammarContributions::default(),
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
        let data_dir = self.data_dir_base.join(&manifest.id).join("data");
        if let Err(err) = std::fs::create_dir_all(&data_dir) {
            tracing::warn!(
                path = %data_dir.display(),
                error = %err,
                "plugin data dir create failed; the data mount is degraded"
            );
        }
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
