//! Dashboard configuration, owned by the dashboard subsystem
//! ([[feedback_mode_owns_its_surface]]), not host core. Self-registers via
//! `linkme` like every other `options!` block; the host's
//! `init_from_linkme()` walks the global slice at boot, so linking
//! `lattice-dashboard` into the binary (it always is — `install` runs in the
//! Phase-B list, DB.2) picks these up automatically.
//!
//! Three options (see docs/dev/architecture/dashboard.md §8):
//!
//! - `dashboard.enabled` — auto-open `*dashboard*` on launch with no file arg.
//! - `dashboard.sections` — ordered section ids to show. The config system
//!   has no native list type, so the v1 encoding is a comma/whitespace-
//!   separated string parsed by [`SectionSelection::parse`](crate::SectionSelection::parse).
//!   Empty ⇒ all built-ins in default order; a list ⇒ exactly those ids in
//!   that order.
//! - `dashboard.source` — path to a user file that fully replaces section
//!   composition. Empty ⇒ unset. A missing/unreadable path falls back to the
//!   sections with a warning (DB.6), never a panic.

lattice_config::groups! {
    /// The dashboard launch page.
    pub Dashboard = "dashboard";
}

lattice_config::options! {
    group = Dashboard;

    /// Open the `*dashboard*` launch page when the editor starts with no
    /// file argument. `:dashboard` opens it on demand regardless.
    #[name("dashboard.enabled")]
    pub DashboardEnabled: bool = true;

    /// Ordered section ids to show, comma/whitespace-separated. Empty (the
    /// default) shows every built-in section in its default order. Listing
    /// ids both selects and reorders — an id present shows in this position,
    /// an id omitted is hidden. Unknown ids are skipped.
    #[name("dashboard.sections")]
    pub DashboardSections: String = String::new();

    /// Path to a file whose contents fully replace the composed sections —
    /// the "author the entire dashboard" escape hatch. Empty (the default)
    /// uses section composition. A missing or unreadable path falls back to
    /// the sections with a logged warning.
    #[name("dashboard.source")]
    pub DashboardSource: String = String::new();
}
