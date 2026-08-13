//! Syntax → theme-element style bridge.
//!
//! Maps a [`crate::style::Style`] syntax category to its builtin
//! `syntax.*` / `diagnostic.*` [`lattice_theme::ElementId`], and resolves
//! that to a concrete visual [`lattice_theme::Style`] via the active
//! theme's resolved table. The single source of the syntax→element
//! mapping, shared by the cell builder, both renderers' display-line
//! paths, and the diff overlay.
//!
//! **Home (DX.2, BC.6 diff extraction).** This lived in `lattice-host`
//! (`ui::theme`) while the only consumers were host-side. It moves here so
//! `lattice-diff` (the diff overlay) can reach it without the host. It
//! belongs in `lattice-syntax` rather than the `lattice-theme` leaf
//! because the bridge is inherently *syntax-aware* — it takes a
//! `lattice_syntax::Style` — so the dependency points DOWN from this
//! higher crate onto the theme leaf, keeping `lattice-theme` a minimal
//! renderer-hot-path leaf (it deps only arc-swap + tracing). Host keeps a
//! façade re-export (`lattice_host::ui::theme::resolve_syntax_style`) so
//! existing call sites are unchanged.

use crate::style::Style;
use lattice_theme::{BuiltinElementIds, ElementId, ResolvedTheme};

/// Map a [`Style`] syntax category to its builtin `syntax.*`
/// [`ElementId`]. The single source of the syntax→element mapping,
/// shared by the cell builder, both renderers' display-line paths, and
/// the diff overlay.
pub fn syntax_element_id(ids: &BuiltinElementIds, style: Style) -> ElementId {
    use crate::style::Style as S;
    match style {
        S::Default => ids.syntax_default,
        S::Comment => ids.syntax_comment,
        S::LineComment => ids.syntax_line_comment,
        S::String => ids.syntax_string,
        S::Keyword => ids.syntax_keyword,
        S::Type => ids.syntax_type,
        S::Number => ids.syntax_number,
        S::Function => ids.syntax_function,
        S::Constant => ids.syntax_constant,
        S::Variable => ids.syntax_variable,
        S::Operator => ids.syntax_operator,
        S::Punctuation => ids.syntax_punctuation,
        S::Attribute => ids.syntax_attribute,
        S::Heading1 => ids.syntax_heading_1,
        S::Heading2 => ids.syntax_heading_2,
        S::Heading3 => ids.syntax_heading_3,
        S::Heading4 => ids.syntax_heading_4,
        S::Heading5 => ids.syntax_heading_5,
        S::Heading6 => ids.syntax_heading_6,
        S::Bold => ids.syntax_bold,
        S::Italic => ids.syntax_italic,
        S::Link => ids.syntax_link,
        S::Url => ids.syntax_url,
        S::MarkupRaw => ids.syntax_markup_raw,
        S::Markup => ids.syntax_markup,
        // L4b: diagnostic-severity styles (the `gl` popup) reuse the
        // gutter/underline severity element colours.
        S::DiagnosticError => ids.diagnostic_error,
        S::DiagnosticWarning => ids.diagnostic_warning,
        S::DiagnosticInfo => ids.diagnostic_info,
        S::DiagnosticHint => ids.diagnostic_hint,
        S::DiffAdd => ids.diff_add_text,
        S::DiffRemove => ids.diff_remove_text,
        S::MagitSha => ids.magit_sha,
        S::MagitBranchCurrent => ids.magit_branch_current,
        S::MagitRefDecoration => ids.magit_ref_decoration,
        S::MagitRebaseVerb => ids.magit_rebase_verb,
        S::MagitAuthor => ids.magit_author,
        // HP.2: help's inline literals. Both renderers resolve through
        // this one function, so classifying here is all either needs —
        // there is no per-renderer match arm to keep in step.
        S::HelpKey => ids.help_key,
        S::HelpCommand => ids.help_command,
        S::HelpAction => ids.help_action,
        S::HelpLiteral => ids.help_literal,
        // DL.1: already an element id — a mode- or plugin-registered
        // one that has no builtin slot, which is the whole point.
        // Resolution downstream is identical to every arm above.
        S::Element(id) => id,
    }
}

/// Resolve a syntax category to its concrete [`lattice_theme::Style`] via
/// the resolved table. The replacement for the deleted
/// `Theme::syntax_style` color `match` — colors now flow from the active
/// theme's palette through the resolved table. Every syntax consumer
/// (cell builder, display-line paths, diff overlay) calls this, then
/// adapts the host `Style` to its renderer-native form.
pub fn resolve_syntax_style(
    resolved: &ResolvedTheme,
    ids: &BuiltinElementIds,
    style: Style,
) -> lattice_theme::Style {
    resolved.get(syntax_element_id(ids, style))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_theme::{
        BuiltinElementIds, Color, InMemoryThemeRegistry, ResolvedTheme, ThemeRegistry,
    };

    // ---- T.5.b: SyntaxStyle resolution via the resolved table ----
    //
    // The legacy `Theme::syntax_style` color `match` was deleted in
    // T.5.b; every consumer now resolves through `resolve_syntax_style`
    // against the active theme's resolved table. These pin that the
    // default resolved table reproduces the legacy Catppuccin-Mocha
    // literals exactly (keyword = mauve + bold, comment = overlay0 +
    // italic, default = text), so the cutover is colour-identical.

    /// Build the resolved table + builtin ids from the default registry —
    /// the same construction every renderer uses at boot.
    fn defaults() -> (std::sync::Arc<ResolvedTheme>, BuiltinElementIds) {
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        (resolved, ids)
    }

    /// DL.1: a span may name a **mode-registered** element, and it
    /// resolves through exactly the same path as a builtin category.
    ///
    /// Before this, `StyledSpan` could only carry a closed `Style`
    /// variant bridged to `BuiltinElementIds` — so an element
    /// registered by a mode (or, post-1.0, by a WASM plugin, which
    /// cannot add a Rust enum variant at all) had no way to style a
    /// span. Themed highlighting outside the core vocabulary was
    /// impossible by construction.
    #[test]
    fn a_mode_registered_element_can_style_a_span() {
        use lattice_theme::{ElementOwner, StyleSpec};
        let reg = InMemoryThemeRegistry::with_defaults();
        let id = reg.register(
            "listing.file.rust".into(),
            ElementOwner::Mode("directory-listing-mode".into()),
            StyleSpec::new().fg(Color::Rgb(0xDE, 0xA5, 0x84)),
            "test element",
        );
        let ids = BuiltinElementIds::capture(&reg);

        // The bridge hands the id straight back …
        assert_eq!(syntax_element_id(&ids, Style::Element(id)), id);
        // … and resolution yields the element's own style.
        let resolved = reg.resolved();
        let s = resolve_syntax_style(&resolved, &ids, Style::Element(id));
        assert_eq!(s.fg, Some(Color::Rgb(0xDE, 0xA5, 0x84)));
    }

    /// The payload participates in the cache fingerprint: two spans
    /// naming different elements must not collide, or retuning a theme
    /// element would leave a stale matrix painted.
    #[test]
    fn element_styles_fingerprint_distinctly() {
        let reg = InMemoryThemeRegistry::with_defaults();
        let a = reg.register(
            "listing.file.rust".into(),
            lattice_theme::ElementOwner::Mode("m".into()),
            lattice_theme::StyleSpec::new(),
            "a",
        );
        let b = reg.register(
            "listing.file.python".into(),
            lattice_theme::ElementOwner::Mode("m".into()),
            lattice_theme::StyleSpec::new(),
            "b",
        );
        assert_ne!(a, b, "precondition: distinct ids");
        assert_ne!(
            Style::Element(a).fingerprint(),
            Style::Element(b).fingerprint()
        );
        assert_ne!(
            Style::Element(a).fingerprint(),
            Style::Keyword.fingerprint()
        );
        assert_eq!(
            Style::Keyword.fingerprint(),
            Style::Keyword.fingerprint(),
            "stable within a process"
        );
    }

    #[test]
    fn resolve_syntax_keyword_carries_catppuccin_mauve_bold() {
        let (resolved, ids) = defaults();
        let s = resolve_syntax_style(&resolved, &ids, Style::Keyword);
        assert_eq!(s.fg, Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert!(s.modifiers.bold);
    }

    #[test]
    fn resolve_syntax_comment_is_overlay0_italic() {
        let (resolved, ids) = defaults();
        let s = resolve_syntax_style(&resolved, &ids, Style::Comment);
        assert_eq!(s.fg, Some(Color::Rgb(0x6c, 0x70, 0x86)));
        assert!(s.modifiers.italic);
        // Same shape for LineComment.
        let line = resolve_syntax_style(&resolved, &ids, Style::LineComment);
        assert_eq!(line, s);
    }

    #[test]
    fn resolve_syntax_default_uses_text_foreground() {
        let (resolved, ids) = defaults();
        let s = resolve_syntax_style(&resolved, &ids, Style::Default);
        assert_eq!(s.fg, Some(Color::Rgb(0xcd, 0xd6, 0xf4)));
    }

    /// L4b: the diagnostic-severity styles (the `gl` popup) resolve to the
    /// SAME element colours the gutter glyph + underline use, and the four
    /// severities are distinct.
    #[test]
    fn resolve_diagnostic_styles_match_severity_elements_and_differ() {
        use crate::style::Style as S;
        let (resolved, ids) = defaults();
        let err = resolve_syntax_style(&resolved, &ids, S::DiagnosticError);
        let warn = resolve_syntax_style(&resolved, &ids, S::DiagnosticWarning);
        let info = resolve_syntax_style(&resolved, &ids, S::DiagnosticInfo);
        let hint = resolve_syntax_style(&resolved, &ids, S::DiagnosticHint);
        // Identical to the elements the gutter/underline already use.
        assert_eq!(err, resolved.get(ids.diagnostic_error));
        assert_eq!(warn, resolved.get(ids.diagnostic_warning));
        // Severities are visually distinct (error ≠ info).
        assert!(err.fg.is_some());
        assert_ne!(err.fg, info.fg);
        assert_ne!(warn.fg, hint.fg);
    }
}
