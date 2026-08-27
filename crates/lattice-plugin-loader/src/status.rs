//! PL8.H.1 — the loader's plugin **status data model**: the read-only snapshot
//! of every loaded plugin (identity · trust tier · capabilities granted/denied ·
//! health) that the `:plugins` manager view (PL8.H.2/.3) renders.
//!
//! The loader already owns the loaded-plugin set and the `unload` / `reload`
//! APIs; this adds the *observable* state the manager surfaces. All of it is
//! off the keystroke path — computed at load time or flipped by an
//! `Event::PluginCrashed` subscription drained on the runtime.
//!
//! Structured, not pre-formatted: `PluginStatus` carries typed [`Capability`]
//! lists and a [`PluginHealth`] enum so the *view* owns presentation (glyphs,
//! column widths, theming) — the view crate maps these to cells.

use lattice_plugin_host::{Capability, TrustTier};

use crate::source_record::SourceRecord;

/// A loaded plugin's health — the quarantine/reload surface the manager view
/// shows. A component trap taints its instance irrecoverably (wasmtime offers no
/// rollback), so the instance is dead-until-reload; [`Event::PluginCrashed`]
/// fires exactly once per instance on that first trap, flipping health here.
///
/// [`Event::PluginCrashed`]: lattice_protocol::Event::PluginCrashed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginHealth {
    /// Loaded and running — no trap observed.
    Healthy,
    /// A seam export trapped: the instance is dead until a `:plugin-reload`
    /// mints a fresh `Store`. `func` is the export that trapped (`"on-event"` /
    /// `"generate"` / …); `kind` is the stable label (`"fuel"` / `"epoch"` /
    /// `"trap"`) — the `Event::PluginCrashed` provenance, verbatim.
    Quarantined { func: String, kind: String },
}

impl PluginHealth {
    /// True once the plugin has crashed (a manager-view filter / glyph gate).
    pub fn is_quarantined(&self) -> bool {
        matches!(self, PluginHealth::Quarantined { .. })
    }
}

#[cfg(test)]
mod build_state_tests {
    use super::BuildState;

    #[test]
    fn every_state_has_a_distinct_label() {
        // Two states rendering the same cell would make the column useless
        // exactly when it matters — `stale` vs `build-failed` are different
        // problems with different fixes.
        let labels = [
            BuildState::NotBuilt.label(),
            BuildState::Cached.label(),
            BuildState::Stale.label(),
            BuildState::Building.label(),
            BuildState::Failed.label(),
        ];
        let mut seen = std::collections::HashSet::new();
        for l in labels {
            assert!(seen.insert(l), "duplicate build label: {l}");
        }
    }
}

/// WT.4: a plugin the loader tried to load and could not.
///
/// **A plugin that failed to load is indistinguishable from one that was never
/// installed**, and that is not a cosmetic gap — it is what turned the reported
/// failure into a debugging session rather than a glance. The editor opened, the
/// file opened, and org was simply absent: no language, no highlighting, no
/// folds, no chords, and nothing anywhere saying why.
///
/// Held separately from [`PluginStatus`] rather than as another `PluginHealth`
/// variant, because a failed load has none of what that type carries: no
/// host-issued id, no granted capabilities, no trust tier that was ever applied.
/// Modelling it as a degenerate `PluginStatus` would mean inventing all three,
/// and `:plugins`' row→plugin index mapping would then point at rows with no
/// plugin behind them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedLoad {
    /// The manifest id where one could be read, else the directory's file name.
    /// A plugin whose manifest is itself the problem still needs a name in the
    /// report, or the user cannot tell which of their plugins is being described.
    pub name: String,
    /// Where it was loaded from — the actionable half. The name says *what*
    /// broke; this says *which copy on disk* to go and look at.
    pub dir: std::path::PathBuf,
    /// The rendered [`crate::PluginLoaderError`]. A string rather than the error
    /// itself: this is a snapshot for a view, outliving the load that produced
    /// it, and the view has no use for the variant.
    pub error: String,
}

/// A read-only snapshot of one loaded plugin for the manager view. Cloned out of
/// the loader's loaded-set under its lock (never a live borrow), so the view
/// renders a stable frame while loads/unloads proceed.
#[derive(Debug, Clone)]
pub struct PluginStatus {
    /// The host-issued numeric plugin id (the `u32` inside `SourceLayer::Plugin`
    /// and `Event::PluginCrashed.plugin`).
    pub id: u32,
    /// The manifest id — the user-facing name and the `:plugin-unload <name>`
    /// key.
    pub name: String,
    /// The trust tier the plugin loaded under (`Bundled` / `UserInstalled`) —
    /// which gates `proc:spawn`.
    pub tier: TrustTier,
    /// Capabilities the plugin requested **and** received under its tier.
    pub granted: Vec<Capability>,
    /// Requested-but-withheld capabilities (tier-gated, e.g. `proc:spawn` for a
    /// user-installed plugin). Never fatal — the plugin loaded degraded.
    pub denied: Vec<Capability>,
    /// Whether the plugin is running or quarantined after a crash.
    pub health: PluginHealth,
    /// PM.8a: where the plugin came from, read from its on-disk `.source`
    /// marker. Persisted rather than remembered, so it is still right on the
    /// boot *after* the one that installed it.
    pub source: SourceRecord,
    /// PM.8a: whether the cached artifact matches its source.
    pub build: BuildState,
}

/// PM.8a: how current a plugin's built artifact is.
///
/// Answered from the `.build-stamp` PM.5 writes, so it survives a restart
/// like the source does. Only meaningful for a buildable source — there is no
/// such thing as a stale prebuilt or a stale bundled plugin, which is why
/// [`BuildState::NotBuilt`] exists rather than reporting those as `Cached`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildState {
    /// Nothing the editor builds: bundled, prebuilt, or an unknown source.
    NotBuilt,
    /// The artifact was built from the source as it stands.
    Cached,
    /// The source has changed since the artifact was built. The plugin is
    /// running old code until the next build.
    Stale,
    /// A build is running right now.
    ///
    /// The one state that is NOT derived from disk. Everything else here is a
    /// fact about files that outlives the process; this is a fact about the
    /// process, so it lives in memory and disappears with it — which is
    /// correct, because a build interrupted by a crash is not still running
    /// after a restart.
    Building,
    /// The last build attempted this session failed. The plugin is running
    /// whatever it was running before (or nothing, if it never built).
    ///
    /// Also in-memory: on the next boot the artifact either exists — and the
    /// stamp says whether it is stale — or it does not. Persisting a failure
    /// would mean showing a user an error about a build they may since have
    /// fixed.
    Failed,
}

impl BuildState {
    /// The view's BUILD cell.
    pub fn label(&self) -> &'static str {
        match self {
            BuildState::NotBuilt => "—",
            BuildState::Cached => "cached",
            BuildState::Stale => "stale",
            BuildState::Building => "building…",
            BuildState::Failed => "build-failed",
        }
    }
}
