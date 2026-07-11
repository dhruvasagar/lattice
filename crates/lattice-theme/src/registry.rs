//! The theme-element registry + resolution + the builtin element
//! set.
//!
//! Registration assigns each element a process-stable
//! [`ElementId`]. Resolution turns every element's reference-form
//! [`StyleSpec`] into a concrete [`Style`] — walking the `inherit`
//! chain and looking colors up in the active [`Palette`] — **once**,
//! into a flat [`ResolvedTheme`] table the renderers read at O(1)
//! per glyph (`resolved.get(id)`). This is the paramount-#1
//! contract: the registry / palette / inheritance machinery lives at
//! theme-build time; the read is an array index.
//!
//! T.2 lands the registry struct, resolution, and the builtin
//! elements whose defaults reproduce today's exact colors (pinned by
//! `resolved_builtins_match_legacy_literals`). T.3 registers it as a
//! ServiceRegistry service at boot and exposes the handle to
//! consumers.
//!
//! Design: `docs/dev/architecture/theme-system.md` §3.4–§3.5, §7.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use arc_swap::ArcSwap;

use crate::element::{ColorRef, ElementId, ElementName, ElementOwner, StyleSpec, ThemeElement};
use crate::palette::{Palette, default_palette};
use crate::themes::{NamedTheme, builtin_themes};
use crate::{Color, FontScale, Style, Weight};

/// Maximum `inherit` chain depth before resolution bails (cycle
/// guard). Builtins are flat or one-deep; this is a safety net for
/// user/plugin specs.
const MAX_INHERIT_DEPTH: u8 = 16;

/// The flat, resolved read table for the active theme. `styles[id]`
/// is the fully-resolved [`Style`] for element `id`. Rebuilt on
/// theme/palette change; published via `ArcSwap` so the renderer's
/// read is lock-free.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTheme {
    styles: Box<[Style]>,
    version: u64,
}

impl ResolvedTheme {
    /// The resolved style for `id`. Out-of-range ids resolve to the
    /// empty style (an unregistered element is styleless, never a
    /// panic).
    pub fn get(&self, id: ElementId) -> Style {
        self.styles
            .get(id.0 as usize)
            .copied()
            .unwrap_or_else(Style::empty)
    }

    /// Monotonic version, bumped each rebuild. Folds into
    /// `lattice_cells::MatrixVersion::theme` so a palette change
    /// rebuilds the cell matrix (design §7).
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }
}

impl Default for ResolvedTheme {
    /// An empty table (version 0) — every `get` returns
    /// `Style::empty()`. The placeholder a default `RenderState`
    /// carries before the first real publish snapshots the live
    /// table; never the live render target.
    fn default() -> Self {
        ResolvedTheme {
            styles: Box::new([]),
            version: 0,
        }
    }
}

/// Introspection snapshot for a single registered element, returned
/// by [`ThemeRegistry::describe`] and rendered by `:describe-element`
/// (T.9.d). Bundles the element's identity + owner + authoring
/// (reference-form) [`StyleSpec`] default + doc string + its concrete
/// resolved [`Style`] under the active theme — everything the help
/// view needs in one read, no second registry round-trip.
#[derive(Debug, Clone, PartialEq)]
pub struct ElementInfo {
    pub name: ElementName,
    pub owner: ElementOwner,
    /// The owner-supplied authoring spec (palette-key references +
    /// inherit parent), pre-resolution. `:describe-element` renders
    /// this as the `Spec:` line so the user sees what the element
    /// *references*, not only the baked color.
    pub default: StyleSpec,
    pub doc: &'static str,
    /// The concrete style the element resolves to under the active
    /// palette + override set — what the renderer actually paints.
    pub resolved: Style,
}

/// Registration + resolution surface. Lives as a ServiceRegistry
/// service (T.3); modes reach it through the handle, never through
/// `&mut Editor`.
pub trait ThemeRegistry: Send + Sync {
    /// Register an element with its owner-supplied default. Idempotent
    /// by name — re-registering an existing name returns the existing
    /// id and leaves its default unchanged.
    fn register(
        &self,
        name: ElementName,
        owner: ElementOwner,
        default: StyleSpec,
        doc: &'static str,
    ) -> ElementId;

    /// The interned id for a name, if registered.
    fn id(&self, name: &ElementName) -> Option<ElementId>;

    /// The current resolved read table (rebuilt lazily if a
    /// registration / palette change left it dirty).
    fn resolved(&self) -> Arc<ResolvedTheme>;

    /// T.9: set (or replace) the theme-global override for an element
    /// (`:set ui.*`, user TOML, a theme's override list). Overlays the
    /// override's set fields on the element's resolved default. Marks
    /// the table dirty (re-resolved on next `resolved()`).
    fn set_override(&self, name: ElementName, spec: StyleSpec);

    /// T.9: swap the active theme — replace the palette AND the full
    /// override set atomically (`:colorscheme`). Prior overrides are
    /// cleared. Marks the table dirty.
    fn set_theme(&self, palette: Palette, overrides: Vec<(ElementName, StyleSpec)>);

    /// T.9.d: element metadata for introspection (`:describe-element`
    /// / `:describe-face`). Returns the element's identity + owner +
    /// authoring default + doc + concrete resolved style, or `None` if
    /// `name` is not a registered element. Reads the resolved table
    /// (rebuilding it lazily if dirty) so the `resolved` field reflects
    /// the active theme — a theme-build-time read, never on the hot
    /// path.
    fn describe(&self, name: &ElementName) -> Option<ElementInfo>;

    /// T.11.1: register (or replace, by name) a named theme in the
    /// catalog. Idempotent by name — re-registering a name replaces its
    /// palette + overrides. This is the seam `init.rs` (and, later, a
    /// WASM plugin via WIT) uses to contribute a palette; the builtin
    /// themes are seeded here at boot. Does NOT change the active theme —
    /// only `apply_theme` does that.
    fn register_theme(&self, theme: NamedTheme);

    /// T.11.1: the names of every registered theme, in registration
    /// order (builtins first, then user/plugin additions). Drives
    /// `:colorscheme` completion + the T.12 picker.
    fn theme_names(&self) -> Vec<String>;

    /// T.11.1: swap the active theme to the registered theme `name`
    /// (`:colorscheme <name>`). Returns `false` (active theme untouched)
    /// if `name` is not registered — the caller echoes an error, never a
    /// panic. On a hit, equivalent to `set_theme` with the registered
    /// theme's palette + overrides (marks the table dirty).
    fn apply_theme(&self, name: &str) -> bool;

    /// T.12a: snapshot the active palette + the active theme-global
    /// override set as a `(Palette, Vec<(ElementName, StyleSpec)>)`
    /// pair. The colorscheme picker captures this on the first live
    /// preview so `<Esc>` can restore the theme active when the picker
    /// opened via [`Self::set_theme`]. Cheap clone of the inner state
    /// under a read lock; no resolution.
    fn active_theme(&self) -> (Palette, Vec<(ElementName, StyleSpec)>);
}

/// The canonical handle type. Register and look up under THIS type in
/// the ServiceRegistry ([[feedback_servicesregistry_arc_typeid]]).
pub type ThemeRegistryHandle = Arc<dyn ThemeRegistry>;

struct RegistryInner {
    /// Indexed by `ElementId.0`.
    elements: Vec<ThemeElement>,
    by_name: HashMap<ElementName, ElementId>,
    palette: Palette,
    /// T.9: theme-global element overrides (the active theme's overrides
    /// + `:set ui.*` + user TOML). Keyed by name so an override may
    /// target an element registered later (a mode element). Resolution
    /// overlays the override's set fields on top of the element's
    /// resolved default (design §5.1, override scope 1).
    overrides: HashMap<ElementName, StyleSpec>,
    /// T.11.1: the named-theme catalog — `(Palette, overrides)` pairs by
    /// name, in registration order. `:colorscheme` / the picker resolve
    /// against this; `apply_theme` swaps the active palette+overrides to
    /// a catalog entry. Seeded with `builtin_themes()` at boot;
    /// `init.rs` / plugins append via `register_theme`.
    themes: Vec<NamedTheme>,
    version: u64,
    dirty: bool,
}

/// In-memory [`ThemeRegistry`]. Holds the element table + active
/// palette behind an `RwLock`, and the resolved read table behind an
/// `ArcSwap` for lock-free reads.
pub struct InMemoryThemeRegistry {
    inner: RwLock<RegistryInner>,
    resolved: ArcSwap<ResolvedTheme>,
}

impl InMemoryThemeRegistry {
    /// Empty registry with the given palette. Register elements, then
    /// `resolved()` builds the table on first read.
    pub fn new(palette: Palette) -> Self {
        InMemoryThemeRegistry {
            inner: RwLock::new(RegistryInner {
                elements: Vec::new(),
                by_name: HashMap::new(),
                palette,
                overrides: HashMap::new(),
                themes: Vec::new(),
                version: 0,
                dirty: true,
            }),
            resolved: ArcSwap::from_pointee(ResolvedTheme {
                styles: Box::new([]),
                version: 0,
            }),
        }
    }

    /// Registry seeded with the default palette + all builtin core
    /// elements, resolved and ready. The T.3 boot path + tests use
    /// this.
    pub fn with_defaults() -> Self {
        let reg = Self::new(default_palette());
        register_builtins(&reg);
        // T.11.1: seed the named-theme catalog with the builtins so
        // `:colorscheme` / the picker can resolve them; `init.rs` /
        // plugins append more via `register_theme`.
        for theme in builtin_themes() {
            reg.register_theme(theme);
        }
        reg.rebuild();
        reg
    }

    /// Replace the active palette (a `:colorscheme` / `:set ui.*`
    /// path lands in T.9). Marks the table dirty.
    pub fn set_palette(&self, palette: Palette) {
        let mut inner = self.inner.write().expect("theme registry lock poisoned");
        inner.palette = palette;
        inner.dirty = true;
    }

    /// Re-resolve every element into a fresh [`ResolvedTheme`] and
    /// publish it. Called on dirty reads; O(elements), theme-build
    /// time only (never on the hot path).
    pub fn rebuild(&self) {
        let mut guard = self.inner.write().expect("theme registry lock poisoned");
        let n = guard.elements.len();
        let mut styles = Vec::with_capacity(n);
        {
            let inner: &RegistryInner = &guard;
            for i in 0..n {
                styles.push(resolve_element(inner, ElementId(i as u32), 0));
            }
        }
        guard.version = guard.version.wrapping_add(1);
        let version = guard.version;
        guard.dirty = false;
        drop(guard);
        self.resolved.store(Arc::new(ResolvedTheme {
            styles: styles.into_boxed_slice(),
            version,
        }));
    }
}

impl ThemeRegistry for InMemoryThemeRegistry {
    fn register(
        &self,
        name: ElementName,
        owner: ElementOwner,
        default: StyleSpec,
        doc: &'static str,
    ) -> ElementId {
        let mut inner = self.inner.write().expect("theme registry lock poisoned");
        if let Some(id) = inner.by_name.get(&name) {
            return *id;
        }
        let id = ElementId(inner.elements.len() as u32);
        inner.by_name.insert(name.clone(), id);
        inner.elements.push(ThemeElement {
            id,
            name,
            owner,
            default,
            doc,
        });
        inner.dirty = true;
        id
    }

    fn id(&self, name: &ElementName) -> Option<ElementId> {
        self.inner
            .read()
            .expect("theme registry lock poisoned")
            .by_name
            .get(name)
            .copied()
    }

    fn resolved(&self) -> Arc<ResolvedTheme> {
        let dirty = self
            .inner
            .read()
            .expect("theme registry lock poisoned")
            .dirty;
        if dirty {
            self.rebuild();
        }
        self.resolved.load_full()
    }

    fn set_override(&self, name: ElementName, spec: StyleSpec) {
        let mut inner = self.inner.write().expect("theme registry lock poisoned");
        inner.overrides.insert(name, spec);
        inner.dirty = true;
    }

    fn set_theme(&self, palette: Palette, overrides: Vec<(ElementName, StyleSpec)>) {
        let mut inner = self.inner.write().expect("theme registry lock poisoned");
        inner.palette = palette;
        inner.overrides = overrides.into_iter().collect();
        inner.dirty = true;
    }

    fn register_theme(&self, theme: NamedTheme) {
        let mut inner = self.inner.write().expect("theme registry lock poisoned");
        // Idempotent by name: replace an existing entry in place (keeps
        // registration order stable), else append.
        if let Some(slot) = inner.themes.iter_mut().find(|t| t.name == theme.name) {
            *slot = theme;
        } else {
            inner.themes.push(theme);
        }
    }

    fn theme_names(&self) -> Vec<String> {
        let inner = self.inner.read().expect("theme registry lock poisoned");
        inner.themes.iter().map(|t| t.name.to_string()).collect()
    }

    fn apply_theme(&self, name: &str) -> bool {
        // Clone the catalog entry's palette + overrides under a READ
        // lock, release it, THEN swap via `set_theme` (which takes a
        // write lock) — never hold both, so no re-entrant deadlock.
        let found = {
            let inner = self.inner.read().expect("theme registry lock poisoned");
            inner
                .themes
                .iter()
                .find(|t| t.name == name)
                .map(|t| (t.palette.clone(), t.overrides.clone()))
        };
        match found {
            Some((palette, overrides)) => {
                self.set_theme(palette, overrides);
                true
            }
            None => false,
        }
    }

    fn active_theme(&self) -> (Palette, Vec<(ElementName, StyleSpec)>) {
        let inner = self.inner.read().expect("theme registry lock poisoned");
        let palette = inner.palette.clone();
        let overrides = inner
            .overrides
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        (palette, overrides)
    }

    fn describe(&self, name: &ElementName) -> Option<ElementInfo> {
        // The resolved style must reflect the active theme, so resolve
        // the table first (rebuilds lazily if dirty), THEN read the
        // element metadata + id under the lock. Order matters: a
        // pre-`resolved()` read could miss an override that just
        // dirtied the table.
        let resolved = self.resolved();
        let inner = self.inner.read().expect("theme registry lock poisoned");
        let id = *inner.by_name.get(name)?;
        let elem = inner.elements.get(id.0 as usize)?;
        Some(ElementInfo {
            name: elem.name.clone(),
            owner: elem.owner.clone(),
            default: elem.default.clone(),
            doc: elem.doc,
            resolved: resolved.get(id),
        })
    }
}

// ---- resolution ----

fn resolve_element(inner: &RegistryInner, id: ElementId, depth: u8) -> Style {
    match inner.elements.get(id.0 as usize) {
        Some(elem) => {
            let mut style = resolve_spec(inner, &elem.default, depth);
            // T.9: overlay the theme-global override (if any) on top of
            // the resolved default. The override's set fields win; an
            // override never re-inherits (it adjusts, not redefines).
            if let Some(ovr) = inner.overrides.get(&elem.name) {
                apply_overlay(inner, &mut style, ovr);
            }
            style
        }
        None => Style::empty(),
    }
}

fn resolve_named(inner: &RegistryInner, name: &ElementName, depth: u8) -> Style {
    // Exact match first, then dotted-parent fallback
    // (`markdown.heading.1` → `markdown.heading` → `markdown`).
    if let Some(id) = inner.by_name.get(name) {
        return resolve_element(inner, *id, depth);
    }
    match name.parent() {
        Some(parent) => resolve_named(inner, &parent, depth),
        None => Style::empty(),
    }
}

fn resolve_spec(inner: &RegistryInner, spec: &StyleSpec, depth: u8) -> Style {
    let mut style = match &spec.inherit {
        Some(parent) if depth < MAX_INHERIT_DEPTH => resolve_named(inner, parent, depth + 1),
        Some(parent) => {
            tracing::warn!(
                "theme: inherit chain too deep at `{}` — breaking cycle",
                parent
            );
            Style::empty()
        }
        None => Style::empty(),
    };
    apply_overlay(inner, &mut style, spec);
    style
}

/// Apply a spec's *set* fields (fg / bg / each tri-state modifier /
/// rich attrs) on top of an existing [`Style`], leaving unset fields
/// untouched. Used both for the inherit base (in [`resolve_spec`]) and
/// for theme-global overrides (in [`resolve_element`]). `inherit` is
/// NOT re-applied here — the caller handles the base.
fn apply_overlay(inner: &RegistryInner, style: &mut Style, spec: &StyleSpec) {
    if let Some(cref) = &spec.fg {
        if let Some(c) = resolve_color(inner, cref) {
            style.fg = Some(c);
        }
    }
    if let Some(cref) = &spec.bg {
        if let Some(c) = resolve_color(inner, cref) {
            style.bg = Some(c);
        }
    }

    let m = &spec.modifiers;
    if let Some(b) = m.bold {
        style.modifiers.bold = b;
    }
    if let Some(b) = m.italic {
        style.modifiers.italic = b;
    }
    if let Some(b) = m.underline {
        style.modifiers.underline = b;
    }
    if let Some(b) = m.dim {
        style.modifiers.dim = b;
    }
    if let Some(b) = m.reverse {
        style.modifiers.reverse = b;
    }

    if let Some(ratio) = spec.scale {
        style.scale = Some(FontScale::from_ratio(ratio));
    }
    if let Some(family) = spec.family {
        style.family = Some(family);
    }
    if let Some(weight) = spec.weight {
        style.weight = Some(weight);
    }
}

fn resolve_color(inner: &RegistryInner, cref: &ColorRef) -> Option<Color> {
    match cref {
        ColorRef::Palette(key) => match inner.palette.get(key) {
            Some(c) => Some(c),
            None => {
                tracing::warn!("theme: unknown palette key `{}`", key.as_str());
                None
            }
        },
        ColorRef::Literal(c) => Some(*c),
        ColorRef::Default => Some(Color::Default),
    }
}

// ---- builtin elements ----

/// Register every core element with its palette-referencing default.
/// Idempotent (safe to call once at boot). The defaults reproduce
/// today's `Theme::default()` + `syntax_style()` values exactly — the
/// resolved table is byte-identical to the legacy literals (the
/// parity pin), while every color goes through the palette.
pub fn register_builtins(reg: &dyn ThemeRegistry) {
    let core = ElementOwner::Core;
    let reg_one = |name: &'static str, spec: StyleSpec, doc: &'static str| {
        reg.register(ElementName::from_static(name), core.clone(), spec, doc);
    };
    let spec = StyleSpec::new;

    // ---- Pane chrome ----
    reg_one(
        "pane.status.active",
        spec().reverse().bold(),
        "Active pane's status line.",
    );
    reg_one(
        "pane.status.inactive",
        spec().fg("overlay").dim(),
        "Inactive pane's status line.",
    );
    reg_one(
        "pane.inactive_overlay",
        spec().dim(),
        "Dim overlay on inactive panes' content.",
    );
    reg_one(
        "pane.separator",
        spec().fg("overlay"),
        "Split separator between panes.",
    );

    // ---- Modeline (ML.1b): per-role segments on a palette-driven bar.
    // Active pane = a raised `surface1` bar with per-role foregrounds
    // (lualine/helix convention); inactive = a receded `surface0` bar,
    // uniformly muted (`overlay`). Palette-keyed so all builtin themes
    // resolve appropriately without per-theme edits. The `modeline.*`
    // role keys mirror `lattice_host::modeline::ROLE_*`.
    reg_one(
        "modeline.active",
        spec().bg("surface1"),
        "Active pane's modeline bar (base background; per-role fg overlays it).",
    );
    reg_one(
        "modeline.inactive",
        spec().bg("surface0").fg("overlay"),
        "Inactive pane's modeline bar (uniform muted; no per-role colour).",
    );
    reg_one(
        "modeline.mode",
        spec().fg("blue").bold(),
        "Lean modal-state tag (`NOR`/`INS`/…) in the active modeline.",
    );
    reg_one(
        "modeline.path",
        spec().fg("text"),
        "Buffer path segment in the active modeline.",
    );
    reg_one(
        "modeline.position",
        spec().fg("subtext"),
        "Cursor line:column segment in the active modeline.",
    );
    reg_one(
        "modeline.lang",
        spec().fg("teal"),
        "Language label segment in the active modeline.",
    );
    reg_one(
        "modeline.mode_item",
        spec().fg("subtext"),
        "Mode-contributed items (LSP / diff) in the active modeline.",
    );

    // ---- Editor canvas (T.11.0b: palette-driven so a colorscheme /
    // palette swap recolors the whole canvas — the light-theme seam) ----
    reg_one(
        "editor.background",
        spec().bg("base"),
        "Editor canvas background.",
    );
    reg_one(
        "editor.foreground",
        spec().fg("text"),
        "Editor default foreground text.",
    );
    reg_one(
        "editor.cursor",
        spec().bg("text").fg("base"),
        "Block-cursor cell (inverted: bg=text fg=base).",
    );
    reg_one(
        "ui.popup.background",
        spec().bg("mantle"),
        "Popup / overlay surface background.",
    );
    reg_one(
        "ui.popup.title",
        spec().fg("blue").bold(),
        "Popup header title (bold accent — describe/help/hover popups).",
    );
    reg_one(
        "ui.popup.hint",
        spec().fg("overlay"),
        "Popup header hint (dim — e.g. 'Esc to dismiss').",
    );

    // ---- File tree ----
    reg_one(
        "file_tree.dir",
        spec().fg("blue").bold(),
        "Directory entries in the file tree.",
    );
    reg_one(
        "file_tree.hidden",
        spec().fg("overlay").dim(),
        "Hidden entries in the file tree.",
    );
    reg_one("file_tree.file", spec(), "Regular file entries.");

    // ---- Terminal ANSI palette (the 16 colours programs draw in) ----
    // Each ANSI slot maps to a palette accent so the embedded terminal
    // recolours with the active colorscheme (catppuccin-style mapping:
    // magenta→pink, cyan→teal, black/white→surface/subtext). Replaces the
    // GPUI terminal's old hardcoded dim-VGA xterm palette. Bright variants
    // (8-15) reuse the same accents; only black/white brighten.
    reg_one("terminal.ansi.0", spec().fg("surface1"), "ANSI 0 — black.");
    reg_one("terminal.ansi.1", spec().fg("red"), "ANSI 1 — red.");
    reg_one("terminal.ansi.2", spec().fg("green"), "ANSI 2 — green.");
    reg_one("terminal.ansi.3", spec().fg("yellow"), "ANSI 3 — yellow.");
    reg_one("terminal.ansi.4", spec().fg("blue"), "ANSI 4 — blue.");
    reg_one("terminal.ansi.5", spec().fg("pink"), "ANSI 5 — magenta.");
    reg_one("terminal.ansi.6", spec().fg("teal"), "ANSI 6 — cyan.");
    reg_one("terminal.ansi.7", spec().fg("subtext"), "ANSI 7 — white.");
    reg_one(
        "terminal.ansi.8",
        spec().fg("surface2"),
        "ANSI 8 — bright black.",
    );
    reg_one("terminal.ansi.9", spec().fg("red"), "ANSI 9 — bright red.");
    reg_one(
        "terminal.ansi.10",
        spec().fg("green"),
        "ANSI 10 — bright green.",
    );
    reg_one(
        "terminal.ansi.11",
        spec().fg("yellow"),
        "ANSI 11 — bright yellow.",
    );
    reg_one(
        "terminal.ansi.12",
        spec().fg("blue"),
        "ANSI 12 — bright blue.",
    );
    reg_one(
        "terminal.ansi.13",
        spec().fg("pink"),
        "ANSI 13 — bright magenta.",
    );
    reg_one(
        "terminal.ansi.14",
        spec().fg("teal"),
        "ANSI 14 — bright cyan.",
    );
    reg_one(
        "terminal.ansi.15",
        spec().fg("text"),
        "ANSI 15 — bright white.",
    );

    // ---- Diagnostics ----
    reg_one(
        "diagnostic.error",
        spec().fg("red").bold(),
        "Error-severity diagnostic sign + text.",
    );
    reg_one(
        "diagnostic.warning",
        spec().fg("yellow").bold(),
        "Warning-severity diagnostic.",
    );
    reg_one(
        "diagnostic.info",
        spec().fg("blue"),
        "Info-severity diagnostic.",
    );
    reg_one(
        "diagnostic.hint",
        spec().fg("overlay").dim(),
        "Hint-severity diagnostic.",
    );

    // ---- Whitespace + current line ----
    reg_one(
        "whitespace",
        spec().fg("overlay").dim(),
        "Rendered whitespace markers.",
    );
    reg_one(
        "whitespace.trailing",
        spec().fg("red"),
        "Trailing whitespace markers.",
    );
    reg_one(
        "editor.cursor_line",
        spec().bg("cursor_line.bg"),
        "Current-line background tint.",
    );

    // ---- *messages* buffer levels ----
    reg_one(
        "messages.timestamp",
        spec().fg("overlay").dim(),
        "Timestamp column in *messages*.",
    );
    reg_one("messages.trace", spec().dim(), "TRACE-level message.");
    reg_one("messages.debug", spec().fg("cyan"), "DEBUG-level message.");
    reg_one("messages.info", spec(), "INFO-level message.");
    reg_one(
        "messages.warn",
        spec().fg("yellow").bold(),
        "WARN-level message.",
    );
    reg_one(
        "messages.error",
        spec().fg("red").bold(),
        "ERROR-level message.",
    );

    // ---- Diff ----
    reg_one(
        "diff.add.sign",
        spec().fg("green").bold(),
        "`+` gutter sign (added line).",
    );
    reg_one(
        "diff.change.sign",
        spec().fg("yellow").bold(),
        "`~` gutter sign (changed line).",
    );
    reg_one(
        "diff.remove.sign",
        spec().fg("red").bold(),
        "`-` gutter sign (removed line).",
    );
    reg_one(
        "diff.conflict.sign",
        spec().fg("purple").bold(),
        "`?` gutter sign (three-way conflict).",
    );
    reg_one(
        "diff.add.line",
        spec().bg("diff.add.bg"),
        "Added-line background tint.",
    );
    reg_one(
        "diff.change.line",
        spec().bg("diff.change.bg"),
        "Changed-line background tint.",
    );
    reg_one(
        "diff.remove.line",
        spec().bg("diff.deletion.bg"),
        "Removed-line background tint (baseline/left pane of a side-by-side diff). \
         Reuses the deletion-block palette role for a consistent red.",
    );
    reg_one(
        "diff.deletion_block",
        spec().bg("diff.deletion.bg"),
        "Deletion-block virtual-row background tint.",
    );
    reg_one(
        "diff.conflict.line",
        spec().bg("diff.conflict.bg"),
        "Conflict-region background tint.",
    );

    // ---- Fold markers ----
    // Gutter glyphs on a foldable head row: `▾` when the fold is open,
    // `▸` when collapsed. Muted by cross-editor convention — VS Code
    // (`editorGutter.foldingControlForeground` → `icon.foreground`),
    // Zed, JetBrains, Sublime, and Neovim's `FoldColumn` all render fold
    // controls in a low-emphasis gray rather than an accent. Open uses
    // the dim `overlay` tone so always-visible markers don't clutter;
    // closed steps up to `subtext` (the line-number tone) for a touch
    // more presence since it signals hidden content. Themes retune both.
    reg_one(
        "gutter.fold.open",
        spec().fg("overlay"),
        "`▾` fold marker on an open (expanded) foldable head row.",
    );
    reg_one(
        "gutter.fold.closed",
        spec().fg("subtext"),
        "`▸` fold marker on a closed (collapsed) fold head row.",
    );

    // ---- Search + selection + LSP overlays (T.6) ----
    // These hoist the scattered/drifted hardcoded overlay literals out
    // of the renderers so BOTH peers read the same registered styles
    // (closing the TUI/GPUI parity drift). Search match + current are
    // BACKGROUND tints in both renderers (the TUI's legacy fg-recolor
    // is retired); document-highlight keeps its 3 distinct kinds.
    reg_one(
        "search.match",
        spec().bg("overlay"),
        "All hlsearch matches (bg tint).",
    );
    reg_one(
        "search.current",
        spec().bg(Color::Rgb(0x6c, 0x5a, 0x1e)),
        "Current search match (warm bg tint).",
    );
    reg_one(
        "selection",
        spec().bg(Color::Rgb(0x45, 0x47, 0x5a)),
        "Visual-mode selection bg.",
    );
    reg_one(
        "doc_highlight.read",
        spec().bg(Color::Rgb(20, 50, 25)),
        "LSP document-highlight: read occurrence.",
    );
    reg_one(
        "doc_highlight.write",
        spec().bg(Color::Rgb(60, 20, 20)),
        "LSP document-highlight: write/mutation.",
    );
    reg_one(
        "doc_highlight.text",
        spec().bg(Color::Rgb(20, 30, 60)),
        "LSP document-highlight: text occurrence.",
    );
    reg_one(
        "substitute.preview",
        spec().bg(Color::Rgb(0xf3, 0x8b, 0xa8)),
        "`:s///` live-preview match (bg; TUI adds strikethrough).",
    );
    reg_one(
        "inlay.hint",
        spec().fg(Color::Rgb(0x7f, 0x84, 0x9c)),
        "Inlay hint virtual text (overlay1).",
    );

    // ---- Completion annotations (T.6) ----
    // The 5 base (unselected) colours; the selected-row BRIGHTENING
    // stays renderer logic applied on top of the resolved base color.
    reg_one(
        "completion.annotation.kind",
        spec().fg("subtext"),
        "Completion annotation: kind.",
    );
    reg_one(
        "completion.annotation.doc",
        spec().fg(Color::Rgb(0x89, 0xdc, 0xeb)),
        "Completion annotation: doc snippet.",
    );
    reg_one(
        "completion.annotation.keybinding",
        spec().fg("yellow"),
        "Completion annotation: keybinding.",
    );
    reg_one(
        "completion.annotation.source",
        spec().fg("purple"),
        "Completion annotation: source.",
    );
    reg_one(
        "completion.annotation.custom",
        spec().fg("blue"),
        "Completion annotation: custom/plugin.",
    );
    // ---- MARG §8: per-segment file-metadata marginalia ----
    // eza / `ls --color` convention for the permission string
    // (one slot per bit class) plus size + mtime columns. Consumed
    // by `Annotation::Styled` segments emitted by the file/dir picker.
    reg_one(
        "completion.annotation.perm.type",
        spec().fg("blue"),
        "Marginalia: permission type char (d/l/b/c/p/s/-).",
    );
    reg_one(
        "completion.annotation.perm.read",
        spec().fg("yellow"),
        "Marginalia: permission read bit (r).",
    );
    reg_one(
        "completion.annotation.perm.write",
        spec().fg("red"),
        "Marginalia: permission write bit (w).",
    );
    reg_one(
        "completion.annotation.perm.exec",
        spec().fg("green"),
        "Marginalia: permission execute bit (x).",
    );
    reg_one(
        "completion.annotation.perm.special",
        spec().fg("pink"),
        "Marginalia: permission special bit (setuid/setgid/sticky).",
    );
    reg_one(
        "completion.annotation.perm.none",
        spec().fg("overlay"),
        "Marginalia: permission absent bit (-).",
    );
    reg_one(
        "completion.annotation.size",
        spec().fg("orange"),
        "Marginalia: file size column.",
    );
    reg_one(
        "completion.annotation.mtime",
        spec().fg("green"),
        "Marginalia: file modified-time column.",
    );
    // ---- MARG §9: picker marginalia rollout (location / status /
    // latency / args / buffer-id / register). Same `Annotation::Styled`
    // mechanism, new slot families consumed by the non-file pickers. ----
    reg_one(
        "completion.annotation.location.path",
        spec().fg("overlay"),
        "Marginalia: location path head (grep/jumps/marks).",
    );
    reg_one(
        "completion.annotation.location.line",
        spec().fg("yellow"),
        "Marginalia: location line number.",
    );
    reg_one(
        "completion.annotation.location.col",
        spec().fg("overlay"),
        "Marginalia: location column number.",
    );
    reg_one(
        "completion.annotation.status.dirty",
        spec().fg("red"),
        "Marginalia: dirty-buffer marker.",
    );
    reg_one(
        "completion.annotation.status.active",
        spec().fg("green"),
        "Marginalia: current-buffer marker.",
    );
    reg_one(
        "completion.annotation.latency.reflex",
        spec().fg("green"),
        "Marginalia: reflex-latency command class.",
    );
    reg_one(
        "completion.annotation.latency.display",
        spec().fg("blue"),
        "Marginalia: display-latency command class.",
    );
    reg_one(
        "completion.annotation.latency.background",
        spec().fg("orange"),
        "Marginalia: background-latency command class.",
    );
    reg_one(
        "completion.annotation.args",
        spec().fg("subtext"),
        "Marginalia: command argument hint.",
    );
    reg_one(
        "completion.annotation.buffer-id",
        spec().fg("overlay"),
        "Marginalia: buffer id (#N).",
    );
    reg_one(
        "completion.annotation.register",
        spec().fg("purple"),
        "Marginalia: register / mark name.",
    );

    // ---- Syntax (mirrors host `Theme::syntax_style`) ----
    reg_one(
        "syntax.default",
        spec().fg("text"),
        "Default foreground text.",
    );
    reg_one(
        "syntax.comment",
        spec().fg("overlay").italic(),
        "Block / doc comments.",
    );
    // LineComment is byte-identical to Comment — demonstrates inherit.
    reg_one(
        "syntax.line_comment",
        spec().inherit("syntax.comment"),
        "Line comments (inherits syntax.comment).",
    );
    reg_one("syntax.string", spec().fg("green"), "String literals.");
    reg_one(
        "syntax.keyword",
        spec().fg("purple").bold(),
        "Language keywords.",
    );
    reg_one("syntax.type", spec().fg("yellow"), "Type names.");
    reg_one("syntax.number", spec().fg("orange"), "Numeric literals.");
    reg_one("syntax.function", spec().fg("blue"), "Function names.");
    reg_one("syntax.constant", spec().fg("orange"), "Constants.");
    reg_one("syntax.variable", spec().fg("text"), "Variables.");
    reg_one("syntax.operator", spec().fg("teal"), "Operators.");
    reg_one(
        "syntax.punctuation",
        spec().fg("subtext"),
        "Punctuation / delimiters.",
    );
    reg_one(
        "syntax.attribute",
        spec().fg("red"),
        "Attributes / annotations.",
    );
    // T.10: heading levels carry a rich-vocabulary `weight` (finer than
    // the bold modifier). The GPUI peer honors it in per-run font
    // shaping so headings render heavier than body bold; the TUI degrades
    // (any SemiBold-or-heavier weight maps to its bold attribute, and
    // these already set `.bold()`, so the TUI is visually unchanged).
    //
    // F.3 (Thread F): heading levels also carry a rich-vocabulary `scale`
    // (emacs `:height`) descending by level — h1 largest, h6 barely above
    // body. The GPUI peer honors it via variable row height (F.2): the
    // whole heading display line is shaped at `font_size * scale`. The TUI
    // degrades (a fixed cell grid cannot vary font size; headings stay
    // bold+colored+underlined). Because Heading tokens are markdown-
    // exclusive, this core syntax-element default IS effectively buffer-
    // local — only markdown buffers carry these tokens (Option A, design
    // §6.1; T.8 buffer-local remap stays deferred for true per-buffer
    // divergence like variable-pitch prose).
    reg_one(
        "syntax.heading.1",
        spec()
            .fg("red")
            .bold()
            .underline()
            .weight(Weight::ExtraBold)
            .scale(1.6),
        "Markup heading level 1.",
    );
    reg_one(
        "syntax.heading.2",
        spec().fg("orange").bold().weight(Weight::Bold).scale(1.4),
        "Markup heading level 2.",
    );
    reg_one(
        "syntax.heading.3",
        spec().fg("yellow").bold().weight(Weight::Bold).scale(1.25),
        "Markup heading level 3.",
    );
    reg_one(
        "syntax.heading.4",
        spec().fg("green").bold().scale(1.15),
        "Markup heading level 4.",
    );
    reg_one(
        "syntax.heading.5",
        spec().fg("blue").bold().scale(1.1),
        "Markup heading level 5.",
    );
    reg_one(
        "syntax.heading.6",
        spec().fg("purple").bold().scale(1.05),
        "Markup heading level 6.",
    );
    reg_one(
        "syntax.bold",
        spec().fg("maroon").bold(),
        "Strong / bold markup.",
    );
    reg_one(
        "syntax.italic",
        spec().fg("pink").italic(),
        "Emphasis / italic markup.",
    );
    reg_one(
        "syntax.link",
        spec().fg("blue").underline(),
        "Markup links.",
    );
    reg_one("syntax.url", spec().fg("cyan").underline(), "Bare URLs.");
    reg_one(
        "syntax.markup_raw",
        spec().fg("overlay").dim(),
        "Inline code / raw markup.",
    );
    reg_one(
        "syntax.markup",
        spec().fg("subtext").bold(),
        "Generic markup punctuation.",
    );
}

// ---- builtin element id capture (T.4) ----

/// The interned [`ElementId`]s for the builtin elements the renderers
/// read, captured **once at boot** from the [`ThemeRegistry`] and held
/// for the process lifetime. `Copy` + small, so it snapshots into
/// `RenderState` per publish for free; a read is then
/// `resolved.get(ids.<elem>)` — an array index, no per-frame name
/// lookup (design §7).
///
/// Grown one consumer-group at a time as Thread B migrates renderers
/// off the flat `Theme` struct (T.4.a diagnostics → T.4.b diff → …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinElementIds {
    // T.4.a — diagnostics.
    pub diagnostic_error: ElementId,
    pub diagnostic_warning: ElementId,
    pub diagnostic_info: ElementId,
    pub diagnostic_hint: ElementId,
    // T.4.b — diff gutter signs (fg styles) + line/block tints (bg).
    pub diff_add_sign: ElementId,
    pub diff_change_sign: ElementId,
    pub diff_remove_sign: ElementId,
    pub diff_conflict_sign: ElementId,
    pub diff_add_line: ElementId,
    pub diff_change_line: ElementId,
    /// D-fix.3b: removed-line tint for the baseline/left pane.
    pub diff_remove_line: ElementId,
    pub diff_deletion_block: ElementId,
    pub diff_conflict_line: ElementId,
    // Fold-marker gutter glyphs (`▾` open head, `▸` collapsed head).
    // Muted by cross-editor convention (VS Code / Zed / JetBrains /
    // Sublime / Neovim all render fold controls in a low-emphasis gray,
    // never an accent). `closed` gets a touch more presence than `open`
    // because it signals hidden content; the `⋯ N lines` summary carries
    // the rest of that signal. Both renderers read these ids so the TUI
    // and GPUI gutters stay in lockstep and themes can retune the tone.
    pub gutter_fold_open: ElementId,
    pub gutter_fold_closed: ElementId,
    // T.4.c — pane chrome + file tree (writer-free elements only;
    // `pane.status.*` / `pane.separator` carry live `:set ui.*`
    // overrides and migrate with the registry-override path in T.9).
    pub pane_status_active: ElementId,
    pub pane_status_inactive: ElementId,
    pub pane_separator: ElementId,
    pub pane_inactive_overlay: ElementId,
    // ML.1b — modeline per-role segments + active/inactive bar base.
    pub modeline_active: ElementId,
    pub modeline_inactive: ElementId,
    pub modeline_mode: ElementId,
    pub modeline_path: ElementId,
    pub modeline_position: ElementId,
    pub modeline_lang: ElementId,
    pub modeline_mode_item: ElementId,
    // T.11.0b — editor canvas (bg / fg / block cursor / popup surface).
    // Palette-driven so a `:colorscheme` / palette swap recolors the
    // whole canvas; the readiness for light themes.
    pub editor_background: ElementId,
    pub editor_foreground: ElementId,
    pub editor_cursor: ElementId,
    pub ui_popup_background: ElementId,
    pub ui_popup_title: ElementId,
    pub ui_popup_hint: ElementId,
    pub file_tree_dir: ElementId,
    pub file_tree_hidden: ElementId,
    pub file_tree_file: ElementId,
    // T.4.d — current-line tint + *messages* level styling. (Whitespace
    // is read in the cell builder, so it migrates with the cell-path
    // resolved wiring in T.5.)
    pub editor_cursor_line: ElementId,
    pub messages_timestamp: ElementId,
    pub messages_trace: ElementId,
    pub messages_debug: ElementId,
    pub messages_info: ElementId,
    pub messages_warn: ElementId,
    pub messages_error: ElementId,
    // T.5 — syntax categories (the cell builder + both display-line
    // paths map `lattice_syntax::Style` → these) + whitespace markers.
    pub syntax_default: ElementId,
    pub syntax_comment: ElementId,
    pub syntax_line_comment: ElementId,
    pub syntax_string: ElementId,
    pub syntax_keyword: ElementId,
    pub syntax_type: ElementId,
    pub syntax_number: ElementId,
    pub syntax_function: ElementId,
    pub syntax_constant: ElementId,
    pub syntax_variable: ElementId,
    pub syntax_operator: ElementId,
    pub syntax_punctuation: ElementId,
    pub syntax_attribute: ElementId,
    pub syntax_heading_1: ElementId,
    pub syntax_heading_2: ElementId,
    pub syntax_heading_3: ElementId,
    pub syntax_heading_4: ElementId,
    pub syntax_heading_5: ElementId,
    pub syntax_heading_6: ElementId,
    pub syntax_bold: ElementId,
    pub syntax_italic: ElementId,
    pub syntax_link: ElementId,
    pub syntax_url: ElementId,
    pub syntax_markup_raw: ElementId,
    pub syntax_markup: ElementId,
    pub whitespace: ElementId,
    pub whitespace_trailing: ElementId,
    // T.6 — search + selection + LSP overlays (BOTH renderers read
    // these; closes the TUI/GPUI parity drift). Search match/current
    // are bg tints in both peers now.
    pub search_match: ElementId,
    pub search_current: ElementId,
    pub selection: ElementId,
    pub doc_highlight_read: ElementId,
    pub doc_highlight_write: ElementId,
    pub doc_highlight_text: ElementId,
    pub substitute_preview: ElementId,
    pub inlay_hint: ElementId,
    // T.6 — completion-annotation base (unselected) colours; the
    // selected-row brightening stays renderer logic on top of these.
    pub completion_annotation_kind: ElementId,
    pub completion_annotation_doc: ElementId,
    pub completion_annotation_keybinding: ElementId,
    pub completion_annotation_source: ElementId,
    pub completion_annotation_custom: ElementId,
    // MARG §8: per-segment file-metadata marginalia slots.
    pub completion_annotation_perm_type: ElementId,
    pub completion_annotation_perm_read: ElementId,
    pub completion_annotation_perm_write: ElementId,
    pub completion_annotation_perm_exec: ElementId,
    pub completion_annotation_perm_special: ElementId,
    pub completion_annotation_perm_none: ElementId,
    pub completion_annotation_size: ElementId,
    pub completion_annotation_mtime: ElementId,
    // MARG §9: picker marginalia rollout slots.
    pub completion_annotation_location_path: ElementId,
    pub completion_annotation_location_line: ElementId,
    pub completion_annotation_location_col: ElementId,
    pub completion_annotation_status_dirty: ElementId,
    pub completion_annotation_status_active: ElementId,
    pub completion_annotation_latency_reflex: ElementId,
    pub completion_annotation_latency_display: ElementId,
    pub completion_annotation_latency_background: ElementId,
    pub completion_annotation_args: ElementId,
    pub completion_annotation_buffer_id: ElementId,
    pub completion_annotation_register: ElementId,
    // Terminal ANSI 0-15 (the 16-colour palette programs draw in). Each
    // maps to a palette accent so the embedded terminal recolours with the
    // colorscheme instead of a hardcoded dim-VGA xterm palette. The GPUI
    // terminal renderer reads these; index = ANSI colour number.
    pub terminal_ansi: [ElementId; 16],
}

impl Default for BuiltinElementIds {
    /// All [`ElementId::INVALID`] — the placeholder a default
    /// `RenderState` carries before the boot capture. Reads against an
    /// empty/default resolved table return `Style::empty()`.
    fn default() -> Self {
        BuiltinElementIds {
            diagnostic_error: ElementId::INVALID,
            diagnostic_warning: ElementId::INVALID,
            diagnostic_info: ElementId::INVALID,
            diagnostic_hint: ElementId::INVALID,
            diff_add_sign: ElementId::INVALID,
            diff_change_sign: ElementId::INVALID,
            diff_remove_sign: ElementId::INVALID,
            diff_conflict_sign: ElementId::INVALID,
            diff_add_line: ElementId::INVALID,
            diff_change_line: ElementId::INVALID,
            diff_remove_line: ElementId::INVALID,
            diff_deletion_block: ElementId::INVALID,
            diff_conflict_line: ElementId::INVALID,
            gutter_fold_open: ElementId::INVALID,
            gutter_fold_closed: ElementId::INVALID,
            pane_status_active: ElementId::INVALID,
            pane_status_inactive: ElementId::INVALID,
            pane_separator: ElementId::INVALID,
            pane_inactive_overlay: ElementId::INVALID,
            terminal_ansi: [ElementId::INVALID; 16],
            modeline_active: ElementId::INVALID,
            modeline_inactive: ElementId::INVALID,
            modeline_mode: ElementId::INVALID,
            modeline_path: ElementId::INVALID,
            modeline_position: ElementId::INVALID,
            modeline_lang: ElementId::INVALID,
            modeline_mode_item: ElementId::INVALID,
            editor_background: ElementId::INVALID,
            editor_foreground: ElementId::INVALID,
            editor_cursor: ElementId::INVALID,
            ui_popup_background: ElementId::INVALID,
            ui_popup_title: ElementId::INVALID,
            ui_popup_hint: ElementId::INVALID,
            file_tree_dir: ElementId::INVALID,
            file_tree_hidden: ElementId::INVALID,
            file_tree_file: ElementId::INVALID,
            editor_cursor_line: ElementId::INVALID,
            messages_timestamp: ElementId::INVALID,
            messages_trace: ElementId::INVALID,
            messages_debug: ElementId::INVALID,
            messages_info: ElementId::INVALID,
            messages_warn: ElementId::INVALID,
            messages_error: ElementId::INVALID,
            syntax_default: ElementId::INVALID,
            syntax_comment: ElementId::INVALID,
            syntax_line_comment: ElementId::INVALID,
            syntax_string: ElementId::INVALID,
            syntax_keyword: ElementId::INVALID,
            syntax_type: ElementId::INVALID,
            syntax_number: ElementId::INVALID,
            syntax_function: ElementId::INVALID,
            syntax_constant: ElementId::INVALID,
            syntax_variable: ElementId::INVALID,
            syntax_operator: ElementId::INVALID,
            syntax_punctuation: ElementId::INVALID,
            syntax_attribute: ElementId::INVALID,
            syntax_heading_1: ElementId::INVALID,
            syntax_heading_2: ElementId::INVALID,
            syntax_heading_3: ElementId::INVALID,
            syntax_heading_4: ElementId::INVALID,
            syntax_heading_5: ElementId::INVALID,
            syntax_heading_6: ElementId::INVALID,
            syntax_bold: ElementId::INVALID,
            syntax_italic: ElementId::INVALID,
            syntax_link: ElementId::INVALID,
            syntax_url: ElementId::INVALID,
            syntax_markup_raw: ElementId::INVALID,
            syntax_markup: ElementId::INVALID,
            whitespace: ElementId::INVALID,
            whitespace_trailing: ElementId::INVALID,
            search_match: ElementId::INVALID,
            search_current: ElementId::INVALID,
            selection: ElementId::INVALID,
            doc_highlight_read: ElementId::INVALID,
            doc_highlight_write: ElementId::INVALID,
            doc_highlight_text: ElementId::INVALID,
            substitute_preview: ElementId::INVALID,
            inlay_hint: ElementId::INVALID,
            completion_annotation_kind: ElementId::INVALID,
            completion_annotation_doc: ElementId::INVALID,
            completion_annotation_keybinding: ElementId::INVALID,
            completion_annotation_source: ElementId::INVALID,
            completion_annotation_custom: ElementId::INVALID,
            completion_annotation_perm_type: ElementId::INVALID,
            completion_annotation_perm_read: ElementId::INVALID,
            completion_annotation_perm_write: ElementId::INVALID,
            completion_annotation_perm_exec: ElementId::INVALID,
            completion_annotation_perm_special: ElementId::INVALID,
            completion_annotation_perm_none: ElementId::INVALID,
            completion_annotation_size: ElementId::INVALID,
            completion_annotation_mtime: ElementId::INVALID,
            completion_annotation_location_path: ElementId::INVALID,
            completion_annotation_location_line: ElementId::INVALID,
            completion_annotation_location_col: ElementId::INVALID,
            completion_annotation_status_dirty: ElementId::INVALID,
            completion_annotation_status_active: ElementId::INVALID,
            completion_annotation_latency_reflex: ElementId::INVALID,
            completion_annotation_latency_display: ElementId::INVALID,
            completion_annotation_latency_background: ElementId::INVALID,
            completion_annotation_args: ElementId::INVALID,
            completion_annotation_buffer_id: ElementId::INVALID,
            completion_annotation_register: ElementId::INVALID,
        }
    }
}

impl BuiltinElementIds {
    /// MARG §8: map an [`Annotation::Styled`] segment's slot KEY to its
    /// interned element id, shared by both renderer peers so a styled
    /// marginalia cell resolves identically. Unknown slots fall back to
    /// `completion.annotation.custom` (the plugin-annotation default) —
    /// a paint-time styleless-ish read, never a panic. Adding a new
    /// builtin slot is one arm here plus the field/registration; plugin
    /// slots resolve dynamically once the WASM host lands (design §7).
    ///
    /// [`Annotation::Styled`]: lattice-completion's `Annotation::Styled`
    pub fn annotation_slot(&self, slot: &str) -> ElementId {
        match slot {
            "completion.annotation.kind" => self.completion_annotation_kind,
            "completion.annotation.doc" => self.completion_annotation_doc,
            "completion.annotation.keybinding" => self.completion_annotation_keybinding,
            "completion.annotation.source" => self.completion_annotation_source,
            "completion.annotation.perm.type" => self.completion_annotation_perm_type,
            "completion.annotation.perm.read" => self.completion_annotation_perm_read,
            "completion.annotation.perm.write" => self.completion_annotation_perm_write,
            "completion.annotation.perm.exec" => self.completion_annotation_perm_exec,
            "completion.annotation.perm.special" => self.completion_annotation_perm_special,
            "completion.annotation.perm.none" => self.completion_annotation_perm_none,
            "completion.annotation.size" => self.completion_annotation_size,
            "completion.annotation.mtime" => self.completion_annotation_mtime,
            "completion.annotation.location.path" => self.completion_annotation_location_path,
            "completion.annotation.location.line" => self.completion_annotation_location_line,
            "completion.annotation.location.col" => self.completion_annotation_location_col,
            "completion.annotation.status.dirty" => self.completion_annotation_status_dirty,
            "completion.annotation.status.active" => self.completion_annotation_status_active,
            "completion.annotation.latency.reflex" => self.completion_annotation_latency_reflex,
            "completion.annotation.latency.display" => self.completion_annotation_latency_display,
            "completion.annotation.latency.background" => {
                self.completion_annotation_latency_background
            }
            "completion.annotation.args" => self.completion_annotation_args,
            "completion.annotation.buffer-id" => self.completion_annotation_buffer_id,
            "completion.annotation.register" => self.completion_annotation_register,
            _ => self.completion_annotation_custom,
        }
    }

    /// Intern the builtin ids from the registry once at boot. A
    /// missing builtin (a registration bug, never expected) logs once
    /// and falls back to [`ElementId::INVALID`] — a styleless read,
    /// never a panic (graceful degradation, paramount-goal-aligned).
    pub fn capture(reg: &dyn ThemeRegistry) -> Self {
        let id = |name: &'static str| match reg.id(&ElementName::from_static(name)) {
            Some(id) => id,
            None => {
                tracing::warn!("theme: builtin element `{name}` not registered at id-capture");
                ElementId::INVALID
            }
        };
        BuiltinElementIds {
            terminal_ansi: [
                id("terminal.ansi.0"),
                id("terminal.ansi.1"),
                id("terminal.ansi.2"),
                id("terminal.ansi.3"),
                id("terminal.ansi.4"),
                id("terminal.ansi.5"),
                id("terminal.ansi.6"),
                id("terminal.ansi.7"),
                id("terminal.ansi.8"),
                id("terminal.ansi.9"),
                id("terminal.ansi.10"),
                id("terminal.ansi.11"),
                id("terminal.ansi.12"),
                id("terminal.ansi.13"),
                id("terminal.ansi.14"),
                id("terminal.ansi.15"),
            ],
            diagnostic_error: id("diagnostic.error"),
            diagnostic_warning: id("diagnostic.warning"),
            diagnostic_info: id("diagnostic.info"),
            diagnostic_hint: id("diagnostic.hint"),
            diff_add_sign: id("diff.add.sign"),
            diff_change_sign: id("diff.change.sign"),
            diff_remove_sign: id("diff.remove.sign"),
            diff_conflict_sign: id("diff.conflict.sign"),
            diff_add_line: id("diff.add.line"),
            diff_change_line: id("diff.change.line"),
            diff_remove_line: id("diff.remove.line"),
            diff_deletion_block: id("diff.deletion_block"),
            diff_conflict_line: id("diff.conflict.line"),
            gutter_fold_open: id("gutter.fold.open"),
            gutter_fold_closed: id("gutter.fold.closed"),
            pane_status_active: id("pane.status.active"),
            pane_status_inactive: id("pane.status.inactive"),
            pane_separator: id("pane.separator"),
            pane_inactive_overlay: id("pane.inactive_overlay"),
            modeline_active: id("modeline.active"),
            modeline_inactive: id("modeline.inactive"),
            modeline_mode: id("modeline.mode"),
            modeline_path: id("modeline.path"),
            modeline_position: id("modeline.position"),
            modeline_lang: id("modeline.lang"),
            modeline_mode_item: id("modeline.mode_item"),
            editor_background: id("editor.background"),
            editor_foreground: id("editor.foreground"),
            editor_cursor: id("editor.cursor"),
            ui_popup_background: id("ui.popup.background"),
            ui_popup_title: id("ui.popup.title"),
            ui_popup_hint: id("ui.popup.hint"),
            file_tree_dir: id("file_tree.dir"),
            file_tree_hidden: id("file_tree.hidden"),
            file_tree_file: id("file_tree.file"),
            editor_cursor_line: id("editor.cursor_line"),
            messages_timestamp: id("messages.timestamp"),
            messages_trace: id("messages.trace"),
            messages_debug: id("messages.debug"),
            messages_info: id("messages.info"),
            messages_warn: id("messages.warn"),
            messages_error: id("messages.error"),
            syntax_default: id("syntax.default"),
            syntax_comment: id("syntax.comment"),
            syntax_line_comment: id("syntax.line_comment"),
            syntax_string: id("syntax.string"),
            syntax_keyword: id("syntax.keyword"),
            syntax_type: id("syntax.type"),
            syntax_number: id("syntax.number"),
            syntax_function: id("syntax.function"),
            syntax_constant: id("syntax.constant"),
            syntax_variable: id("syntax.variable"),
            syntax_operator: id("syntax.operator"),
            syntax_punctuation: id("syntax.punctuation"),
            syntax_attribute: id("syntax.attribute"),
            syntax_heading_1: id("syntax.heading.1"),
            syntax_heading_2: id("syntax.heading.2"),
            syntax_heading_3: id("syntax.heading.3"),
            syntax_heading_4: id("syntax.heading.4"),
            syntax_heading_5: id("syntax.heading.5"),
            syntax_heading_6: id("syntax.heading.6"),
            syntax_bold: id("syntax.bold"),
            syntax_italic: id("syntax.italic"),
            syntax_link: id("syntax.link"),
            syntax_url: id("syntax.url"),
            syntax_markup_raw: id("syntax.markup_raw"),
            syntax_markup: id("syntax.markup"),
            whitespace: id("whitespace"),
            whitespace_trailing: id("whitespace.trailing"),
            search_match: id("search.match"),
            search_current: id("search.current"),
            selection: id("selection"),
            doc_highlight_read: id("doc_highlight.read"),
            doc_highlight_write: id("doc_highlight.write"),
            doc_highlight_text: id("doc_highlight.text"),
            substitute_preview: id("substitute.preview"),
            inlay_hint: id("inlay.hint"),
            completion_annotation_kind: id("completion.annotation.kind"),
            completion_annotation_doc: id("completion.annotation.doc"),
            completion_annotation_keybinding: id("completion.annotation.keybinding"),
            completion_annotation_source: id("completion.annotation.source"),
            completion_annotation_custom: id("completion.annotation.custom"),
            completion_annotation_perm_type: id("completion.annotation.perm.type"),
            completion_annotation_perm_read: id("completion.annotation.perm.read"),
            completion_annotation_perm_write: id("completion.annotation.perm.write"),
            completion_annotation_perm_exec: id("completion.annotation.perm.exec"),
            completion_annotation_perm_special: id("completion.annotation.perm.special"),
            completion_annotation_perm_none: id("completion.annotation.perm.none"),
            completion_annotation_size: id("completion.annotation.size"),
            completion_annotation_mtime: id("completion.annotation.mtime"),
            completion_annotation_location_path: id("completion.annotation.location.path"),
            completion_annotation_location_line: id("completion.annotation.location.line"),
            completion_annotation_location_col: id("completion.annotation.location.col"),
            completion_annotation_status_dirty: id("completion.annotation.status.dirty"),
            completion_annotation_status_active: id("completion.annotation.status.active"),
            completion_annotation_latency_reflex: id("completion.annotation.latency.reflex"),
            completion_annotation_latency_display: id("completion.annotation.latency.display"),
            completion_annotation_latency_background: id(
                "completion.annotation.latency.background",
            ),
            completion_annotation_args: id("completion.annotation.args"),
            completion_annotation_buffer_id: id("completion.annotation.buffer-id"),
            completion_annotation_register: id("completion.annotation.register"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;

    fn reg() -> InMemoryThemeRegistry {
        InMemoryThemeRegistry::with_defaults()
    }

    fn resolved_of(reg: &InMemoryThemeRegistry, name: &'static str) -> Style {
        let id = reg
            .id(&ElementName::from_static(name))
            .unwrap_or_else(|| panic!("element `{name}` not registered"));
        reg.resolved().get(id)
    }

    #[test]
    fn resolved_builtins_match_legacy_literals() {
        let reg = reg();
        // Chrome — palette accent keys (migrated off `ansi.*` so each theme's
        // tuned accent applies and GPUI gets readable truecolor instead of the
        // dim VGA `Color::Named` approximations; `ansi.*` collapsed to the same
        // `0x0000ee`/`0x7f7f7f` regardless of theme). Default palette = mocha.
        let rgb = Color::Rgb;
        assert_eq!(
            resolved_of(&reg, "pane.status.active"),
            Style::empty().reverse().bold()
        );
        assert_eq!(
            resolved_of(&reg, "pane.status.inactive"),
            Style::empty().fg(rgb(0x6c, 0x70, 0x86)).dim() // overlay
        );
        assert_eq!(
            resolved_of(&reg, "diagnostic.error"),
            Style::empty().fg(rgb(0xf3, 0x8b, 0xa8)).bold() // red
        );
        assert_eq!(
            resolved_of(&reg, "diagnostic.info"),
            Style::empty().fg(rgb(0x89, 0xb4, 0xfa)) // blue
        );
        assert_eq!(
            resolved_of(&reg, "file_tree.dir"),
            Style::empty().fg(rgb(0x89, 0xb4, 0xfa)).bold() // blue
        );
        assert_eq!(resolved_of(&reg, "file_tree.file"), Style::empty());
        assert_eq!(
            resolved_of(&reg, "diff.add.sign"),
            Style::empty().fg(rgb(0xa6, 0xe3, 0xa1)).bold() // green
        );
        assert_eq!(
            resolved_of(&reg, "diff.conflict.sign"),
            Style::empty().fg(rgb(0xcb, 0xa6, 0xf7)).bold() // purple
        );
        // Tints — exact RGB / indexed.
        assert_eq!(
            resolved_of(&reg, "editor.cursor_line"),
            Style::empty().bg(Color::Indexed(236))
        );
        assert_eq!(
            resolved_of(&reg, "diff.add.line"),
            Style::empty().bg(Color::Rgb(0, 50, 0))
        );
        assert_eq!(
            resolved_of(&reg, "diff.deletion_block"),
            Style::empty().bg(Color::Rgb(60, 0, 0))
        );
        // Syntax — exact Catppuccin RGB + modifiers.
        assert_eq!(
            resolved_of(&reg, "syntax.keyword"),
            Style::empty().fg(Color::Rgb(0xcb, 0xa6, 0xf7)).bold()
        );
        assert_eq!(
            resolved_of(&reg, "syntax.comment"),
            Style::empty().fg(Color::Rgb(0x6c, 0x70, 0x86)).italic()
        );
        // T.10: heading.1 carries the rich `ExtraBold` weight; F.3 adds
        // the rich `scale` (1.6× = FontScale(160)).
        assert_eq!(
            resolved_of(&reg, "syntax.heading.1"),
            Style::empty()
                .fg(Color::Rgb(0xf3, 0x8b, 0xa8))
                .bold()
                .underline()
                .weight(Weight::ExtraBold)
                .scale(FontScale::from_ratio(1.6))
        );
        assert_eq!(
            resolved_of(&reg, "syntax.url"),
            Style::empty().fg(Color::Rgb(0x74, 0xc7, 0xec)).underline()
        );
    }

    /// ML.1b: the default theme resolves the modeline elements to their
    /// palette-driven colours (Catppuccin Mocha: blue/text/subtext/teal
    /// + surface1/surface0/overlay).
    #[test]
    fn resolved_modeline_elements_are_palette_driven() {
        let reg = reg();
        assert_eq!(
            resolved_of(&reg, "modeline.active"),
            Style::empty().bg(Color::Rgb(0x45, 0x47, 0x5a)) // surface1
        );
        assert_eq!(
            resolved_of(&reg, "modeline.inactive"),
            Style::empty()
                .bg(Color::Rgb(0x31, 0x32, 0x44)) // surface0
                .fg(Color::Rgb(0x6c, 0x70, 0x86)) // overlay
        );
        assert_eq!(
            resolved_of(&reg, "modeline.mode"),
            Style::empty().fg(Color::Rgb(0x89, 0xb4, 0xfa)).bold() // blue
        );
        assert_eq!(
            resolved_of(&reg, "modeline.path"),
            Style::empty().fg(Color::Rgb(0xcd, 0xd6, 0xf4)) // text
        );
        assert_eq!(
            resolved_of(&reg, "modeline.lang"),
            Style::empty().fg(Color::Rgb(0x94, 0xe2, 0xd5)) // teal
        );
    }

    /// ML.1b: EVERY builtin theme must resolve the modeline elements to a
    /// themed style (fg/bg set, no INVALID fallback). This is the
    /// "20 themes appropriately" guarantee — the elements are
    /// palette-keyed, so each theme's palette supplies its own colours.
    #[test]
    fn every_builtin_theme_themes_the_modeline() {
        for theme in crate::themes::builtin_themes() {
            let reg = reg();
            assert!(
                reg.apply_theme(theme.name),
                "theme `{}` should be registered",
                theme.name
            );
            let resolved = reg.resolved();
            let style = |name: &'static str| {
                let id = reg
                    .id(&ElementName::from_static(name))
                    .expect("modeline element registered");
                resolved.get(id)
            };
            // Bars carry a background; per-role segments carry a foreground.
            assert!(
                style("modeline.active").bg.is_some(),
                "theme `{}`: modeline.active needs a bar bg",
                theme.name
            );
            assert!(
                style("modeline.inactive").bg.is_some() && style("modeline.inactive").fg.is_some(),
                "theme `{}`: modeline.inactive needs a muted bar",
                theme.name
            );
            for role in [
                "modeline.mode",
                "modeline.path",
                "modeline.position",
                "modeline.lang",
                "modeline.mode_item",
            ] {
                assert!(
                    style(role).fg.is_some(),
                    "theme `{}`: {role} needs a themed fg",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn builtin_ids_capture_resolves_diagnostics_to_legacy() {
        // T.4.a parity net (shared by BOTH renderers — each reads
        // `resolved.get(ids.diagnostic_*)`): capture finds the ids and
        // the resolved styles equal the legacy `Theme::default()`
        // diagnostic literals byte-for-byte.
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        // MR.2: the per-segment marginalia slots resolve to eza-convention
        // defaults, and `annotation_slot` maps slot keys to them.
        assert_ne!(ids.completion_annotation_perm_type, ElementId::INVALID);
        assert_eq!(
            resolved.get(ids.completion_annotation_perm_write).fg,
            Some(Color::Rgb(0xf3, 0x8b, 0xa8)) // red
        );
        assert_eq!(
            resolved.get(ids.completion_annotation_perm_exec).fg,
            Some(Color::Rgb(0xa6, 0xe3, 0xa1)) // green
        );
        assert_eq!(
            ids.annotation_slot("completion.annotation.perm.exec"),
            ids.completion_annotation_perm_exec,
            "annotation_slot maps the perm.exec key"
        );
        assert_eq!(
            ids.annotation_slot("completion.annotation.size"),
            ids.completion_annotation_size,
        );
        assert_eq!(
            ids.annotation_slot("totally.unknown.slot"),
            ids.completion_annotation_custom,
            "unknown slots fall back to the custom annotation id"
        );
        assert_ne!(ids.diagnostic_error, ElementId::INVALID);
        assert_ne!(ids.diagnostic_warning, ElementId::INVALID);
        assert_ne!(ids.diagnostic_info, ElementId::INVALID);
        assert_ne!(ids.diagnostic_hint, ElementId::INVALID);
        assert_eq!(
            resolved.get(ids.diagnostic_error),
            Style::empty().fg(Color::Rgb(0xf3, 0x8b, 0xa8)).bold()
        );
        assert_eq!(
            resolved.get(ids.diagnostic_warning),
            Style::empty().fg(Color::Rgb(0xf9, 0xe2, 0xaf)).bold()
        );
        assert_eq!(
            resolved.get(ids.diagnostic_info),
            Style::empty().fg(Color::Rgb(0x89, 0xb4, 0xfa))
        );
        assert_eq!(
            resolved.get(ids.diagnostic_hint),
            Style::empty().fg(Color::Rgb(0x6c, 0x70, 0x86)).dim()
        );
    }

    #[test]
    fn builtin_ids_capture_resolves_picker_rollout_slots() {
        // MP.1 (shared by BOTH renderers — each resolves a `Styled`
        // segment via `ids.annotation_slot(slot)`): the picker-rollout
        // slots intern, `annotation_slot` maps every key, and each family
        // resolves to its intended palette token (asserted against a
        // sibling slot that shares the token, so no hardcoded hex).
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();

        for (key, id) in [
            (
                "completion.annotation.location.path",
                ids.completion_annotation_location_path,
            ),
            (
                "completion.annotation.location.line",
                ids.completion_annotation_location_line,
            ),
            (
                "completion.annotation.location.col",
                ids.completion_annotation_location_col,
            ),
            (
                "completion.annotation.status.dirty",
                ids.completion_annotation_status_dirty,
            ),
            (
                "completion.annotation.status.active",
                ids.completion_annotation_status_active,
            ),
            (
                "completion.annotation.latency.reflex",
                ids.completion_annotation_latency_reflex,
            ),
            (
                "completion.annotation.latency.display",
                ids.completion_annotation_latency_display,
            ),
            (
                "completion.annotation.latency.background",
                ids.completion_annotation_latency_background,
            ),
            ("completion.annotation.args", ids.completion_annotation_args),
            (
                "completion.annotation.buffer-id",
                ids.completion_annotation_buffer_id,
            ),
            (
                "completion.annotation.register",
                ids.completion_annotation_register,
            ),
        ] {
            assert_ne!(id, ElementId::INVALID, "`{key}` should intern");
            assert_eq!(ids.annotation_slot(key), id, "annotation_slot maps `{key}`");
        }

        // Token parity (against sibling slots sharing the same palette token):
        let fg = |id| resolved.get(id).fg;
        assert_eq!(
            fg(ids.completion_annotation_location_line),
            fg(ids.completion_annotation_keybinding), // yellow
        );
        assert_eq!(
            fg(ids.completion_annotation_status_dirty),
            fg(ids.completion_annotation_perm_write), // red
        );
        assert_eq!(
            fg(ids.completion_annotation_latency_reflex),
            fg(ids.completion_annotation_perm_exec), // green
        );
        assert_eq!(
            fg(ids.completion_annotation_latency_display),
            fg(ids.completion_annotation_perm_type), // blue
        );
        assert_eq!(
            fg(ids.completion_annotation_register),
            fg(ids.completion_annotation_source), // purple
        );
        // The accent line is distinct from its dim path/col siblings.
        assert_ne!(
            fg(ids.completion_annotation_location_line),
            fg(ids.completion_annotation_location_path),
        );
    }

    #[test]
    fn builtin_ids_capture_resolves_diff_to_legacy() {
        // T.4.b parity net (shared by both renderers): diff sign
        // styles + line/block tints resolve to the legacy literals.
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        assert_eq!(
            resolved.get(ids.diff_add_sign),
            Style::empty().fg(Color::Rgb(0xa6, 0xe3, 0xa1)).bold()
        );
        assert_eq!(
            resolved.get(ids.diff_conflict_sign),
            Style::empty().fg(Color::Rgb(0xcb, 0xa6, 0xf7)).bold()
        );
        // Tints carry the legacy Rgb on the `bg` channel.
        assert_eq!(
            resolved.get(ids.diff_add_line).bg,
            Some(Color::Rgb(0, 50, 0))
        );
        assert_eq!(
            resolved.get(ids.diff_change_line).bg,
            Some(Color::Rgb(50, 50, 0))
        );
        assert_eq!(
            resolved.get(ids.diff_deletion_block).bg,
            Some(Color::Rgb(60, 0, 0))
        );
        assert_eq!(
            resolved.get(ids.diff_conflict_line).bg,
            Some(Color::Rgb(60, 0, 60))
        );
    }

    #[test]
    fn terminal_ansi_palette_resolves_to_themed_accents() {
        // The embedded terminal's 16-colour palette is sourced from
        // `terminal.ansi.*` roles → palette accents, so a `:colorscheme`
        // swap recolours the terminal and the colours are readable on dark
        // backgrounds (the old hardcoded `0xcd0000`/`0x0000ee` dim-VGA fix).
        // Default palette = mocha.
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        let fg = |i: usize| {
            resolved
                .get(ids.terminal_ansi[i])
                .fg
                .map(|c| c.to_rgb_u32(0))
        };
        // red / green / yellow / blue map to the mocha accents — NOT the
        // dim VGA ANSI values (0xcd0000 / 0x00cd00 / 0x0000ee).
        assert_eq!(fg(1), Some(0x00f3_8ba8), "ANSI red = mocha red");
        assert_eq!(fg(2), Some(0x00a6_e3a1), "ANSI green = mocha green");
        assert_eq!(
            fg(4),
            Some(0x0089_b4fa),
            "ANSI blue = mocha blue (not 0x0000ee)"
        );
        assert_eq!(fg(5), Some(0x00f5_c2e7), "ANSI magenta = pink");
        assert_eq!(fg(6), Some(0x0094_e2d5), "ANSI cyan = teal");
        // bright variants reuse the same accents; black/white brighten.
        assert_eq!(fg(9), fg(1), "bright red reuses red");
        assert_ne!(
            fg(0),
            fg(8),
            "black (surface1) and bright black (surface2) differ"
        );
    }

    #[test]
    fn terminal_ansi_palette_follows_colorscheme_swap() {
        // Swapping the palette recolours the terminal ANSI roles — proof
        // the terminal is theme-driven, not hardcoded. Gruvbox blue differs
        // from mocha blue.
        let mocha = InMemoryThemeRegistry::with_defaults();
        let ids = BuiltinElementIds::capture(&mocha);
        let mocha_blue = mocha
            .resolved()
            .get(ids.terminal_ansi[4])
            .fg
            .unwrap()
            .to_rgb_u32(0);

        let gruv = InMemoryThemeRegistry::new(crate::palette::gruvbox_dark_palette());
        register_builtins(&gruv);
        let gids = BuiltinElementIds::capture(&gruv);
        let gruv_blue = gruv
            .resolved()
            .get(gids.terminal_ansi[4])
            .fg
            .unwrap()
            .to_rgb_u32(0);

        assert_ne!(
            mocha_blue, gruv_blue,
            "terminal blue tracks the colorscheme"
        );
    }

    #[test]
    fn styled_marginalia_slots_follow_colorscheme_swap() {
        // MARG §8: the per-segment file-metadata slots are palette
        // refs, so swapping the colorscheme recolors them — the shared
        // resolution both renderer peers read (`resolved.get(
        // ids.annotation_slot(slot))`). Gruvbox red/peach differ from
        // mocha's, proving the marginalia is theme-driven, not baked.
        let mocha = InMemoryThemeRegistry::with_defaults();
        let mids = BuiltinElementIds::capture(&mocha);
        let m_write = mocha
            .resolved()
            .get(mids.annotation_slot("completion.annotation.perm.write"))
            .fg
            .unwrap()
            .to_rgb_u32(0);

        let gruv = InMemoryThemeRegistry::new(crate::palette::gruvbox_dark_palette());
        register_builtins(&gruv);
        let gids = BuiltinElementIds::capture(&gruv);
        let g_write = gruv
            .resolved()
            .get(gids.annotation_slot("completion.annotation.perm.write"))
            .fg
            .unwrap()
            .to_rgb_u32(0);

        assert_ne!(m_write, g_write, "perm.write tracks the colorscheme");
    }

    #[test]
    fn picker_rollout_slots_follow_colorscheme_swap() {
        // MARG §9: the picker-rollout slots (location/status/latency/…)
        // are palette refs too, so a colorscheme swap recolors the
        // command/buffer/grep/jumps marginalia on BOTH peers (shared
        // `resolved.get(ids.annotation_slot(slot))` resolution).
        let swap = |slot: &str| {
            let mocha = InMemoryThemeRegistry::with_defaults();
            let mids = BuiltinElementIds::capture(&mocha);
            let m = mocha
                .resolved()
                .get(mids.annotation_slot(slot))
                .fg
                .unwrap()
                .to_rgb_u32(0);
            let gruv = InMemoryThemeRegistry::new(crate::palette::gruvbox_dark_palette());
            register_builtins(&gruv);
            let gids = BuiltinElementIds::capture(&gruv);
            let g = gruv
                .resolved()
                .get(gids.annotation_slot(slot))
                .fg
                .unwrap()
                .to_rgb_u32(0);
            (m, g)
        };
        for slot in [
            "completion.annotation.location.line",
            "completion.annotation.status.dirty",
            "completion.annotation.latency.display",
            "completion.annotation.register",
        ] {
            let (m, g) = swap(slot);
            assert_ne!(m, g, "`{slot}` should track the colorscheme");
        }
    }

    #[test]
    fn builtin_ids_capture_resolves_chrome_to_legacy() {
        // Parity net: the writer-free pane/file-tree elements resolve to
        // their palette accent keys. `file_tree.dir`/`.hidden` migrated off
        // `ansi.*` to `blue`/`overlay` so each theme's tuned accent applies
        // and GPUI renders readable truecolor (the dull-blue folder fix).
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        assert_eq!(
            resolved.get(ids.pane_inactive_overlay),
            Style::empty().dim()
        );
        assert_eq!(
            resolved.get(ids.file_tree_dir),
            Style::empty().fg(Color::Rgb(0x89, 0xb4, 0xfa)).bold()
        );
        assert_eq!(
            resolved.get(ids.file_tree_hidden),
            Style::empty().fg(Color::Rgb(0x6c, 0x70, 0x86)).dim()
        );
        assert_eq!(resolved.get(ids.file_tree_file), Style::empty());
    }

    #[test]
    fn editor_canvas_resolves_to_catppuccin_mocha() {
        // T.11.0b parity net: the canvas elements (bg / fg / block
        // cursor / popup surface) resolve to the exact legacy
        // Catppuccin-Mocha literals `GpuiTheme::default()` carried, now
        // sourced from the palette's background family. A `:colorscheme`
        // / palette swap recolors them; the default stays byte-identical.
        let reg = reg();
        assert_eq!(
            resolved_of(&reg, "editor.background").bg,
            Some(Color::Rgb(0x1e, 0x1e, 0x2e))
        );
        assert_eq!(
            resolved_of(&reg, "editor.foreground").fg,
            Some(Color::Rgb(0xcd, 0xd6, 0xf4))
        );
        // Block cursor is inverted: bg = text, fg = base.
        let cursor = resolved_of(&reg, "editor.cursor");
        assert_eq!(cursor.bg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
        assert_eq!(cursor.fg, Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(
            resolved_of(&reg, "ui.popup.background").bg,
            Some(Color::Rgb(0x18, 0x18, 0x25))
        );
        // Popup header: bold blue title + muted overlay hint (shared by the
        // TUI + GPUI peers so the accent is themeable and identical).
        let title = resolved_of(&reg, "ui.popup.title");
        assert_eq!(title.fg, Some(Color::Rgb(0x89, 0xb4, 0xfa)));
        assert!(title.modifiers.bold);
        assert_eq!(
            resolved_of(&reg, "ui.popup.hint").fg,
            Some(Color::Rgb(0x6c, 0x70, 0x86))
        );
    }

    #[test]
    fn builtin_ids_capture_resolves_cursorline_and_messages_to_legacy() {
        // T.4.d parity net: current-line tint (bg) + *messages* level
        // styles resolve to the legacy literals.
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        assert_eq!(
            resolved.get(ids.editor_cursor_line).bg,
            Some(Color::Indexed(236))
        );
        assert_eq!(resolved.get(ids.messages_trace), Style::empty().dim());
        assert_eq!(
            resolved.get(ids.messages_debug),
            Style::empty().fg(Color::Rgb(0x74, 0xc7, 0xec))
        );
        assert_eq!(resolved.get(ids.messages_info), Style::empty());
        assert_eq!(
            resolved.get(ids.messages_warn),
            Style::empty().fg(Color::Rgb(0xf9, 0xe2, 0xaf)).bold()
        );
        assert_eq!(
            resolved.get(ids.messages_error),
            Style::empty().fg(Color::Rgb(0xf3, 0x8b, 0xa8)).bold()
        );
        assert_eq!(
            resolved.get(ids.messages_timestamp),
            Style::empty().fg(Color::Rgb(0x6c, 0x70, 0x86)).dim()
        );
    }

    #[test]
    fn builtin_ids_capture_resolves_syntax_and_whitespace_to_legacy() {
        // T.5 parity net (shared by the cell builder + both
        // display-line paths): syntax categories + whitespace markers
        // resolve to the legacy `Theme::syntax_style` literals.
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        assert_eq!(
            resolved.get(ids.syntax_keyword),
            Style::empty().fg(Color::Rgb(0xcb, 0xa6, 0xf7)).bold()
        );
        assert_eq!(
            resolved.get(ids.syntax_string),
            Style::empty().fg(Color::Rgb(0xa6, 0xe3, 0xa1))
        );
        // LineComment inherits Comment → identical resolved style.
        assert_eq!(
            resolved.get(ids.syntax_line_comment),
            resolved.get(ids.syntax_comment)
        );
        // T.10: heading.1 resolves with the rich `ExtraBold` weight too;
        // F.3 adds the rich `scale` (1.6×).
        assert_eq!(
            resolved.get(ids.syntax_heading_1),
            Style::empty()
                .fg(Color::Rgb(0xf3, 0x8b, 0xa8))
                .bold()
                .underline()
                .weight(Weight::ExtraBold)
                .scale(FontScale::from_ratio(1.6))
        );
        assert_eq!(
            resolved.get(ids.whitespace_trailing),
            Style::empty().fg(Color::Rgb(0xf3, 0x8b, 0xa8))
        );
        assert_eq!(
            resolved.get(ids.whitespace),
            Style::empty().fg(Color::Rgb(0x6c, 0x70, 0x86)).dim()
        );
    }

    #[test]
    fn heading_builtins_carry_rich_weight() {
        // T.10: the heading demo consumers set a rich-vocabulary
        // `weight` on top of their fg + bold. The GPUI peer reads
        // `Style::weight` per run; the TUI degrades it to bold.
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        assert_eq!(
            resolved.get(ids.syntax_heading_1).weight,
            Some(Weight::ExtraBold)
        );
        assert_eq!(
            resolved.get(ids.syntax_heading_2).weight,
            Some(Weight::Bold)
        );
        assert_eq!(
            resolved.get(ids.syntax_heading_3).weight,
            Some(Weight::Bold)
        );
        // heading.4-6 keep bold-only, no rich weight (unchanged).
        assert_eq!(resolved.get(ids.syntax_heading_4).weight, None);
        assert_eq!(resolved.get(ids.syntax_heading_5).weight, None);
        assert_eq!(resolved.get(ids.syntax_heading_6).weight, None);
        // The bold bool + fg survive alongside the weight.
        assert!(resolved.get(ids.syntax_heading_1).modifiers.bold);
        assert!(resolved.get(ids.syntax_heading_1).modifiers.underline);
    }

    #[test]
    fn heading_builtins_carry_descending_scale() {
        // F.3 (Thread F): heading levels carry a rich-vocabulary `scale`
        // descending by level (emacs `:height`). The GPUI peer honors it
        // via variable row height (F.2); the TUI degrades (no per-line
        // font size on a cell grid). Every level sets a scale > 1.0 and
        // h(n) is strictly larger than h(n+1).
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        let scales = [
            resolved.get(ids.syntax_heading_1).scale,
            resolved.get(ids.syntax_heading_2).scale,
            resolved.get(ids.syntax_heading_3).scale,
            resolved.get(ids.syntax_heading_4).scale,
            resolved.get(ids.syntax_heading_5).scale,
            resolved.get(ids.syntax_heading_6).scale,
        ];
        assert_eq!(scales[0], Some(FontScale::from_ratio(1.6)));
        assert_eq!(scales[1], Some(FontScale::from_ratio(1.4)));
        assert_eq!(scales[2], Some(FontScale::from_ratio(1.25)));
        assert_eq!(scales[3], Some(FontScale::from_ratio(1.15)));
        assert_eq!(scales[4], Some(FontScale::from_ratio(1.1)));
        assert_eq!(scales[5], Some(FontScale::from_ratio(1.05)));
        // Strictly descending and all above body (1.0×).
        for w in scales.windows(2) {
            assert!(w[0].unwrap().0 > w[1].unwrap().0);
        }
        assert!(scales[5].unwrap().0 > FontScale::ONE.0);
    }

    #[test]
    fn builtin_ids_capture_resolves_overlays_to_registered() {
        // T.6 parity net (shared by BOTH renderers): search / selection
        // / document-highlight / substitute / inlay / completion-
        // annotation elements resolve to their registered defaults, so
        // each peer reads the SAME style (closing the prior drift).
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        let resolved = reg.resolved();
        assert_ne!(ids.selection, ElementId::INVALID);
        // Search match = overlay0 bg tint (was a fg recolor in the TUI).
        assert_eq!(
            resolved.get(ids.search_match).bg,
            Some(Color::Rgb(0x6c, 0x70, 0x86))
        );
        assert_eq!(
            resolved.get(ids.search_current).bg,
            Some(Color::Rgb(0x6c, 0x5a, 0x1e))
        );
        assert_eq!(
            resolved.get(ids.selection).bg,
            Some(Color::Rgb(0x45, 0x47, 0x5a))
        );
        // Document-highlight keeps its 3 distinct kinds (both peers).
        assert_eq!(
            resolved.get(ids.doc_highlight_read).bg,
            Some(Color::Rgb(20, 50, 25))
        );
        assert_eq!(
            resolved.get(ids.doc_highlight_write).bg,
            Some(Color::Rgb(60, 20, 20))
        );
        assert_eq!(
            resolved.get(ids.doc_highlight_text).bg,
            Some(Color::Rgb(20, 30, 60))
        );
        assert_eq!(
            resolved.get(ids.substitute_preview).bg,
            Some(Color::Rgb(0xf3, 0x8b, 0xa8))
        );
        assert_eq!(
            resolved.get(ids.inlay_hint).fg,
            Some(Color::Rgb(0x7f, 0x84, 0x9c))
        );
        // Completion-annotation base (unselected) colours.
        assert_eq!(
            resolved.get(ids.completion_annotation_keybinding).fg,
            Some(Color::Rgb(0xf9, 0xe2, 0xaf)) // yellow
        );
        assert_eq!(
            resolved.get(ids.completion_annotation_source).fg,
            Some(Color::Rgb(0xcb, 0xa6, 0xf7)) // mauve
        );
        assert_eq!(
            resolved.get(ids.completion_annotation_doc).fg,
            Some(Color::Rgb(0x89, 0xdc, 0xeb))
        );
    }

    #[test]
    fn builtin_ids_default_is_invalid_and_styleless() {
        // The placeholder a default `RenderState` carries: all ids
        // INVALID, every read styleless (never a panic).
        let ids = BuiltinElementIds::default();
        assert_eq!(ids.diagnostic_error, ElementId::INVALID);
        let resolved = ResolvedTheme::default();
        assert_eq!(resolved.get(ids.diagnostic_error), Style::empty());
    }

    #[test]
    fn inherit_reproduces_parent_style() {
        let reg = reg();
        // syntax.line_comment inherits syntax.comment → identical.
        assert_eq!(
            resolved_of(&reg, "syntax.line_comment"),
            resolved_of(&reg, "syntax.comment")
        );
    }

    #[test]
    fn register_is_idempotent_by_name() {
        let reg = InMemoryThemeRegistry::new(default_palette());
        let a = reg.register(
            ElementName::from_static("x.y"),
            ElementOwner::Core,
            StyleSpec::new().fg("purple"),
            "",
        );
        let b = reg.register(
            ElementName::from_static("x.y"),
            ElementOwner::Core,
            StyleSpec::new().fg("red"),
            "",
        );
        assert_eq!(a, b);
    }

    #[test]
    fn dotted_fallback_resolves_through_parent() {
        let reg = InMemoryThemeRegistry::new(default_palette());
        reg.register(
            ElementName::from_static("markdown.heading"),
            ElementOwner::Core,
            StyleSpec::new().fg("purple").bold(),
            "",
        );
        // An element whose default inherits an UNREGISTERED specific
        // name falls back through the dotted parent.
        let id = reg.register(
            ElementName::from_static("uses_fallback"),
            ElementOwner::Core,
            StyleSpec::new().inherit("markdown.heading.1"),
            "",
        );
        let s = reg.resolved().get(id);
        assert_eq!(s.fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert!(s.modifiers.bold);
    }

    #[test]
    fn unknown_palette_key_leaves_channel_unset() {
        let reg = InMemoryThemeRegistry::new(Palette::new()); // empty palette
        let id = reg.register(
            ElementName::from_static("orphan"),
            ElementOwner::Core,
            StyleSpec::new().fg("no.such.key"),
            "",
        );
        // Missing key logs + leaves fg None (no panic, no garbage).
        assert_eq!(reg.resolved().get(id).fg, None);
    }

    #[test]
    fn set_override_overlays_on_resolved_default() {
        // T.9: a theme-global override overlays its set fields on the
        // element's resolved default; unset fields keep the default.
        let reg = reg();
        let base = resolved_of(&reg, "syntax.keyword"); // mauve + bold
        assert_eq!(base.fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert!(base.modifiers.bold);
        // Override only the fg (literal); bold from the default survives.
        reg.set_override(
            ElementName::from_static("syntax.keyword"),
            StyleSpec::new().fg(Color::Rgb(1, 2, 3)),
        );
        let after = resolved_of(&reg, "syntax.keyword");
        assert_eq!(after.fg, Some(Color::Rgb(1, 2, 3)));
        assert!(
            after.modifiers.bold,
            "default bold survives a fg-only override"
        );
    }

    #[test]
    fn set_theme_swaps_palette_and_overrides() {
        // T.9: `:colorscheme` swap replaces palette + overrides
        // atomically; prior overrides are cleared.
        let reg = reg();
        reg.set_override(
            ElementName::from_static("syntax.string"),
            StyleSpec::new().fg(Color::Rgb(9, 9, 9)),
        );
        assert_eq!(
            resolved_of(&reg, "syntax.string").fg,
            Some(Color::Rgb(9, 9, 9))
        );
        // New theme: palette with a different `green`, no overrides.
        let palette = default_palette().with("green", Color::Rgb(4, 5, 6));
        reg.set_theme(palette, Vec::new());
        // string references `green` → now (4,5,6); the prior override is gone.
        assert_eq!(
            resolved_of(&reg, "syntax.string").fg,
            Some(Color::Rgb(4, 5, 6))
        );
    }

    #[test]
    fn set_theme_to_macchiato_recolors_keyword_to_macchiato_mauve() {
        // T.9.b: swapping to the Macchiato palette re-resolves
        // `syntax.keyword` (which references `mauve`) to Macchiato's
        // mauve. This is the registry half of the `:colorscheme` swap.
        let reg = reg();
        let ids = BuiltinElementIds::capture(&reg);
        // Before: mocha mauve.
        assert_eq!(
            reg.resolved().get(ids.syntax_keyword).fg,
            Some(Color::Rgb(0xcb, 0xa6, 0xf7))
        );
        reg.set_theme(crate::macchiato_palette(), Vec::new());
        assert_eq!(
            reg.resolved().get(ids.syntax_keyword).fg,
            Some(Color::Rgb(0xc6, 0xa0, 0xf6)),
            "keyword resolves to Macchiato mauve after the swap"
        );
    }

    #[test]
    fn builtin_themes_lookup_by_name_returns_macchiato_palette() {
        // T.9.b: the named-theme lookup `:colorscheme` performs resolves
        // `catppuccin-macchiato` to the Macchiato palette.
        let theme = crate::builtin_themes()
            .into_iter()
            .find(|t| t.name == "catppuccin-macchiato")
            .expect("macchiato registered");
        assert_eq!(
            theme.palette.get(&crate::PaletteKey::from_static("purple")),
            Some(Color::Rgb(0xc6, 0xa0, 0xf6))
        );
        assert!(theme.overrides.is_empty());
    }

    #[test]
    fn theme_catalog_register_enumerate_and_apply() {
        // T.11.1: the named-theme catalog — builtins seeded at boot,
        // listed by `theme_names`, swapped by `apply_theme`;
        // `register_theme` adds a custom theme (the init.rs / plugin
        // seam). `apply_theme` recolours the canvas elements (T.11.0b).
        let reg = reg();
        let names = reg.theme_names();
        assert!(names.iter().any(|n| n == "catppuccin-mocha"));
        assert!(names.iter().any(|n| n == "catppuccin-latte"));

        // Swap to the registered LIGHT theme → canvas resolves light.
        assert!(reg.apply_theme("catppuccin-latte"));
        assert_eq!(
            resolved_of(&reg, "editor.background").bg,
            Some(Color::Rgb(0xef, 0xf1, 0xf5))
        );
        // Unknown name → false, active theme untouched (still Latte).
        assert!(!reg.apply_theme("no-such-theme"));
        assert_eq!(
            resolved_of(&reg, "editor.background").bg,
            Some(Color::Rgb(0xef, 0xf1, 0xf5))
        );

        // register_theme adds a theme that apply_theme can then swap to.
        reg.register_theme(NamedTheme {
            name: "test-custom",
            palette: crate::palette::default_palette(),
            overrides: Vec::new(),
        });
        assert!(reg.theme_names().iter().any(|n| n == "test-custom"));
        assert!(reg.apply_theme("test-custom"));
        assert_eq!(
            resolved_of(&reg, "editor.background").bg,
            Some(Color::Rgb(0x1e, 0x1e, 0x2e))
        );
    }

    #[test]
    fn describe_returns_metadata_and_resolved_style() {
        // T.9.d: `describe` bundles owner + doc + authoring default +
        // concrete resolved style for a registered element.
        let reg = reg();
        let info = reg
            .describe(&ElementName::from_static("syntax.keyword"))
            .expect("syntax.keyword registered");
        assert_eq!(info.name.as_str(), "syntax.keyword");
        assert_eq!(info.owner, ElementOwner::Core);
        assert_eq!(info.doc, "Language keywords.");
        // Authoring default references the `mauve` palette key + bold.
        assert_eq!(
            info.default.fg,
            Some(ColorRef::Palette(crate::PaletteKey::from_static("purple")))
        );
        assert_eq!(info.default.modifiers.bold, Some(true));
        // Resolved style is the concrete mocha mauve + bold.
        assert_eq!(info.resolved.fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert!(info.resolved.modifiers.bold);
    }

    #[test]
    fn describe_unknown_element_is_none() {
        let reg = reg();
        assert!(
            reg.describe(&ElementName::from_static("no.such.element"))
                .is_none()
        );
    }

    #[test]
    fn describe_reflects_active_override() {
        // T.9.d: the `resolved` field tracks the active override set —
        // after `set_override` the described resolved style updates,
        // while the authoring `default` (the reference form) is
        // unchanged.
        let reg = reg();
        reg.set_override(
            ElementName::from_static("syntax.keyword"),
            StyleSpec::new().fg(Color::Rgb(1, 2, 3)),
        );
        let info = reg
            .describe(&ElementName::from_static("syntax.keyword"))
            .expect("registered");
        assert_eq!(info.resolved.fg, Some(Color::Rgb(1, 2, 3)));
        // The override only set fg; the default's bold still resolves.
        assert!(info.resolved.modifiers.bold);
        // The authoring default still references the palette key.
        assert_eq!(
            info.default.fg,
            Some(ColorRef::Palette(crate::PaletteKey::from_static("purple")))
        );
    }

    #[test]
    fn describe_reports_inherit_parent() {
        // T.9.d: an element whose default inherits another surfaces the
        // parent in `default.inherit` so the help view can show it.
        let reg = reg();
        let info = reg
            .describe(&ElementName::from_static("syntax.line_comment"))
            .expect("registered");
        assert_eq!(
            info.default
                .inherit
                .as_ref()
                .map(|n| n.as_str().to_string()),
            Some("syntax.comment".to_string())
        );
    }

    #[test]
    fn palette_swap_rebuilds_resolved_table() {
        let reg = reg();
        let before = resolved_of(&reg, "syntax.keyword");
        assert_eq!(before.fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        // Swap "purple" to a different color; keyword re-colors.
        let new_palette = default_palette().with("purple", Color::Rgb(1, 2, 3));
        reg.set_palette(new_palette);
        let after = resolved_of(&reg, "syntax.keyword");
        assert_eq!(after.fg, Some(Color::Rgb(1, 2, 3)));
        assert!(reg.resolved().version() > 0);
    }
}
