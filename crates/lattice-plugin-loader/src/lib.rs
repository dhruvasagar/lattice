//! `lattice-plugin-loader` — the editor-side plugin loader (Phase 8).
//!
//! Phase 7 shipped the plugin *runtime* ([`lattice_plugin_host`]): the wasmtime
//! engine, the WIT API package, the capability/fuel/crash model, and every
//! extension seam, each exercised end-to-end by guest fixtures. That crate is
//! deliberately **substrate-neutral** — it owns "engine + seams" and knows
//! nothing about the editor: no XDG discovery, no ex-commands, no native
//! registries. A headless test harness or a future non-editor host can drive it
//! unchanged.
//!
//! This crate is the **subsystem that composes the runtime with the editor's
//! native registries**. It discovers plugins on disk, loads them
//! (`compile → instantiate_plugin → activate`, then — PL8.B — drains each seam's
//! contribution into its native registry), owns the loaded-plugin state past
//! boot as a service, and exposes the user-facing load/unload/reload surface
//! (PL8.C).
//!
//! # Where this sits (the loader-home decision)
//!
//! Three homes were weighed (slice plan, "where the loader lives"): inline in
//! `lattice-host`, folded into `lattice-plugin-host`, or a dedicated crate. This
//! is the dedicated crate — the genuinely-better long-term fit (heuristic #1):
//! the runtime crate stays "engine + seams"; inlining in the host would grow
//! `Editor::` methods + a host dispatch arm (the half-migration the
//! mode-ownership acid test forbids). The loader reaches the native registries
//! through the same [`SubsystemBoot`](lattice_mode::SubsystemBoot) seam every
//! other subsystem installs through, so wiring it into the editor is one line
//! ([`install`]) and zero host internals.
//!
//! # Status (PL8.A)
//!
//! PL8.A stands the host up at boot and proves the `compile → instantiate →
//! activate` spine end-to-end, loading *no* real plugins yet. On-disk discovery
//! + the seam→registry drain are PL8.B; the ex-command surface is PL8.C.
//!
//! Design: `docs/dev/architecture/plugin-host.md`,
//! `docs/dev/architecture/boot-composition.md`. Slice plan:
//! `docs/dev/operations/slice-plans/plugin-loader.md`.

pub mod install;

pub use install::install;

use std::sync::{Arc, Mutex};

use lattice_plugin_host::{
    LoadedPlugin, ManifestError, PluginBudget, PluginHost, PluginHostError, PluginId,
    PluginManifest, TrustTier,
};

/// The service handle other layers reach the loader through — the ex-command
/// surface (PL8.C), the plugin-manager view (PL8.H). Per the `ServiceRegistry`
/// Arc/TypeId rule, register **and** look up with this exact alias
/// (`Arc<PluginLoader>`), never a bare `PluginLoader`.
pub type PluginLoaderHandle = Arc<PluginLoader>;

/// A live loaded plugin: its host-issued [`PluginId`], its manifest id (the
/// user-facing name, and the key for `:plugin-unload <name>` — PL8.C), and the
/// live instance whose `Store` must stay alive for the plugin's contributions
/// to remain valid.
struct LoadedRecord {
    #[allow(dead_code)] // read by PL8.C unload / PL8.H manager view.
    id: PluginId,
    name: String,
    /// Kept alive: dropping [`LoadedPlugin`] drops its `Store<PluginState>` —
    /// the plugin's entire runtime footprint. PL8.B attaches the seam actors +
    /// the [`PluginTeardown`](lattice_plugin_host::PluginTeardown) token that
    /// reverses each contributed surface; PL8.A holds the bare instance so the
    /// spine is honest (a loaded plugin is a *live* instance, not a handle).
    _plugin: LoadedPlugin,
}

/// The plugin loader subsystem: owns the runtime handle and the loaded-plugin
/// set, and drives the `compile → instantiate → activate` spine. Stood up at
/// boot by [`install`], which registers it as a [`PluginLoaderHandle`] service
/// so the user surface reaches it generically.
pub struct PluginLoader {
    host: Arc<PluginHost>,
    /// `std::sync::Mutex`, not `tokio::sync::Mutex`: the lock is only ever taken
    /// to push / read the loaded set *after* the async load work completes,
    /// never held across an `.await`, so a blocking mutex is correct and
    /// cheaper than an async one.
    loaded: Mutex<Vec<LoadedRecord>>,
}

/// Why a plugin failed to load. Every variant is graceful-degradation input for
/// the caller (PL8.B logs + skips; the editor never aborts boot on one bad
/// plugin) — the load path returns a value, never panics.
#[derive(Debug, thiserror::Error)]
pub enum PluginLoaderError {
    /// The manifest was malformed or declared an unrecognised capability.
    #[error("plugin manifest invalid: {0}")]
    Manifest(#[from] ManifestError),
    /// The component failed to compile, instantiate, or activate — a wasm trap,
    /// fuel/epoch exhaustion, or a capability failure. Carries the runtime's
    /// typed cause.
    #[error("plugin runtime error: {0}")]
    Host(#[from] PluginHostError),
}

impl PluginLoader {
    /// Construct a loader over `host`. The loaded set starts empty; [`install`]
    /// registers the handle as a service and (PL8.B) drives discovery.
    pub fn new(host: Arc<PluginHost>) -> Self {
        Self {
            host,
            loaded: Mutex::new(Vec::new()),
        }
    }

    /// The number of currently-loaded plugins. The spine proof + the PL8.H
    /// manager view read it.
    pub fn loaded_count(&self) -> usize {
        self.loaded
            .lock()
            .expect("plugin-loader loaded-set mutex poisoned")
            .len()
    }

    /// Whether a plugin with manifest id `name` is currently loaded (the
    /// `:plugin-unload <name>` / `:plugin-reload <name>` resolution, PL8.C).
    pub fn is_loaded(&self, name: &str) -> bool {
        self.loaded
            .lock()
            .expect("plugin-loader loaded-set mutex poisoned")
            .iter()
            .any(|r| r.name == name)
    }

    /// The load spine: compile `bytes`, instantiate under `manifest` + `tier`,
    /// activate, and record the live instance. Returns the host-issued
    /// [`PluginId`].
    ///
    /// Any failure surfaces as a typed [`PluginLoaderError`] and leaves the
    /// loader **and the editor live** — no partial record is stored on error,
    /// and the caller (PL8.B discovery) logs + skips so one bad plugin never
    /// aborts boot or another plugin.
    ///
    /// PL8.A drives *only* the lifecycle spine — no seam actors, no registry
    /// drain (that is PL8.B). `manifest` + `tier` thread the capability model
    /// through from the first load so it is honest from day one (a
    /// `TrustTier::UserInstalled` plugin's withheld capabilities are already
    /// computed, even though nothing consumes them until PL8.C surfaces them).
    pub async fn load_component(
        &self,
        bytes: &[u8],
        manifest: &PluginManifest,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        // `compile` is synchronous (Cranelift AOT, cached on disk per PH7.1b);
        // `instantiate_plugin` + `activate` are async and run on the caller's
        // multi-thread pool, never the current-thread editor actor.
        let component = self.host.compile(bytes)?;
        let mut plugin = self
            .host
            .instantiate_plugin(&component, manifest, tier, PluginBudget::default())
            .await?;
        plugin.activate().await?;

        let id = plugin.id();
        let record = LoadedRecord {
            id,
            name: manifest.id.clone(),
            _plugin: plugin,
        };
        // The lock is taken only here — after every `.await`, never across one.
        self.loaded
            .lock()
            .expect("plugin-loader loaded-set mutex poisoned")
            .push(record);
        // One-shot, user-actionable event (the "LSP server attached" class) —
        // `info!`, not `debug!`, per the diagnostic-logs rule.
        tracing::info!(plugin = %manifest.id, id = id.0, "plugin loaded");
        Ok(id)
    }
}
