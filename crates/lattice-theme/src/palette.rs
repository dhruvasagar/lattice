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

/// A palette entry's name (`"mauve"`, `"ansi.red"`, `"diff.add.bg"`).
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
        .with("overlay0", rgb(0x6c, 0x70, 0x86))
        .with("overlay2", rgb(0x93, 0x99, 0xb2))
        .with("green", rgb(0xa6, 0xe3, 0xa1))
        .with("mauve", rgb(0xcb, 0xa6, 0xf7))
        .with("yellow", rgb(0xf9, 0xe2, 0xaf))
        .with("peach", rgb(0xfa, 0xb3, 0x87))
        .with("blue", rgb(0x89, 0xb4, 0xfa))
        .with("teal", rgb(0x94, 0xe2, 0xd5))
        .with("red", rgb(0xf3, 0x8b, 0xa8))
        .with("maroon", rgb(0xeb, 0xa0, 0xac))
        .with("pink", rgb(0xf5, 0xc2, 0xe7))
        .with("sapphire", rgb(0x74, 0xc7, 0xec))
        // ---- ANSI-named chrome (degrades on 16-color terminals) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Specific tints (one-off backgrounds) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", Color::Indexed(236))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_resolves_catppuccin_and_ansi() {
        let p = default_palette();
        assert_eq!(p.get(&"mauve".into()), Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
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
}
