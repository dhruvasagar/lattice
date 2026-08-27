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

pub mod build;
pub mod discovery;
mod ex_commands;
pub mod install;
pub mod pipeline;
pub mod resolve;
pub mod source_record;
pub mod watch;

pub use build::{
    BuildOutcome, CargoComponentBuilder, ComponentBuilder, Stamp, artifact_path, build_plugin,
    source_stamp,
};
pub use discovery::{
    DiscoveredPlugin, default_core_plugins_dir, default_init_dir, default_plugins_dir,
    default_source_cache_dir, discover, discover_one,
};
pub use install::{autoload_enabled, disable_autoload, enable_autoload, install};
pub use pipeline::{Install, RequiredSpec, install_all, install_required, to_required_spec};
pub use resolve::{
    Fetcher, GitRunner, HttpFetcher, PluginSource, Resolved, SystemGit, git_cache_dir, resolve,
};
pub use source_record::SourceRecord;

use std::sync::{Arc, Mutex};

use lattice_completion::{CompletionSourceContribution, CompletionSourceKind, SourceId};
use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    ActivationPolicy, AsyncContextSource, AsyncGutterDecorationSource, CapabilitySet,
    ContextSourceRegistryHandle, GutterDecorationSourceRegistryHandle, LifecycleFuture, Mode,
    ModeContext, ModeId, ModeKind, ModeRegistryHandle, PluginMetaSinkHandle,
};
use lattice_picker::{PickerRegistryHandle, PickerSourceGenerator};
use lattice_plugin_host::{
    Capability, LoadedPlugin, ManifestError, PluginBudget, PluginHost, PluginHostError, PluginId,
    PluginManifest, PluginSeam, PluginTeardown, TeardownRegistries, TeardownReport, TrustTier,
    WasmCompletionSource, WasmContextSource, WasmDecorationSource, WasmPickerSource,
};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

mod status;
pub use status::{BuildState, FailedLoad, PluginHealth, PluginStatus};

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
    /// PL8.H.1: the trust tier this plugin loaded under — reported in the
    /// manager view's status, and the gate that decides `denied` below.
    tier: TrustTier,
    /// PL8.H.1: capabilities the plugin requested AND received under `tier`
    /// (`requested` minus `denied`). Computed once at load from the manifest.
    granted: Vec<Capability>,
    /// PL8.H.1: requested-but-withheld capabilities (tier-gated). Never fatal —
    /// the plugin loaded degraded; the manager view surfaces this.
    denied: Vec<Capability>,
    /// PL8.H.1: live health — `Healthy` at load, flipped to `Quarantined` by the
    /// `Event::PluginCrashed` subscription ([`PluginLoader::subscribe_health`]).
    health: PluginHealth,
    /// PM.3: the mode this plugin enables by default (from its manifest), gated by
    /// `<id>.enabled`. Kept so the `OptionChanged` subscription
    /// ([`PluginLoader::subscribe_mode_gates`]) can map a changed `<id>.enabled`
    /// back to the modes to (de)activate. Empty ⇒ no default mode.
    default_modes: Vec<String>,
    /// PM.8a: where the plugin came from (its `.source` marker at load time).
    source: crate::source_record::SourceRecord,
}

/// PM.8b: what a build for one plugin is doing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BuildActivity {
    Running,
    Failed,
}

/// PM.8a: is this plugin's artifact current with its source?
///
/// Recomputed from disk per snapshot rather than cached at load, because the
/// interesting transition happens *while the editor runs* — a user edits a
/// local plugin's source and wants the view to say `stale` without a restart.
/// It is two small file reads per row, on the `:plugins` refresh path, not a
/// per-frame cost.
fn build_state_of(record: &LoadedRecord, activity: Option<&BuildActivity>) -> BuildState {
    // PM.8b: an in-flight or just-failed build is the more current answer —
    // the artifact on disk describes the *previous* build, and reporting
    // `cached` while a rebuild is running would tell the user their `b` did
    // nothing.
    match activity {
        Some(BuildActivity::Running) => return BuildState::Building,
        Some(BuildActivity::Failed) => return BuildState::Failed,
        None => {}
    }
    let Some(source) = record.source.as_plugin_source() else {
        return BuildState::NotBuilt;
    };
    if matches!(source, crate::resolve::PluginSource::Prebuilt { .. }) {
        // A prebuilt is downloaded, never built, so it has no staleness.
        return BuildState::NotBuilt;
    }
    let Some(dir) = record.source_dir.as_ref() else {
        return BuildState::NotBuilt;
    };
    // The stamp records what the artifact was built from and (WT.3) against;
    // the source and this editor's ABI are what they stand at now. PM.5 owns
    // both halves of that comparison.
    let stamp_path = dir.join(".build-stamp");
    let Ok(text) = std::fs::read_to_string(&stamp_path) else {
        // No stamp: the artifact was placed by hand or by a lattice predating
        // PM.5. Nothing to compare against, so nothing to claim.
        return BuildState::NotBuilt;
    };
    let Some(stamped) = crate::build::Stamp::parse(&text) else {
        // WT.3: a stamp from a lattice predating the ABI field. It cannot say
        // what the artifact was built against, so it cannot support a `cached`
        // claim — and `cached` is exactly the false reassurance that made the
        // original failure invisible.
        return BuildState::NotBuilt;
    };
    let build_dir = match &source {
        crate::resolve::PluginSource::Local(p) => p.clone(),
        // A git plugin builds out of its source-cache checkout.
        _ => crate::default_source_cache_dir().join(&record.name),
    };
    if !build_dir.is_dir() {
        return BuildState::NotBuilt;
    }
    if stamped == crate::build::Stamp::current(&build_dir) {
        BuildState::Cached
    } else {
        // Either the source moved or the ABI did. Both are answered the same
        // way — rebuild from source — so `:plugins` does not need to
        // distinguish them, and the build log says which it was.
        BuildState::Stale
    }
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
    /// PL8.E: the runtime-mutable decoration-producer registry (RCU-register a
    /// loaded decoration plugin's `WasmDecorationSource`). The host's per-tick
    /// `maybe_refresh_wasm_decorations` reads the same handle wait-free.
    pub decoration_registry: Option<GutterDecorationSourceRegistryHandle>,
    /// IM.6b: where a loaded `media` plugin's producer is registered.
    pub media_registry: Option<lattice_mode::MediaSourceRegistryHandle>,
    /// OM.A1: where a plugin's agenda-row producer lands. Absent leaves the
    /// seam `NotWired` — the load fails loudly rather than reporting success
    /// and contributing nothing to every `:agenda` forever.
    pub agenda_registry: Option<lattice_mode::AgendaSourceRegistryHandle>,
    /// TC.2: the runtime-mutable context-producer registry (RCU-register a
    /// loaded context plugin's `WasmContextSource`). The host's reparse-driven
    /// refresh reads the same handle wait-free.
    pub context_registry: Option<ContextSourceRegistryHandle>,
    /// TC.4: the theme registry a `theme` plugin's elements register into — the
    /// SAME one builtins use, so a plugin element is themeable and
    /// `:customize`-able like any other.
    pub theme_registry: Option<lattice_theme::ThemeRegistryHandle>,
    /// CM.6b: the compilation parser-factory registry an `error-parser`
    /// plugin's factory RCU-registers into. The compilation service reads
    /// the same handle once per run to mint each pipe reader's parser.
    pub parser_factories: Option<lattice_compilation::CompilationParserFactoriesHandle>,
    /// CR.3: the help-topic registry a `help` plugin's pages RCU-register
    /// into — the SAME one the builtin docs live in, so a plugin page opens,
    /// completes and cross-links like any other.
    pub help_topics: Option<lattice_help::topics::HelpTopicRegistryHandle>,
    /// CR.4: the dashboard section registry a `dashboard` plugin's sections
    /// RCU-register into. Shadowing rather than overwriting (CR.2), so a
    /// plugin replacing a builtin section is reversed by unload.
    pub dashboard_sections: Option<lattice_dashboard::DashboardRegistryHandle>,
    /// TR.2b: the transient-menu registry a `transient-source` plugin's menu
    /// registers into — the SAME one magit's menus live in, so a plugin menu
    /// opens through `Effect::OpenTransient` like any other. Owned by
    /// `editor_boot` since TR.1, which is what stops a plugin menu depending
    /// on whether magit happened to load.
    pub transient_registry: Option<lattice_picker::TransientSourceRegistryHandle>,
    /// PO.2: the boundary tracer the loader attaches to each async seam actor
    /// (`actor.with_tracer(...)` before spawning `run()`), so the actor emits a
    /// `PluginTraceRecord` per guest call. `None` degrades to no tracing.
    pub tracer: Option<lattice_plugin_host::PluginTracerHandle>,
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
    pub decoration_registry: bool,
    pub context_registry: bool,
    pub theme_registry: bool,
    /// CM.6b: the compilation parser-factory registry.
    pub parser_factories: bool,
    /// CR.3: the help-topic registry.
    pub help_topics: bool,
    /// CR.4: the dashboard section registry.
    pub dashboard_sections: bool,
    /// IM.6b: the inline-media producer registry.
    ///
    /// Added at OM.A1 alongside `agenda_registry`. It was missing — media
    /// drained through a service this struct never reported on, so a boot
    /// ordering regression there would have degraded `drain_media` to a
    /// `NotWired` skip with nothing asserting otherwise. Adding the sibling
    /// and leaving this one silent would be aligned-by-silence.
    pub media_registry: bool,
    /// OM.A1: the agenda-row producer registry.
    pub agenda_registry: bool,
    /// TR.2b: the transient-menu registry.
    pub transient_registry: bool,
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
            && self.decoration_registry
            && self.context_registry
            && self.theme_registry
            && self.parser_factories
            && self.help_topics
            && self.dashboard_sections
            && self.transient_registry
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
    /// PM.8b: builds running (or failed) **this session**, keyed by plugin
    /// name.
    ///
    /// The only piece of build state not derived from disk. It is
    /// deliberately not persisted: a build interrupted by a crash is not
    /// still running after a restart, and a failure the user has since fixed
    /// should not greet them on the next boot. On a fresh start the artifact
    /// either exists — with a stamp saying whether it is stale — or it does
    /// not, and that is the whole truth.
    building: Mutex<std::collections::HashMap<String, BuildActivity>>,
    /// PM.8b: how many builds are running, as a lock-free counter.
    ///
    /// Duplicated from `building` on purpose. The headerline's `version()` is
    /// polled by the cells worker on **every tick** and the trait's contract
    /// says it must not block; taking a mutex there — even an uncontended one
    /// — puts a lock on a per-tick path for a number that is almost always
    /// zero. The map stays the source of truth for *which* plugin is doing
    /// what; this is the cheap "is anything happening" the tick asks.
    building_count: std::sync::atomic::AtomicUsize,
    /// PM.7b: specs declared via `plugin-manager.require`, accumulated as
    /// config guests load and drained once by the boot task.
    ///
    /// It lives here rather than being returned from `load_discovered`
    /// because the seam is one of several a guest may provide — an init.rs
    /// that also contributes a keymap goes down the same path — and threading
    /// a second return value through every arm to serve one of them would put
    /// the cost on all of them.
    required: Mutex<Vec<pipeline::RequiredSpec>>,
    /// WT.4: plugins that tried to load and could not, for `:plugins`.
    ///
    /// In memory, not on disk. A load failure is a fact about *this* boot
    /// against *this* editor — persisting it would mean showing a user an error
    /// about a plugin they have since rebuilt, which is the same reasoning
    /// [`BuildState::Failed`] is in-memory for.
    failed: Mutex<Vec<FailedLoad>>,
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

/// The canonical id of the user-config plugin — the `:reload-config` /
/// auto-reload target, and the `id = "init"` a `<config>/lattice/init/plugin.toml`
/// declares.
pub(crate) const INIT_PLUGIN_ID: &str = "init";

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
        self.id
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
            building: Mutex::new(std::collections::HashMap::new()),
            building_count: std::sync::atomic::AtomicUsize::new(0),
            required: Mutex::new(Vec::new()),
            failed: Mutex::new(Vec::new()),
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
            building: Mutex::new(std::collections::HashMap::new()),
            building_count: std::sync::atomic::AtomicUsize::new(0),
            required: Mutex::new(Vec::new()),
            failed: Mutex::new(Vec::new()),
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
            decoration_registry: self.env.decoration_registry.is_some(),
            context_registry: self.env.context_registry.is_some(),
            theme_registry: self.env.theme_registry.is_some(),
            parser_factories: self.env.parser_factories.is_some(),
            help_topics: self.env.help_topics.is_some(),
            dashboard_sections: self.env.dashboard_sections.is_some(),
            media_registry: self.env.media_registry.is_some(),
            agenda_registry: self.env.agenda_registry.is_some(),
            transient_registry: self.env.transient_registry.is_some(),
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
            // Already loaded by something else — skip rather than load it a
            // SECOND time. Boot has two paths into the same directory: a
            // `require`d plugin is staged into the user root and loaded
            // eagerly (so `enable_mode` can fire against a real load), and the
            // on-disk scan then walks that same root. Without this, every
            // `require`d plugin registered its modes, commands and keymaps
            // twice and appeared twice in `:plugins`.
            //
            // The guard belongs here, on the SCANNING path, rather than in
            // `load_discovered`: "load everything in this directory" can
            // always skip what is already in, whereas an explicit
            // `:plugin-load` is a request the user made and `reload` unloads
            // before loading again.
            if self.is_loaded(&plugin.manifest.id) {
                tracing::debug!(
                    plugin = %plugin.manifest.id,
                    dir = %plugin.dir.display(),
                    "already loaded; not loading it a second time"
                );
                continue;
            }
            match self.load_discovered(&plugin, tier).await {
                Ok(_) => {
                    loaded += 1;
                    self.clear_failure(&plugin.manifest.id);
                }
                Err(err) => {
                    // WT.4: `warn!` reaches `*messages*`, and the record reaches
                    // `:plugins`. Both, because they answer different questions:
                    // the log says a thing went wrong just now, the record
                    // answers "why is org not here?" asked ten minutes later.
                    tracing::warn!(
                        plugin = %plugin.manifest.id,
                        dir = %plugin.dir.display(),
                        error = %err,
                        "plugin failed to load; skipped"
                    );
                    self.record_failure(&plugin.manifest.id, &plugin.dir, &err.to_string());
                }
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

        // PL8.H.1: resolve the capability grant once for the manager-view status.
        // `grant` is pure (manifest + tier), so this mirrors exactly what each
        // seam spawn computes internally — `denied` is the tier-withheld set,
        // `granted` the requested capabilities that survived it.
        let outcome = lattice_plugin_host::grant(manifest, tier);
        let denied = outcome.denied.clone();
        let granted: Vec<Capability> = manifest
            .requested
            .iter()
            .filter(|cap| !denied.contains(cap))
            .cloned()
            .collect();

        let mut record = LoadedRecord {
            id: PluginId(0),
            name: manifest.id.clone(),
            source_dir: Some(plugin.dir.clone()),
            lifecycle: None,
            tasks: Vec::new(),
            teardown: PluginTeardown::new(PluginId(0)),
            tier,
            granted,
            denied,
            health: PluginHealth::Healthy,
            default_modes: manifest.default_modes.clone(),
            source: plugin.source.clone(),
        };
        // EVERY host id this load issued, in DRAIN order (see the sort below —
        // no longer the order `provides` lists them in).
        //
        // Each `spawn_*` issues its own — deliberately, since a provenance id
        // must never be derived from guest-controlled input and so cannot be
        // keyed on the manifest's string id. That means a plugin providing N
        // seams stamps its contributions with N provenances, and teardown has
        // to reverse all of them. Keeping only the first is why bundled
        // `auto-pair` (grammar, modes, config, help) leaked its `:help` pages
        // on unload.
        let mut seam_ids: Vec<PluginId> = Vec::new();

        if manifest.provides.is_empty() {
            // Lifecycle-only (base `plugin` world): instantiate + activate.
            let mut instance = self
                .host
                .instantiate_plugin(&component, manifest, tier, PluginBudget::default())
                .await?;
            instance.activate().await?;
            seam_ids.push(instance.id());
            record.lifecycle = Some(instance);
        } else {
            // OM.0: drain in DEPENDENCY order, not manifest order. A
            // `mode-keymap-binding` resolves its command name against the
            // `CommandRegistry` at registration, so a mode binding a chord to
            // the plugin's own grammar action needs `grammar` drained first —
            // and `provides` is guest-controlled input, so trusting its order
            // made a load-bearing invariant depend on a comment in someone
            // else's TOML. `drain_rank` decides; the sort is stable, so ties
            // keep the author's ordering.
            let mut seams = manifest.provides.clone();
            seams.sort_by_key(|s| s.drain_rank());
            for seam in &seams {
                match seam {
                    PluginSeam::PickerSource => {
                        let id = self
                            .drain_picker(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Config => {
                        let id = self
                            .drain_config(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Events => {
                        let id = self
                            .drain_events(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Grammar => {
                        let id = self.drain_grammar(&component, manifest, tier)?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Modes => {
                        let id = self
                            .drain_mode(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::CompletionSource => {
                        let id = self
                            .drain_completion(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Keymap => {
                        let id = self
                            .drain_keymap(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Media => {
                        let id = self
                            .drain_media(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Decorations => {
                        let id = self
                            .drain_decorations(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Context => {
                        let id = self
                            .drain_context(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Theme => {
                        let id = self
                            .drain_theme(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    // PO.5: `logging` is a host import the guest CONSUMES (Layer 2),
                    // not a contribution it provides — it never appears in a
                    // well-formed `provides`, and the import is wired into the
                    // linker for every async world regardless. A malformed manifest
                    // that lists it drains nothing (no-op), never an error.
                    // PM.7b: the `require` seam. Drained during the ordinary
                    // load, so the component compiled at the top of this
                    // function is reused — the alternative (spawning the
                    // guest a second time to read its specs) would compile
                    // init.rs twice on every boot to fetch a list.
                    PluginSeam::PluginManager => {
                        let id = self
                            .drain_require(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    // CM.6b: live. The registry holds a FACTORY rather than a
                    // parser because the compilation `ParserRegistry` is built
                    // per pipe reader — stdout and stderr each get one — and a
                    // `WasmErrorParser` owns a `Store`, so it cannot be shared
                    // between them. Each reader mints its own, which is also
                    // semantically right: the two streams carry independent
                    // pending state.
                    PluginSeam::ErrorParser => {
                        let id = self.drain_error_parser(&component, manifest, tier)?;
                        seam_ids.push(id);
                    }
                    // OM.A1: an agenda-row producer. Live, like `media` — the
                    // guest stays instantiated and is called once per file of
                    // every scan, so its per-scan state (`begin`) has
                    // somewhere to live.
                    PluginSeam::AgendaSource => {
                        let id = self
                            .drain_agenda(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    // TR.2b: a keyed menu. Live, like `dashboard` — a menu's
                    // rows depend on where it was opened from, so the guest
                    // stays instantiated and `build` is called per open.
                    PluginSeam::TransientSource => {
                        let id = self
                            .drain_transient(&component, manifest, tier, &mut record)
                            .await?;
                        seam_ids.push(id);
                    }
                    // CR.3: the plugin's `:help` pages. Data, not a live
                    // guest — the bodies cross once here and the store is
                    // dropped, so reading `:help` never touches wasm.
                    PluginSeam::Help => {
                        let id = self.drain_help(&component, manifest, tier).await?;
                        seam_ids.push(id);
                    }
                    // LG.3c: the plugin's languages. Data like `help` — the
                    // grammar bytes and query sources cross once here, the
                    // host compiles the grammar, and the guest is dropped.
                    // Parsing never touches wasm-the-plugin again.
                    PluginSeam::Language => {
                        let id = self.drain_language(&component, manifest, tier).await?;
                        seam_ids.push(id);
                    }
                    // CR.4: the plugin's launch-page sections. Unlike `help`,
                    // each keeps a live guest — a section is a function of a
                    // `DashboardCtx`, so it is called per compose.
                    PluginSeam::Dashboard => {
                        let id = self.drain_dashboard(&component, manifest, tier)?;
                        seam_ids.push(id);
                    }
                    PluginSeam::Logging => {} // Exhaustive: every contribution `PluginSeam` variant is drained
                                              // (PL8.E closed the last, decorations). A new seam variant
                                              // must add its drain here — the compiler enforces it rather
                                              // than a silent skip.
                }
            }
        }

        // The FIRST id is the plugin's user-facing identity (`:list-plugins`,
        // `SourceLayer::Plugin` rendering); all of them are what teardown
        // reverses.
        let Some(&id) = seam_ids.first() else {
            return Err(PluginLoaderError::NothingLoaded);
        };
        record.id = id;
        record.teardown.plugin_id = id;
        record.teardown.seam_ids = seam_ids;

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
        // CI.1: announce the load AFTER the full drain (every seam registered) so
        // a subscriber's handler observes a fully-loaded plugin — an `init.rs`
        // runs its deferred `on-plugin-loaded` config here. Fires for `init.rs`
        // itself too (harmless: handlers match other plugins by name).
        if let Some(bus) = &self.env.bus {
            bus.publish(lattice_protocol::Event::PluginLoaded {
                name: manifest.id.clone(),
                id: id.0,
            });
        }
        // PM.3: a plugin declaring a `default_mode` gets a `<id>.enabled` bool
        // gate (default true). Register it, read its current value, and enable /
        // disable the declared mode accordingly — the batteries-included path
        // (auto-pair on out of the box), user-overridable via `:set
        // <id>.enabled=false`. Subsequent changes are handled by
        // `subscribe_mode_gates`.
        self.apply_default_mode_gate(&manifest.id, &manifest.default_modes);
        // One-shot, user-actionable event (the "LSP server attached" class).
        tracing::info!(plugin = %manifest.id, id = id.0, "plugin loaded");
        Ok(id)
    }

    /// The name of a plugin's enable-gate option — `<id>.enabled` (PM.3).
    fn enabled_option_name(plugin_id: &str) -> String {
        format!("{plugin_id}.enabled")
    }

    /// PM.3: register (if new) the `<id>.enabled` gate for a plugin declaring a
    /// `default_mode`, then request the mode's enablement to match the option's
    /// current value. A no-op when the plugin declares no default mode, or when no
    /// config registry / bus is wired.
    fn apply_default_mode_gate(&self, plugin_id: &str, default_modes: &[String]) {
        if default_modes.is_empty() {
            return;
        }
        let (Some(registry), Some(bus)) =
            (self.env.config_registry.as_ref(), self.env.bus.as_ref())
        else {
            return;
        };
        let option = Self::enabled_option_name(plugin_id);
        // Idempotent: a re-load (or a plugin that declared the option itself)
        // leaves the existing value untouched; only the first load registers it.
        lattice_plugin_host::config_host::register_plugin_option(
            registry,
            &option,
            lattice_plugin_host::config_host::PluginOptionKind::Boolean,
            "true",
            "Enable this plugin's default mode.",
        );
        let enabled = registry
            .lookup(&option)
            .map(|opt| opt.get_formatted() == "true")
            .unwrap_or(true);
        // One gate, N modes (OC.1a). `<id>.enabled` is the PLUGIN's switch, so
        // a plugin with two on-by-default modes gets one option rather than
        // one per mode — the user is turning org on or off, not curating its
        // internals.
        for mode in default_modes {
            bus.publish(lattice_protocol::Event::ModeEnablementRequested {
                mode: mode.clone(),
                enabled,
            });
        }
    }

    /// PM.7/PM.8 follow-up: honour a `require`'s `enable-mode` sugar.
    ///
    /// Publishes the same `ModeEnablementRequested` the manifest
    /// `default_mode` gate publishes — one mechanism, two ways of asking for
    /// it (a plugin declaring its own default, or a user's `init.rs` asking
    /// for it at the call site).
    ///
    /// The host never learns the mode-id statically: it arrives in the spec
    /// and is forwarded as an opaque string, so the mode stays the plugin's
    /// own surface (`feedback_mode_owns_its_surface`).
    ///
    /// A missing bus is a silent skip — the same degradation every other
    /// event publisher here uses when the editor is not fully wired (tests,
    /// headless harnesses).
    pub fn request_mode_enablement(&self, mode: &str) {
        let Some(bus) = self.env.bus.as_ref() else {
            return;
        };
        bus.publish(lattice_protocol::Event::ModeEnablementRequested {
            mode: mode.to_string(),
            enabled: true,
        });
    }

    /// PL8.H.1: a read-only snapshot of every loaded plugin — identity, trust
    /// tier, capabilities granted/denied, and health — for the `:plugins`
    /// manager view (PL8.H.2/.3). Cloned out under the loaded-set lock, so the
    /// view renders a stable frame while loads/unloads proceed.
    pub fn plugin_status(&self) -> Vec<PluginStatus> {
        // Snapshot the in-flight set once, outside the loaded-set lock: two
        // locks held at once is how a deadlock gets written, and the build
        // task takes `building` while the view takes `loaded`.
        let activity = self.building.lock().map(|m| m.clone()).unwrap_or_default();
        let mut rows: Vec<PluginStatus> = self
            .loaded
            .lock()
            .expect("plugin-loader loaded-set mutex poisoned")
            .iter()
            .map(|r| PluginStatus {
                id: r.id.0,
                name: r.name.clone(),
                tier: r.tier,
                granted: r.granted.clone(),
                denied: r.denied.clone(),
                health: r.health.clone(),
                source: r.source.clone(),
                build: build_state_of(r, activity.get(&r.name)),
            })
            .collect();
        // Stable, name-sorted order (not raw load order). The `:plugins` view keys
        // its in-view chords on `cursor.line → this Vec's index`, and a `:plugin`
        // reload internally unloads + re-appends (moving the record to the end of
        // `loaded`). Sorting by name keeps a reloaded plugin's row in place, so the
        // cursor still targets it, and makes the list order predictable for the
        // user rather than discovery-order. Names are unique per loaded set.
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// WT.4: the plugins that tried to load this session and could not.
    ///
    /// Name-sorted like [`plugin_status`](Self::plugin_status), for the same
    /// reason: the view is read down a column, and discovery order is not
    /// something a user can predict or reproduce.
    pub fn failed_loads(&self) -> Vec<FailedLoad> {
        let mut rows = self.failed.lock().map(|f| f.clone()).unwrap_or_default();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Record (or replace) `name`'s load failure.
    ///
    /// Replaces rather than appends: a `:plugin-reload` that fails again should
    /// leave one row saying what is wrong now, not a growing pile of attempts.
    /// The view is a description of the current state, not a history.
    fn record_failure(&self, name: &str, dir: &std::path::Path, error: &str) {
        let Ok(mut failed) = self.failed.lock() else {
            // A poisoned mutex here would mean losing a diagnostic, and losing a
            // diagnostic is not worth taking the editor down over — this whole
            // mechanism exists because a missing message cost a debugging
            // session, so it must not itself become a crash.
            tracing::debug!(plugin = name, "failed-load set poisoned; not recording");
            return;
        };
        failed.retain(|f| f.name != name);
        failed.push(FailedLoad {
            name: name.to_string(),
            dir: dir.to_path_buf(),
            error: error.to_string(),
        });
    }

    /// Drop `name`'s failure record — it loaded.
    ///
    /// Called on every successful load rather than only on a reload, because the
    /// paths into a load are several (boot scan, `require`, `:plugin-load`,
    /// `:plugin-reload`) and a stale "failed" row surviving a load that worked
    /// is a worse lie than no row at all.
    fn clear_failure(&self, name: &str) {
        if let Ok(mut failed) = self.failed.lock() {
            failed.retain(|f| f.name != name);
        }
    }

    /// PL8.H.1: mark the plugin `plugin` quarantined (its instance trapped) — the
    /// body of the `Event::PluginCrashed` subscription ([`subscribe_health`]),
    /// exposed directly so a test can drive the health flip without a live bus.
    /// A crash id matching no loaded plugin is ignored (it may have been unloaded
    /// between the trap and the drain) — never a panic.
    ///
    /// [`subscribe_health`]: Self::subscribe_health
    pub fn mark_quarantined(&self, plugin: u32, func: String, kind: String) {
        let mut loaded = self
            .loaded
            .lock()
            .expect("plugin-loader loaded-set mutex poisoned");
        if let Some(record) = loaded.iter_mut().find(|r| r.id.0 == plugin) {
            record.health = PluginHealth::Quarantined { func, kind };
        }
    }

    /// PL8.H.1: subscribe to `Event::PluginCrashed` so a trapped plugin's health
    /// flips to `Quarantined` in the manager view. Filtered by kind (indexed
    /// dispatch); events drain on the shared runtime via a `Channel` sink, OFF
    /// the keystroke path (the bus calls the sink lock-dropped). Holds a
    /// `Weak<Self>` so the drain task never keeps the loader alive — the loop
    /// ends when the loader drops. Called once by [`install`]; a no-op if no
    /// bus/runtime was wired (the minimal test constructor).
    pub fn subscribe_health(self: &Arc<Self>) {
        let (Some(bus), Some(runtime)) = (self.env.bus.as_ref(), self.env.runtime.as_ref()) else {
            return;
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        bus.subscribe(
            EventFilter::kind(EventKind::PluginCrashed),
            SubscriptionTarget::Channel(tx),
        );
        let weak = Arc::downgrade(self);
        runtime.spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Event::PluginCrashed { plugin, func, kind } = event {
                    let Some(loader) = weak.upgrade() else { break };
                    loader.mark_quarantined(plugin, func, kind);
                }
            }
        });
    }

    /// PM.3: react to `<id>.enabled` changes — the config gate for a plugin's
    /// default mode. On a `:set <id>.enabled=<bool>` (an `OptionChanged`), map the
    /// option back to the loaded plugin's `default_mode` and request the mode's
    /// enablement to match, so the toggle activates / deactivates it live. Mirrors
    /// [`Self::subscribe_health`]; a no-op when no bus/runtime was wired.
    pub fn subscribe_mode_gates(self: &Arc<Self>) {
        let (Some(bus), Some(runtime)) = (self.env.bus.as_ref(), self.env.runtime.as_ref()) else {
            return;
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        bus.subscribe(
            EventFilter::kind(EventKind::OptionChanged),
            SubscriptionTarget::Channel(tx),
        );
        let bus = bus.clone();
        let weak = Arc::downgrade(self);
        runtime.spawn(async move {
            while let Some(event) = rx.recv().await {
                let Event::OptionChanged { name, new, .. } = event else {
                    continue;
                };
                let Some(plugin_id) = name.strip_suffix(".enabled") else {
                    continue;
                };
                let Some(loader) = weak.upgrade() else { break };
                // Map `<id>.enabled` → the loaded plugin's default modes.
                let modes = {
                    let loaded = loader
                        .loaded
                        .lock()
                        .expect("plugin-loader loaded-set mutex poisoned");
                    loaded
                        .iter()
                        .find(|r| r.name == plugin_id)
                        .map(|r| r.default_modes.clone())
                        .unwrap_or_default()
                };
                for mode in modes {
                    bus.publish(lattice_protocol::Event::ModeEnablementRequested {
                        mode,
                        enabled: new == "true",
                    });
                }
            }
        });
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
        // WT.4: a `discover_one` failure is deliberately NOT recorded above —
        // that is "this directory is not a plugin", which the caller asked about
        // and gets as an error. Past this point the directory *is* a plugin, so
        // a failure is a plugin that should be here and is not, and it belongs
        // in `:plugins` however the load was triggered.
        let outcome = self.load_discovered(&plugin, tier).await;
        match &outcome {
            Ok(_) => self.clear_failure(&plugin.manifest.id),
            Err(err) => self.record_failure(&plugin.manifest.id, dir, &err.to_string()),
        }
        outcome
    }

    /// PM.8b: how many builds are running right now.
    ///
    /// The `:plugins` headerline reads this, per the
    /// async-buffer-status-in-headerline rule — a build takes seconds to
    /// minutes and the user needs to see it is happening somewhere other than
    /// a status line that the next echo will overwrite.
    pub fn builds_in_flight(&self) -> usize {
        self.building_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set_build_activity(&self, name: &str, activity: Option<BuildActivity>) {
        if let Ok(mut map) = self.building.lock() {
            match activity {
                Some(a) => {
                    map.insert(name.to_string(), a);
                }
                None => {
                    map.remove(name);
                }
            }
            // Recount under the same lock the map was mutated under, so the
            // counter can never disagree with it.
            let running = map
                .values()
                .filter(|a| matches!(a, BuildActivity::Running))
                .count();
            self.building_count
                .store(running, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// PM.8b: force a fresh build of `name` from its recorded source, then
    /// reload it.
    ///
    /// "Force" is the difference from an ordinary load: the build service
    /// short-circuits on a matching stamp, which is exactly what you do NOT
    /// want when a user pressed rebuild. The stamp is removed first so the
    /// build is unconditional — the user asked, not the staleness check.
    ///
    /// Returns the error when the rebuild could not happen or did not
    /// succeed, having left the plugin as it was. A failed rebuild never
    /// unloads a working plugin: PM.5's `StaleKept` keeps the old artifact,
    /// and this reloads from it.
    ///
    /// Blocking work runs on `spawn_blocking`; only the reload is awaited.
    pub async fn rebuild(&self, name: &str) -> Result<(), String> {
        let (source, dir) = {
            let loaded = self
                .loaded
                .lock()
                .map_err(|_| "plugin registry unavailable".to_string())?;
            let record = loaded
                .iter()
                .find(|r| r.name == name)
                .ok_or_else(|| format!("`{name}` is not loaded"))?;
            (record.source.clone(), record.source_dir.clone())
        };
        if !source.is_buildable() {
            // Bundled ships prebuilt and Unknown has nowhere to build from.
            // Saying so beats running a build that cannot work.
            return Err(format!(
                "`{name}` has no buildable source ({})",
                source.label()
            ));
        }
        let Some(plugin_source) = source.as_plugin_source() else {
            return Err(format!("`{name}` has no recorded source"));
        };
        let Some(user_root) = default_plugins_dir() else {
            return Err("no config directory for the plugin cache".to_string());
        };

        self.set_build_activity(name, Some(BuildActivity::Running));
        // Drop the stamp so the build is unconditional — see above.
        if let Some(dir) = &dir {
            let _ = std::fs::remove_file(dir.join(".build-stamp"));
        }

        let spec = pipeline::RequiredSpec {
            name: name.to_string(),
            source: plugin_source,
            enable_mode: None,
            pinned: false,
        };
        let cache_root = default_source_cache_dir();
        let install = tokio::task::spawn_blocking(move || {
            pipeline::install_required(
                &resolve::SystemGit,
                &resolve::HttpFetcher,
                &build::CargoComponentBuilder,
                &spec,
                &cache_root,
                &user_root,
            )
        })
        .await
        .map_err(|e| format!("rebuild task failed: {e}"))?;

        match install {
            pipeline::Install::Ready {
                stale: Some(err), ..
            } => {
                self.set_build_activity(name, Some(BuildActivity::Failed));
                Err(err)
            }
            pipeline::Install::Skipped { error, .. } => {
                self.set_build_activity(name, Some(BuildActivity::Failed));
                Err(error)
            }
            pipeline::Install::Ready { .. } => {
                // Clear before the reload, not after: the reload republishes
                // status, and a row still reading `building…` after its build
                // finished is the kind of stuck indicator users stop trusting.
                self.set_build_activity(name, None);
                let tier = TrustTier::UserInstalled;
                self.reload(name, tier)
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("rebuilt, but reload failed: {e}"))
            }
        }
    }

    /// PM.7b: take the plugins declared via `require` so far, leaving the
    /// queue empty.
    ///
    /// Drained exactly once per boot by the install task. Draining rather than
    /// reading is what stops a second call from resolving, building and
    /// loading the same set twice.
    pub fn take_required(&self) -> Vec<pipeline::RequiredSpec> {
        self.required
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
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
        // PO.1/PO.5: reclaim this plugin's boundary-trace state (its per-plugin
        // ring, gate override, and hot-path gate). ids are monotonic, so without
        // this every unload/reload would leak a ring — the global ring keeps the
        // historical records. The tracer lock is poison-tolerant, so this never
        // fails the unload.
        if let Some(tracer) = &self.env.tracer {
            tracer.forget_plugin(record.id.0);
        }
        // CI.1: announce the unload AFTER teardown reversed every contribution, so
        // a handler tears down its own dependent setup against a plugin that's
        // already gone from the registries.
        if let Some(bus) = &self.env.bus {
            bus.publish(lattice_protocol::Event::PluginUnloaded {
                name: record.name.clone(),
                id: record.id.0,
            });
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

    /// Load the `init` config if it isn't loaded, or reload it if it is — the
    /// idempotent "make init reflect what's on disk" the auto-reload watcher
    /// (PL8.D.4) fires on every change to `<config>/lattice/init/`. First good
    /// build loads; a rebuild reloads (unbinding the old keymaps / commands and
    /// re-applying); a *broken* rebuild leaves `init` unloaded (reload unloads
    /// before it fails to re-load), which the next good build heals — `is_loaded`
    /// is then false, so this loads rather than reloads. Errors propagate for the
    /// caller to log; never panics.
    pub async fn sync_init(
        &self,
        init_dir: &std::path::Path,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        if self.is_loaded(INIT_PLUGIN_ID) {
            self.reload(INIT_PLUGIN_ID, tier).await
        } else {
            self.load_path(init_dir, tier).await
        }
    }

    /// Reverse a plugin's registry contributions against the live registries.
    /// The `ArcSwap`-held registries (command / picker / mode) are RCU'd —
    /// snapshot-clone → `&mut` → [`PluginTeardown::unload`] → store — while the
    /// `Arc`-shared interior-mutable ones (config / keymap / bus) pass by
    /// reference. A missing registry handle (a partially-wired test loader)
    /// downgrades to a logged no-op reversal, never a panic.
    fn run_teardown(&self, teardown: &PluginTeardown) -> TeardownReport {
        // CR.3: the help registry is reversed FIRST, and deliberately outside
        // the all-or-nothing `let-else` below.
        //
        // Two reasons. It lives in `lattice-help`, so `PluginTeardown::unload`
        // — which is in `lattice-plugin-host` — cannot touch it without
        // pulling that crate across the boundary for one field. And it shares
        // nothing with the handles the `let-else` demands, so gating it on
        // them would mean an under-wired loader leaves a plugin's `:help`
        // pages behind after `:plugin-unload` reported success. Stale docs
        // for code that is gone is a worse failure than the partial teardown
        // that caused it, and a quieter one.
        // Over EVERY seam id — `help` is rarely a plugin's first seam, and
        // reversing only `plugin_id` is what left `auto-pair`'s pages behind.
        let provenances = teardown.provenances();
        let mut help_topics_removed = 0;
        if let Some(help_h) = self.env.help_topics.as_ref() {
            help_h.rcu(|current| {
                let mut next = (**current).clone();
                help_topics_removed = 0;
                for id in &provenances {
                    help_topics_removed += next.unregister_plugin(id.0 as u64);
                }
                Arc::new(next)
            });
        }
        // LG.3c: same placement and reasoning as `help` above, with one
        // difference worth noting — the language registry is process-global,
        // so unlike every other registry here there is no handle that can be
        // absent and therefore no way for this to be silently skipped by an
        // under-wired loader. Leaving a language registered would be worse
        // than stale docs: a buffer would keep claiming a grammar its plugin
        // no longer provides.
        let languages_removed: usize = provenances
            .iter()
            .map(|id| lattice_syntax::plugin_lang::unregister_plugin(id.0 as u64))
            .sum();
        // CR.4: same placement, same reasoning — plus one of its own. Leaving
        // a plugin's section registered after unload would keep calling a
        // guest whose plugin is gone on every compose.
        let mut dashboard_sections_removed = 0;
        if let Some(dash_h) = self.env.dashboard_sections.as_ref() {
            dash_h.rcu(|current| {
                let mut next = (**current).clone();
                dashboard_sections_removed = 0;
                for id in &provenances {
                    dashboard_sections_removed += next.unregister_plugin(id.0 as u64);
                }
                Arc::new(next)
            });
        }
        // TR.2b: same placement and reasoning as `help` above — the registry is
        // `Arc`-shared with interior mutability rather than one of the `&mut`
        // snapshots below, and leaving a name registered would be worse than
        // stale docs: the entry holds a client whose actor has ended, so the
        // chord would report a host error rather than "unknown source".
        let mut transient_sources_removed = 0;
        if let Some(tr_h) = self.env.transient_registry.as_ref() {
            for name in &teardown.transient_sources {
                if tr_h.unregister(name) {
                    transient_sources_removed += 1;
                }
            }
        }
        let (
            Some(cmd_h),
            Some(pick_h),
            Some(mode_h),
            Some(config),
            Some(keymap),
            Some(bus),
            Some(deco_h),
            Some(ctx_h),
            Some(theme_h),
            Some(parsers_h),
        ) = (
            self.env.command_registry.as_ref(),
            self.env.picker_registry.as_ref(),
            self.env.mode_registry.as_ref(),
            self.env.config_registry.as_ref(),
            self.env.keymap.as_ref(),
            self.env.bus.as_ref(),
            self.env.decoration_registry.as_ref(),
            self.env.context_registry.as_ref(),
            self.env.theme_registry.as_ref(),
            self.env.parser_factories.as_ref(),
        )
        else {
            tracing::warn!(
                "plugin teardown skipped: loader missing a registry handle (partial unload)"
            );
            return TeardownReport {
                help_topics: help_topics_removed,
                dashboard_sections: dashboard_sections_removed,
                languages: languages_removed,
                transient_sources: transient_sources_removed,
                ..TeardownReport::default()
            };
        };

        // Owned snapshots of the ArcSwap registries for the `&mut` unload needs.
        let mut commands = (**cmd_h.load()).clone();
        let mut pickers = (**pick_h.load()).clone();
        let mut modes = (**mode_h.load()).clone();
        let mut decorations = (**deco_h.load()).clone();
        let mut contexts = (**ctx_h.load()).clone();
        let report = {
            let mut media_reg = self
                .env
                .media_registry
                .as_ref()
                .map(|r| (**r.load()).clone())
                .unwrap_or_default();
            let mut agenda_reg = self
                .env
                .agenda_registry
                .as_ref()
                .map(|r| (**r.load()).clone())
                .unwrap_or_default();
            let mut reg = TeardownRegistries {
                media: &mut media_reg,
                agenda: &mut agenda_reg,
                commands: &mut commands,
                pickers: &mut pickers,
                modes: &mut modes,
                keymap,
                config,
                bus,
                decorations: &mut decorations,
                contexts: &mut contexts,
                theme: &**theme_h,
                // CM.6b: RCU'd inside `unload` (it holds the `ArcSwap`
                // handle directly rather than a `&mut` snapshot), because a
                // compilation run may be reading it concurrently and the
                // common case removes nothing at all.
                parsers: parsers_h,
            };
            let report = teardown.unload(&mut reg);
            // The media / agenda snapshots are `&mut` clones, so the reversal
            // has to be published back the same way the ArcSwap registries
            // below are. Missing this is how an unloaded producer keeps
            // contributing until the next reload.
            if let Some(h) = self.env.media_registry.as_ref() {
                h.store(Arc::new(media_reg));
            }
            if let Some(h) = self.env.agenda_registry.as_ref() {
                h.store(Arc::new(agenda_reg));
            }
            report
        };
        // Publish the reversed snapshots (RCU store).
        cmd_h.store(Arc::new(commands));
        pick_h.store(Arc::new(pickers));
        mode_h.store(Arc::new(modes));
        deco_h.store(Arc::new(decorations));
        ctx_h.store(Arc::new(contexts));
        let mut report = report;
        report.help_topics = help_topics_removed;
        report.transient_sources = transient_sources_removed;
        report.languages = languages_removed;
        report.dashboard_sections = dashboard_sections_removed;
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
        // PO.2: attach the boundary tracer so the actor emits a trace record per
        // guest call (a no-op when unwired).
        let actor = actor.with_tracer(self.env.tracer.clone());
        let task = runtime.spawn(actor.run());

        // The spec fetch is a guest call; a malformed spec fails registration
        // loudly rather than registering a broken source.
        let source = WasmPickerSource::connect(client).await?;
        let id = source.plugin_id();
        // PL8.F: `spec().id` is `Cow` now — own it for the teardown token
        // before `source` moves into the generator below.
        let source_id = source.spec().id.to_string();

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
        record.teardown.picker_sources.push(source_id);
        Ok(id)
    }

    /// TR.2b — drain the transient seam: spawn the menu actor, ask the guest for
    /// the name it registers under, and install a `register_async` builder into
    /// the editor's `TransientSourceRegistry`. Records the actor task + the
    /// menu name on `record` for teardown.
    ///
    /// The `id()` call happens ONCE, here, because it keys the registry entry
    /// and cannot change. `build` is called per open — that is the point of the
    /// seam, and it is what makes a plugin menu context-aware the way magit's
    /// dispatch is.
    async fn drain_transient(
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
            .ok_or(PluginLoaderError::NotWired("transient-source"))?;
        let runtime = self
            .env
            .runtime
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("transient-source"))?;
        let registry = self
            .env
            .transient_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("transient-source"))?;
        // The builder resolves each row's command NAME at build time, so it
        // needs the live registry rather than a snapshot taken here — a menu
        // opened later must see commands registered later.
        let commands = self
            .env
            .command_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("transient-source"))?;

        let (client, actor) = self
            .host
            .spawn_transient_source(
                component,
                manifest,
                tier,
                PluginBudget::default(),
                bus,
                self.env.config_registry.as_ref(),
            )
            .await?;

        // Drive the actor's request loop FIRST — `menu_id()` below is a guest
        // call over the client channel, which the actor must be running to
        // answer (else the await deadlocks). Same ordering as `drain_picker`.
        let actor = actor.with_tracer(self.env.tracer.clone());
        let id = client.id();
        let task = runtime.spawn(actor.run());

        // A menu with no name has nothing to register under, so a failed or
        // blank `id()` fails the drain loudly rather than registering an
        // unreachable menu.
        let name = self.host_transient_name(&client, &manifest.id).await?;

        registry.register_async(
            name.clone(),
            lattice_plugin_host::transient_builder(
                client,
                (*commands).clone(),
                manifest.id.clone(),
            ),
        );

        record.tasks.push(task);
        record.teardown.transient_sources.push(name.clone());
        tracing::debug!(
            plugin = %manifest.id,
            menu = %name,
            "transient plugin registered its menu"
        );
        Ok(id)
    }

    /// Ask a transient guest for its menu name, rejecting a blank one.
    ///
    /// Split out so the failure reads as one sentence at the call site: a blank
    /// name would register a menu `Effect::OpenTransient` could never address,
    /// which is a plugin that loads "successfully" and contributes nothing.
    async fn host_transient_name(
        &self,
        client: &lattice_plugin_host::TransientClient,
        plugin: &str,
    ) -> Result<String, PluginLoaderError> {
        let name = client.menu_id().await?;
        let trimmed = name.trim();
        if trimmed.is_empty() {
            tracing::warn!(plugin, "transient plugin returned a blank menu name");
            return Err(PluginLoaderError::NotWired("transient-source"));
        }
        Ok(trimmed.to_string())
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
        // PO.2: attach the boundary tracer so the actor emits a trace record per
        // guest call (a no-op when unwired).
        let actor = actor.with_tracer(self.env.tracer.clone());
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
        // PO.3: attach the boundary tracer so the sync grammar trampoline emits a
        // gated boundary-trace record per guest call (zero cost at the default
        // gate — a single relaxed-atomic load; design §4).
        let set = self.host.instantiate_grammar_plugin(
            component,
            manifest,
            tier,
            bus,
            self.env.tracer.as_ref(),
            // AP.3: the shared editor config registry, so a grammar action can
            // read an option (auto-pair's `auto-pair.style` gate).
            self.env.config_registry.as_ref(),
        )?;
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
        // No teardown token needed: `PluginTeardown::unload` unconditionally
        // removes every `SourceLayer::Plugin(id)` command by provenance (grammar
        // here + the modes seam's `:<mode>` toggles).
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
                // MO.1: resolves each declared option override's name + value
                // against the registry `:set` writes to. Absent in a loader
                // built without config (the minimal harnesses) — the overrides
                // are then skipped with a warning naming the mode, the same
                // logged-skip every other unwired handle takes.
                self.env.config_registry.as_deref(),
            )
            .await?;
        mode_registry.store(Arc::new(next));

        // Give each plugin-registered mode the SAME `:<mode-name>` toggle
        // ex-command native modes get at boot (`register_mode_toggle_commands`)
        // — so plugin modes are togglable by name uniformly and the
        // `:describe-mode` / `:list-modes` "Toggle with `:<mode-id>`" hint is
        // honest. Registered under `SourceLayer::Plugin(id)` so unload's
        // `unregister_plugin` reverses them; the registry `generation` bump the
        // registration triggers refreshes `:`-command completion. RCU the
        // command registry (load → clone → register → store), mirroring
        // `drain_grammar` above.
        if !mode_ids.is_empty() {
            let mut next_cmds = (**commands.load()).clone();
            for mode in &mode_ids {
                let name = mode.to_string();
                next_cmds.register_plugin_ex_command(
                    id.0,
                    &name,
                    lattice_grammar::registry::MODE_TOGGLE_COMMAND_DOC,
                    lattice_grammar::registry::mode_toggle_ex_command_spec(&name),
                );
            }
            commands.store(Arc::new(next_cmds));
        }

        tracing::debug!(
            plugin = %manifest.id,
            modes = ?mode_ids,
            "mode plugin registered its minor modes"
        );
        // Teardown tokens: each mode reverses via `ModeRegistry::unregister` +
        // `KeymapHandle::remove_layer(MinorMode(id))`; the `:<mode>` toggle
        // ex-commands registered above reverse via `unregister_plugin(id)` in
        // `PluginTeardown::unload` (provenance-driven, no per-command token).
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
        // PO.2: attach the boundary tracer so the actor emits a trace record per
        // guest call (a no-op when unwired).
        let actor = actor.with_tracer(self.env.tracer.clone());
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

    /// Drain the decorations seam (PL8.E): spawn the producer actor, wrap the
    /// plugin's [`WasmDecorationSource`] as a native
    /// [`AsyncGutterDecorationSource`], and RCU-register it into the
    /// runtime-mutable [`GutterDecorationSourceRegistryHandle`] the host's
    /// per-tick refresh drives.
    ///
    /// The decoration producer is the one hot-path-sensitive seam: it must NEVER
    /// run at paint time (a per-frame WASM call would violate paramount #1). The
    /// host caches its output per buffer and the renderer reads only the cache;
    /// the async `gutter_decorations` runs on the spawned actor (off the
    /// keystroke path), mirroring the picker / completion actor seams — driven on
    /// the runtime, recorded on `record` for unload. A missing service degrades
    /// the seam to a logged skip (`NotWired`); a spawn trap maps to
    /// TC.2 — drain the context seam: spawn the producer actor, register the
    /// `WasmContextSource` into the context registry by copy-on-write RCU, and
    /// spawn the actor's `run` loop on the runtime. Records the actor task +
    /// source id on `record` for teardown. Mirror of [`Self::drain_decorations`]
    /// — a context source, like a decoration source, carries no id/doc metadata,

    /// TC.4 — drain the theme seam: instantiate the component, drive its
    /// `register-theme-elements` export once, and record the namespaced element
    /// names for teardown. No actor and no registry RCU: elements are declared
    /// synchronously into the shared registry, like config options.
    /// PM.7b: run a config guest's `register-plugins` export and record the
    /// plugins it declared.
    ///
    /// The specs are *declarations*. Nothing is resolved, cloned, built or
    /// downloaded here — see `plugin_manager_host` for why that split is
    /// load-bearing. The boot task drains them via
    /// [`PluginLoader::take_required`] and runs the pipeline off-thread.
    ///
    /// A guest that declares the seam and requires nothing is fine: the drain
    /// is empty and the load still counts (the plugin loaded, it just asked
    /// for no company).
    async fn drain_require(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        _record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let (id, specs) = self
            .host
            .spawn_plugin_manager_plugin(component, manifest, PluginBudget::default(), tier)
            .await?;
        tracing::debug!(
            plugin = %manifest.id,
            count = specs.len(),
            "config guest declared plugins via require"
        );
        if !specs.is_empty()
            && let Ok(mut queue) = self.required.lock()
        {
            queue.extend(specs.into_iter().map(pipeline::to_required_spec));
        }
        // The seam contributes no registry entries, so there is nothing for
        // teardown to reverse — unloading the guest cannot un-install the
        // plugins it asked for, any more than removing a package list
        // uninstalls the packages.
        Ok(id)
    }

    async fn drain_theme(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        record: &mut LoadedRecord,
    ) -> Result<PluginId, PluginLoaderError> {
        let registry = self
            .env
            .theme_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("theme"))?;
        let (id, elements) = self
            .host
            .spawn_theme_plugin(component, manifest, tier, PluginBudget::default(), registry)
            .await?;
        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            elements = elements.len(),
            "theme plugin registered its elements"
        );
        record.teardown.theme_elements = elements;
        Ok(id)
    }

    /// CM.6b — the `error-parser` seam's drain.
    ///
    /// Mints the factory (which instantiates once, so a component that
    /// cannot start fails the load rather than silently contributing
    /// nothing to every build) and RCU-registers it into the compilation
    /// parser-factory registry.
    ///
    /// Takes no `record`: teardown is by **provenance**, not by token —
    /// `PluginTeardown` removes every factory carrying this plugin's
    /// host-issued id, exactly as it does for commands. There is no list
    /// to record and therefore none to forget.
    fn drain_error_parser(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        let registry = self
            .env
            .parser_factories
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("error-parser"))?;

        // The Reflex-class budget, not the lifecycle default: `feed` runs
        // once per captured line on a fast producer's critical path.
        let (id, factory) =
            self.host
                .error_parser_factory(component, manifest, tier, PluginBudget::grammar())?;

        let factory: Arc<dyn lattice_compilation::CompilationParserFactory> = Arc::new(factory);
        registry.rcu(|current| {
            let mut next = (**current).clone();
            next.register(factory.clone());
            Arc::new(next)
        });
        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            "error-parser plugin registered its parser factory"
        );
        Ok(id)
    }

    /// CR.3 — the `help` seam's drain.
    ///
    /// Drives `register-help-topics` once, then RCU-registers what the guest
    /// declared into the help registry, each topic stamped with this plugin's
    /// host-issued id. Teardown is by that provenance
    /// (`HelpTopicRegistry::unregister_plugin`), so — like `error-parser` —
    /// there is no list to record on `record` and therefore none to forget.
    ///
    /// The guest is dropped when `spawn_help_plugin` returns: the bodies are
    /// already across as owned `String`s, so nothing about the plugin needs to
    /// be alive for `:help` to render its pages.
    ///
    /// A plugin that declared no topics still loads. It is a strange plugin,
    /// not a broken one, and failing the load would be a worse answer than a
    /// debug line.
    async fn drain_help(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        let registry = self
            .env
            .help_topics
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("help"))?;

        let (id, specs) = self
            .host
            .spawn_help_plugin(component, manifest, tier, PluginBudget::default())
            .await?;

        let count = specs.len();
        if count > 0 {
            registry.rcu(|current| {
                let mut next = (**current).clone();
                for spec in &specs {
                    next.register(lattice_help::topics::HelpTopic {
                        name: spec.name.clone(),
                        summary: spec.summary.clone(),
                        // `Static` needs a `&'static str` and a plugin's body
                        // is runtime data, so the owned-`String` variant is
                        // the one that fits. `Dynamic` would be the wrong
                        // shape twice over: it re-invokes a closure on every
                        // open, and the guest it would have to call is gone.
                        body: lattice_help::topics::HelpTopicBody::Owned(spec.body.clone()),
                        related_command_patterns: spec.related_command_patterns.clone(),
                        plugin_id: Some(id.0 as u64),
                    });
                }
                Arc::new(next)
            });
        }
        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            topics = count,
            "help plugin registered its topics"
        );
        Ok(id)
    }

    /// LG.3c — the `language` seam's drain.
    ///
    /// Drives `register-languages` once, then compiles each declared grammar
    /// and registers it, stamped with this plugin's host-issued id. Teardown
    /// is by that provenance, so — like `error-parser` and `help` — there is
    /// no list to record on `record` and therefore none to forget.
    ///
    /// **The grammar is compiled HERE, after the guest's store is gone.**
    /// `spawn_language_plugin` returns plain bytes; turning them into a
    /// `tree_sitter::Language` costs ~100 ms of Cranelift, and doing it inside
    /// the guest call would hold a `wasmtime::Store` alive across it for no
    /// reason. This runs on the loader's off-boot-thread task, which is the
    /// only place that cost is acceptable — it is emphatically not on the
    /// keystroke or frame path.
    ///
    /// **One bad language costs only itself.** A grammar that fails to load or
    /// a query that fails to compile is logged with the offending language and
    /// reason named, and the plugin's other languages — and its other
    /// contributions — still register. A plugin that declared no languages
    /// still loads: strange, not broken.
    async fn drain_language(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        let (id, specs) = self
            .host
            .spawn_language_plugin(component, manifest, tier, PluginBudget::default())
            .await?;

        let mut registered = 0usize;
        for spec in &specs {
            // Loaded by the GRAMMAR's export name, registered under the
            // LANGUAGE's name — they differ whenever a grammar's upstream
            // name is not the filetype's (`sequel` vs `sql`).
            let grammar =
                match lattice_syntax::wasm_grammar::load(&spec.grammar_name, &spec.grammar) {
                    Ok(g) => g,
                    Err(err) => {
                        tracing::warn!(
                            plugin = %manifest.id,
                            language = %spec.name,
                            %err,
                            "plugin language rejected: grammar failed to load"
                        );
                        continue;
                    }
                };
            let grammar_spec = lattice_syntax::GrammarSpec {
                grammar,
                highlights: spec.highlights.clone(),
                folds: spec.folds.clone(),
                injections: spec.injections.clone(),
                indents: spec.indents.clone(),
                textobjects: spec.textobjects.clone(),
            };
            let exts: Vec<&str> = spec.extensions.iter().map(String::as_str).collect();
            match lattice_syntax::plugin_lang::register_with_grammar(
                &spec.name,
                &exts,
                &grammar_spec,
                id.0 as u64,
            ) {
                Ok(_) => registered += 1,
                Err(err) => tracing::warn!(
                    plugin = %manifest.id,
                    language = %spec.name,
                    %err,
                    "plugin language rejected"
                ),
            }
        }

        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            declared = specs.len(),
            registered,
            "language plugin registered its languages"
        );
        Ok(id)
    }

    /// CR.4 — the `dashboard` seam's drain.
    ///
    /// Instantiates one live guest per declared section and RCU-registers
    /// them, each stamped with this plugin's host-issued id. Teardown is by
    /// that provenance; CR.2's shadow stack means removing a plugin section
    /// that replaced a builtin resurfaces the builtin with no restore step.
    ///
    /// Synchronous, unlike every other declaration drain here: the world
    /// instantiates against the sync linker because `render-section` runs
    /// inside the compositor and must not suspend.
    fn drain_dashboard(
        &self,
        component: &lattice_plugin_host::Component,
        manifest: &PluginManifest,
        tier: TrustTier,
    ) -> Result<PluginId, PluginLoaderError> {
        let registry = self
            .env
            .dashboard_sections
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("dashboard"))?;

        // The Reflex-class budget, not the lifecycle default: `render-section`
        // runs on the actor thread during a Display-class action, and the fuel
        // cap is what bounds a pathological guest to a bounded stall.
        let (id, sections) = self.host.spawn_dashboard_sections(
            component,
            manifest,
            tier,
            PluginBudget::grammar(),
        )?;

        let count = sections.len();
        if count > 0 {
            let sections: Vec<Arc<dyn lattice_dashboard::DashboardSection>> = sections
                .into_iter()
                .map(|s| Arc::new(s) as Arc<dyn lattice_dashboard::DashboardSection>)
                .collect();
            registry.rcu(|current| {
                let mut next = (**current).clone();
                for section in &sections {
                    next.register(section.clone());
                }
                Arc::new(next)
            });
        }
        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            sections = count,
            "dashboard plugin registered its sections"
        );
        Ok(id)
    }
    /// so there is no `connect` spec round-trip.
    async fn drain_context(
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
            .ok_or(PluginLoaderError::NotWired("context"))?;
        let runtime = self
            .env
            .runtime
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("context"))?;
        let registry = self
            .env
            .context_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("context"))?;

        let (client, actor) = self
            .host
            .spawn_context_source(
                component,
                manifest,
                tier,
                PluginBudget::context(),
                bus,
                self.env.config_registry.as_ref(),
            )
            .await?;
        // PO.2: attach the boundary tracer so the actor emits a trace record per
        // guest call (a no-op when unwired).
        let actor = actor.with_tracer(self.env.tracer.clone());
        let task = runtime.spawn(actor.run());
        let source = WasmContextSource::new(client);
        let id = source.plugin_id();

        // Copy-on-write RCU into the wait-free registry (load -> clone ->
        // register -> store), like the decoration seam. Concurrent host
        // refreshes keep reading the prior snapshot until the store lands.
        let producer: Arc<dyn AsyncContextSource> = Arc::new(source);
        registry.rcu(|current| {
            let mut next = (**current).clone();
            next.register(producer.clone());
            Arc::new(next)
        });

        record.tasks.push(task);
        // Teardown token: the context registry unregisters this producer by id.
        record.teardown.context_sources.push(id.0 as u64);
        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            "context plugin registered its context-scope producer"
        );
        Ok(id)
    }

    /// IM.6b: drain the `media` seam — spawn the producer actor and register
    /// its source. The twin of [`Self::drain_decorations`]; a media provider
    /// carries no id/doc metadata either, so there is no `connect` round-trip.
    async fn drain_media(
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
            .ok_or(PluginLoaderError::NotWired("media"))?;
        let runtime = self
            .env
            .runtime
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("media"))?;
        let registry = self
            .env
            .media_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("media"))?;

        let (client, actor) = self
            .host
            .spawn_media_source(component, manifest, tier, PluginBudget::default(), bus)
            .await?;
        let actor = actor.with_tracer(self.env.tracer.clone());
        let task = runtime.spawn(actor.run());
        let source = lattice_plugin_host::WasmMediaSource::new(client);
        let id = source.plugin_id();

        // Copy-on-write RCU into the wait-free registry, like every other
        // producer seam: concurrent host refreshes keep reading the prior
        // snapshot until the store lands, so there is no lock on the read path.
        let producer: Arc<dyn lattice_mode::AsyncMediaSource> = Arc::new(source);
        registry.rcu(|current| {
            let mut next = (**current).clone();
            next.register(producer.clone());
            Arc::new(next)
        });

        record.tasks.push(task);
        // Teardown token: the media registry unregisters this producer by id.
        record.teardown.media_sources.push(id.0 as u64);
        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            "media plugin registered its inline-media producer"
        );
        Ok(id)
    }

    /// OM.A1: instantiate the plugin's agenda-source, resolve the extensions
    /// it claims ONCE, and register it as a producer.
    ///
    /// The `extensions()` round-trip happens here rather than per file for
    /// the reason `WasmAgendaSource::extensions` records: the answer cannot
    /// change, and a scan already pays one boundary crossing per file.
    ///
    /// A source claiming NOTHING is registered anyway and logged at `warn`.
    /// Refusing the load would fail a plugin over one empty list while its
    /// other seams were fine; registering silently would leave a producer
    /// that can never contribute — the `NotWired` shape. The log is the
    /// middle answer, and it names the plugin.
    async fn drain_agenda(
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
            .ok_or(PluginLoaderError::NotWired("agenda-source"))?;
        let runtime = self
            .env
            .runtime
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("agenda-source"))?;
        let registry = self
            .env
            .agenda_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("agenda-source"))?;

        let (client, actor) = self
            .host
            .spawn_agenda_source(component, manifest, tier, PluginBudget::default(), bus)
            .await?;
        let actor = actor.with_tracer(self.env.tracer.clone());
        let task = runtime.spawn(actor.run());

        // Ask before registering: the producer is immutable once in the
        // registry, and a `claims()` that consults a lock per file would put
        // contention on the walk for an answer that never changes.
        let declared = client.extensions().await.unwrap_or_else(|e| {
            tracing::warn!(
                plugin = %manifest.id,
                error = %e,
                "agenda plugin could not declare its file extensions; it will scan nothing"
            );
            Vec::new()
        });
        let extensions = lattice_plugin_host::normalise_extensions(declared);
        if extensions.is_empty() {
            tracing::warn!(
                plugin = %manifest.id,
                "agenda plugin claims no file extensions; it will never be offered a file"
            );
        }

        // OM.A3: the minor the provider activates on the agenda view, so the
        // source can act on its own rows. Resolved here for the same reason
        // `extensions` is — the provider reads it on every open, and it
        // cannot change. A `none` (or a failed call) is a source that only
        // produces rows, which is the ordinary case.
        let view_mode = match client.view_mode().await {
            Ok(m) => m.filter(|m| !m.trim().is_empty()),
            Err(e) => {
                tracing::debug!(
                    plugin = %manifest.id,
                    error = %e,
                    "agenda plugin declared no view mode"
                );
                None
            }
        };
        let source = lattice_plugin_host::WasmAgendaSource::new(client, extensions, view_mode);
        let id = source.plugin_id();

        // Copy-on-write RCU into the wait-free registry, like every other
        // producer seam: a scan already running keeps reading the prior
        // snapshot until the store lands, so there is no lock on the read
        // path.
        let producer: Arc<dyn lattice_mode::AsyncAgendaSource> = Arc::new(source);
        registry.rcu(|current| {
            let mut next = (**current).clone();
            next.register(producer.clone());
            Arc::new(next)
        });

        record.tasks.push(task);
        record.teardown.agenda_sources.push(id.0 as u64);
        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            "agenda plugin registered its agenda-row producer"
        );
        Ok(id)
    }

    /// `PluginLoaderError::Host` and skips only this plugin.
    async fn drain_decorations(
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
            .ok_or(PluginLoaderError::NotWired("decorations"))?;
        let runtime = self
            .env
            .runtime
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("decorations"))?;
        let registry = self
            .env
            .decoration_registry
            .as_ref()
            .ok_or(PluginLoaderError::NotWired("decorations"))?;

        let (client, actor) = self
            .host
            .spawn_decoration_source(component, manifest, tier, PluginBudget::default(), bus)
            .await?;
        // Drive the producer actor on the multi-thread runtime (off the keystroke
        // path). Unlike picker / completion there is no `connect` spec round-trip
        // — a decoration source carries no id/doc metadata; it is a pure producer.
        // PO.2: attach the boundary tracer so the actor emits a trace record per
        // guest call (a no-op when unwired).
        let actor = actor.with_tracer(self.env.tracer.clone());
        let task = runtime.spawn(actor.run());
        let source = WasmDecorationSource::new(client);
        let id = source.plugin_id();

        // Copy-on-write RCU into the wait-free registry (load → clone → register
        // → store), like the picker seam. Concurrent host refreshes keep reading
        // the prior snapshot until the store lands — no lock on the read path.
        let producer: Arc<dyn AsyncGutterDecorationSource> = Arc::new(source);
        registry.rcu(|current| {
            let mut next = (**current).clone();
            next.register(producer.clone());
            Arc::new(next)
        });

        record.tasks.push(task);
        // Teardown token: the decoration registry unregisters this producer by id.
        record.teardown.decoration_sources.push(id.0 as u64);
        tracing::debug!(
            plugin = %manifest.id,
            id = id.0,
            "decoration plugin registered its gutter-decoration producer"
        );
        Ok(id)
    }
}
