//! Plugin host — the WASM Component Model extension substrate (Phase 7).
//!
//! Design fragment: `docs/dev/architecture/plugin-host.md`. Slice plan:
//! `docs/dev/operations/slice-plans/plugin-host.md`. Spec: `design.md` §5.5.
//!
//! **PH7.0 scaffold.** This slice ships the minimal host: a wasmtime
//! [`Engine`], the host side of `wasmtime::component::bindgen!` bound to the
//! `plugin` lifecycle world (`../../wit/plugin.wit`), and a [`PluginHost`]
//! that compiles a component and instantiates it, calling its `activate` /
//! `deactivate` exports. The first consumer of this lifecycle world is the
//! user's `init.rs` compiled to WASM; the degenerate case — a component whose
//! `activate` registers nothing — is exactly what the tests instantiate to
//! prove the round-trip.
//!
//! Deliberately out of scope here (owned by later slices, kept honest rather
//! than half-built): the async ABI, Store-per-plugin tokio tasks, fuel /
//! epoch limits, and the on-disk module cache (PH7.1); the per-plugin WASI
//! capability view (PH7.2); every contribution seam (PH7.3+).

use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

// Host bindings for the `plugin` lifecycle world. Synchronous for the
// scaffold; the async ABI (design.md §5.5) lands with the runtime core
// (PH7.1). The generated `Plugin` type carries `call_activate` /
// `call_deactivate`.
wasmtime::component::bindgen!({
    world: "plugin",
    path: "../../wit",
});

/// Per-`Store` host state.
///
/// Empty in the scaffold — there are no host imports to service yet. PH7.2
/// grows this into the plugin's WASI view, resource tables, capability grant,
/// and fuel meter (`design.md` §5.5, plugin-host.md §3).
struct PluginState;

/// Typed error surface for the plugin host. No host path panics — every
/// failure mode (bad engine config, malformed component bytes, instantiation
/// failure, a trapping lifecycle export) is a value here, per the
/// four-artefact graceful-error clause. `anyhow::Error` is wasmtime's error
/// type; each variant carries it as `#[source]`.
#[derive(Debug, thiserror::Error)]
pub enum PluginHostError {
    /// The wasmtime engine could not be built from the host config.
    #[error("failed to build the wasmtime engine")]
    Engine(#[source] anyhow::Error),

    /// Component bytes were malformed or failed AOT compilation. This is the
    /// path a garbage `.wasm` / non-component input takes — rejected as a
    /// value, never a panic.
    #[error("failed to compile the plugin component")]
    Compile(#[source] anyhow::Error),

    /// The component compiled but could not be instantiated (e.g. an import
    /// the host does not provide).
    #[error("failed to instantiate the plugin component")]
    Instantiate(#[source] anyhow::Error),

    /// A lifecycle export trapped (panic, and — once PH7.1 lands — fuel
    /// exhaustion or an epoch deadline). The offending call is a no-op with a
    /// typed error; the host stays live.
    #[error("plugin lifecycle export `{func}` failed")]
    Lifecycle {
        /// The export that failed (`"activate"` / `"deactivate"`).
        func: &'static str,
        /// The underlying wasmtime trap / error.
        #[source]
        source: anyhow::Error,
    },
}

/// The wasmtime engine plus the (currently import-free) component linker.
///
/// One host per editor process; it compiles and instantiates plugin
/// components. Cheap to hold, expensive to build (the engine owns Cranelift),
/// so construct it once.
pub struct PluginHost {
    engine: Engine,
    linker: Linker<PluginState>,
}

impl PluginHost {
    /// Build a host with the default engine configuration.
    ///
    /// The linker is empty: the scaffold's lifecycle world imports nothing,
    /// so a no-op component instantiates against it directly. PH7.2 adds the
    /// WASI view and PH7.3+ add the host-services imports.
    pub fn new() -> Result<Self, PluginHostError> {
        let engine = Engine::default();
        let linker = Linker::new(&engine);
        Ok(Self { engine, linker })
    }

    /// Compile component bytes (AOT via Cranelift) into a reusable
    /// [`Component`]. Malformed / non-component input returns
    /// [`PluginHostError::Compile`] — no panic.
    pub fn compile(&self, bytes: &[u8]) -> Result<Component, PluginHostError> {
        Component::new(&self.engine, bytes).map_err(|e| PluginHostError::Compile(e.into()))
    }

    /// Instantiate a compiled component into a live [`LoadedPlugin`].
    ///
    /// Each instantiation gets its own [`Store`] — the Store-per-plugin
    /// isolation boundary the runtime core (PH7.1) drives on its own task.
    pub fn instantiate(&self, component: &Component) -> Result<LoadedPlugin, PluginHostError> {
        let mut store = Store::new(&self.engine, PluginState);
        let bindings = Plugin::instantiate(&mut store, component, &self.linker)
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        Ok(LoadedPlugin { store, bindings })
    }
}

/// A live plugin instance: its `Store` and the lifecycle bindings.
///
/// Dropping it tears the `Store` down (the reload/teardown seam PH7.12
/// formalises). The `Store` is private so `PluginState` stays an internal
/// detail of the host.
pub struct LoadedPlugin {
    store: Store<PluginState>,
    bindings: Plugin,
}

impl LoadedPlugin {
    /// Call the component's `activate` export. For `init.rs` this runs the
    /// user's configuration; for the scaffold's no-op component it returns
    /// immediately. A trap surfaces as [`PluginHostError::Lifecycle`].
    pub fn activate(&mut self) -> Result<(), PluginHostError> {
        self.bindings
            .call_activate(&mut self.store)
            .map_err(|source| PluginHostError::Lifecycle {
                func: "activate",
                source: source.into(),
            })
    }

    /// Call the component's `deactivate` export (teardown / reload).
    pub fn deactivate(&mut self) -> Result<(), PluginHostError> {
        self.bindings
            .call_deactivate(&mut self.store)
            .map_err(|source| PluginHostError::Lifecycle {
                func: "deactivate",
                source: source.into(),
            })
    }
}
