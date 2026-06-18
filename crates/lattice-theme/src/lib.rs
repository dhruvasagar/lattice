//! Renderer-neutral theme primitives.
//!
//! The `Color` / `Style` / `Modifiers` / `NamedColor` value types,
//! the rich-vocabulary attribute types (`FontScale` / `Weight` /
//! `FamilyId`), and the `parse_color` helper. Every type here is
//! pure data with no renderer-specific dependency. Renderer crates
//! (`lattice-ui-tui`, `lattice-ui-gpui`) ship adapters that convert
//! these into their native style types (ratatui `Style` / `Color`,
//! GPUI `Hsla` + per-run font shaping).
//!
//! Until T.1 (theme-system slice plan) these lived in
//! `lattice-host/src/ui/theme.rs`; they moved here so cells, modes,
//! the host, and both renderers share one definition. The host
//! re-exports them from their old path so existing call sites are
//! unchanged. The element registry + palette + resolution land here
//! next (T.2/T.3).
//!
//! Design: `docs/dev/architecture/theme-system.md`.

mod element;
mod palette;
mod registry;
mod themes;

pub use element::{
    ColorRef, ElementId, ElementName, ElementOwner, ModifierSet, StyleSpec, ThemeElement,
};
pub use palette::{default_palette, macchiato_palette, Palette, PaletteKey};
pub use registry::{
    register_builtins, BuiltinElementIds, ElementInfo, InMemoryThemeRegistry, ResolvedTheme,
    ThemeRegistry, ThemeRegistryHandle,
};
pub use themes::{builtin_themes, NamedTheme};

/// A single style: optional foreground + optional background +
/// modifiers (bold/italic/etc) + the rich-vocabulary attributes
/// (`scale` / `family` / `weight`). `None` for fg/bg means "do not
/// set this channel" (matches ratatui's empty-style semantics and
/// GPUI's `Style::transparent_black` background semantics).
///
/// `Eq + Hash` is load-bearing: the host folds a content-hash of the
/// `Theme` into [`lattice_cells::MatrixVersion::theme`] so a palette
/// change rebuilds the cell matrix. Every field must therefore be
/// `Hash` — which is why the rich-vocabulary attributes use
/// fixed-point / enum / interned-id representations
/// ([`FontScale`] is `u16` hundredths, not `f32`) rather than the
/// `f32` an authoring `StyleSpec` carries (T.2 resolves the ratio to
/// fixed-point here).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: Modifiers,
    // ---- rich vocabulary (theme-system §3.4) ----
    /// Relative height multiplier (emacs `:height` float, quantized
    /// to fixed-point). `None` ⇒ 1.0×. Honored by the GPUI peer's
    /// per-run font shaping (T.10); a no-op on the fixed-grid TUI.
    pub scale: Option<FontScale>,
    /// Font family selector. `None` ⇒ the buffer's default family.
    /// Honored by GPUI; a no-op on the TUI (single grid font).
    pub family: Option<FamilyId>,
    /// Font weight, finer than the `bold` modifier. `None` ⇒
    /// inherit/default. Honored by GPUI; the TUI maps any
    /// bold-or-heavier weight to its bold attribute.
    pub weight: Option<Weight>,
}

impl Style {
    /// Style with no fg/bg/modifiers -- the renderer's "use my
    /// existing style." Equivalent to `ratatui::Style::default()`
    /// or `ratatui::Style::new()`.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    pub fn bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub fn bold(mut self) -> Self {
        self.modifiers.bold = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.modifiers.italic = true;
        self
    }

    pub fn underline(mut self) -> Self {
        self.modifiers.underline = true;
        self
    }

    pub fn dim(mut self) -> Self {
        self.modifiers.dim = true;
        self
    }

    pub fn reverse(mut self) -> Self {
        self.modifiers.reverse = true;
        self
    }

    /// Set the relative height multiplier (rich vocabulary).
    pub fn scale(mut self, scale: FontScale) -> Self {
        self.scale = Some(scale);
        self
    }

    /// Set the font family (rich vocabulary).
    pub fn family(mut self, family: FamilyId) -> Self {
        self.family = Some(family);
        self
    }

    /// Set the font weight (rich vocabulary).
    pub fn weight(mut self, weight: Weight) -> Self {
        self.weight = Some(weight);
        self
    }
}

/// Text-attribute modifiers. Bools rather than bitflags so a new
/// modifier (strikethrough, blink, ...) is a struct-field add
/// instead of a flag-byte expansion; the renderers' adapter code
/// pattern-matches against the explicit field set rather than
/// chasing flag bits.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub reverse: bool,
}

/// Relative font-height multiplier, stored as **hundredths**
/// (`100` = 1.0×, `160` = 1.6×). Fixed-point rather than `f32` so
/// [`Style`] stays `Eq + Hash` (the theme is content-hashed into the
/// cell-matrix version). An authoring `StyleSpec` carries an `f32`
/// ratio; resolution quantizes it here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontScale(pub u16);

impl FontScale {
    /// 1.0× — the no-op scale.
    pub const ONE: FontScale = FontScale(100);

    /// Quantize an `f32` ratio (e.g. `1.6`) to fixed-point
    /// hundredths. Clamps negatives to 0.
    pub fn from_ratio(ratio: f32) -> Self {
        let h = (ratio * 100.0).round();
        FontScale(if h < 0.0 { 0 } else { h as u16 })
    }

    /// The multiplier as an `f32` ratio (e.g. `1.6`). Used by the
    /// GPUI peer when sizing a run.
    pub fn as_ratio(self) -> f32 {
        self.0 as f32 / 100.0
    }
}

/// Font weight, finer-grained than the `bold` [`Modifiers`] flag.
/// Maps onto the GPUI peer's font-weight axis; the TUI renders any
/// weight at `SemiBold` or heavier as its bold attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Thin,
    ExtraLight,
    Light,
    Normal,
    Medium,
    SemiBold,
    Bold,
    ExtraBold,
    Black,
}

/// An interned font-family selector. The name→id interning + the
/// id→family resolution live with the renderer-side font table
/// (T.10); the id is renderer-neutral so a `Style` can name a family
/// without the theme crate depending on a font stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FamilyId(pub u32);

/// Renderer-neutral color. The variants cover every shape any
/// terminal-or-GPU renderer ever needs: `Default` for "use the
/// terminal/window's default", `Named` for the 16 ANSI palette
/// names (TUI's 16-color fallback path), `Indexed` for the
/// 256-color palette, `Rgb` for 24-bit truecolor.
///
/// TUI renderer maps `Rgb` to `Indexed`-closest-match when the
/// terminal doesn't support truecolor. GPUI ignores `Named` /
/// `Indexed` lookups in palette-aware mode and reads `Rgb`
/// directly. The host owns the lossless form; each renderer
/// owns its own lossy-mapping at adapter time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// Terminal / window default for this channel. Maps to
    /// `ratatui::Color::Reset`.
    Default,
    /// One of the 16 named ANSI colors. The TUI's primary
    /// palette path; GPUI maps these to its theme's named-color
    /// table.
    Named(NamedColor),
    /// 256-color palette index (xterm 256-color extension).
    Indexed(u8),
    /// 24-bit truecolor.
    Rgb(u8, u8, u8),
}

impl Color {
    /// Convert to a 24-bit `0xRRGGBB` packed `u32` for GPU-side
    /// renderers (which want raw truecolor, not the renderer-
    /// neutral [`Color`] enum). [`Color::Default`] returns
    /// `fallback` — the caller decides what "use the terminal /
    /// window default channel" means in pixel-space.
    ///
    /// Named colors map to canonical ANSI RGB values that match
    /// what xterm + most modern terminal emulators use. The
    /// indexed (xterm 256) path computes the 6×6×6 cube + the
    /// 24-step grayscale ramp standardly.
    pub fn to_rgb_u32(self, fallback: u32) -> u32 {
        use NamedColor as N;
        match self {
            Color::Default => fallback,
            Color::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
            Color::Named(n) => match n {
                N::Black => 0x000000,
                N::Red => 0xcd0000,
                N::Green => 0x00cd00,
                N::Yellow => 0xcdcd00,
                N::Blue => 0x0000ee,
                N::Magenta => 0xcd00cd,
                N::Cyan => 0x00cdcd,
                N::Gray => 0xe5e5e5,
                N::DarkGray => 0x7f7f7f,
                N::LightRed => 0xff0000,
                N::LightGreen => 0x00ff00,
                N::LightYellow => 0xffff00,
                N::LightBlue => 0x5c5cff,
                N::LightMagenta => 0xff00ff,
                N::LightCyan => 0x00ffff,
                N::White => 0xffffff,
            },
            Color::Indexed(idx) => indexed_to_rgb_u32(idx),
        }
    }
}

/// Map an xterm 256-colour index to a packed `0xRRGGBB` value.
/// - 0..=15: ANSI base colors (matches [`Color::Named`] mapping)
/// - 16..=231: 6×6×6 cube; each channel steps through
///   `[0, 95, 135, 175, 215, 255]`
/// - 232..=255: 24-step grayscale ramp from `0x080808` to
///   `0xeeeeee` in `+10` increments
fn indexed_to_rgb_u32(idx: u8) -> u32 {
    if idx < 16 {
        let names = [
            NamedColor::Black,
            NamedColor::Red,
            NamedColor::Green,
            NamedColor::Yellow,
            NamedColor::Blue,
            NamedColor::Magenta,
            NamedColor::Cyan,
            NamedColor::Gray,
            NamedColor::DarkGray,
            NamedColor::LightRed,
            NamedColor::LightGreen,
            NamedColor::LightYellow,
            NamedColor::LightBlue,
            NamedColor::LightMagenta,
            NamedColor::LightCyan,
            NamedColor::White,
        ];
        Color::Named(names[idx as usize]).to_rgb_u32(0)
    } else if idx < 232 {
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let n = idx - 16;
        let r = STEPS[(n / 36) as usize];
        let g = STEPS[((n / 6) % 6) as usize];
        let b = STEPS[(n % 6) as usize];
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    } else {
        let level = 8 + 10 * (idx - 232) as u32;
        (level << 16) | (level << 8) | level
    }
}

/// The 16 named ANSI colors. Order matches ratatui's
/// `Color::Black..White` enumeration so the adapter is a
/// straightforward variant-by-variant match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
}

/// Parse a user-typed color name into a [`Color`]. Accepts the 16
/// ANSI names (lowercase + dark-prefixed variants), `default` /
/// `reset` for terminal-default, and 6-digit hex (`#cba6f7` or
/// `cba6f7`, case-insensitive) → [`Color::Rgb`]. T.9.c: hex unblocks
/// a theme/`:set ui.*` author writing a one-off truecolor without a
/// palette entry. A `#`-prefixed string that is NOT exactly 6 hex
/// digits, or any other unknown word, returns the `unknown color`
/// error rather than guessing.
pub fn parse_color(s: &str) -> Result<Color, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "default" | "reset" => Color::Default,
        "black" => Color::Named(NamedColor::Black),
        "red" => Color::Named(NamedColor::Red),
        "green" => Color::Named(NamedColor::Green),
        "yellow" => Color::Named(NamedColor::Yellow),
        "blue" => Color::Named(NamedColor::Blue),
        "magenta" => Color::Named(NamedColor::Magenta),
        "cyan" => Color::Named(NamedColor::Cyan),
        "gray" | "grey" | "white" => Color::Named(NamedColor::Gray),
        "darkgray" | "darkgrey" => Color::Named(NamedColor::DarkGray),
        "lightred" => Color::Named(NamedColor::LightRed),
        "lightgreen" => Color::Named(NamedColor::LightGreen),
        "lightyellow" => Color::Named(NamedColor::LightYellow),
        "lightblue" => Color::Named(NamedColor::LightBlue),
        "lightmagenta" => Color::Named(NamedColor::LightMagenta),
        "lightcyan" => Color::Named(NamedColor::LightCyan),
        other => return parse_hex_color(other).ok_or_else(|| format!("unknown color `{other}`")),
    })
}

/// Parse a 6-digit hex color (`#cba6f7` or `cba6f7`). The leading `#`
/// is optional; the remaining text must be exactly 6 ASCII hex digits.
/// `None` for any other shape — the caller maps that to the
/// `unknown color` error so a malformed hex never silently degrades.
fn parse_hex_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parse_color_named() {
        assert_eq!(parse_color("red").unwrap(), Color::Named(NamedColor::Red));
        assert_eq!(
            parse_color("DarkGray").unwrap(),
            Color::Named(NamedColor::DarkGray)
        );
        assert_eq!(parse_color("default").unwrap(), Color::Default);
    }

    #[test]
    fn rgb_to_u32_packs_24_bit() {
        // 0xRRGGBB ordering. 0xff0000 = red, 0x00ff00 = green,
        // 0x0000ff = blue.
        assert_eq!(Color::Rgb(0xff, 0x00, 0x00).to_rgb_u32(0), 0xff0000);
        assert_eq!(Color::Rgb(0x00, 0xff, 0x00).to_rgb_u32(0), 0x00ff00);
        assert_eq!(Color::Rgb(0x00, 0x00, 0xff).to_rgb_u32(0), 0x0000ff);
        assert_eq!(Color::Rgb(0x12, 0x34, 0x56).to_rgb_u32(0), 0x123456);
    }

    #[test]
    fn default_color_returns_fallback() {
        // `Color::Default` means "use the terminal / window
        // default channel" — there's no truecolour answer, so we
        // hand back the caller's chosen fallback.
        assert_eq!(Color::Default.to_rgb_u32(0xdeadbe), 0xdeadbe);
        assert_eq!(Color::Default.to_rgb_u32(0), 0);
    }

    #[test]
    fn named_red_canonical_ansi_value() {
        // The 16 named ANSI colors map to standard xterm RGB.
        // Red == 0xcd0000 in the canonical xterm palette.
        assert_eq!(Color::Named(NamedColor::Red).to_rgb_u32(0), 0xcd0000);
        assert_eq!(Color::Named(NamedColor::White).to_rgb_u32(0), 0xffffff);
        assert_eq!(Color::Named(NamedColor::Black).to_rgb_u32(0), 0x000000);
    }

    #[test]
    fn indexed_below_16_matches_named() {
        // Indexed 0..=15 must agree with their Named equivalents
        // (callers should not see a discontinuity between the
        // 16-color named palette and the indexed-256 path).
        assert_eq!(
            Color::Indexed(1).to_rgb_u32(0),
            Color::Named(NamedColor::Red).to_rgb_u32(0)
        );
        assert_eq!(
            Color::Indexed(15).to_rgb_u32(0),
            Color::Named(NamedColor::White).to_rgb_u32(0)
        );
    }

    #[test]
    fn indexed_cube_corner_pure_black() {
        // Index 16 is the start of the 6×6×6 colour cube — pure
        // (0,0,0) black.
        assert_eq!(Color::Indexed(16).to_rgb_u32(0), 0x000000);
    }

    #[test]
    fn indexed_cube_corner_pure_white() {
        // Index 231 is the end of the cube — (255,255,255) white.
        assert_eq!(Color::Indexed(231).to_rgb_u32(0), 0xffffff);
    }

    #[test]
    fn indexed_grayscale_ramp() {
        // 232..=255 is a 24-step grey ramp from 0x080808 to
        // 0xeeeeee in +10 increments.
        assert_eq!(Color::Indexed(232).to_rgb_u32(0), 0x080808);
        assert_eq!(Color::Indexed(255).to_rgb_u32(0), 0xeeeeee);
    }

    #[test]
    fn parse_color_unknown_errors() {
        assert!(parse_color("rainbow").is_err());
    }

    #[test]
    fn parse_color_hex_with_and_without_hash() {
        // T.9.c: `#cba6f7` and `cba6f7` both parse to the same RGB.
        assert_eq!(
            parse_color("#cba6f7").unwrap(),
            Color::Rgb(0xcb, 0xa6, 0xf7)
        );
        assert_eq!(parse_color("cba6f7").unwrap(), Color::Rgb(0xcb, 0xa6, 0xf7));
        // Case-insensitive (parse lowercases first).
        assert_eq!(
            parse_color("#CBA6F7").unwrap(),
            Color::Rgb(0xcb, 0xa6, 0xf7)
        );
    }

    #[test]
    fn parse_color_invalid_hex_errors() {
        // Non-hex digits, wrong length, and a bare `#` all error
        // rather than silently degrading.
        assert!(parse_color("#xyz").is_err());
        assert!(parse_color("#cba6f").is_err()); // 5 digits
        assert!(parse_color("#cba6f7a").is_err()); // 7 digits
        assert!(parse_color("#").is_err());
        assert!(parse_color("zzzzzz").is_err()); // 6 non-hex chars
    }

    // ---- T.1: rich-vocabulary attribute types ----

    #[test]
    fn font_scale_roundtrips_through_fixed_point() {
        assert_eq!(FontScale::from_ratio(1.6), FontScale(160));
        assert_eq!(FontScale::ONE.as_ratio(), 1.0);
        assert_eq!(FontScale::from_ratio(1.6).as_ratio(), 1.6);
    }

    #[test]
    fn style_stays_hashable_with_rich_vocab() {
        // Load-bearing: Style is folded into the cell-matrix version
        // hash. Adding the rich-vocab fields must not break Hash/Eq.
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let s = Style::empty()
            .fg(Color::Rgb(1, 2, 3))
            .bold()
            .scale(FontScale::from_ratio(1.6))
            .weight(Weight::SemiBold)
            .family(FamilyId(7));
        let mut h = DefaultHasher::new();
        s.hash(&mut h);
        let _ = h.finish();
        assert_eq!(s, s);
    }

    #[test]
    fn empty_style_has_no_rich_attrs() {
        let s = Style::empty();
        assert_eq!(s.scale, None);
        assert_eq!(s.family, None);
        assert_eq!(s.weight, None);
    }
}
