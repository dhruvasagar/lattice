//! Declarative contributions on [`crate::Mode`].
//!
//! `Keymap` and `KeymapBinding` moved to `lattice-keymap::contribution`
//! in K.3 (2026-06-07) — re-exported here for backward compatibility.
//!
//! Stubs still pending real impls:
//! - [`DecorationProvider`] -- M.4 / decoration registry.

use lattice_core::BufferId;

use crate::services::ServiceRegistry;

pub use lattice_keymap::{Keymap, KeymapBinding};

/// RAII subscription handle. Unsubscribes from the event bus on drop.
///
/// Acquire in `Mode::on_activate` via `ctx.events_handle()` +
/// `EventBus::subscribe_typed`; store in the mode's `Guard` struct so
/// deactivation cleanup is compiler-enforced. Modes with conditional
/// subscriptions (e.g. skip when no URI) use `Option<Subscription>`.
///
/// MO.4.c: replaces the `_private:()` stub; `Mode::subscriptions()`
/// removed — `on_activate` + Guard IS the subscription mechanism.
pub struct Subscription {
    bus: std::sync::Arc<lattice_runtime::EventBus>,
    id: lattice_runtime::SubscriptionId,
}

impl Subscription {
    pub fn new(
        bus: std::sync::Arc<lattice_runtime::EventBus>,
        id: lattice_runtime::SubscriptionId,
    ) -> Self {
        Self { bus, id }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.bus.unsubscribe(self.id);
    }
}

/// Stub. Reserved for the WIT plugin-facing contribution surface
/// (M.10). Not used by the `Mode` trait today — see
/// `Mode::gutter_decorations` for the live decoration path.
#[derive(Debug, Clone)]
pub struct DecorationProvider {
    _private: (),
}

/// Renderer-agnostic diff-sign kind for the gutter diff column.
/// Mirrors `DiffSignKind` without importing `lattice-host`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GutterDiffKind {
    Add,
    Remove,
    Change,
    Conflict,
}

/// Renderer-agnostic diagnostic severity level for the gutter
/// severity column. Ordered ascending by severity so `max()` selects
/// the most severe: `Hint < Info < Warning < Error`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GutterSeverityLevel {
    Hint,
    Info,
    Warning,
    Error,
}

/// A single gutter decoration contributed by a [`crate::Mode`].
/// Each variant maps to one physical gutter column.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GutterDecoration {
    /// Diff-sign column (between severity and line numbers).
    Diff { line: u32, kind: GutterDiffKind },
    /// LSP diagnostic severity column (leftmost gutter cell).
    Severity {
        line: u32,
        level: GutterSeverityLevel,
    },
}

/// CM.3c: render-time carrier for `compilation-mode`'s severity gutter
/// marks. The off-thread compilation drain builds a per-buffer severity
/// index and ships it to the host (`AppEffect::CompilationGutterSet`),
/// which stores it in `render_state`; the renderer reads that slot for the
/// pane's buffer and registers this into the [`DecorationCtx`]'s
/// `ServiceRegistry`. `CompilationMode::gutter_decorations` then pulls it
/// and maps each `(line, level)` to [`GutterDecoration::Severity`].
///
/// Deliberately lives here (in `lattice-mode`), NOT in `lattice-compilation`,
/// so neither renderer needs a `lattice-compilation` dependency to inject it
/// — the same dependency-inversion the `Mode::gutter_decorations` seam uses
/// for `LspDiagnosticsData` / `DiffDecorationData`. `entries` is shared
/// (`Arc`) so the render-path read is an O(1) pointer clone.
pub struct CompilationSeverityData {
    pub entries: std::sync::Arc<Vec<(u32, GutterSeverityLevel)>>,
}

/// Read-only context passed to [`crate::Mode::gutter_decorations`].
/// Same dep-inversion pattern as [`StatusLineCtx`]: the App populates
/// a `ServiceRegistry` with typed render-state snapshots; modes pull
/// their own data via [`Self::service`].
pub struct DecorationCtx<'a> {
    pub buffer_id: BufferId,
    services: &'a ServiceRegistry,
}

impl<'a> DecorationCtx<'a> {
    pub fn new(buffer_id: BufferId, services: &'a ServiceRegistry) -> Self {
        Self {
            buffer_id,
            services,
        }
    }

    pub fn service<T: std::any::Any + Send + Sync>(&self) -> Option<std::sync::Arc<T>> {
        self.services.get::<T>()
    }
}

// ML.3: `StatusLineItem` + `StatusLineCtx` retired with the
// `Mode::status_line_items` trait. Modes contribute modeline content as
// registered elements pushed over the event bus
// (`crate::ModelineElementUpdate`), not via a render-path service pull.

// ── SG.1: generic gutter signs ──────────────────────────────────────────────

/// SG.1 — a sign **definition**: what it looks like, how it is styled, how it
/// competes for its cell.
///
/// vim's `:sign define` / `:sign place` split, and the split is load-bearing
/// rather than historical. A definition is registered once and carries the
/// expensive, reusable parts — the glyph and its theme element. A *placement*
/// is `(line, name)` and happens per keystroke, per visible line, on every
/// refresh. Folding the two together would re-carry a glyph and a theme key
/// across the boundary for every marked line of every refresh, to say something
/// that was already true at load.
///
/// The host knows what a sign IS and nothing about what any particular sign
/// MEANS — which is what makes this a mechanism rather than a feature. A
/// provider's marks, a plugin's breakpoints and a future built-in all place
/// signs through the same registry and are styled through the same theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignDefinition {
    /// The name placements refer to. A provider's own namespace by convention
    /// (`org-agenda-mark`), unenforced — last definition wins, as with every
    /// other registry here.
    pub name: String,
    /// The glyph when `ui.nerd_fonts` is on. One or two cells.
    pub text: String,
    /// The BMP fallback, used when it is off — **the same cell width**, per the
    /// icon-degradation rule, so toggling the option cannot shift the gutter's
    /// geometry.
    pub fallback: String,
    /// The theme element the glyph is painted in (`gutter.sign.*` by
    /// convention). Resolved by the renderer through the ordinary theme
    /// registry, so a user or a theme retunes a plugin's signs without either
    /// knowing about the other.
    pub theme_element: String,
    /// Which sign wins when two land on one line. Higher wins; ties break on
    /// name so the answer is stable rather than incidental to hash order.
    ///
    /// One cell, one sign: a column that stacked them would either grow
    /// unpredictably or silently drop one, and vim's answer — priority — is the
    /// one users already know.
    pub priority: i32,
}

impl SignDefinition {
    /// The glyph for the current palette. Not a theme question — the theme
    /// decides the COLOUR, the font capability decides the GLYPH, and
    /// conflating them is how a themed editor renders tofu.
    pub fn glyph(&self, nerd_fonts: bool) -> &str {
        if nerd_fonts && !self.text.is_empty() {
            &self.text
        } else {
            &self.fallback
        }
    }
}

/// SG.1 — the registered sign definitions.
///
/// Read on the render path (one lookup per placed line) and written rarely (a
/// provider registering at load), which is the `ArcSwap` shape every other
/// contribution registry here uses.
#[derive(Debug, Default)]
pub struct SignRegistry {
    defs: std::collections::HashMap<String, std::sync::Arc<SignDefinition>>,
}

impl SignRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Define a sign. Replaces a definition of the same name — last write wins,
    /// so a reloaded plugin's new glyph takes effect rather than being refused.
    pub fn define(&mut self, def: SignDefinition) {
        self.defs.insert(def.name.clone(), std::sync::Arc::new(def));
    }

    /// Forget one, by name. What a plugin's teardown reverses.
    pub fn undefine(&mut self, name: &str) {
        self.defs.remove(name);
    }

    /// Forget every sign a namespace defined — `org.` removes `org.mark` and
    /// its peers. The unload path, since a plugin's definitions are not tracked
    /// individually anywhere else.
    pub fn undefine_prefix(&mut self, prefix: &str) {
        self.defs.retain(|name, _| !name.starts_with(prefix));
    }

    pub fn get(&self, name: &str) -> Option<&std::sync::Arc<SignDefinition>> {
        self.defs.get(name)
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Register **and** look up with this exact alias (the `ServiceRegistry` TypeId
/// rule).
pub type SignRegistryHandle = std::sync::Arc<arc_swap::ArcSwap<SignRegistry>>;

/// SG.1 — pick the winner when several signs land on one line.
///
/// Higher priority wins; equal priorities break on name. The tiebreak is not
/// arbitrary politeness — without it the painted glyph depends on iteration
/// order, so the same buffer renders differently between runs and a test that
/// passes today fails when a `HashMap` reseeds.
pub fn winning_sign<'a>(
    a: &'a std::sync::Arc<SignDefinition>,
    b: &'a std::sync::Arc<SignDefinition>,
) -> &'a std::sync::Arc<SignDefinition> {
    match a.priority.cmp(&b.priority) {
        std::cmp::Ordering::Greater => a,
        std::cmp::Ordering::Less => b,
        std::cmp::Ordering::Equal => {
            if a.name <= b.name {
                a
            } else {
                b
            }
        }
    }
}
