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
}
