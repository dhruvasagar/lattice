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
    /// SG.1: a generic sign placement — vim's `:sign place`.
    ///
    /// Carries only the line and the definition's NAME; the glyph, its theme
    /// element and its priority live in the [`SignRegistry`]. A placement is
    /// produced per visible line on every refresh, so it stays the cheap half
    /// of the pair on purpose.
    ///
    /// An unknown id resolves to nothing and paints nothing — a provider
    /// placing a sign it never defined is a provider bug, and refusing to paint
    /// is the answer that keeps the gutter honest rather than inventing a
    /// glyph for it.
    Sign { line: u32, sign: SignId },
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
    /// The glyph when `ui.nerd_fonts` is on. **One cell** — SG.2b put signs
    /// in the gutter's single shared mark cell, so anything wider would push
    /// every line of content right. [`Self::glyph_char`] is what the
    /// renderers paint and it takes the first character.
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
    /// Which sign wins when two land on one line **of the same column**.
    /// Higher wins; ties break on name so the answer is stable rather than
    /// incidental to hash order.
    ///
    /// One cell, one sign: a cell that stacked them would either grow
    /// unpredictably or silently drop one, and vim's answer — priority — is the
    /// one users already know.
    pub priority: i32,
    /// SG.4a — which gutter column this sign paints in.
    ///
    /// Columns exist because contention is only meaningful between marks that
    /// mean comparable things. A diagnostic and a git-diff mark are both
    /// "something is true of this line", but they answer different questions,
    /// and a single contended cell would drop the git gutter on exactly the
    /// lines a diagnostic touches — the lines a user is most likely to be
    /// looking at. Vim's single `signcolumn` accepts that trade; Helix and Zed
    /// do not, and neither does this.
    ///
    /// A name the host does not paint falls back to the FIRST column rather
    /// than vanishing, on the same principle as the `gutter.sign` theme
    /// fallback: a sign was placed to say something, and the failure mode that
    /// loses the information entirely is the worst one available.
    ///
    /// Use [`SIGN_COLUMN_MARK`] / [`SIGN_COLUMN_DIFF`] for the built-ins.
    pub column: String,
}

/// SG.4a — the leftmost gutter column: diagnostics, compilation severity, and
/// any sign that does not name a column of its own. Vim's `signcolumn`.
pub const SIGN_COLUMN_MARK: &str = "mark";

/// SG.4a — the git-diff column, between the mark column and the line numbers.
/// Separate from [`SIGN_COLUMN_MARK`] so a diagnostic cannot hide a hunk mark.
pub const SIGN_COLUMN_DIFF: &str = "diff";

/// SG.4a — the built-in gutter columns, left to right.
///
/// The host owns the ORDER (a gutter whose columns moved per buffer would be
/// unreadable) but nothing about what goes in each one — that is entirely the
/// registry's answer. Making this list user-configurable is the obvious next
/// step and is deliberately not taken here: it changes the gutter's WIDTH,
/// which every scroll, wrap and cursor-column calculation reads.
pub const BUILTIN_SIGN_COLUMNS: [&str; 2] = [SIGN_COLUMN_MARK, SIGN_COLUMN_DIFF];

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

    /// SG.2b — the single character the renderers paint into the gutter's
    /// mark cell.
    ///
    /// The cell is one column, so this TRUNCATES rather than trusting a
    /// producer to have obeyed the one-cell rule. A definition that ignores
    /// it loses its tail; the alternative is a gutter that silently widens
    /// and pushes every line of content sideways, which is a pixel change to
    /// content the user did not edit and costs far more than the glyph.
    /// An empty definition paints a blank, so a producer can place a sign
    /// that reserves the cell without drawing in it.
    pub fn glyph_char(&self, nerd_fonts: bool) -> char {
        self.glyph(nerd_fonts).chars().next().unwrap_or(' ')
    }
}

/// SG.1 — a definition's interned handle.
///
/// **Placements carry this, not a name**, and the reason is the render path:
/// `GutterDecoration` is `Copy` and one placement exists per visible marked
/// line per refresh, so a `String` there would be both a clone per line and the
/// end of `Copy` for every consumer. The name→id resolution happens ONCE, where
/// a placement is produced — at the WASM boundary, off the render path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignId(pub u32);

/// SG.1 — the registered sign definitions.
///
/// Read on the render path (one lookup per placed line) and written rarely (a
/// provider registering at load), which is the `ArcSwap` shape every other
/// contribution registry here uses.
/// `Clone` because the `ArcSwap` write path is copy-on-write: a producer
/// defining a sign clones the current snapshot, mutates, and stores. The
/// clone is `Vec<Option<Arc<_>>>` + the name index — Arc bumps, not glyph
/// copies — and it happens at registration, never on the render path.
#[derive(Debug, Default, Clone)]
pub struct SignRegistry {
    /// Indexed by [`SignId`]. Never shrinks: an id handed out must keep
    /// resolving, or an in-flight placement from a producer that ran before an
    /// `undefine` would paint some LATER sign's glyph. A removed definition
    /// leaves a `None` hole instead.
    defs: Vec<Option<std::sync::Arc<SignDefinition>>>,
    by_name: std::collections::HashMap<String, SignId>,
}

impl SignRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Define a sign, or redefine one of the same name.
    ///
    /// A redefinition KEEPS the id, so a plugin reloading with a new glyph does
    /// not orphan placements already in flight — they simply start painting the
    /// new glyph, which is what "redefine" should mean.
    pub fn define(&mut self, def: SignDefinition) -> SignId {
        let def = std::sync::Arc::new(def);
        if let Some(&id) = self.by_name.get(&def.name) {
            self.defs[id.0 as usize] = Some(def);
            return id;
        }
        let id = SignId(self.defs.len() as u32);
        self.by_name.insert(def.name.clone(), id);
        self.defs.push(Some(def));
        id
    }

    /// The id a name resolves to, for a producer turning its own vocabulary
    /// into placements.
    pub fn id_of(&self, name: &str) -> Option<SignId> {
        self.by_name.get(name).copied()
    }

    /// Forget one, by name. What a plugin's teardown reverses. The id is
    /// retired rather than reused, so an in-flight placement from a producer
    /// that ran before the removal paints nothing instead of painting some
    /// LATER sign's glyph.
    pub fn undefine(&mut self, name: &str) {
        if let Some(id) = self.by_name.remove(name) {
            self.defs[id.0 as usize] = None;
        }
    }

    /// Forget every sign a namespace defined — `org.` removes `org.mark` and
    /// its peers. The unload path, since a plugin's definitions are not tracked
    /// individually anywhere else.
    pub fn undefine_prefix(&mut self, prefix: &str) {
        let names: Vec<String> = self
            .by_name
            .keys()
            .filter(|n| n.starts_with(prefix))
            .cloned()
            .collect();
        for name in names {
            self.undefine(&name);
        }
    }

    /// The definition behind a placement, or `None` for a retired id — which
    /// paints nothing.
    pub fn get(&self, id: SignId) -> Option<&std::sync::Arc<SignDefinition>> {
        self.defs.get(id.0 as usize)?.as_ref()
    }

    /// Every live definition with its id, for a consumer that has to
    /// pre-resolve something per definition — the publish path turning each
    /// `theme_element` into an `ElementId` (SG.2b). Skips retired ids, so a
    /// caller never has to re-check `get`. Order is id order, which is
    /// definition order; nothing here depends on it, but it is stable rather
    /// than hash-dependent, which is the property that keeps such a caller's
    /// tests from reseeding.
    pub fn iter(&self) -> impl Iterator<Item = (SignId, &std::sync::Arc<SignDefinition>)> {
        self.defs
            .iter()
            .enumerate()
            .filter_map(|(i, d)| d.as_ref().map(|d| (SignId(i as u32), d)))
    }

    /// How many definitions are live. Retired ids do not count.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// Register **and** look up with this exact alias (the `ServiceRegistry` TypeId
/// rule).
pub type SignRegistryHandle = std::sync::Arc<arc_swap::ArcSwap<SignRegistry>>;

/// SG.2b — the priority at which the built-in diagnostic severity mark holds
/// the gutter's mark cell.
///
/// Signs and diagnostics share ONE cell rather than each getting a column,
/// because a column costs every buffer a column of content forever whether or
/// not anything is ever placed in it, and because a diagnostic *is* a mark —
/// giving plugin signs a segregated column beside it would make them
/// second-class occupants of a gutter they should share. Contention is what
/// `priority` is for, and vim resolves exactly this contention the same way.
///
/// `10` is vim's default sign priority, so a producer that ships the vim
/// default lands level with diagnostics, which is the intuition a user
/// carries in.
pub const SEVERITY_SIGN_PRIORITY: i32 = 10;

// ── SG.4a: the built-in signs ───────────────────────────────────────────────
//
// Diagnostics and diff marks are signs like any other. Nothing about them is
// privileged in the host any more: they are definitions in the same registry a
// plugin writes, painted through the same theme elements, contended by the
// same priority rule. What used to be two hardcoded gutter paths is now two
// producers naming what they mean.
//
// Their priorities span the diagnostic severity order, because "most severe
// wins" was the semantics the old `Severity` arm's `max()` gave and it has to
// survive the unification. `10` is vim's default sign priority and the floor:
// a plugin sign shipping the vim default ties with a HINT (broken by name) and
// loses to everything above it. Outranking an ERROR now means exceeding
// `DIAGNOSTIC_ERROR_PRIORITY`, which is a real change from SG.2b — where any
// priority above 10 did it — and the stricter reading is the right one. A
// sign that displaces a compiler error had better mean it.

/// The lowest diagnostic, level with vim's default sign priority.
pub const DIAGNOSTIC_HINT_PRIORITY: i32 = 10;
pub const DIAGNOSTIC_INFO_PRIORITY: i32 = 20;
pub const DIAGNOSTIC_WARNING_PRIORITY: i32 = 30;
/// The highest built-in. A sign must EXCEED this to take the cell from an
/// error.
pub const DIAGNOSTIC_ERROR_PRIORITY: i32 = 40;

/// Diff marks never contend with each other — a line belongs to at most one
/// hunk — so they share one priority, and it is the vim default because
/// nothing about a hunk mark argues for out-ranking a plugin's sign in a
/// column plugins do not normally use.
pub const DIFF_SIGN_PRIORITY: i32 = 10;

/// SG.4a — the interned ids of the built-in signs.
///
/// The `BuiltinElementIds` shape, for the same reason: a producer emitting a
/// mark per visible line must not hash a string per line to say which mark it
/// is. Interned once at registration, published, and read as a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinSignIds {
    pub diagnostic_error: SignId,
    pub diagnostic_warning: SignId,
    pub diagnostic_info: SignId,
    pub diagnostic_hint: SignId,
    pub diff_add: SignId,
    pub diff_change: SignId,
    pub diff_remove: SignId,
    pub diff_conflict: SignId,
}

impl Default for BuiltinSignIds {
    /// Every id `SignId(u32::MAX)`, which resolves to nothing.
    ///
    /// A test fixture or a stripped host that never registered the built-ins
    /// then paints NO marks, rather than painting whatever happens to sit at
    /// id 0 — which would be some plugin's sign, silently, and only in the
    /// configurations nobody looks at.
    fn default() -> Self {
        let none = SignId(u32::MAX);
        Self {
            diagnostic_error: none,
            diagnostic_warning: none,
            diagnostic_info: none,
            diagnostic_hint: none,
            diff_add: none,
            diff_change: none,
            diff_remove: none,
            diff_conflict: none,
        }
    }
}

impl BuiltinSignIds {
    /// The id for a severity level — what an LSP or compilation producer calls
    /// instead of building a `Severity` decoration.
    pub fn for_severity(&self, level: GutterSeverityLevel) -> SignId {
        match level {
            GutterSeverityLevel::Error => self.diagnostic_error,
            GutterSeverityLevel::Warning => self.diagnostic_warning,
            GutterSeverityLevel::Info => self.diagnostic_info,
            GutterSeverityLevel::Hint => self.diagnostic_hint,
        }
    }

    /// The id for a diff kind — what the diff mode calls instead of building a
    /// `Diff` decoration.
    pub fn for_diff(&self, kind: GutterDiffKind) -> SignId {
        match kind {
            GutterDiffKind::Add => self.diff_add,
            GutterDiffKind::Change => self.diff_change,
            GutterDiffKind::Remove => self.diff_remove,
            GutterDiffKind::Conflict => self.diff_conflict,
        }
    }
}

/// SG.4a — register the built-in signs into `registry` and return their ids.
///
/// `glyphs` supplies the four diagnostic glyphs, which are live typed options
/// (`ui.diagnostic-*-glyph`) rather than constants — so this is called again
/// when one changes. Redefinition KEEPS the id (SG.1), which is what makes
/// re-registering safe with placements already in flight: they simply start
/// painting the new glyph. That property was built before anything needed it;
/// this is the thing that needed it.
///
/// Diff glyphs stay `+ ~ - ?` — cross-editor convention, and no option has
/// ever exposed them.
pub fn register_builtin_signs(
    registry: &mut SignRegistry,
    glyphs: DiagnosticGlyphs,
) -> BuiltinSignIds {
    let mut sev = |name: &str, glyph: char, element: &str, priority: i32| {
        let g = glyph.to_string();
        registry.define(SignDefinition {
            name: name.to_string(),
            // The SAME glyph for both palettes: these are the user's own
            // configured characters, and silently substituting a different one
            // when `ui.nerd_fonts` flips would be a surprise no option asked
            // for. A user wanting a Nerd Font diagnostic glyph sets the option
            // to one.
            text: g.clone(),
            fallback: g,
            theme_element: element.to_string(),
            priority,
            column: SIGN_COLUMN_MARK.to_string(),
        })
    };
    let diagnostic_error = sev(
        "diagnostic.error",
        glyphs.error,
        "diagnostic.error",
        DIAGNOSTIC_ERROR_PRIORITY,
    );
    let diagnostic_warning = sev(
        "diagnostic.warning",
        glyphs.warning,
        "diagnostic.warning",
        DIAGNOSTIC_WARNING_PRIORITY,
    );
    let diagnostic_info = sev(
        "diagnostic.info",
        glyphs.info,
        "diagnostic.info",
        DIAGNOSTIC_INFO_PRIORITY,
    );
    let diagnostic_hint = sev(
        "diagnostic.hint",
        glyphs.hint,
        "diagnostic.hint",
        DIAGNOSTIC_HINT_PRIORITY,
    );
    let mut diff = |name: &str, glyph: char, element: &str| {
        let g = glyph.to_string();
        registry.define(SignDefinition {
            name: name.to_string(),
            text: g.clone(),
            fallback: g,
            theme_element: element.to_string(),
            priority: DIFF_SIGN_PRIORITY,
            column: SIGN_COLUMN_DIFF.to_string(),
        })
    };
    let diff_add = diff("diff.add", '+', "diff.add.sign");
    let diff_change = diff("diff.change", '~', "diff.change.sign");
    let diff_remove = diff("diff.remove", '-', "diff.remove.sign");
    let diff_conflict = diff("diff.conflict", '?', "diff.conflict.sign");
    BuiltinSignIds {
        diagnostic_error,
        diagnostic_warning,
        diagnostic_info,
        diagnostic_hint,
        diff_add,
        diff_change,
        diff_remove,
        diff_conflict,
    }
}

/// The four `ui.diagnostic-*-glyph` values, read once by the caller so
/// `lattice-mode` needs no typed-options dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticGlyphs {
    pub error: char,
    pub warning: char,
    pub info: char,
    pub hint: char,
}

impl Default for DiagnosticGlyphs {
    /// The same defaults the renderers carried before the unification, so a
    /// host that reads no options still paints what it used to.
    fn default() -> Self {
        Self {
            error: '■',
            warning: '▲',
            info: '●',
            hint: '·',
        }
    }
}

/// SG.2b — may this sign take the mark cell from a diagnostic on the same
/// line?
///
/// **Strictly greater**, so a tie goes to the diagnostic. A diagnostic is a
/// state of the user's code that they need to see and did not ask for; a sign
/// is something a producer chose to show. When neither has a claim the
/// other lacks, hiding the error is the more expensive mistake. A producer
/// that genuinely outranks an error — a debugger stopped on this very line —
/// says so by exceeding [`SEVERITY_SIGN_PRIORITY`].
pub fn sign_beats_severity(def: &SignDefinition) -> bool {
    def.priority > SEVERITY_SIGN_PRIORITY
}

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

#[cfg(test)]
mod sign_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn def(name: &str, priority: i32) -> SignDefinition {
        SignDefinition {
            name: name.to_string(),
            text: "\u{f111}".to_string(),
            fallback: "\u{25cf}".to_string(),
            theme_element: format!("gutter.sign.{name}"),
            priority,
            column: SIGN_COLUMN_MARK.to_string(),
        }
    }

    #[test]
    fn a_definition_resolves_by_its_id() {
        let mut r = SignRegistry::new();
        let id = r.define(def("mark", 10));
        assert_eq!(r.id_of("mark"), Some(id));
        assert_eq!(r.get(id).unwrap().name, "mark");
        assert_eq!(r.len(), 1);
    }

    /// A redefinition KEEPS the id, so placements already in flight start
    /// painting the new glyph rather than being orphaned.
    #[test]
    fn redefining_keeps_the_id() {
        let mut r = SignRegistry::new();
        let first = r.define(def("mark", 10));
        let second = r.define(def("mark", 99));
        assert_eq!(first, second);
        assert_eq!(r.get(first).unwrap().priority, 99);
        assert_eq!(r.len(), 1, "and does not accumulate a second definition");
    }

    /// An id is RETIRED, never reused. A placement produced before the removal
    /// must paint nothing — reusing the slot would make it paint some later
    /// sign's glyph, which is a wrong answer where nothing is the right one.
    #[test]
    fn a_removed_id_is_retired_rather_than_reused() {
        let mut r = SignRegistry::new();
        let old = r.define(def("mark", 10));
        r.undefine("mark");
        assert!(r.get(old).is_none(), "the stale placement paints nothing");
        let new = r.define(def("other", 10));
        assert_ne!(old, new, "the slot is not handed to a different sign");
        assert!(r.get(old).is_none());
    }

    #[test]
    fn a_namespace_can_be_removed_at_once() {
        let mut r = SignRegistry::new();
        let a = r.define(def("org.mark", 1));
        let b = r.define(def("org.flag", 1));
        let keep = r.define(def("dap.breakpoint", 1));
        r.undefine_prefix("org.");
        assert!(r.get(a).is_none() && r.get(b).is_none());
        assert!(
            r.get(keep).is_some(),
            "another plugin's signs are untouched"
        );
        assert_eq!(r.len(), 1);
    }

    /// Higher priority wins. One cell, one sign — a column that stacked them
    /// would grow unpredictably or drop one silently.
    #[test]
    fn priority_decides_which_sign_paints() {
        let mut r = SignRegistry::new();
        let lo = r.define(def("low", 1));
        let hi = r.define(def("high", 100));
        let (lo, hi) = (r.get(lo).unwrap(), r.get(hi).unwrap());
        assert_eq!(winning_sign(lo, hi).name, "high");
        assert_eq!(winning_sign(hi, lo).name, "high", "and it is symmetric");
    }

    /// Ties break on NAME, not on iteration order. Without it the painted glyph
    /// depends on hash seeding — the same buffer renders differently between
    /// runs, and a test that passes today fails when the map reseeds.
    #[test]
    fn equal_priorities_break_on_name_not_on_luck() {
        let mut r = SignRegistry::new();
        let a = r.define(def("aaa", 5));
        let z = r.define(def("zzz", 5));
        let (a, z) = (r.get(a).unwrap(), r.get(z).unwrap());
        assert_eq!(winning_sign(a, z).name, "aaa");
        assert_eq!(winning_sign(z, a).name, "aaa");
    }

    /// The theme decides the COLOUR; the font capability decides the GLYPH.
    /// Conflating them is how a themed editor renders tofu.
    #[test]
    fn the_palette_decides_the_glyph_not_the_theme() {
        let d = def("mark", 1);
        assert_eq!(d.glyph(true), "\u{f111}");
        assert_eq!(d.glyph(false), "\u{25cf}");
        assert_eq!(
            d.glyph(true).chars().count(),
            d.glyph(false).chars().count(),
            "both palettes occupy the same cell width, so toggling \
             `ui.nerd_fonts` cannot shift the gutter"
        );
    }

    /// SG.2b: the mark cell is one column, so a definition that ignores the
    /// one-cell rule loses its tail rather than widening the gutter and
    /// pushing every line of content sideways.
    #[test]
    fn a_wide_glyph_is_truncated_rather_than_widening_the_gutter() {
        let mut d = def("wide", 1);
        d.text = "ab".into();
        d.fallback = "cd".into();
        assert_eq!(d.glyph_char(true), 'a');
        assert_eq!(d.glyph_char(false), 'c');
    }

    /// An empty definition reserves the cell without drawing in it.
    #[test]
    fn an_empty_glyph_paints_a_blank() {
        let mut d = def("blank", 1);
        d.text = String::new();
        d.fallback = String::new();
        assert_eq!(d.glyph_char(true), ' ');
        assert_eq!(d.glyph_char(false), ' ');
    }

    /// SG.2b: signs and diagnostics share one cell, and the tie goes to the
    /// diagnostic. An error is a state of the user's code they did not ask
    /// for; when neither has a claim the other lacks, hiding the error is
    /// the more expensive mistake.
    #[test]
    fn a_tie_with_a_diagnostic_leaves_the_error_visible() {
        assert!(!sign_beats_severity(&def("tie", SEVERITY_SIGN_PRIORITY)));
        assert!(!sign_beats_severity(&def(
            "below",
            SEVERITY_SIGN_PRIORITY - 1
        )));
        assert!(sign_beats_severity(&def(
            "above",
            SEVERITY_SIGN_PRIORITY + 1
        )));
    }

    /// SG.4a: the built-ins are signs like any other — same registry, same
    /// theme resolution, same priority rule. Nothing about them is privileged.
    #[test]
    fn the_builtins_register_as_ordinary_signs() {
        let mut r = SignRegistry::new();
        let ids = register_builtin_signs(&mut r, DiagnosticGlyphs::default());
        assert_eq!(r.len(), 8, "four diagnostics + four diff kinds");
        assert_eq!(r.id_of("diagnostic.error"), Some(ids.diagnostic_error));
        assert_eq!(r.id_of("diff.add"), Some(ids.diff_add));
        // Each lands in the column its meaning belongs to. A diagnostic and a
        // hunk mark answer different questions, so they must not contend.
        assert_eq!(
            r.get(ids.diagnostic_error).unwrap().column,
            SIGN_COLUMN_MARK
        );
        assert_eq!(r.get(ids.diff_add).unwrap().column, SIGN_COLUMN_DIFF);
    }

    /// "Most severe wins" was the semantics the retired `Severity` arm's
    /// `max()` gave, and it has to survive the unification — now expressed as
    /// priority, like every other sign's.
    #[test]
    fn diagnostic_priorities_preserve_the_severity_order() {
        let mut r = SignRegistry::new();
        let ids = register_builtin_signs(&mut r, DiagnosticGlyphs::default());
        let p = |id: SignId| r.get(id).unwrap().priority;
        assert!(p(ids.diagnostic_error) > p(ids.diagnostic_warning));
        assert!(p(ids.diagnostic_warning) > p(ids.diagnostic_info));
        assert!(p(ids.diagnostic_hint) < p(ids.diagnostic_info));
        // And an error is what a sign must EXCEED to take the cell — a
        // stricter bar than SG.2b's, where any priority above 10 did it.
        assert_eq!(p(ids.diagnostic_error), DIAGNOSTIC_ERROR_PRIORITY);
        for (a, b) in [
            (ids.diagnostic_error, ids.diagnostic_warning),
            (ids.diagnostic_warning, ids.diagnostic_info),
            (ids.diagnostic_info, ids.diagnostic_hint),
        ] {
            let (a, b) = (r.get(a).unwrap(), r.get(b).unwrap());
            assert_eq!(
                winning_sign(a, b).name,
                a.name,
                "the more severe diagnostic takes the cell"
            );
        }
    }

    /// Re-registering with new glyphs is what a `:set ui.diagnostic-*-glyph`
    /// does. The ids must NOT move, or every placement already in flight would
    /// resolve to nothing and the marks would blink out on an option change.
    #[test]
    fn re_registering_with_new_glyphs_keeps_every_id() {
        let mut r = SignRegistry::new();
        let first = register_builtin_signs(&mut r, DiagnosticGlyphs::default());
        let second = register_builtin_signs(
            &mut r,
            DiagnosticGlyphs {
                error: 'E',
                warning: 'W',
                info: 'I',
                hint: 'H',
            },
        );
        assert_eq!(first, second, "ids are stable across a redefinition");
        assert_eq!(r.len(), 8, "and no duplicates were minted");
        assert_eq!(
            r.get(first.diagnostic_error).unwrap().glyph_char(false),
            'E'
        );
    }

    /// A host that never registered the built-ins must paint NOTHING for them,
    /// not whatever happens to sit at id 0 — which would be some plugin's
    /// sign, silently, in exactly the configurations nobody looks at.
    #[test]
    fn unregistered_builtin_ids_resolve_to_nothing() {
        let mut r = SignRegistry::new();
        r.define(def("a-plugins-sign", 1));
        let ids = BuiltinSignIds::default();
        assert!(r.get(ids.diagnostic_error).is_none());
        assert!(r.get(ids.diff_add).is_none());
    }

    /// The publish path pre-resolves one theme element per DEFINITION, so it
    /// needs to walk them — and must not be handed a retired id, which
    /// resolves to nothing.
    #[test]
    fn iter_yields_live_definitions_and_skips_retired_ids() {
        let mut r = SignRegistry::new();
        let a = r.define(def("a", 1));
        let b = r.define(def("b", 2));
        r.undefine("a");
        let seen: Vec<SignId> = r.iter().map(|(id, _)| id).collect();
        assert_eq!(seen, vec![b], "the retired id must not be walked");
        assert!(r.get(a).is_none());
    }
}
