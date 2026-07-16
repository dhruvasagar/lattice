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
//! native registries**. It discovers plugins on disk ([`discovery`]), loads
//! them (`compile → spawn each declared seam → drain the contribution into its
//! native registry`), owns the loaded-plugin state past boot as a service, and
//! (PL8.C) will expose the user-facing load/unload/reload surface.
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
//! # Status (PL8.B — picker / config / events / grammar / modes / completion)
//!
//! On-disk discovery + six seam→registry drains are live: a plugin dropped in
//! `<data>/lattice/plugins/` loads at boot and its contribution is reachable.
//! - **picker** RCU-registers its source into the
//!   [`PickerRegistryHandle`](lattice_picker::PickerRegistryHandle);
//! - **config** registers its typed options into the live
//!   [`ConfigRegistry`](lattice_config::ConfigRegistry);
//! - **events** subscribes its handlers on the [`EventBus`];
//! - **grammar** registers its motions / operators / text-objects / ex-commands
//!   into the runtime-mutable
//!   [`CommandRegistryHandle`](lattice_grammar::CommandRegistryHandle) (B3a/B3b)
//!   — the sync-trampoline seam, so the dispatcher fires it on keystroke off a
//!   wait-free `.load()` snapshot with no actor task;
//! - **modes** registers its minor modes into the runtime-mutable
//!   [`ModeRegistryHandle`](lattice_mode::ModeRegistryHandle) (B2), each mode's
//!   keymap binding landing in its own gated `MinorMode` layer on the
//!   [`KeymapHandle`] — declarative data, so the guest `Store` drops after
//!   registration (no task, nothing to keep alive);
//! - **completion** wraps its `WasmCompletionSource` as a native async
//!   `CompletionSourceContribution` carried by a loader-owned universal
//!   [`PluginCompletionMode`], so the aggregator reads it through
//!   `Mode::completion_sources()` like any LSP / snippet source (option A —
//!   completion is mode-attached; the async `generate` runs on a spawned actor,
//!   off the keystroke path).
//!
//! Each records provenance for `:list-plugins` via the [`PluginMetaSink`] seam.
//! That closes the PL8.B seam drains. **PL8.C** adds the user-facing lifecycle:
//! the loader self-registers `:plugin-load` / `:plugin-unload` / `:plugin-reload`
//! into the runtime-mutable command registry ([`register_ex_commands`](PluginLoader::register_ex_commands),
//! option A — zero host code), and [`unload`](PluginLoader::unload) reverses every
//! registry contribution via [`PluginTeardown`]. Decoration caching is the
//! separate hot-path slice PL8.E; `init.rs`-as-WASM is PL8.D.
//!
//! Design: `docs/dev/architecture/plugin-host.md`,
//! `docs/dev/architecture/boot-composition.md`. Slice plan:
//! `docs/dev/operations/slice-plans/plugin-loader.md`.

pub mod discovery;
mod ex_commands;
pub mod install;

pub use discovery::{
    DiscoveredPlugin, default_init_dir, default_plugins_dir, discover, discover_one,
};
pub use install::install;

use std::sync::{Arc, Mutex};

use lattice_completion::{CompletionSourceContribution, CompletionSourceKind, SourceId};
use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    ActivationPolicy, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    ModeRegistryHandle, PluginMetaSinkHandle,
};
use lattice_picker::{PickerRegistryHandle, PickerSourceGenerator};
use lattice_plugin_host::{
    LoadedPlugin, ManifestError, PluginBudget, PluginHost, PluginHostError, PluginId,
    PluginManifest, PluginSeam, PluginTeardown, TeardownRegistries, TeardownReport, TrustTier,
    WasmCompletionSource, WasmPickerSource,
};
use lattice_runtime::EventBus;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

/// The service handle other layers reach the loader through — the ex-command
/// surface (PL8.C), the plugin-manager view (PL8.H). Per the `ServiceRegistry`
/// Arc/TypeId rule, register **and** look up with this exact alias
/// (`Arc<PluginLoader>`), never a bare `PluginLoader`.
pub type PluginLoaderHandle = Arc<PluginLoader>;

/// A live loaded plugin: its host-issued [`PluginId`], its manifest id (the
/// user-facing name, and the key for `:plugin-unload <name>`), the source dir to
/// re-load from on `:plugin-reload`, the actor-lifecycle handles, and the
/// [`PluginTeardown`] bundle that reverses its registry contributions on unload.
struct LoadedRecord {
    id: PluginId,
    name: String,
    /// The directory this plugin was loaded from — re-loaded on
    /// `:plugin-reload`. `None` for plugins loaded from bytes in tests (reload
    /// is then a no-op with a logged reason).
    source_dir: Option<std::path::PathBuf>,
    /// Lifecycle-only plugins (base `plugin` world — `init.rs`, no-op) keep
    /// their instance alive here; dropping it drops the `Store`. Seam plugins
    /// are driven by their actor task instead, so this is `None` for them.
    lifecycle: Option<LoadedPlugin>,
    /// The detached actor tasks driving this plugin's seams (picker / events /
    /// completion). Aborted on unload — the actor-lifecycle half. Kept so the
    /// tasks are not cancelled by a dropped `JoinHandle` (tokio detaches on
    /// drop). [`PluginTeardown`] reverses the *registry* half; these are the
    /// running actors it does not cover.
    tasks: Vec<JoinHandle<()>>,
    /// The registry-contribution reversal bundle — each drain fills its surface's
    /// tokens (grammar / picker / modes / config options / event subscriptions);
    /// `PluginTeardown::unload` consumes it against the live registries on
    /// `:plugin-unload` / reload.
    teardown: PluginTeardown,
}

/// The editor-side runtime environment the loader drives seams against —
/// captured once from the boot context in [`install`], or built directly by a
/// headless harness / test. Every handle is `Option` because a given consumer
/// wires only the seams it exercises; a seam drain with an absent handle is a
/// logged skip, never a panic. Grows one field per seam without churning the
/// [`PluginLoader::with_services`] signature.
#[derive(Default, Clone)]
pub struct LoaderServices {
    /// The shared multi-thread runtime handle (seam actors + discovery run here,
    /// never the current-thread editor actor).
    pub runtime: Option<Handle>,
    /// The typed event bus — seam-actor crash quarantine binds to it, and event
    /// plugins subscribe through it.
    pub bus: Option<Arc<EventBus>>,
    /// The runtime-mutable picker registry (RCU-register loaded picker sources).
    pub picker_registry: Option<PickerRegistryHandle>,
    /// The config registry (already interior-mutable) plugin options register into.
    pub config_registry: Option<Arc<ConfigRegistry>>,
    /// The runtime-mutable command registry (B3a/B3b) a grammar plugin's
    /// motions / operators / text-objects / ex-commands register into. The
    /// dispatch path (`DocumentActor`, host-side ex-command / completion reads)
    /// snapshots it wait-free with `.load()`, so a plugin registered at runtime
    /// is live for a buffer on its next keystroke.
    pub command_registry: Option<CommandRegistryHandle>,
    /// The runtime-mutable mode registry (B2) a mode plugin's minor modes
    /// register into. RCU'd like the command registry — an owned snapshot is
    /// cloned, `spawn_mode_plugin` drains into it, and it is published, so
    /// keymap-resolution / mode-activation reads stay wait-free.
    pub mode_registry: Option<ModeRegistryHandle>,
    /// The interior-mutable keymap handle a mode plugin's per-mode `MinorMode`
    /// keymap bindings land in (a shared clone — its writes are internally
    /// mutex+ArcSwap-routed, so `spawn_mode_plugin` mutates it through a `&`).
    pub keymap: Option<KeymapHandle>,
    /// The provenance sink — records `PluginId → name/doc` for `:list-plugins`.
    pub meta_sink: Option<PluginMetaSinkHandle>,
}

/// Which drain-required services the loader captured at [`install`] time —
/// reported by [`PluginLoader::wired_seams`]. Every flag must be `true` after a
/// real editor boot; a `false` is a boot-ordering regression (the loader was
/// installed before that service registered) that silently degrades the
/// dependent seam's drain to a `NotWired` skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiredSeams {
    pub runtime: bool,
    pub bus: bool,
    pub picker_registry: bool,
    pub config_registry: bool,
    pub command_registry: bool,
    pub mode_registry: bool,
    pub keymap: bool,
    pub meta_sink: bool,
}

impl WiredSeams {
    /// True when every drain-required service was captured — the boot pin's
    /// assertion. `runtime` + `bus` are always present (`install` clones them
    /// from the boot context directly, not via `service::<T>()`).
    pub fn all(&self) -> bool {
        self.runtime
            && self.bus
            && self.picker_registry
            && self.config_registry
            && self.command_registry
            && self.mode_registry
            && self.keymap
            && self.meta_sink
    }
}

/// The plugin loader subsystem: owns the runtime, the loaded-plugin set, and the
/// discovery + load orchestration. Stood up at boot by [`install`], which
/// captures the editor environment and registers the loader as a
/// [`PluginLoaderHandle`] service so the user surface reaches it generically.
pub struct PluginLoader {
    host: Arc<PluginHost>,
    env: LoaderServices,
    /// `std::sync::Mutex` (not `tokio`): taken only to push / read the loaded
    /// set *after* the async load work completes, never across an `.await`.
    loaded: Mutex<Vec<LoadedRecord>>,
}

/// Why a plugin failed to load. Every variant is graceful-degradation input for
/// the caller (discovery logs + skips; the editor never aborts boot on one bad
/// plugin) — the load path returns a value, never panics.
#[derive(Debug, thiserror::Error)]
pub enum PluginLoaderError {
    /// The manifest was malformed or declared an unrecognised capability/seam.
    #[error("plugin manifest invalid: {0}")]
    Manifest(#[from] ManifestError),
    /// The component failed to compile, instantiate, activate, or spawn a seam —
    /// a wasm trap, fuel/epoch exhaustion, or a capability failure.
    #[error("plugin runtime error: {0}")]
    Host(#[from] PluginHostError),
    /// A seam was declared but the loader was constructed without the editor
    /// environment needed to drive it (the minimal test constructor). Never
    /// happens on the real boot path.
    #[error("plugin loader not wired for seam `{0}` (no editor environment)")]
    NotWired(&'static str),
    /// The plugin declared no seam the loader can drain yet, so nothing loaded.
    #[error("plugin declares no loadable seam")]
    NothingLoaded,
    /// `:plugin-load <path>` pointed at a directory that is not a plugin (no
    /// `plugin.toml`, bad TOML, or missing/ambiguous component).
    #[error("cannot load plugin from path: {0}")]
    Discovery(String),
    /// `:plugin-unload` / `:plugin-reload <target>` named no currently-loaded
    /// plugin (by manifest id or numeric plugin id).
    #[error("no loaded plugin named `{0}`")]
    NotLoaded(String),
    /// `:plugin-reload` on a plugin the loader can't re-read from disk (loaded
    /// from bytes in a test, or its source dir is gone).
    #[error("plugin `{0}` has no on-disk source to reload from")]
    NotReloadable(String),
}

/// Match a loaded record against a `:plugin-unload` / `:plugin-reload` target —
/// its manifest id (the common case) or its numeric host-issued plugin id.
fn record_matches(record: &LoadedRecord, target: &str) -> bool {
    record.name == target || target.parse::<u32>().ok() == Some(record.id.0)
}

/// Default priority bucket for a plugin completion source — below LSP (200) and
/// snippets (150), above bare buffer-word sources. A per-plugin priority
/// override (a manifest field / `completion.source.<id>.priority` option) is
/// future work; for now every plugin source shares this documented default.
const PLUGIN_COMPLETION_DEFAULT_PRIORITY: u32 = 100;

/// The loader-owned minor mode that carries a plugin's completion source into
/// the native aggregator (option A — completion is mode-attached everywhere:
/// LSP rides the LSP mode, snippets ride the snippet mode). Registered
/// [`ActivationPolicy::Universal`] so the source contributes on every
/// completion-capable buffer; `recompute_active_completion_sources_for` walks
/// the mode registry and picks up [`completion_sources`](Mode::completion_sources)
/// like any native mode's. A manifest-declared scope (attach to a named
/// language / major mode instead of universal) is the natural extension.
#[derive(Debug)]
struct PluginCompletionMode {
    id: ModeId,
    source: CompletionSourceContribution,
}

impl Mode for PluginCompletionMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        self.id.clone()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Universal
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
        vec![self.source.clone()]
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

impl PluginLoader {
    /// Construct a loader over `host` with **no** editor environment — the
    /// minimal constructor for tests exercising only the lifecycle spine.
    /// [`install`] uses [`with_env`](Self::with_env) to wire the real seams.
    pub fn new(host: Arc<PluginHost>) -> Self {
        Self {
            host,
            env: LoaderServices::default(),
            loaded: Mutex::new(Vec::new()),
        }
    }

    /// Construct a loader wired with the editor environment ([`LoaderServices`])
    /// — the boot path ([`install`]) and headless harnesses / tests. The seams a
    /// plugin declares are driven against the wired handles; an absent handle
    /// makes that seam a logged skip.
    pub fn with_services(host: Arc<PluginHost>, services: LoaderServices) -> Self {
        Self {
            host,
            env: services,
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

    /// Which drain-required services the loader captured from the boot context
    /// ([`install`]). A boot-ordering regression (installing the loader before a
    /// service it depends on registers) silently leaves a field `false`, turning
    /// that seam's drain into a `NotWired` skip — so the boot pin asserts every
    /// flag is set after `Editor::boot`. Test/introspection affordance.
    pub fn wired_seams(&self) -> WiredSeams {
        WiredSeams {
            runtime: self.env.runtime.is_some(),
            bus: self.env.bus.is_some(),
            picker_registry: self.env.picker_registry.is_some(),
            config_registry: self.env.config_registry.is_some(),
            command_registry: self.env.command_registry.is_some(),
            mode_registry: self.env.mode_registry.is_some(),
            keymap: self.env.keymap.is_some(),
            meta_sink: self.env.meta_sink.is_some(),
        }
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

    /// Discover every plugin under `dir` and load each, logging + skipping any
    /// that fails (never aborting the others). Returns the count loaded. Runs on
    /// the caller (the multi-thread runtime), off the editor actor.
    pub async fn discover_and_load(&self, dir: &std::path::Path, tier: TrustTier) -> usize {
        let discovered = discovery::discover(dir);
        let mut loaded = 0;
        for plugin in discovered {
            match self.load_discovered(&plugin, tier).await {
                Ok(_) => loaded += 1,
                Err(err) => tracing::warn!(
                    plugin = %plugin.manifest.id,
                    dir = %plugin.dir.display(),
                    error = %err,
                    "plugin failed to load; skipped"
                ),
            }
        }
        loaded
    }

    /// Load one already-discovered plugin: compile, then either drive the
    /// lifecycle spine (empty `provides`) or drain each declared seam, and
    /// record its provenance. Returns the host-issued [`PluginId`].
    pub async fn load_discovered(
        &self,
        plugin: &DiscoveredPlugin,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        let manifest = &plugin.manifest;
        let component = self.host.compile(&plugin.component_bytes)?;

        let mut record = LoadedRecord {
            id: PluginId(0),
            name: manifest.id.clone(),
            source_dir: Some(plugin.dir.clone()),
            lifecycle: None,
            tasks: Vec::new(),
            teardown: PluginTeardown::new(PluginId(0)),
        };
        let mut loaded_id: Option<PluginId> = None;

        if manifest.provides.is_empty() {
            // Lifecycle-only (base `plugin` world): instantiate + activate.
            let mut instance = self
                .host
                .instantiate_plugin(&component, manifest, tier, PluginBudget::default())
                .await?;
            instance.activate().await?;
            loaded_id = Some(instance.id());
            record.lifecycle = Some(instance);
        } else {
            for seam in &manifest.provides {
                match seam {
                    PluginSeam::PickerSource => {
                        let id = self
                            .drain_picker(&component, manifest, tier, &mut record)
                            .await?;
                        loaded_id.get_or_insert(id);
                    }
                    PluginSeam::Config => {
                        let id = self
                            .drain_config(&component, manifest, tier, &mut record)
                            .await?;
                        loaded_id.get_or_insert(id);
                    }
                    PluginSeam::Events => {
                        let id = self
                            .drain_events(&component, manifest, tier, &mut record)
                            .await?;
                        loaded_id.get_or_insert(id);
                    }
                    PluginSeam::Grammar => {
                        let id = self.drain_grammar(&component, manifest, tier, &mut record)?;
                        loaded_id.get_or_insert(id);
                    }
                    PluginSeam::Modes => {
                        let id = self
                            .drain_mode(&component, manifest, tier, &mut record)
                            .await?;
                        loaded_id.get_or_insert(id);
                    }
                    PluginSeam::CompletionSource => {
                        let id = self
                            .drain_completion(&component, manifest, tier, &mut record)
                            .await?;
                        loaded_id.get_or_insert(id);
                    }
                    PluginSeam::Keymap => {
                        let id = self
                            .drain_keymap(&component, manifest, tier, &mut record)
                            .await?;
                        loaded_id.get_or_insert(id);
                    }
                    other => tracing::warn!(
                        seam = %other,
                        plugin = %manifest.id,
                        "plugin declares a seam the loader does not drain yet (PL8.B follow-on); skipped"
                    ),
                }
            }
        }

        let Some(id) = loaded_id else {
            return Err(PluginLoaderError::NothingLoaded);
        };
        record.id = id;
        record.teardown.plugin_id = id;

        // Provenance: `SourceLayer::Plugin(id)` renders as the name, and
        // `:list-plugins` shows it. Doc falls back to the manifest field.
        if let Some(sink) = &self.env.meta_sink {
            sink.register_plugin(
                id.0,
                manifest.id.clone(),
                manifest.doc.clone().unwrap_or_default(),
            );
        }

        self.loaded
            .lock()
            .expect("plugin-loader loaded-set mutex poisoned")
            .push(record);
        // One-shot, user-actionable event (the "LSP server attached" class).
        tracing::info!(plugin = %manifest.id, id = id.0, "plugin loaded");
        Ok(id)
    }

    /// Self-register the `:plugin-load` / `:plugin-unload` / `:plugin-reload`
    /// ex-commands into the runtime-mutable command registry (option A — the
    /// loader owns its full command surface; zero host code). Plain command
    /// names resolve directly via `id_by_name` (no `expand_alias` host entry),
    /// exactly like plugin-contributed ex-commands. Called once by [`install`]
    /// after the loader is constructed; a no-op (logged) if no command registry
    /// was wired.
    ///
    /// The apply closures capture `Arc<Self>`, so the command registry holds the
    /// loader and the loader holds the registry — a benign cycle (both are
    /// app-lifetime boot services that never drop).
    pub fn register_ex_commands(self: &Arc<Self>) {
        let Some(registry) = self.env.command_registry.clone() else {
            tracing::warn!(
                "no command registry wired; :plugin-load / :plugin-unload / :plugin-reload unavailable"
            );
            return;
        };
        // load → clone → register → store (single-threaded at boot; no retry).
        let mut next = (**registry.load()).clone();
        ex_commands::register_all(&mut next, self);
        registry.store(Arc::new(next));
    }

    /// Spawn an async [`load_path`](Self::load_path) on the loader's own runtime
    /// — the `:plugin-load` apply path (a sync ex-command closure kicking off
    /// async work). Completion / failure surfaces via `tracing` (→ `*messages*`).
    pub(crate) fn spawn_load_path(self: &Arc<Self>, dir: std::path::PathBuf) {
        let Some(runtime) = self.env.runtime.clone() else {
            tracing::warn!("no runtime wired; :plugin-load cannot run");
            return;
        };
        let this = Arc::clone(self);
        runtime.spawn(async move {
            match this.load_path(&dir, TrustTier::UserInstalled).await {
                Ok(id) => {
                    tracing::info!(id = id.0, dir = %dir.display(), "plugin loaded (:plugin-load)")
                }
                Err(err) => {
                    tracing::warn!(dir = %dir.display(), error = %err, ":plugin-load failed")
                }
            }
        });
    }

    /// Spawn an async [`reload`](Self::reload) on the loader's own runtime — the
    /// `:plugin-reload` apply path. Reports via `tracing` (→ `*messages*`).
    pub(crate) fn spawn_reload(self: &Arc<Self>, target: String) {
        let Some(runtime) = self.env.runtime.clone() else {
            tracing::warn!("no runtime wired; :plugin-reload cannot run");
            return;
        };
        let this = Arc::clone(self);
        runtime.spawn(async move {
            match this.reload(&target, TrustTier::UserInstalled).await {
                Ok(id) => {
                    tracing::info!(id = id.0, plugin = %target, "plugin reloaded (:plugin-reload)")
                }
                Err(err) => {
                    tracing::warn!(plugin = %target, error = %err, ":plugin-reload failed")
                }
            }
        });
    }

    /// Load a single plugin from an explicit directory — the `:plugin-load <path>`
    /// entry point (PL8.C). Unlike [`discover_and_load`](Self::discover_and_load)
    /// (a tree scan that silently skips non-plugin dirs), a direct request
    /// surfaces a bad path as a [`PluginLoaderError::Discovery`] the user sees.
    pub async fn load_path(
        &self,
        dir: &std::path::Path,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        let plugin = discovery::discover_one(dir).map_err(PluginLoaderError::Discovery)?;
        self.load_discovered(&plugin, tier).await
    }

    /// Unload the plugin named `target` (its manifest id, or its numeric plugin
    /// id): abort its actor tasks and reverse every registry contribution via
    /// [`PluginTeardown`]. Returns the [`TeardownReport`] (what each surface
    /// removed), or `None` if no loaded plugin matched. **Synchronous** —
    /// teardown and `JoinHandle::abort` don't await — so an ex-command `apply`
    /// closure can call it directly. Idempotent per the teardown contract.
    pub fn unload(&self, target: &str) -> Option<TeardownReport> {
        let record = {
            let mut loaded = self
                .loaded
                .lock()
                .expect("plugin-loader loaded-set mutex poisoned");
            let pos = loaded.iter().position(|r| record_matches(r, target))?;
            loaded.remove(pos)
        };

        // The running-actor half: abort the detached seam tasks (picker / events
        // / completion). The registry half is the `PluginTeardown` below.
        for task in &record.tasks {
            task.abort();
        }
        let report = self.run_teardown(&record.teardown);
        if let Some(sink) = &self.env.meta_sink {
            sink.unregister_plugin(record.id.0);
        }
        tracing::info!(
            plugin = %record.name,
            id = record.id.0,
            ?report,
            "plugin unloaded"
        );
        Some(report)
    }

    /// Reload the plugin named `target`: [`unload`](Self::unload) it, then
    /// re-[`load_path`](Self::load_path) from its recorded source directory —
    /// minting a fresh `Store` with a fresh, untripped `Quarantine` (the reload
    /// contract, teardown.rs §"Why no reload method"). Errors if `target` names
    /// no loaded plugin ([`NotLoaded`](PluginLoaderError::NotLoaded)) or it has
    /// no on-disk source ([`NotReloadable`](PluginLoaderError::NotReloadable)).
    pub async fn reload(
        &self,
        target: &str,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        // Capture the source dir before unloading (unload removes the record).
        let dir = {
            let loaded = self
                .loaded
                .lock()
                .expect("plugin-loader loaded-set mutex poisoned");
            let record = loaded
                .iter()
                .find(|r| record_matches(r, target))
                .ok_or_else(|| PluginLoaderError::NotLoaded(target.to_string()))?;
            record
                .source_dir
                .clone()
                .ok_or_else(|| PluginLoaderError::NotReloadable(record.name.clone()))?
        };
        self.unload(target);
        self.load_path(&dir, tier).await
    }

    /// Reverse a plugin's registry contributions against the live registries.
    /// The `ArcSwap`-held registries (command / picker / mode) are RCU'd —
    /// snapshot-clone → `&mut` → [`PluginTeardown::unload`] → store — while the
    /// `Arc`-shared interior-mutable ones (config / keymap / bus) pass by
    /// reference. A missing registry handle (a partially-wired test loader)
    /// downgrades to a logged no-op reversal, never a panic.
    fn run_teardown(&self, teardown: &PluginTeardown) -> TeardownReport {
        let (
            Some(cmd_h),
            Some(pick_h),
            Some(mode_h),
            Some(config),
            Some(keymap),
            Some(bus),
        ) = (
            self.env.command_registry.as_ref(),
            self.env.picker_registry.as_ref(),
            self.env.mode_registry.as_ref(),
            self.env.config_registry.as_ref(),
            self.env.keymap.as_ref(),
            self.env.bus.as_ref(),
        )
        else {
            tracing::warn!(
                "plugin teardown skipped: loader missing a registry handle (partial unload)"
            );
            return TeardownReport::default();
        };

        // Owned snapshots of the ArcSwap registries for the `&mut` unload needs.
        let mut commands = (**cmd_h.load()).clone();
        let mut pickers = (**pick_h.load()).clone();
        let mut modes = (**mode_h.load()).clone();
        let report = {
            let mut reg = TeardownRegistries {
                commands: &mut commands,
                pickers: &mut pickers,
                modes: &mut modes,
                keymap,
                config: &**config,
                bus: &**bus,
            };
            teardown.unload(&mut reg)
        };
        // Publish the reversed snapshots (RCU store).
        cmd_h.store(Arc::new(commands));
        pick_h.store(Arc::new(pickers));
        mode_h.store(Arc::new(modes));
        report
    }

    /// Drain the picker seam: spawn the source actor, fetch its spec, register
    /// the `WasmPickerSource` into the picker registry by copy-on-write RCU, and
    /// spawn the actor's `run` loop on the runtime. Records the actor task +
    /// source id on `record` for teardown (PL8.C).
    async fn drain_picker(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let bus = self
            .env
            .bus
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("picker-source"))?;
        let runtime = self
            .env
            .runtime
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("picker-source"))?;
        let registry = self
            .env
            .picker_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("picker-source"))?;

        let (client, actor) = self
            .host
            .spawn_picker_source(component, manifest, tier, PluginBudget::default(), bus)
            .await?;

        // Drive the actor's request loop on the multi-thread runtime FIRST —
        // `connect` below issues a `spec()` guest call over the client channel,
        // which the actor must be running to answer (else the await deadlocks).
        let task = runtime.spawn(actor.run());

        // The spec fetch is a guest call; a malformed spec fails registration
        // loudly rather than registering a broken source.
        let source = WasmPickerSource::connect(client).await?;
        let id = source.plugin_id();
        let source_id: &'static str = source.spec().id;

        // Copy-on-write RCU into the wait-free registry: clone the current
        // snapshot, add the source, publish. Concurrent picker-open readers keep
        // seeing the old snapshot until the store lands — no lock on their path.
        let generator: Arc<dyn PickerSourceGenerator> = Arc::new(source);
        registry.rcu(|current| {
            let mut next = (**current).clone();
            next.register_generator(generator.clone());
            Arc::new(next)
        });

        record.tasks.push(task);
        // Teardown token: the picker registry unregisters this source by id.
        record.teardown.picker_sources.push(source_id.to_string());
        Ok(id)
    }

    /// Drain the config seam: run the guest's `register-options` against the live
    /// config registry (already interior-mutable — `:set` / `:describe-option` /
    /// `:customize` treat plugin options uniformly). One-shot: no actor to spawn.
    async fn drain_config(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let registry = self
            .env
            .config_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("config"))?;
        let (id, names) = self
            .host
            .spawn_config_plugin(component, manifest, tier, PluginBudget::default(), registry)
            .await?;
        tracing::debug!(plugin = %manifest.id, options = ?names, "config plugin registered options");
        // Teardown token: the config registry unregisters each option by name.
        record.teardown.config_options = names;
        Ok(id)
    }

    /// Drain the events seam: register the guest's subscriptions on the bus and
    /// drive its `on-event` actor on the runtime (off the keystroke path). A
    /// trapping handler quarantines the plugin without touching the publisher or
    /// other subscribers (the event-seam isolation contract).
    async fn drain_events(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let bus = self
            .env
            .bus
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("events"))?;
        let runtime = self
            .env
            .runtime
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("events"))?;

        let (subscriptions, actor) = self
            .host
            .spawn_event_plugin(component, manifest, tier, PluginBudget::event(), bus)
            .await?;
        let id = actor.id();
        let task = runtime.spawn(actor.run());
        record.tasks.push(task);
        tracing::debug!(
            plugin = %manifest.id,
            subscriptions = subscriptions.len(),
            "event plugin subscribed"
        );
        // Teardown token: the bus unsubscribes each subscription id.
        record.teardown.subscriptions = subscriptions;
        Ok(id)
    }

    /// Drain the grammar seam: instantiate the `grammar-plugin` component, drive
    /// its `register-grammar` export, and register the resulting native specs
    /// into the runtime-mutable command registry (B3a/B3b).
    ///
    /// The grammar seam is the **synchronous** one (the PH7.7 fork): each
    /// contributed motion / operator / text-object / ex-command carries a sync
    /// trampoline the dispatcher fires on keystroke, so — unlike the async actor
    /// seams (picker / events) — there is *no* actor `run()` loop to spawn. The
    /// command registry itself owns the guest `Store` (inside the boxed
    /// trampolines the specs carry), so registering the set is all that keeps the
    /// plugin's grammar alive; teardown (PL8.C) unregisters by `plugin_id`.
    ///
    /// Registration is **load → clone → register → store**, *not* an
    /// [`rcu`](arc_swap::ArcSwap::rcu): [`register_all`] consumes the set (the
    /// specs own non-`Clone` boxed trampolines) so it cannot run inside a
    /// retrying `rcu` closure. B3a made `CommandRegistry: Clone` (Arc'd spec
    /// closures) precisely so this snapshot clone is cheap (Arc bumps, no deep
    /// copy). Loads are serialized — boot discovery is a sequential
    /// [`discover_and_load`](Self::discover_and_load) loop, and the PL8.C
    /// `:plugin-load` ex-command dispatches one at a time — so the load→store
    /// window carries no lost-write race against a concurrent grammar
    /// registration. A malformed spec fails loudly with `PluginHostError`
    /// (mapped to [`PluginLoaderError::Host`]); a *runtime* `apply` trap degrades
    /// to a graceful no-op inside the trampoline (`CommandError::Plugin`), never
    /// a host crash.
    ///
    /// [`register_all`]: lattice_plugin_host::GrammarContributionSet::register_all
    fn drain_grammar(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let bus = self
            .env
            .bus
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("grammar"))?;
        let registry = self
            .env
            .command_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("grammar"))?;

        // SYNC end to end (no async host import to drive) — instantiation +
        // `register-grammar` run at load time, off the keystroke path (the
        // sibling `self.host.compile` above is likewise a synchronous call).
        let set = self
            .host
            .instantiate_grammar_plugin(component, manifest, tier, bus)?;
        let id = set.plugin_id();
        let count = set.len();

        // load → clone → register → store. `register_all` consumes `set`, so a
        // retrying `rcu` closure is impossible; the serialized-load invariant
        // (doc above) makes the plain swap race-free in practice.
        let mut next = (**registry.load()).clone();
        set.register_all(&mut next);
        registry.store(Arc::new(next));

        tracing::debug!(
            plugin = %manifest.id,
            contributions = count,
            "grammar plugin registered its motions / operators / text-objects / ex-commands"
        );
        // Teardown token: grammar reverses by provenance (all
        // `SourceLayer::Plugin(id)` entries), so just record that it ran.
        record.teardown.has_grammar = true;
        Ok(id)
    }

    /// Drain the modes seam: instantiate the `modes-plugin` component, drive its
    /// `register-modes` export, and register each accepted minor mode into the
    /// runtime-mutable mode registry (B2), binding each mode's declared keymap
    /// into its own `MinorMode` layer.
    ///
    /// A registered mode is **declarative data** — id / kind / activation policy
    /// / capability requirements + keymap bindings that resolve to *existing*
    /// commands — so once `spawn_mode_plugin` copies it into the registry, the
    /// guest `Store` drops (no actor task, no live callback, nothing to keep
    /// alive); teardown (PL8.C) removes the modes + keymap layers by `plugin_id`.
    ///
    /// Registration RCUs the mode registry — **load → clone → spawn → store**.
    /// `spawn_mode_plugin` takes `&mut ModeRegistry` (registration drains after
    /// its async `register-modes`) so it holds the borrow across an `.await`;
    /// passing a local owned snapshot clone keeps that sound, and it can't run in
    /// a retrying `rcu` closure anyway (it instantiates a guest). B2 made
    /// `ModeRegistry` an `ArcSwap` handle + `Clone` for exactly this. The
    /// `commands` snapshot (read-only, for bind-time command resolution) and the
    /// interior-mutable `keymap` handle come straight from the wired services.
    /// A missing service degrades the modes seam to a logged skip
    /// (`NotWired("modes")`), never a boot abort; a `register-modes` trap maps to
    /// `PluginLoaderError::Host` and skips only this plugin.
    async fn drain_mode(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let mode_registry = self
            .env
            .mode_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("modes"))?;
        let keymap = self
            .env
            .keymap
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("modes"))?;
        let commands = self
            .env
            .command_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("modes"))?;

        // Read-only command snapshot for keymap-binding resolution; owned so it
        // outlives the `&mut next` borrow across the await.
        let commands_snapshot = commands.load_full();
        // load → clone → spawn → store (see the RCU note above).
        let mut next = (**mode_registry.load()).clone();
        let (id, mode_ids) = self
            .host
            .spawn_mode_plugin(
                component,
                manifest,
                tier,
                PluginBudget::default(),
                &mut next,
                &commands_snapshot,
                keymap,
            )
            .await?;
        mode_registry.store(Arc::new(next));

        tracing::debug!(
            plugin = %manifest.id,
            modes = ?mode_ids,
            "mode plugin registered its minor modes"
        );
        // Teardown tokens: each mode reverses via `ModeRegistry::unregister` +
        // `KeymapHandle::remove_layer(MinorMode(id))`.
        record.teardown.modes.extend(mode_ids);
        Ok(id)
    }

    /// Drain the completion seam: spawn the source actor, wrap the plugin's
    /// `WasmCompletionSource` as a native async `CompletionSourceContribution`,
    /// and register a loader-owned universal [`PluginCompletionMode`] carrying it
    /// into the runtime-mutable mode registry (option A).
    ///
    /// Completion is mode-attached across the whole editor (the aggregator
    /// `recompute_active_completion_sources_for` walks the mode registry calling
    /// `completion_sources()`), so the source rides a mode rather than a parallel
    /// registry. The async `generate` runs on the spawned actor (off the
    /// keystroke path); matching / ranking / annotation stay native, so paramount
    /// #1 holds. The `WasmCompletionSource` actor mirrors the picker seam — driven
    /// on the runtime, recorded on `record` for unload (PL8.C).
    ///
    /// Runtime-visibility caveat: a Universal mode contributes on a buffer only
    /// once it is *active* there and the completion-source cache is recomputed
    /// (on mode-activation transitions). At boot, discovery runs before buffers
    /// open, so the first cache build includes it; a plugin loaded *after* buffers
    /// are open reaches new buffers immediately but needs a re-activation +
    /// recompute pass for existing ones — that pass lands with the PL8.C
    /// `:plugin-load` / reload surface. A missing service degrades the seam to a
    /// logged skip (`NotWired`); a spawn/connect trap maps to
    /// `PluginLoaderError::Host` and skips only this plugin.
    async fn drain_completion(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let bus = self
            .env
            .bus
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("completion-source"))?;
        let runtime = self
            .env
            .runtime
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("completion-source"))?;
        let mode_registry = self
            .env
            .mode_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("completion-source"))?;

        let (client, actor) = self
            .host
            .spawn_completion_source(component, manifest, tier, PluginBudget::default(), bus)
            .await?;
        // Drive the actor FIRST — `connect` issues a `spec()` guest call over the
        // client channel, which the actor must be running to answer.
        let task = runtime.spawn(actor.run());
        let source = WasmCompletionSource::connect(client).await?;
        let id = source.plugin_id();
        let source_id = source.id().to_string();

        let contribution = CompletionSourceContribution {
            id: SourceId::new(&source_id),
            default_priority: PLUGIN_COMPLETION_DEFAULT_PRIORITY,
            auto_trigger: true,
            trigger_chars: Vec::new(),
            popup_filter_chord: None,
            kind: CompletionSourceKind::Async(Arc::new(source)),
        };

        // RCU-register the carrier mode (load → clone → register → store; B2 made
        // ModeRegistry an ArcSwap handle + Clone). The `-mode` suffix satisfies
        // the registry's naming gate.
        let mode_id = format!("{}-completion-mode", manifest.id);
        let mode = PluginCompletionMode {
            id: ModeId::new(&mode_id),
            source: contribution,
        };
        let carrier_id = ModeId::new(&mode_id);
        let mut next = (**mode_registry.load()).clone();
        match next.register(mode) {
            Ok(_) => mode_registry.store(Arc::new(next)),
            Err(error) => {
                // An id collision (a mode already owns `<id>-completion-mode`)
                // leaves the source unreachable — abort the actor and fail loudly
                // rather than leak a dangling task or claim a phantom load.
                task.abort();
                tracing::warn!(
                    plugin = %manifest.id,
                    mode = %mode_id,
                    %error,
                    "completion carrier mode id collision; source unreachable, skipped"
                );
                return Err(PluginLoaderError::NothingLoaded);
            }
        }

        record.tasks.push(task);
        // Teardown token: unregistering the carrier mode drops the source; the
        // actor task is aborted separately from `record.tasks`.
        record.teardown.modes.push(carrier_id);
        tracing::debug!(
            plugin = %manifest.id,
            source = %source_id,
            mode = %mode_id,
            "completion plugin registered its source on a universal carrier mode"
        );
        Ok(id)
    }

    /// Drain the keymap seam (PL8.D): bind the plugin's user keybindings into
    /// `KeymapLayer::User` and record them as teardown tokens (unbound on unload).
    ///
    /// Direct registration, like config — the seam binds into the shared,
    /// interior-mutable [`KeymapHandle`] during `register-keymap`, so there is no
    /// RCU and no actor task. The command-registry snapshot resolves each
    /// binding's command name at bind time; an unregistered command / unparseable
    /// chord / withheld `KeymapCapability::User` binds nothing (logged, no trap).
    /// The first consumer is the user's `init.rs` (PL8.D.3).
    async fn drain_keymap(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let keymap = self
            .env
            .keymap
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("keymap"))?;
        let commands = self
            .env
            .command_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("keymap"))?;

        // Owned command snapshot for bind-time command-name resolution.
        let commands_snapshot = commands.load_full();
        let (id, tokens) = self
            .host
            .spawn_keymap_plugin(
                component,
                manifest,
                tier,
                PluginBudget::default(),
                keymap,
                &commands_snapshot,
            )
            .await?;

        tracing::debug!(
            plugin = %manifest.id,
            bindings = tokens.len(),
            "keymap plugin bound user keybindings into KeymapLayer::User"
        );
        // Teardown tokens: each binding is unbound from `KeymapLayer::User` on
        // unload / reload.
        record.teardown.keymap_bindings = tokens;
        Ok(id)
    }
}
