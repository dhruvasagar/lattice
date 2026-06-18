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
use crate::palette::{default_palette, Palette};
use crate::{Color, FontScale, Style};

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
}

/// The canonical handle type. Register and look up under THIS type in
/// the ServiceRegistry ([[feedback_servicesregistry_arc_typeid]]).
pub type ThemeRegistryHandle = Arc<dyn ThemeRegistry>;

struct RegistryInner {
    /// Indexed by `ElementId.0`.
    elements: Vec<ThemeElement>,
    by_name: HashMap<ElementName, ElementId>,
    palette: Palette,
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
        let dirty = self.inner.read().expect("theme registry lock poisoned").dirty;
        if dirty {
            self.rebuild();
        }
        self.resolved.load_full()
    }
}

// ---- resolution ----

fn resolve_element(inner: &RegistryInner, id: ElementId, depth: u8) -> Style {
    match inner.elements.get(id.0 as usize) {
        Some(elem) => resolve_spec(inner, &elem.default, depth),
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

    style
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
        spec().fg("ansi.darkgray").dim(),
        "Inactive pane's status line.",
    );
    reg_one(
        "pane.inactive_overlay",
        spec().dim(),
        "Dim overlay on inactive panes' content.",
    );
    reg_one(
        "pane.separator",
        spec().fg("ansi.darkgray"),
        "Split separator between panes.",
    );

    // ---- File tree ----
    reg_one(
        "file_tree.dir",
        spec().fg("ansi.blue").bold(),
        "Directory entries in the file tree.",
    );
    reg_one(
        "file_tree.hidden",
        spec().fg("ansi.darkgray").dim(),
        "Hidden entries in the file tree.",
    );
    reg_one("file_tree.file", spec(), "Regular file entries.");

    // ---- Diagnostics ----
    reg_one(
        "diagnostic.error",
        spec().fg("ansi.red").bold(),
        "Error-severity diagnostic sign + text.",
    );
    reg_one(
        "diagnostic.warning",
        spec().fg("ansi.yellow").bold(),
        "Warning-severity diagnostic.",
    );
    reg_one(
        "diagnostic.info",
        spec().fg("ansi.blue"),
        "Info-severity diagnostic.",
    );
    reg_one(
        "diagnostic.hint",
        spec().fg("ansi.darkgray").dim(),
        "Hint-severity diagnostic.",
    );

    // ---- Whitespace + current line ----
    reg_one(
        "whitespace",
        spec().fg("ansi.darkgray").dim(),
        "Rendered whitespace markers.",
    );
    reg_one(
        "whitespace.trailing",
        spec().fg("ansi.red"),
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
        spec().fg("ansi.darkgray").dim(),
        "Timestamp column in *messages*.",
    );
    reg_one("messages.trace", spec().dim(), "TRACE-level message.");
    reg_one(
        "messages.debug",
        spec().fg("ansi.cyan"),
        "DEBUG-level message.",
    );
    reg_one("messages.info", spec(), "INFO-level message.");
    reg_one(
        "messages.warn",
        spec().fg("ansi.yellow").bold(),
        "WARN-level message.",
    );
    reg_one(
        "messages.error",
        spec().fg("ansi.red").bold(),
        "ERROR-level message.",
    );

    // ---- Diff ----
    reg_one(
        "diff.add.sign",
        spec().fg("ansi.green").bold(),
        "`+` gutter sign (added line).",
    );
    reg_one(
        "diff.change.sign",
        spec().fg("ansi.yellow").bold(),
        "`~` gutter sign (changed line).",
    );
    reg_one(
        "diff.remove.sign",
        spec().fg("ansi.red").bold(),
        "`-` gutter sign (removed line).",
    );
    reg_one(
        "diff.conflict.sign",
        spec().fg("ansi.magenta").bold(),
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
        "diff.deletion_block",
        spec().bg("diff.deletion.bg"),
        "Deletion-block virtual-row background tint.",
    );
    reg_one(
        "diff.conflict.line",
        spec().bg("diff.conflict.bg"),
        "Conflict-region background tint.",
    );

    // ---- Syntax (mirrors host `Theme::syntax_style`) ----
    reg_one("syntax.default", spec().fg("text"), "Default foreground text.");
    reg_one(
        "syntax.comment",
        spec().fg("overlay0").italic(),
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
        spec().fg("mauve").bold(),
        "Language keywords.",
    );
    reg_one("syntax.type", spec().fg("yellow"), "Type names.");
    reg_one("syntax.number", spec().fg("peach"), "Numeric literals.");
    reg_one("syntax.function", spec().fg("blue"), "Function names.");
    reg_one("syntax.constant", spec().fg("peach"), "Constants.");
    reg_one("syntax.variable", spec().fg("text"), "Variables.");
    reg_one("syntax.operator", spec().fg("teal"), "Operators.");
    reg_one(
        "syntax.punctuation",
        spec().fg("overlay2"),
        "Punctuation / delimiters.",
    );
    reg_one("syntax.attribute", spec().fg("red"), "Attributes / annotations.");
    reg_one(
        "syntax.heading.1",
        spec().fg("red").bold().underline(),
        "Markup heading level 1.",
    );
    reg_one(
        "syntax.heading.2",
        spec().fg("peach").bold(),
        "Markup heading level 2.",
    );
    reg_one(
        "syntax.heading.3",
        spec().fg("yellow").bold(),
        "Markup heading level 3.",
    );
    reg_one(
        "syntax.heading.4",
        spec().fg("green").bold(),
        "Markup heading level 4.",
    );
    reg_one(
        "syntax.heading.5",
        spec().fg("blue").bold(),
        "Markup heading level 5.",
    );
    reg_one(
        "syntax.heading.6",
        spec().fg("mauve").bold(),
        "Markup heading level 6.",
    );
    reg_one("syntax.bold", spec().fg("maroon").bold(), "Strong / bold markup.");
    reg_one("syntax.italic", spec().fg("pink").italic(), "Emphasis / italic markup.");
    reg_one("syntax.link", spec().fg("blue").underline(), "Markup links.");
    reg_one("syntax.url", spec().fg("sapphire").underline(), "Bare URLs.");
    reg_one(
        "syntax.markup_raw",
        spec().fg("overlay0").dim(),
        "Inline code / raw markup.",
    );
    reg_one(
        "syntax.markup",
        spec().fg("overlay2").bold(),
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
        }
    }
}

impl BuiltinElementIds {
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
            diagnostic_error: id("diagnostic.error"),
            diagnostic_warning: id("diagnostic.warning"),
            diagnostic_info: id("diagnostic.info"),
            diagnostic_hint: id("diagnostic.hint"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Color, NamedColor};

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
        // Chrome — ANSI named, matches Theme::default() exactly.
        assert_eq!(
            resolved_of(&reg, "pane.status.active"),
            Style::empty().reverse().bold()
        );
        assert_eq!(
            resolved_of(&reg, "pane.status.inactive"),
            Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim()
        );
        assert_eq!(
            resolved_of(&reg, "diagnostic.error"),
            Style::empty().fg(Color::Named(NamedColor::Red)).bold()
        );
        assert_eq!(
            resolved_of(&reg, "diagnostic.info"),
            Style::empty().fg(Color::Named(NamedColor::Blue))
        );
        assert_eq!(
            resolved_of(&reg, "file_tree.dir"),
            Style::empty().fg(Color::Named(NamedColor::Blue)).bold()
        );
        assert_eq!(resolved_of(&reg, "file_tree.file"), Style::empty());
        assert_eq!(
            resolved_of(&reg, "diff.add.sign"),
            Style::empty().fg(Color::Named(NamedColor::Green)).bold()
        );
        assert_eq!(
            resolved_of(&reg, "diff.conflict.sign"),
            Style::empty().fg(Color::Named(NamedColor::Magenta)).bold()
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
        assert_eq!(
            resolved_of(&reg, "syntax.heading.1"),
            Style::empty()
                .fg(Color::Rgb(0xf3, 0x8b, 0xa8))
                .bold()
                .underline()
        );
        assert_eq!(
            resolved_of(&reg, "syntax.url"),
            Style::empty().fg(Color::Rgb(0x74, 0xc7, 0xec)).underline()
        );
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
        assert_ne!(ids.diagnostic_error, ElementId::INVALID);
        assert_ne!(ids.diagnostic_warning, ElementId::INVALID);
        assert_ne!(ids.diagnostic_info, ElementId::INVALID);
        assert_ne!(ids.diagnostic_hint, ElementId::INVALID);
        assert_eq!(
            resolved.get(ids.diagnostic_error),
            Style::empty().fg(Color::Named(NamedColor::Red)).bold()
        );
        assert_eq!(
            resolved.get(ids.diagnostic_warning),
            Style::empty().fg(Color::Named(NamedColor::Yellow)).bold()
        );
        assert_eq!(
            resolved.get(ids.diagnostic_info),
            Style::empty().fg(Color::Named(NamedColor::Blue))
        );
        assert_eq!(
            resolved.get(ids.diagnostic_hint),
            Style::empty().fg(Color::Named(NamedColor::DarkGray)).dim()
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
            StyleSpec::new().fg("mauve"),
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
            StyleSpec::new().fg("mauve").bold(),
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
    fn palette_swap_rebuilds_resolved_table() {
        let reg = reg();
        let before = resolved_of(&reg, "syntax.keyword");
        assert_eq!(before.fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        // Swap "mauve" to a different color; keyword re-colors.
        let new_palette = default_palette().with("mauve", Color::Rgb(1, 2, 3));
        reg.set_palette(new_palette);
        let after = resolved_of(&reg, "syntax.keyword");
        assert_eq!(after.fg, Some(Color::Rgb(1, 2, 3)));
        assert!(reg.resolved().version() > 0);
    }
}
