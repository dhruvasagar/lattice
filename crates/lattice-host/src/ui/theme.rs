//! The host `Theme` + its `SyntaxStyle → visual style` mapping.
//!
//! The renderer-neutral *primitives* — `Color`, `Style`,
//! `Modifiers`, `NamedColor`, the rich-vocabulary attribute types,
//! and `parse_color` — live in the leaf crate `lattice-theme` and
//! are **re-exported here** so every existing `lattice_host::ui::theme`
//! / `host_theme` call site is unchanged (T.1, theme-system slice
//! plan). Renderer crates (`lattice-ui-tui`, `lattice-ui-gpui`) ship
//! adapters that convert these into their native style types
//! (ratatui `Style` / `Color`, GPUI `Hsla` + per-run font shaping);
//! the host owns the canonical theme, each renderer maintains a
//! cached adapted view for hot-path reads.
//!
//! The `SyntaxStyle → visual style` bridge (`resolve_syntax_style` /
//! `syntax_element_id`) moved DOWN to `lattice-syntax` at DX.2 (BC.6
//! diff extraction) and is re-exported here unchanged — see the
//! re-export note below. The element registry + palette + resolution
//! (`lattice-theme` T.2/T.3) subsume the old flat struct; renderers read
//! the resolved table via `ResolvedTheme` + `BuiltinElementIds`. See
//! `docs/dev/architecture/theme-system.md`.

pub use lattice_theme::{
    BuiltinElementIds, Color, ColorRef, ElementInfo, ElementName, ElementOwner, FamilyId,
    FontScale, InMemoryThemeRegistry, Modifiers, NamedColor, NamedTheme, ResolvedTheme, Style,
    StyleSpec, ThemeRegistry, ThemeRegistryHandle, Weight, builtin_themes, parse_color,
};

// DX.2 (BC.6 diff extraction): the syntax->theme-element style bridge
// (`resolve_syntax_style` / `syntax_element_id`) moved DOWN to
// `lattice-syntax` (`lattice_syntax::theme_style`). The bridge is
// syntax-aware (it takes a `lattice_syntax::Style`), so it belongs in the
// higher crate depending onto the `lattice-theme` leaf — keeping
// `lattice-theme` a minimal renderer-hot-path leaf — and so `lattice-diff`
// (the diff overlay) can reach it without the host. Re-exported here so
// every existing `lattice_host::ui::theme::` / `crate::ui::theme::` call
// site (cell builder, both renderers, the diff overlay) is unchanged; the
// overlay's import flips to `lattice_syntax::resolve_syntax_style` when it
// moves into `lattice-diff` at DX.6. Tests for the bridge moved with it.
pub use lattice_syntax::{resolve_syntax_style, syntax_element_id};

// T.6.t (2026-06-18): the host `Theme` struct is DELETED. All STYLE
// fields moved to the element / resolved-table system (T.4/T.5); the
// final 8 non-style fields (`dim_inactive_panes`,
// `pane_separator_{vertical,horizontal}`, `nerd_fonts`, the four
// `diagnostic_*_glyph` chars) migrated to `ui.*` typed options in
// `crate::ui::theme_options`. Renderers read the style table via
// `ResolvedTheme` + `BuiltinElementIds`, and the non-style flags/chars
// via the typed-options registry. The cell-matrix invalidation key
// (`MatrixVersion::theme`) is now `ResolvedTheme::version()`, not a
// content-hash of this struct. See `docs/dev/architecture/theme-system.md`.
