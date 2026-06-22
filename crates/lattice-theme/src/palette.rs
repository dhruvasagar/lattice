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

    /// Iterate over every key this palette defines. Used by the
    /// completeness test to assert each builtin theme covers the full
    /// role-key set `default_palette` defines (no resolution fallback).
    pub fn keys(&self) -> impl Iterator<Item = &PaletteKey> {
        self.colors.keys()
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

// ============================================================
// T.11.1 — the multi-theme library: 9 cross-editor families ×
// {dark, light}. Every palette below fills the FULL role-key set
// `default_palette` defines, so resolution never falls back. The
// `ansi.*` block is theme-independent (copied verbatim from the
// Catppuccin palettes): chrome degrades to terminal-named colors
// the same way regardless of colorscheme.
// ============================================================

/// Gruvbox **dark** (official). Warm retro palette: dark bg0 base,
/// light fg foreground, the canonical bright aqua/green/orange accents.
pub fn gruvbox_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Gruvbox dark accents ----
        .with("text", rgb(0xeb, 0xdb, 0xb2))
        .with("overlay", rgb(0x92, 0x83, 0x74))
        .with("subtext", rgb(0xa8, 0x99, 0x84))
        .with("green", rgb(0xb8, 0xbb, 0x26))
        .with("purple", rgb(0xd3, 0x86, 0x9b))
        .with("yellow", rgb(0xfa, 0xbd, 0x2f))
        .with("orange", rgb(0xfe, 0x80, 0x19))
        .with("blue", rgb(0x83, 0xa5, 0x98))
        .with("teal", rgb(0x8e, 0xc0, 0x7c))
        .with("red", rgb(0xfb, 0x49, 0x34))
        .with("maroon", rgb(0xcc, 0x24, 0x1d))
        .with("pink", rgb(0xd3, 0x86, 0x9b))
        .with("cyan", rgb(0x8e, 0xc0, 0x7c))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (dark) ----
        .with("base", rgb(0x28, 0x28, 0x28))
        .with("mantle", rgb(0x1d, 0x20, 0x21))
        .with("crust", rgb(0x16, 0x18, 0x19))
        .with("surface0", rgb(0x3c, 0x38, 0x36))
        .with("surface1", rgb(0x50, 0x49, 0x45))
        .with("surface2", rgb(0x66, 0x5c, 0x54))
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x3c, 0x38, 0x36))
}

/// Gruvbox **light** (official). Light bg0 base (`#fbf1c7`), dark fg
/// text, accents are the deeper "faded" gruvbox-light variants for
/// contrast on a cream canvas.
pub fn gruvbox_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Gruvbox light accents (faded, for contrast) ----
        .with("text", rgb(0x3c, 0x38, 0x36))
        .with("overlay", rgb(0x7c, 0x6f, 0x64))
        .with("subtext", rgb(0x66, 0x5c, 0x54))
        .with("green", rgb(0x79, 0x74, 0x0e))
        .with("purple", rgb(0xb1, 0x62, 0x86))
        .with("yellow", rgb(0xb5, 0x76, 0x14))
        .with("orange", rgb(0xaf, 0x3a, 0x03))
        .with("blue", rgb(0x07, 0x66, 0x78))
        .with("teal", rgb(0x42, 0x7b, 0x58))
        .with("red", rgb(0x9d, 0x00, 0x06))
        .with("maroon", rgb(0xcc, 0x24, 0x1d))
        .with("pink", rgb(0x8f, 0x3f, 0x71))
        .with("cyan", rgb(0x42, 0x7b, 0x58))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (LIGHT) ----
        .with("base", rgb(0xfb, 0xf1, 0xc7))
        .with("mantle", rgb(0xf2, 0xe5, 0xbc))
        .with("crust", rgb(0xeb, 0xdb, 0xb2))
        .with("surface0", rgb(0xeb, 0xdb, 0xb2))
        .with("surface1", rgb(0xd5, 0xc4, 0xa1))
        .with("surface2", rgb(0xbd, 0xae, 0x93))
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xd5, 0xe8, 0xc8))
        .with("diff.change.bg", rgb(0xf0, 0xe8, 0xc0))
        .with("diff.deletion.bg", rgb(0xf5, 0xd5, 0xd0))
        .with("diff.conflict.bg", rgb(0xee, 0xd8, 0xe6))
        .with("cursor_line.bg", rgb(0xeb, 0xdb, 0xb2))
}

/// Tokyo Night **dark** ("Night"). Deep blue-black base, soft blue-white
/// text, the signature blue/cyan/purple/green accents.
pub fn tokyonight_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Tokyo Night (Night) accents ----
        .with("text", rgb(0xc0, 0xca, 0xf5))
        .with("overlay", rgb(0x56, 0x5f, 0x89))
        .with("subtext", rgb(0x9a, 0xa5, 0xce))
        .with("green", rgb(0x9e, 0xce, 0x6a))
        .with("purple", rgb(0xbb, 0x9a, 0xf7))
        .with("yellow", rgb(0xe0, 0xaf, 0x68))
        .with("orange", rgb(0xff, 0x9e, 0x64))
        .with("blue", rgb(0x7a, 0xa2, 0xf7))
        .with("teal", rgb(0x1a, 0xbc, 0x9c))
        .with("red", rgb(0xf7, 0x76, 0x8e))
        .with("maroon", rgb(0xdb, 0x4b, 0x4b))
        .with("pink", rgb(0xff, 0x75, 0xa0))
        .with("cyan", rgb(0x7d, 0xcf, 0xff))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (dark) ----
        .with("base", rgb(0x1a, 0x1b, 0x26))
        .with("mantle", rgb(0x16, 0x16, 0x1e))
        .with("crust", rgb(0x13, 0x14, 0x1a))
        .with("surface0", rgb(0x24, 0x28, 0x3b))
        .with("surface1", rgb(0x29, 0x2e, 0x42))
        .with("surface2", rgb(0x41, 0x48, 0x68))
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x29, 0x2e, 0x42))
}

/// Tokyo Night **light** ("Day"). Light gray-blue base, dark navy text,
/// deeper accent variants tuned for the bright canvas.
pub fn tokyonight_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Tokyo Night Day accents (deeper for contrast) ----
        .with("text", rgb(0x34, 0x35, 0x5e))
        .with("overlay", rgb(0x9d, 0xa0, 0xc2))
        .with("subtext", rgb(0x6c, 0x6e, 0x8f))
        .with("green", rgb(0x58, 0x7d, 0x39))
        .with("purple", rgb(0x9d, 0x59, 0xcf))
        .with("yellow", rgb(0x8c, 0x6c, 0x3e))
        .with("orange", rgb(0xb1, 0x5c, 0x00))
        .with("blue", rgb(0x2e, 0x7d, 0xe9))
        .with("teal", rgb(0x11, 0x8c, 0x74))
        .with("red", rgb(0xf5, 0x2a, 0x65))
        .with("maroon", rgb(0xc6, 0x47, 0x60))
        .with("pink", rgb(0xd2, 0x0d, 0x8b))
        .with("cyan", rgb(0x00, 0x7e, 0xc5))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (LIGHT) ----
        .with("base", rgb(0xe1, 0xe2, 0xe7))
        .with("mantle", rgb(0xd6, 0xd8, 0xdf))
        .with("crust", rgb(0xc4, 0xc8, 0xda))
        .with("surface0", rgb(0xc4, 0xc8, 0xda))
        .with("surface1", rgb(0xb6, 0xba, 0xcb))
        .with("surface2", rgb(0xa8, 0xae, 0xc1))
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xd5, 0xe8, 0xcb))
        .with("diff.change.bg", rgb(0xe8, 0xe4, 0xc8))
        .with("diff.deletion.bg", rgb(0xf2, 0xd2, 0xd5))
        .with("diff.conflict.bg", rgb(0xe6, 0xd5, 0xee))
        .with("cursor_line.bg", rgb(0xc4, 0xc8, 0xda))
}

/// Dracula **dark** (official). Dark slate base (`#282a36`), bright
/// foreground (`#f8f8f2`), the signature pink/purple/green/cyan accents.
pub fn dracula_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Dracula accents ----
        .with("text", rgb(0xf8, 0xf8, 0xf2))
        .with("overlay", rgb(0x62, 0x72, 0xa4))
        .with("subtext", rgb(0x9a, 0xa5, 0xce))
        .with("green", rgb(0x50, 0xfa, 0x7b))
        .with("purple", rgb(0xbd, 0x93, 0xf9))
        .with("yellow", rgb(0xf1, 0xfa, 0x8c))
        .with("orange", rgb(0xff, 0xb8, 0x6c))
        .with("blue", rgb(0x8b, 0xe9, 0xfd))
        .with("teal", rgb(0x8b, 0xe9, 0xfd))
        .with("red", rgb(0xff, 0x55, 0x55))
        .with("maroon", rgb(0xe0, 0x40, 0x40))
        .with("pink", rgb(0xff, 0x79, 0xc6))
        .with("cyan", rgb(0x8b, 0xe9, 0xfd))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (dark) ----
        .with("base", rgb(0x28, 0x2a, 0x36))
        .with("mantle", rgb(0x22, 0x24, 0x2e))
        .with("crust", rgb(0x1a, 0x1c, 0x24))
        .with("surface0", rgb(0x44, 0x47, 0x5a))
        .with("surface1", rgb(0x53, 0x57, 0x6e))
        .with("surface2", rgb(0x62, 0x72, 0xa4))
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x44, 0x47, 0x5a))
}

/// Dracula **light** ("Alucard", the official light variant). Light base
/// (`#f8f8f2`), dark text, deeper accent variants for contrast.
pub fn dracula_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Alucard accents (deeper for contrast) ----
        .with("text", rgb(0x1f, 0x1f, 0x1f))
        .with("overlay", rgb(0x6c, 0x66, 0x4b))
        .with("subtext", rgb(0x52, 0x4c, 0x3f))
        .with("green", rgb(0x14, 0x71, 0x0a))
        .with("purple", rgb(0x64, 0x4a, 0xc9))
        .with("yellow", rgb(0x84, 0x6e, 0x15))
        .with("orange", rgb(0xa1, 0x55, 0x07))
        .with("blue", rgb(0x03, 0x6a, 0x96))
        .with("teal", rgb(0x03, 0x6a, 0x96))
        .with("red", rgb(0xcb, 0x3a, 0x2a))
        .with("maroon", rgb(0xa3, 0x2c, 0x20))
        .with("pink", rgb(0xa3, 0x12, 0x7e))
        .with("cyan", rgb(0x03, 0x6a, 0x96))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (LIGHT) ----
        .with("base", rgb(0xf8, 0xf8, 0xf2))
        .with("mantle", rgb(0xef, 0xef, 0xe4))
        .with("crust", rgb(0xe4, 0xe4, 0xd8))
        .with("surface0", rgb(0xe4, 0xe4, 0xd8))
        .with("surface1", rgb(0xd4, 0xd4, 0xc8))
        .with("surface2", rgb(0xc4, 0xc4, 0xb8))
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xd5, 0xe8, 0xc8))
        .with("diff.change.bg", rgb(0xf0, 0xea, 0xc4))
        .with("diff.deletion.bg", rgb(0xf5, 0xd2, 0xcc))
        .with("diff.conflict.bg", rgb(0xee, 0xd2, 0xe8))
        .with("cursor_line.bg", rgb(0xe4, 0xe4, 0xd8))
}

/// Nord **dark** (official). Polar-night bg (`#2e3440`), snow-storm fg
/// (`#d8dee9`), frost (teal/blue/cyan) + aurora (red/orange/yellow/
/// green/purple) accents.
pub fn nord_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Nord accents (frost + aurora) ----
        .with("text", rgb(0xd8, 0xde, 0xe9))
        .with("overlay", rgb(0x61, 0x6e, 0x88))
        .with("subtext", rgb(0xab, 0xb2, 0xbf))
        .with("green", rgb(0xa3, 0xbe, 0x8c))
        .with("purple", rgb(0xb4, 0x8e, 0xad))
        .with("yellow", rgb(0xeb, 0xcb, 0x8b))
        .with("orange", rgb(0xd0, 0x87, 0x70))
        .with("blue", rgb(0x81, 0xa1, 0xc1))
        .with("teal", rgb(0x8f, 0xbc, 0xbb))
        .with("red", rgb(0xbf, 0x61, 0x6a))
        .with("maroon", rgb(0xb4, 0x57, 0x5f))
        .with("pink", rgb(0xb4, 0x8e, 0xad))
        .with("cyan", rgb(0x88, 0xc0, 0xd0))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (polar night) ----
        .with("base", rgb(0x2e, 0x34, 0x40))
        .with("mantle", rgb(0x29, 0x2e, 0x39))
        .with("crust", rgb(0x24, 0x29, 0x33))
        .with("surface0", rgb(0x3b, 0x42, 0x52))
        .with("surface1", rgb(0x43, 0x4c, 0x5e))
        .with("surface2", rgb(0x4c, 0x56, 0x6a))
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x3b, 0x42, 0x52))
}

/// Nord **light** (inverted: snow-storm bg, polar-night text). Community-
/// grade — Nord has no official light flavour, so this inverts the
/// base/text and deepens the aurora/frost accents for contrast.
pub fn nord_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Nord light accents (polar-night text + deepened accents) ----
        .with("text", rgb(0x2e, 0x34, 0x40))
        .with("overlay", rgb(0x7b, 0x84, 0x94))
        .with("subtext", rgb(0x4c, 0x56, 0x6a))
        .with("green", rgb(0x5c, 0x74, 0x47))
        .with("purple", rgb(0x84, 0x5c, 0x7e))
        .with("yellow", rgb(0x9a, 0x7d, 0x36))
        .with("orange", rgb(0xa6, 0x53, 0x36))
        .with("blue", rgb(0x40, 0x5d, 0x7e))
        .with("teal", rgb(0x3e, 0x6d, 0x6b))
        .with("red", rgb(0x99, 0x3b, 0x44))
        .with("maroon", rgb(0x86, 0x32, 0x3a))
        .with("pink", rgb(0x84, 0x5c, 0x7e))
        .with("cyan", rgb(0x3a, 0x6d, 0x80))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (snow storm, LIGHT) ----
        .with("base", rgb(0xec, 0xef, 0xf4))
        .with("mantle", rgb(0xe5, 0xe9, 0xf0))
        .with("crust", rgb(0xd8, 0xde, 0xe9))
        .with("surface0", rgb(0xd8, 0xde, 0xe9))
        .with("surface1", rgb(0xc8, 0xd0, 0xde))
        .with("surface2", rgb(0xb4, 0xbe, 0xd0))
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xd5, 0xe8, 0xcc))
        .with("diff.change.bg", rgb(0xee, 0xe8, 0xcc))
        .with("diff.deletion.bg", rgb(0xf2, 0xd5, 0xd8))
        .with("diff.conflict.bg", rgb(0xe8, 0xd8, 0xe8))
        .with("cursor_line.bg", rgb(0xd8, 0xde, 0xe9))
}

/// Solarized **dark** (official). base03 background (`#002b36`), base0
/// body text, the fixed 8-accent wheel (violet→purple, magenta→pink,
/// orange→orange, cyan→cyan).
pub fn solarized_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Solarized accents ----
        .with("text", rgb(0x83, 0x94, 0x96)) // base0
        .with("overlay", rgb(0x58, 0x6e, 0x75)) // base01
        .with("subtext", rgb(0x65, 0x7b, 0x83)) // base00
        .with("green", rgb(0x85, 0x99, 0x00))
        .with("purple", rgb(0x6c, 0x71, 0xc4)) // violet
        .with("yellow", rgb(0xb5, 0x89, 0x00))
        .with("orange", rgb(0xcb, 0x4b, 0x16))
        .with("blue", rgb(0x26, 0x8b, 0xd2))
        .with("teal", rgb(0x2a, 0xa1, 0x98)) // cyan slot reused for teal
        .with("red", rgb(0xdc, 0x32, 0x2f))
        .with("maroon", rgb(0xb5, 0x26, 0x23))
        .with("pink", rgb(0xd3, 0x36, 0x82)) // magenta
        .with("cyan", rgb(0x2a, 0xa1, 0x98))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (base03..base01, dark) ----
        .with("base", rgb(0x00, 0x2b, 0x36)) // base03
        .with("mantle", rgb(0x00, 0x26, 0x30))
        .with("crust", rgb(0x00, 0x1e, 0x26))
        .with("surface0", rgb(0x07, 0x36, 0x42)) // base02
        .with("surface1", rgb(0x0c, 0x40, 0x4e))
        .with("surface2", rgb(0x58, 0x6e, 0x75)) // base01
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x07, 0x36, 0x42))
}

/// Solarized **light** (official). base3 background (`#fdf6e3`), base00
/// body text. Same fixed accent wheel as the dark variant — Solarized's
/// accents are shared across both modes by design.
pub fn solarized_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Solarized accents (same wheel; text/overlay flip) ----
        .with("text", rgb(0x65, 0x7b, 0x83)) // base00
        .with("overlay", rgb(0x93, 0xa1, 0xa1)) // base1
        .with("subtext", rgb(0x83, 0x94, 0x96)) // base0
        .with("green", rgb(0x85, 0x99, 0x00))
        .with("purple", rgb(0x6c, 0x71, 0xc4)) // violet
        .with("yellow", rgb(0xb5, 0x89, 0x00))
        .with("orange", rgb(0xcb, 0x4b, 0x16))
        .with("blue", rgb(0x26, 0x8b, 0xd2))
        .with("teal", rgb(0x2a, 0xa1, 0x98))
        .with("red", rgb(0xdc, 0x32, 0x2f))
        .with("maroon", rgb(0xb5, 0x26, 0x23))
        .with("pink", rgb(0xd3, 0x36, 0x82)) // magenta
        .with("cyan", rgb(0x2a, 0xa1, 0x98))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (base3..base1, LIGHT) ----
        .with("base", rgb(0xfd, 0xf6, 0xe3)) // base3
        .with("mantle", rgb(0xf5, 0xee, 0xdb))
        .with("crust", rgb(0xee, 0xe8, 0xd5)) // base2
        .with("surface0", rgb(0xee, 0xe8, 0xd5)) // base2
        .with("surface1", rgb(0xdc, 0xd6, 0xc4))
        .with("surface2", rgb(0x93, 0xa1, 0xa1)) // base1
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xdc, 0xe8, 0xc0))
        .with("diff.change.bg", rgb(0xf0, 0xe6, 0xbe))
        .with("diff.deletion.bg", rgb(0xf2, 0xd5, 0xc8))
        .with("diff.conflict.bg", rgb(0xea, 0xd6, 0xdc))
        .with("cursor_line.bg", rgb(0xee, 0xe8, 0xd5))
}

/// Atom **One Dark** (official). Slate base (`#282c34`), light gray-blue
/// text, the One blue/purple/green/red/cyan accents.
pub fn one_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- One Dark accents ----
        .with("text", rgb(0xab, 0xb2, 0xbf))
        .with("overlay", rgb(0x5c, 0x63, 0x70))
        .with("subtext", rgb(0x82, 0x89, 0x97))
        .with("green", rgb(0x98, 0xc3, 0x79))
        .with("purple", rgb(0xc6, 0x78, 0xdd))
        .with("yellow", rgb(0xe5, 0xc0, 0x7b))
        .with("orange", rgb(0xd1, 0x9a, 0x66))
        .with("blue", rgb(0x61, 0xaf, 0xef))
        .with("teal", rgb(0x56, 0xb6, 0xc2))
        .with("red", rgb(0xe0, 0x6c, 0x75))
        .with("maroon", rgb(0xbe, 0x50, 0x46))
        .with("pink", rgb(0xc6, 0x78, 0xdd))
        .with("cyan", rgb(0x56, 0xb6, 0xc2))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (dark) ----
        .with("base", rgb(0x28, 0x2c, 0x34))
        .with("mantle", rgb(0x23, 0x27, 0x2e))
        .with("crust", rgb(0x1e, 0x22, 0x27))
        .with("surface0", rgb(0x33, 0x37, 0x40))
        .with("surface1", rgb(0x3e, 0x44, 0x51))
        .with("surface2", rgb(0x4b, 0x52, 0x63))
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x2c, 0x31, 0x3a))
}

/// Atom **One Light** (official). Near-white base (`#fafafa`), dark slate
/// text, the deeper One-Light accent variants for contrast.
pub fn one_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- One Light accents ----
        .with("text", rgb(0x38, 0x3a, 0x42))
        .with("overlay", rgb(0xa0, 0xa1, 0xa7))
        .with("subtext", rgb(0x69, 0x6c, 0x77))
        .with("green", rgb(0x50, 0xa1, 0x4f))
        .with("purple", rgb(0xa6, 0x26, 0xa4))
        .with("yellow", rgb(0xc1, 0x84, 0x01))
        .with("orange", rgb(0x98, 0x66, 0x01))
        .with("blue", rgb(0x40, 0x78, 0xf2))
        .with("teal", rgb(0x01, 0x84, 0xbc))
        .with("red", rgb(0xe4, 0x50, 0x49))
        .with("maroon", rgb(0xca, 0x12, 0x43))
        .with("pink", rgb(0xa6, 0x26, 0xa4))
        .with("cyan", rgb(0x01, 0x84, 0xbc))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (LIGHT) ----
        .with("base", rgb(0xfa, 0xfa, 0xfa))
        .with("mantle", rgb(0xf0, 0xf0, 0xf0))
        .with("crust", rgb(0xe5, 0xe5, 0xe6))
        .with("surface0", rgb(0xe5, 0xe5, 0xe6))
        .with("surface1", rgb(0xd4, 0xd4, 0xd5))
        .with("surface2", rgb(0xc2, 0xc2, 0xc3))
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xd5, 0xe8, 0xc8))
        .with("diff.change.bg", rgb(0xf0, 0xe8, 0xc4))
        .with("diff.deletion.bg", rgb(0xf5, 0xd2, 0xd2))
        .with("diff.conflict.bg", rgb(0xee, 0xd2, 0xe8))
        .with("cursor_line.bg", rgb(0xee, 0xee, 0xee))
}

/// Everforest **dark** (official, medium contrast). Warm green-tinted
/// dark base (`#2d353b`), soft fg, the muted forest accents.
pub fn everforest_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Everforest dark accents (medium) ----
        .with("text", rgb(0xd3, 0xc6, 0xaa))
        .with("overlay", rgb(0x7a, 0x84, 0x78))
        .with("subtext", rgb(0x9d, 0xa9, 0xa0))
        .with("green", rgb(0xa7, 0xc0, 0x80))
        .with("purple", rgb(0xd6, 0x99, 0xb6))
        .with("yellow", rgb(0xdb, 0xbc, 0x7f))
        .with("orange", rgb(0xe6, 0x98, 0x75))
        .with("blue", rgb(0x7f, 0xbb, 0xb3))
        .with("teal", rgb(0x83, 0xc0, 0x92))
        .with("red", rgb(0xe6, 0x7e, 0x80))
        .with("maroon", rgb(0xd6, 0x69, 0x6b))
        .with("pink", rgb(0xd6, 0x99, 0xb6))
        .with("cyan", rgb(0x7f, 0xbb, 0xb3))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (dark) ----
        .with("base", rgb(0x2d, 0x35, 0x3b))
        .with("mantle", rgb(0x27, 0x2e, 0x33))
        .with("crust", rgb(0x23, 0x2a, 0x2e))
        .with("surface0", rgb(0x34, 0x3f, 0x44))
        .with("surface1", rgb(0x3d, 0x48, 0x4d))
        .with("surface2", rgb(0x47, 0x54, 0x58))
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x34, 0x3f, 0x44))
}

/// Everforest **light** (official, medium contrast). Warm cream base
/// (`#fdf6e3`), dark green-gray text, the deeper light-mode forest accents.
pub fn everforest_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Everforest light accents (medium) ----
        .with("text", rgb(0x5c, 0x6a, 0x72))
        .with("overlay", rgb(0xa6, 0xb0, 0xa0))
        .with("subtext", rgb(0x82, 0x91, 0x86))
        .with("green", rgb(0x8d, 0xa1, 0x01))
        .with("purple", rgb(0xdf, 0x69, 0xba))
        .with("yellow", rgb(0xdf, 0xa0, 0x00))
        .with("orange", rgb(0xf5, 0x7d, 0x26))
        .with("blue", rgb(0x35, 0x79, 0x6e))
        .with("teal", rgb(0x35, 0xa7, 0x7c))
        .with("red", rgb(0xf8, 0x55, 0x52))
        .with("maroon", rgb(0xc2, 0x36, 0x33))
        .with("pink", rgb(0xdf, 0x69, 0xba))
        .with("cyan", rgb(0x35, 0x79, 0x6e))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (LIGHT) ----
        .with("base", rgb(0xfd, 0xf6, 0xe3))
        .with("mantle", rgb(0xf4, 0xf0, 0xd9))
        .with("crust", rgb(0xef, 0xeb, 0xd4))
        .with("surface0", rgb(0xef, 0xeb, 0xd4))
        .with("surface1", rgb(0xe6, 0xe2, 0xcc))
        .with("surface2", rgb(0xbd, 0xc3, 0xaf))
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xdc, 0xe8, 0xc0))
        .with("diff.change.bg", rgb(0xf0, 0xe6, 0xbe))
        .with("diff.deletion.bg", rgb(0xf2, 0xd5, 0xc8))
        .with("diff.conflict.bg", rgb(0xea, 0xd6, 0xdc))
        .with("cursor_line.bg", rgb(0xef, 0xeb, 0xd4))
}

/// Rosé Pine **dark** (official "main"). Base `#191724`, soft `text`
/// `#e0def4`, the muted rose/pine/foam/iris accents.
pub fn rosepine_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Rosé Pine accents ----
        .with("text", rgb(0xe0, 0xde, 0xf4))
        .with("overlay", rgb(0x6e, 0x6a, 0x86))
        .with("subtext", rgb(0x90, 0x8c, 0xaa))
        .with("green", rgb(0x9c, 0xcf, 0xd8)) // foam (no true green; nearest)
        .with("purple", rgb(0xc4, 0xa7, 0xe7)) // iris
        .with("yellow", rgb(0xf6, 0xc1, 0x77)) // gold
        .with("orange", rgb(0xeb, 0xbc, 0xba)) // rose (warm) nearest orange
        .with("blue", rgb(0x31, 0x74, 0x8f)) // pine
        .with("teal", rgb(0x9c, 0xcf, 0xd8)) // foam
        .with("red", rgb(0xeb, 0x6f, 0x92)) // love
        .with("maroon", rgb(0xb4, 0x63, 0x7a))
        .with("pink", rgb(0xeb, 0xbc, 0xba)) // rose
        .with("cyan", rgb(0x9c, 0xcf, 0xd8)) // foam
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (dark) ----
        .with("base", rgb(0x19, 0x17, 0x24))
        .with("mantle", rgb(0x1f, 0x1d, 0x2e)) // surface
        .with("crust", rgb(0x15, 0x13, 0x20))
        .with("surface0", rgb(0x1f, 0x1d, 0x2e)) // surface
        .with("surface1", rgb(0x26, 0x23, 0x3a)) // overlay
        .with("surface2", rgb(0x40, 0x3d, 0x52)) // highlight med
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x21, 0x1f, 0x30))
}

/// Rosé Pine **Dawn** (official light flavour). Base `#faf4ed`, dark text
/// `#575279`, the warmer light-mode rose/pine/foam/iris accents.
pub fn rosepine_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Rosé Pine Dawn accents ----
        .with("text", rgb(0x57, 0x52, 0x79))
        .with("overlay", rgb(0x98, 0x93, 0xa5))
        .with("subtext", rgb(0x79, 0x74, 0x93))
        .with("green", rgb(0x56, 0x94, 0x9f)) // foam
        .with("purple", rgb(0x90, 0x7a, 0xa9)) // iris
        .with("yellow", rgb(0xea, 0x9d, 0x34)) // gold
        .with("orange", rgb(0xd7, 0x82, 0x7e)) // rose
        .with("blue", rgb(0x28, 0x69, 0x83)) // pine
        .with("teal", rgb(0x56, 0x94, 0x9f)) // foam
        .with("red", rgb(0xb4, 0x63, 0x7a)) // love
        .with("maroon", rgb(0x9d, 0x4f, 0x66))
        .with("pink", rgb(0xd7, 0x82, 0x7e)) // rose
        .with("cyan", rgb(0x56, 0x94, 0x9f)) // foam
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (LIGHT) ----
        .with("base", rgb(0xfa, 0xf4, 0xed))
        .with("mantle", rgb(0xff, 0xfa, 0xf3)) // surface
        .with("crust", rgb(0xf2, 0xe9, 0xe1))
        .with("surface0", rgb(0xff, 0xfa, 0xf3)) // surface
        .with("surface1", rgb(0xf4, 0xed, 0xe8)) // overlay
        .with("surface2", rgb(0xdf, 0xda, 0xd9)) // highlight med
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xdc, 0xe8, 0xcc))
        .with("diff.change.bg", rgb(0xf0, 0xe6, 0xc8))
        .with("diff.deletion.bg", rgb(0xf2, 0xd8, 0xd5))
        .with("diff.conflict.bg", rgb(0xea, 0xd8, 0xe2))
        .with("cursor_line.bg", rgb(0xf4, 0xed, 0xe8))
}

/// Monokai **dark** (classic). Base `#272822`, fg `#f8f8f2`, the iconic
/// pink/green/orange/blue/purple/yellow accents.
pub fn monokai_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Monokai accents ----
        .with("text", rgb(0xf8, 0xf8, 0xf2))
        .with("overlay", rgb(0x75, 0x71, 0x5e))
        .with("subtext", rgb(0xa5, 0xa2, 0x8f))
        .with("green", rgb(0xa6, 0xe2, 0x2e))
        .with("purple", rgb(0xae, 0x81, 0xff))
        .with("yellow", rgb(0xe6, 0xdb, 0x74))
        .with("orange", rgb(0xfd, 0x97, 0x1f))
        .with("blue", rgb(0x66, 0xd9, 0xef))
        .with("teal", rgb(0x66, 0xd9, 0xef))
        .with("red", rgb(0xf9, 0x26, 0x72))
        .with("maroon", rgb(0xd9, 0x1e, 0x5e))
        .with("pink", rgb(0xf9, 0x26, 0x72))
        .with("cyan", rgb(0x66, 0xd9, 0xef))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (dark) ----
        .with("base", rgb(0x27, 0x28, 0x22))
        .with("mantle", rgb(0x22, 0x23, 0x1e))
        .with("crust", rgb(0x1d, 0x1e, 0x19))
        .with("surface0", rgb(0x3e, 0x3d, 0x32))
        .with("surface1", rgb(0x49, 0x48, 0x3e))
        .with("surface2", rgb(0x75, 0x71, 0x5e))
        // ---- Specific tints (dark) ----
        .with("diff.add.bg", rgb(0, 50, 0))
        .with("diff.change.bg", rgb(50, 50, 0))
        .with("diff.deletion.bg", rgb(60, 0, 0))
        .with("diff.conflict.bg", rgb(60, 0, 60))
        .with("cursor_line.bg", rgb(0x3e, 0x3d, 0x32))
}

/// Monokai **light** (community-grade adaptation). Light base, dark text,
/// the Monokai accents deepened for legibility on a bright canvas. No
/// official Monokai light exists; this is a tuned community-style variant.
pub fn monokai_light_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- Monokai light accents (deepened) ----
        .with("text", rgb(0x29, 0x2a, 0x24))
        .with("overlay", rgb(0x9b, 0x97, 0x82))
        .with("subtext", rgb(0x6e, 0x6a, 0x57))
        .with("green", rgb(0x67, 0x8c, 0x0e))
        .with("purple", rgb(0x7c, 0x4d, 0xd8))
        .with("yellow", rgb(0xa6, 0x8c, 0x0c))
        .with("orange", rgb(0xc7, 0x66, 0x06))
        .with("blue", rgb(0x18, 0x8a, 0xa6))
        .with("teal", rgb(0x18, 0x8a, 0xa6))
        .with("red", rgb(0xd1, 0x0d, 0x52))
        .with("maroon", rgb(0xa8, 0x0a, 0x42))
        .with("pink", rgb(0xd1, 0x0d, 0x52))
        .with("cyan", rgb(0x18, 0x8a, 0xa6))
        // ---- ANSI-named chrome (theme-independent) ----
        .with("ansi.red", Color::Named(N::Red))
        .with("ansi.green", Color::Named(N::Green))
        .with("ansi.yellow", Color::Named(N::Yellow))
        .with("ansi.blue", Color::Named(N::Blue))
        .with("ansi.magenta", Color::Named(N::Magenta))
        .with("ansi.cyan", Color::Named(N::Cyan))
        .with("ansi.darkgray", Color::Named(N::DarkGray))
        // ---- Background family (LIGHT) ----
        .with("base", rgb(0xfa, 0xfa, 0xf4))
        .with("mantle", rgb(0xf1, 0xf1, 0xe8))
        .with("crust", rgb(0xe6, 0xe6, 0xda))
        .with("surface0", rgb(0xe6, 0xe6, 0xda))
        .with("surface1", rgb(0xd6, 0xd6, 0xc8))
        .with("surface2", rgb(0xc4, 0xc4, 0xb4))
        // ---- Specific tints (LIGHT) ----
        .with("diff.add.bg", rgb(0xdc, 0xe8, 0xc4))
        .with("diff.change.bg", rgb(0xf0, 0xe8, 0xbe))
        .with("diff.deletion.bg", rgb(0xf2, 0xd2, 0xd8))
        .with("diff.conflict.bg", rgb(0xea, 0xd2, 0xe2))
        .with("cursor_line.bg", rgb(0xe6, 0xe6, 0xda))
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
