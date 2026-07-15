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
//! # Status (PL8.B — picker / config / events / grammar seams)
//!
//! On-disk discovery + four seam→registry drains are live: a plugin dropped in
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
//!   wait-free `.load()` snapshot with no actor task.
//!
//! Each records provenance for `:list-plugins` via the [`PluginMetaSink`] seam.
//! The remaining seams (completion, modes) declare-but-warn until their drains
//! land (modes is the next slice, on B2's runtime-mutable
//! [`ModeRegistryHandle`](lattice_mode::ModeRegistryHandle)); decoration caching
//! is PL8.E; the ex-command surface + teardown is PL8.C.
//!
//! Design: `docs/dev/architecture/plugin-host.md`,
//! `docs/dev/architecture/boot-composition.md`. Slice plan:
//! `docs/dev/operations/slice-plans/plugin-loader.md`.

pub mod discovery;
pub mod install;

pub use discovery::{DiscoveredPlugin, default_plugins_dir, discover};
pub use install::install;

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_mode::PluginMetaSinkHandle;
use lattice_picker::{PickerRegistryHandle, PickerSourceGenerator};
use lattice_plugin_host::{
    LoadedPlugin, ManifestError, PluginBudget, PluginHost, PluginHostError, PluginId,
    PluginManifest, PluginSeam, TrustTier, WasmPickerSource,
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
/// user-facing name, and the key for `:plugin-unload <name>` — PL8.C), and
/// whatever must stay alive / be reversed for the plugin's contributions to
/// keep working (PL8.C drives teardown from these).
struct LoadedRecord {
    #[allow(dead_code)] // read by PL8.C unload / PL8.H manager view.
    id: PluginId,
    name: String,
    /// Lifecycle-only plugins (base `plugin` world — `init.rs`, no-op) keep
    /// their instance alive here; dropping it drops the `Store`. Seam plugins
    /// are driven by their actor task instead, so this is `None` for them.
    #[allow(dead_code)]
    lifecycle: Option<LoadedPlugin>,
    /// The detached actor tasks driving this plugin's seams. Aborted on unload
    /// (PL8.C). Kept so the tasks are not cancelled by a dropped `JoinHandle`
    /// (tokio detaches on drop, so this is really the unload handle).
    #[allow(dead_code)]
    tasks: Vec<JoinHandle<()>>,
    /// Picker source ids this plugin registered — unregistered from the picker
    /// registry on unload (PL8.C).
    #[allow(dead_code)]
    picker_sources: Vec<&'static str>,
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
    /// The provenance sink — records `PluginId → name/doc` for `:list-plugins`.
    pub meta_sink: Option<PluginMetaSinkHandle>,
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
            lifecycle: None,
            tasks: Vec::new(),
            picker_sources: Vec::new(),
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
                        let id = self.drain_config(&component, manifest, tier).await?;
                        loaded_id.get_or_insert(id);
                    }
                    PluginSeam::Events => {
                        let id = self
                            .drain_events(&component, manifest, tier, &mut record)
                            .await?;
                        loaded_id.get_or_insert(id);
                    }
                    PluginSeam::Grammar => {
                        let id = self.drain_grammar(&component, manifest, tier)?;
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
        record.picker_sources.push(source_id);
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
        Ok(id)
    }
}
