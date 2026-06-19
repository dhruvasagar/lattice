//! The palette — a named set of base colors owned by the active
//! theme. Swapping the palette re-colors every element that
//! references it (the multi-theme primitive). A [`StyleSpec`]'s
//! `ColorRef::Palette(key)` resolves against the active palette at
//! theme-build time.
//!
//! The default palette below holds three families so the T.2
//! builtin elements can reference it and still resolve to **exactly**
//! today's literal colors (the `resolved_builtins_match_legacy_literals`
//! parity pin):
//!
//! - **Catppuccin Mocha accents** (RGB) — what syntax highlighting
//!   already uses.
//! - **ANSI-named entries** (`ansi.*` → [`Color::Named`]) — the
//!   chrome (pane status, diagnostics, file-tree, messages, diff
//!   signs) deliberately uses named colors so it degrades on 16-color
//!   terminals; the palette preserves that intent rather than
//!   flattening to RGB.
//! - **Specific tints** (diff backgrounds, current-line) — one-off
//!   colors a palette entry names so a theme can still re-tint them.
//!
//! A future theme (T.9) redefines these keys; chrome can migrate to
//! semantic accent roles once a second theme exists to force the
//! abstraction.
//!
//! Design: `docs/dev/architecture/theme-system.md` §3.3.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::{Color, NamedColor};

/// A palette entry's name (`"purple"`, `"ansi.red"`, `"diff.add.bg"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaletteKey(Cow<'static, str>);

impl PaletteKey {
    pub const fn from_static(s: &'static str) -> Self {
        PaletteKey(Cow::Borrowed(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&'static str> for PaletteKey {
    fn from(s: &'static str) -> Self {
        PaletteKey::from_static(s)
    }
}

impl From<String> for PaletteKey {
    fn from(s: String) -> Self {
        PaletteKey(Cow::Owned(s))
    }
}

/// A named set of base colors. `StyleSpec` colors reference these by
/// key; resolution looks them up here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Palette {
    colors: HashMap<PaletteKey, Color>,
}

impl Palette {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert.
    pub fn with(mut self, key: impl Into<PaletteKey>, color: Color) -> Self {
        self.colors.insert(key.into(), color);
        self
    }

    pub fn insert(&mut self, key: impl Into<PaletteKey>, color: Color) {
        self.colors.insert(key.into(), color);
    }

    /// Look up a color by key. `None` if the active palette lacks it
    /// — resolution then falls back (inherit chain → hard default)
    /// and logs once rather than panicking.
    pub fn get(&self, key: &PaletteKey) -> Option<Color> {
        self.colors.get(key).copied()
    }

    pub fn len(&self) -> usize {
        self.colors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.colors.is_empty()
    }
}

/// The default palette (Catppuccin Mocha accents + ANSI chrome +
/// tints). See module docs for why all three families exist.
pub fn default_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Catppuccin Mocha accents (syntax) ----
        .with("text", rgb(0xcd, 0xd6, 0xf4))
        .with("overlay", rgb(0x6c, 0x70, 0x86))
        .with("subtext", rgb(0x93, 0x99, 0xb2))
        .with("green", rgb(0xa6, 0xe3, 0xa1))
        .with("purple", rgb(0xcb, 0xa6, 0xf7))
        .with("yellow", rgb(0xf9, 0xe2, 0xaf))
        .with("orange", rgb(0xfa, 0xb3, 0x87))
        .with("blue", rgb(0x89, 0xb4, 0xfa))
        .with("teal", rgb(0x94, 0xe2, 0xd5))
        .with("red", rgb(0xf3, 0x8b, 0xa8))
        .with("maroon", rgb(0xeb, 0xa0, 0xac))
        .with("pink", rgb(0xf5, 0xc2, 0xe7))
        .with("cyan", rgb(0x74, 0xc7, 0xec))
        // ---- ANSI-named chrome (degrades on 16-color terminals) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (canvas surfaces, T.11.0b) ----
        .with("base", rgb(0x1e, 0x1e, 0x2e))
        .with("mantle", rgb(0x18, 0x18, 0x25))
        .with("crust", rgb(0x11, 0x11, 0x1b))
        .with("surface0", rgb(0x31, 0x32, 0x44))
        .with("surface1", rgb(0x45, 0x47, 0x5a))
        .with("surface2", rgb(0x58, 0x5b, 0x70))
        // ---- Specific tints (one-off backgrounds) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", Color::Indexed(236))
}

/// The Catppuccin **Macchiato** palette (T.9.b). Same key SET as
/// [`default_palette`] — every element references the same keys — but
/// the accent RGB values are Macchiato's. The `ansi.*` named entries
/// stay IDENTICAL to mocha's: the chrome's degradation-on-16-color
/// intent is theme-independent, so a colorscheme swap must not flip a
/// named ANSI entry to a truecolor. The one-off tints also stay
/// identical EXCEPT `cursor_line.bg`, which Macchiato carries as a
/// distinct RGB tint matching its lighter surface.
pub fn macchiato_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Catppuccin Macchiato accents (syntax) ----
        .with("text", rgb(0xca, 0xd3, 0xf5))
        .with("overlay", rgb(0x6e, 0x73, 0x8d))
        .with("subtext", rgb(0x93, 0x9a, 0xb7))
        .with("green", rgb(0xa6, 0xda, 0x95))
        .with("purple", rgb(0xc6, 0xa0, 0xf6))
        .with("yellow", rgb(0xee, 0xd4, 0x9f))
        .with("orange", rgb(0xf5, 0xa9, 0x7f))
        .with("blue", rgb(0x8a, 0xad, 0xf4))
        .with("teal", rgb(0x8b, 0xd5, 0xca))
        .with("red", rgb(0xed, 0x87, 0x96))
        .with("maroon", rgb(0xee, 0x99, 0xa0))
        .with("pink", rgb(0xf5, 0xbd, 0xe6))
        .with("cyan", rgb(0x7d, 0xc4, 0xe4))
        // ---- ANSI-named chrome (IDENTICAL to mocha; theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (canvas surfaces, T.11.0b) ----
        .with("base", rgb(0x24, 0x27, 0x3a))
        .with("mantle", rgb(0x1e, 0x20, 0x30))
        .with("crust", rgb(0x18, 0x19, 0x26))
        .with("surface0", rgb(0x36, 0x3a, 0x4f))
        .with("surface1", rgb(0x49, 0x4d, 0x64))
        .with("surface2", rgb(0x5b, 0x60, 0x78))
        // ---- Specific tints (diff bgs identical to mocha; cursor line distinct) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x36, 0x3a, 0x4f))
}

/// The Catppuccin **Latte** palette (T.11.1) — the project's first
/// LIGHT theme. Same key SET as [`default_palette`] (every element
/// references the same keys; the palette indirection re-colours the
/// whole surface), but with Latte's light `base` + dark `text` + Latte
/// accent RGBs. This is the proof that T.11.0b made the canvas
/// palette-driven: `editor.background` resolves to the light `base` and
/// `editor.foreground` to the dark `text`, so swapping to Latte
/// recolours the GPUI canvas light. `ansi.*` stay terminal-named
/// (theme-independent). The diff tints + cursor-line are LIGHT variants
/// (the Mocha dark tints would read as near-black blocks on a light
/// canvas).
pub fn catppuccin_latte_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Catppuccin Latte accents (syntax) ----
        .with("text", rgb(0x4c, 0x4f, 0x69))
        .with("overlay", rgb(0x9c, 0xa0, 0xb0))
        .with("subtext", rgb(0x7c, 0x7f, 0x93))
        .with("green", rgb(0x40, 0xa0, 0x2b))
        .with("purple", rgb(0x88, 0x39, 0xef))
        .with("yellow", rgb(0xdf, 0x8e, 0x1d))
        .with("orange", rgb(0xfe, 0x64, 0x0b))
        .with("blue", rgb(0x1e, 0x66, 0xf5))
        .with("teal", rgb(0x17, 0x92, 0x99))
        .with("red", rgb(0xd2, 0x0f, 0x39))
        .with("maroon", rgb(0xe6, 0x45, 0x53))
        .with("pink", rgb(0xea, 0x76, 0xcb))
        .with("cyan", rgb(0x20, 0x9f, 0xb5))
        // ---- ANSI-named chrome (theme-independent; same as the others) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (LIGHT canvas surfaces) ----
        .with("base", rgb(0xef, 0xf1, 0xf5))
        .with("mantle", rgb(0xe6, 0xe9, 0xef))
        .with("crust", rgb(0xdc, 0xe0, 0xe8))
        .with("surface0", rgb(0xcc, 0xd0, 0xda))
        .with("surface1", rgb(0xbc, 0xc0, 0xcc))
        .with("surface2", rgb(0xac, 0xb0, 0xbe))
        // ---- Specific tints (LIGHT variants for a light canvas) ----
        .with("diff.add.bg", rgb(0xd8, 0xee, 0xd2))
        .with("diff.change.bg", rgb(0xf0, 0xe8, 0xc8))
        .with("diff.deletion.bg", rgb(0xf5, 0xd5, 0xd5))
        .with("diff.conflict.bg", rgb(0xee, 0xd8, 0xee))
        .with("cursor_line.bg", rgb(0xcc, 0xd0, 0xda))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_resolves_catppuccin_and_ansi() {
        let p = default_palette();
        assert_eq!(p.get(&"purple".into()), Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert_eq!(
            p.get(&"ansi.red".into()),
            Some(Color::Named(NamedColor::Red))
        );
        assert_eq!(p.get(&"cursor_line.bg".into()), Some(Color::Indexed(236)));
    }

    #[test]
    fn missing_key_is_none_not_panic() {
        let p = default_palette();
        assert_eq!(p.get(&"no.such.key".into()), None);
    }

    #[test]
    fn macchiato_palette_differs_in_accents_shares_ansi() {
        // T.9.b: Macchiato's accents differ from mocha's, but the
        // `ansi.*` chrome entries stay identical (degradation intent),
        // and `cursor_line.bg` is a distinct Macchiato tint.
        let mocha = default_palette();
        let mac = macchiato_palette();
        assert_eq!(mac.get(&"purple".into()), Some(Color::Rgb(0xc6, 0xa0, 0xf6)));
        assert_ne!(mac.get(&"purple".into()), mocha.get(&"purple".into()));
        assert_eq!(mac.get(&"text".into()), Some(Color::Rgb(0xca, 0xd3, 0xf5)));
        // ANSI chrome identical to mocha.
        assert_eq!(mac.get(&"ansi.red".into()), mocha.get(&"ansi.red".into()));
        assert_eq!(
            mac.get(&"ansi.darkgray".into()),
            mocha.get(&"ansi.darkgray".into())
        );
        // cursor_line.bg is a distinct Macchiato RGB tint.
        assert_eq!(
            mac.get(&"cursor_line.bg".into()),
            Some(Color::Rgb(0x36, 0x3a, 0x4f))
        );
        assert_ne!(
            mac.get(&"cursor_line.bg".into()),
            mocha.get(&"cursor_line.bg".into())
        );
        // diff bgs identical to mocha.
        assert_eq!(
            mac.get(&"diff.add.bg".into()),
            mocha.get(&"diff.add.bg".into())
        );
        // Same key set (same count).
        assert_eq!(mac.len(), mocha.len());
    }
}
