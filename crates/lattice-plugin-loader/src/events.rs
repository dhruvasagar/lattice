//! LA.1 — typed events the loader publishes about the **mode/language
//! catalog**, as distinct from the plugin lifecycle.
//!
//! `Event::PluginLoaded` already fires once per load, but it says "a plugin
//! finished loading", not "the set of languages and major modes the editor can
//! resolve against just changed". Those are different facts with different
//! subscribers: the first is what an `init.rs` `on-plugin-loaded` handler
//! waits for; the second is what makes an already-open buffer's major mode
//! stale (`mode-architecture.md` §7.4, "Major mode, second trigger").
//!
//! Keeping them apart is what keeps the re-resolution cheap. Re-running the
//! ordered major resolver is O(major-modes × open buffers); riding
//! `PluginLoaded` would pay that for every auto-pair-shaped plugin that cannot
//! possibly have changed the answer.

use lattice_plugin_host::PluginId;

/// Fired **once per plugin load** whose drain could have changed the
/// mode/language catalog — i.e. the manifest declared `language` or `modes`.
///
/// Published after the plugin's *entire* drain completes, never per registered
/// language: a subscriber re-resolving major modes must see a fully-installed
/// catalog, and a plugin that ships a language *and* the major mode that binds
/// it would otherwise be observed half-way through.
///
/// Declared-seam gated rather than registered-count gated. A plugin whose every
/// language was rejected publishes anyway, which costs one wasted re-resolution
/// that finds nothing; the inverse mistake — deriving the gate from what
/// actually registered — would need each drain to report a count upward, and a
/// drain that forgot to would fail silently in exactly the way this whole area
/// keeps failing.
#[derive(Debug, Clone)]
pub struct LanguagesRegistered {
    /// The host-issued id of the plugin whose load changed the catalog. The
    /// plugin's *user-facing* identity is its manifest name, carried by
    /// `Event::PluginLoaded`; this id is here so a subscriber can correlate the
    /// two, not so it can look anything up.
    pub plugin: PluginId,
}

lattice_protocol::register_event!(
    LanguagesRegistered,
    "plugin.languages-registered",
    "Fired once after a plugin that declares languages or major modes finishes loading, \
     signalling that the mode/language catalog changed.",
    "lattice-plugin-loader",
);

/// Fired when what `:plugins` would show has changed **without** a plugin
/// loading or unloading: a build started, finished or failed, or a load
/// failure was recorded or cleared.
///
/// `Event::PluginLoaded` / `PluginUnloaded` cover the lifecycle, and a view
/// that listens only to those is right until the first build that fails
/// outright. That produces no lifecycle event at all — nothing loaded, nothing
/// unloaded — so an open `*plugins*` buffer went on showing the state from
/// before the build, and the failure it existed to report appeared only if the
/// user happened to press refresh. The commonest case was the worst one: open
/// `:plugins` to see why the config is missing, while the cold first build is
/// still running, and watch a view that never changes.
///
/// Carries nothing. A subscriber re-reads [`PluginLoader::plugin_status`] and
/// [`PluginLoader::failed_loads`]; a payload describing the change would be a
/// second source of truth for a snapshot that is cheap to take.
///
/// [`PluginLoader::plugin_status`]: crate::PluginLoader::plugin_status
/// [`PluginLoader::failed_loads`]: crate::PluginLoader::failed_loads
#[derive(Debug, Clone, Copy, Default)]
pub struct PluginStatusChanged;

lattice_protocol::register_event!(
    PluginStatusChanged,
    "plugin.status-changed",
    "Fired when a plugin's build state or load-failure record changes without a \
     load or unload — a build started, finished or failed.",
    "lattice-plugin-loader",
);
