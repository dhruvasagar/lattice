//! Theme-element identity + the authoring (reference) style form.
//!
//! A *theme element* is a named, semantic styleable role
//! (`syntax.keyword`, `diff.add.sign`, `markdown.heading.1`). It
//! carries no color — only identity, an owner, and a default
//! [`StyleSpec`]. A `StyleSpec` describes how an element is styled
//! *by reference*: colors name a [`PaletteKey`](crate::PaletteKey)
//! rather than baking an absolute RGB, and a spec may `inherit`
//! another element and override specific attributes (emacs
//! `:inherit`, vim `:hi link`).
//!
//! Resolution (`crate::registry`) turns a `StyleSpec` into a
//! concrete [`Style`](crate::Style) once at theme-build time.
//!
//! Design: `docs/dev/architecture/theme-system.md` §3.1–§3.2.

use std::borrow::Cow;

use crate::{Color, FamilyId, Weight};

/// Interned, process-stable index for a registered theme element.
/// Allocated at registration; the hot-path read is
/// `resolved.get(id)` — an array index, no string hashing.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementId(pub u32);

impl ElementId {
    /// Out-of-range sentinel. `ResolvedTheme::get(INVALID)` returns
    /// `Style::empty()` (out-of-range ids are styleless, never a
    /// panic), so it is the safe default + not-found fallback for the
    /// id-capture helpers ([`crate::BuiltinElementIds`]).
    pub const INVALID: ElementId = ElementId(u32::MAX);
}

/// A theme element's dotted, hierarchical name (`markdown.heading.1`).
/// Fallback walks the dotted parents (`markdown.heading.1` →
/// `markdown.heading` → `markdown`) when the more-specific element is
/// unstyled by the active theme.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ElementName(Cow<'static, str>);

impl ElementName {
    /// Construct from a `&'static str` (the builtin path).
    pub const fn from_static(s: &'static str) -> Self {
        ElementName(Cow::Borrowed(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The dotted parent, if any: `"a.b.c"` → `"a.b"`, `"a"` → `None`.
    /// Used for hierarchical fallback at resolution time.
    pub fn parent(&self) -> Option<ElementName> {
        self.0
            .rsplit_once('.')
            .map(|(head, _)| ElementName(Cow::Owned(head.to_string())))
    }
}

impl From<&'static str> for ElementName {
    fn from(s: &'static str) -> Self {
        ElementName::from_static(s)
    }
}

impl From<String> for ElementName {
    fn from(s: String) -> Self {
        ElementName(Cow::Owned(s))
    }
}

impl std::fmt::Display for ElementName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who owns an element (and thus owns its default styling). Core
/// elements ship with the editor; modes/plugins register their own.
///
/// The mode/plugin id is a string rather than a `ModeId` /
/// `PluginId` because `lattice-theme` is a leaf crate — it cannot
/// depend on `lattice-mode`. Callers pass `mode.id().as_str()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ElementOwner {
    Core,
    Mode(Cow<'static, str>),
    Plugin(Cow<'static, str>),
}

/// A color by reference. `Palette` is the normal path; `Literal` is
/// the escape hatch for a one-off a palette entry would
/// over-generalize; `Default` means the terminal/window default
/// channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ColorRef {
    Palette(crate::PaletteKey),
    Literal(Color),
    Default,
}

impl From<crate::PaletteKey> for ColorRef {
    fn from(k: crate::PaletteKey) -> Self {
        ColorRef::Palette(k)
    }
}

/// A bare string is the common case: a palette-key reference. So
/// `spec.fg("mauve")` means "the palette's `mauve`", not a literal —
/// reference-not-absolute by default (design §2). Use
/// `ColorRef::Literal(..)` explicitly for a one-off color.
impl From<&'static str> for ColorRef {
    fn from(s: &'static str) -> Self {
        ColorRef::Palette(crate::PaletteKey::from_static(s))
    }
}

impl From<Color> for ColorRef {
    fn from(c: Color) -> Self {
        ColorRef::Literal(c)
    }
}

/// Tri-state modifier set: `Some(true)` sets, `Some(false)` clears,
/// `None` inherits. Inheritance can therefore *clear* a parent's
/// bold, not only add — emacs faces distinguish "unspecified" from
/// "off".
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModifierSet {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
    pub reverse: Option<bool>,
}

/// How an element is styled, by reference. The form a mode default,
/// a theme override, or a buffer-local remap is written in.
/// Resolution (`crate::registry`) produces a concrete
/// [`Style`](crate::Style).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StyleSpec {
    /// Inherit another element's resolved style; this spec's set
    /// fields override. Resolved by walking the chain at build time.
    pub inherit: Option<ElementName>,
    pub fg: Option<ColorRef>,
    pub bg: Option<ColorRef>,
    pub modifiers: ModifierSet,
    /// Relative height ratio (emacs `:height` float). Resolution
    /// quantizes to fixed-point [`FontScale`](crate::FontScale).
    pub scale: Option<f32>,
    pub family: Option<FamilyId>,
    pub weight: Option<Weight>,
}

impl StyleSpec {
    /// An empty spec — resolves to `Style::empty()` (or the inherit
    /// base) with no overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inherit another element's resolved style.
    pub fn inherit(mut self, name: impl Into<ElementName>) -> Self {
        self.inherit = Some(name.into());
        self
    }

    /// Set the foreground by reference (palette key, literal, or
    /// default). Accepts a `PaletteKey`, a `Color`, or a `ColorRef`.
    pub fn fg(mut self, fg: impl Into<ColorRef>) -> Self {
        self.fg = Some(fg.into());
        self
    }

    /// Set the background by reference.
    pub fn bg(mut self, bg: impl Into<ColorRef>) -> Self {
        self.bg = Some(bg.into());
        self
    }

    pub fn bold(mut self) -> Self {
        self.modifiers.bold = Some(true);
        self
    }

    pub fn italic(mut self) -> Self {
        self.modifiers.italic = Some(true);
        self
    }

    pub fn underline(mut self) -> Self {
        self.modifiers.underline = Some(true);
        self
    }

    pub fn dim(mut self) -> Self {
        self.modifiers.dim = Some(true);
        self
    }

    pub fn reverse(mut self) -> Self {
        self.modifiers.reverse = Some(true);
        self
    }

    /// Explicitly clear a modifier the inherit base set.
    pub fn no_bold(mut self) -> Self {
        self.modifiers.bold = Some(false);
        self
    }

    /// Set the relative height ratio (rich vocabulary).
    pub fn scale(mut self, ratio: f32) -> Self {
        self.scale = Some(ratio);
        self
    }

    pub fn family(mut self, family: FamilyId) -> Self {
        self.family = Some(family);
        self
    }

    pub fn weight(mut self, weight: Weight) -> Self {
        self.weight = Some(weight);
        self
    }
}

/// A registered theme element: identity + metadata + the
/// owner-supplied default (itself a reference-form [`StyleSpec`]).
#[derive(Debug, Clone)]
pub struct ThemeElement {
    pub id: ElementId,
    pub name: ElementName,
    pub owner: ElementOwner,
    pub default: StyleSpec,
    /// Self-documenting help (`:describe-element`, design §8).
    pub doc: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_name_parent_walks_dotted_segments() {
        let n = ElementName::from_static("markdown.heading.1");
        let p = n.parent().expect("has parent");
        assert_eq!(p.as_str(), "markdown.heading");
        let pp = p.parent().expect("has grandparent");
        assert_eq!(pp.as_str(), "markdown");
        assert_eq!(pp.parent(), None);
    }

    #[test]
    fn color_ref_from_conversions() {
        assert_eq!(
            ColorRef::from(Color::Rgb(1, 2, 3)),
            ColorRef::Literal(Color::Rgb(1, 2, 3))
        );
    }

    #[test]
    fn modifier_set_defaults_to_all_inherit() {
        let m = ModifierSet::default();
        assert_eq!(m.bold, None);
        assert_eq!(m.reverse, None);
    }

    #[test]
    fn style_spec_builder_sets_tri_state_modifiers() {
        let s = StyleSpec::new().bold().no_bold();
        assert_eq!(s.modifiers.bold, Some(false));
        let s2 = StyleSpec::new().italic();
        assert_eq!(s2.modifiers.italic, Some(true));
        assert_eq!(s2.modifiers.bold, None);
    }
}
