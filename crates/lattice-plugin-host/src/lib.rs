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
//! Still owned by later slices: the on-disk module cache + lazy instantiation
//! (PH7.1b); the per-plugin WASI capability view (PH7.2); every contribution
//! seam (PH7.3+). The first consumer of the `plugin` lifecycle world is the
//! user's `init.rs`; the no-op component the tests instantiate is the
//! degenerate `init.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

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

    /// Component bytes were malformed or failed AOT compilation.
    #[error("failed to compile the plugin component")]
    Compile(#[source] anyhow::Error),

    /// The component compiled but could not be instantiated (a missing
    /// import, or fuel exhausted during the start function).
    #[error("failed to instantiate the plugin component")]
    Instantiate(#[source] anyhow::Error),

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
}

/// Classify a wasmtime call error into a [`TrapKind`] for reporting.
fn classify_trap(err: &wasmtime::Error) -> TrapKind {
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

/// Per-`Store` host state. Empty in the scaffold — no host imports to service
/// yet. PH7.2 grows this into the plugin's WASI view, resource tables,
/// capability grant, and fuel meter.
struct PluginState;

/// The wasmtime engine, the (import-free) component linker, and the epoch
/// ticker. One host per editor process; construct it once (the engine owns
/// Cranelift).
pub struct PluginHost {
    engine: Engine,
    linker: Linker<PluginState>,
    // Dropped last-in-first-out after `engine`/`linker`; keeps the ticker
    // alive for the host's lifetime and stops it on drop.
    _epoch_ticker: EpochTicker,
}

impl PluginHost {
    /// Build a host with the async engine configuration: async support (the
    /// canonical ABI), fuel metering, and epoch interruption. Spawns the
    /// epoch-ticker thread.
    pub fn new() -> Result<Self, PluginHostError> {
        let mut config = Config::new();
        // Async is always available on the engine in wasmtime 46; the generated
        // exports are async (see the `bindgen!` above). Fuel + epoch give the
        // two hard per-call budgets.
        config.consume_fuel(true);
        config.epoch_interruption(true);

        let engine = Engine::new(&config).map_err(|e| PluginHostError::Engine(e.into()))?;
        let linker = Linker::new(&engine);
        let epoch_ticker = EpochTicker::spawn(&engine, EPOCH_TICK_INTERVAL);

        Ok(Self {
            engine,
            linker,
            _epoch_ticker: epoch_ticker,
        })
    }

    /// Compile component bytes (AOT via Cranelift) into a reusable
    /// [`Component`]. Malformed / non-component input returns
    /// [`PluginHostError::Compile`] — no panic. Compilation is synchronous
    /// regardless of the async engine.
    pub fn compile(&self, bytes: &[u8]) -> Result<Component, PluginHostError> {
        Component::new(&self.engine, bytes).map_err(|e| PluginHostError::Compile(e.into()))
    }

    /// Instantiate a compiled component into a live [`LoadedPlugin`] with the
    /// default per-call budget.
    pub async fn instantiate(
        &self,
        component: &Component,
    ) -> Result<LoadedPlugin, PluginHostError> {
        self.instantiate_with_budget(component, PluginBudget::default())
            .await
    }

    /// Instantiate a compiled component into a live [`LoadedPlugin`], setting
    /// the per-call resource budget its lifecycle exports will run under.
    ///
    /// Each instantiation gets its own [`Store`] — the Store-per-plugin
    /// isolation boundary. Instantiation itself runs with generous
    /// [`INSTANTIATION_FUEL`]; the tighter [`PluginBudget`] applies per
    /// lifecycle call.
    pub async fn instantiate_with_budget(
        &self,
        component: &Component,
        budget: PluginBudget,
    ) -> Result<LoadedPlugin, PluginHostError> {
        let mut store = Store::new(&self.engine, PluginState);
        store
            .set_fuel(INSTANTIATION_FUEL)
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        // Default epoch behaviour is to trap on deadline; arm it generously
        // for instantiation (per-call arming happens before each export).
        store.set_epoch_deadline(budget.epoch_deadline);

        let bindings = Plugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;

        Ok(LoadedPlugin {
            store,
            bindings,
            budget,
        })
    }
}

/// A live plugin instance: its `Store`, the lifecycle bindings, and the
/// per-call budget. Dropping it tears the `Store` down (the reload/teardown
/// seam PH7.12 formalises).
pub struct LoadedPlugin {
    store: Store<PluginState>,
    bindings: Plugin,
    budget: PluginBudget,
}

impl LoadedPlugin {
    /// Re-arm the fuel + epoch budget before a lifecycle call, so each call
    /// gets a fresh allowance rather than sharing a running total.
    fn arm_budget(&mut self) -> Result<(), PluginHostError> {
        self.store
            .set_fuel(self.budget.fuel)
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        self.store.set_epoch_deadline(self.budget.epoch_deadline);
        Ok(())
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
