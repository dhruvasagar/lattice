//! The section trait and the read-only facts passed to it.
//!
//! A section is a pure function of [`DashboardCtx`] to a
//! [`DashboardFragment`](crate::DashboardFragment): it performs no I/O and
//! holds no editor state. Built-in sections are native Rust (the built-in
//! surface stays native, like the vim grammar); plugin sections (DB.8) will
//! satisfy the same trait through a WASM shim.

use crate::fragment::DashboardFragment;

/// Read-only facts a section needs to render.
///
/// DB.1 carries the primitives the pure sections need. Later slices grow it
/// with the resolved-theme snapshot (DB.3/DB.4) and help/keymap lookups so
/// the "help & bindings" section stays truthful — added when the wiring that
/// can supply them lands, not before.
#[derive(Debug, Clone)]
pub struct DashboardCtx {
    /// Pane width in cells, used by the compositor for centring (DB.4).
    pub pane_width: usize,
    /// Whether Nerd Font glyphs may be used (else the BMP-block fallback).
    pub nerd_fonts: bool,
    /// The editor version string, for the branding section.
    pub version: String,
}

impl Default for DashboardCtx {
    fn default() -> Self {
        Self {
            pane_width: 80,
            nerd_fonts: false,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// A contributor of one dashboard section.
///
/// The registry sorts by [`order`](DashboardSection::order) (ties broken by
/// [`id`](DashboardSection::id)) for the default layout, and looks up by
/// `id` when the user pins an explicit order via `dashboard.sections`.
pub trait DashboardSection: Send + Sync {
    /// Stable identifier, used in `dashboard.sections` and (CR.4) for plugin
    /// replace-by-id. Lowercase kebab, e.g. `"getting-started"`.
    ///
    /// Section ids are deliberately NOT namespaced by plugin, unlike help
    /// topics: replacing a builtin section is a stated capability, so a
    /// plugin registering `getting-started` is doing something supported.
    fn id(&self) -> &str;

    /// CR.2: the host-issued plugin id that contributed this section,
    /// `None` for builtins. Provenance IS the teardown token —
    /// [`DashboardRegistry::unregister_plugin`] removes by it.
    ///
    /// Defaulted, so no native section changes.
    fn plugin_id(&self) -> Option<u64> {
        None
    }

    /// Default sort key for the built-in layout. Lower sorts first.
    fn order(&self) -> i32;

    /// Whether this section shows when the user has not customised
    /// `dashboard.sections`.
    fn default_enabled(&self) -> bool {
        true
    }

    /// Render this section's contribution.
    fn render(&self, ctx: &DashboardCtx) -> DashboardFragment;
}
