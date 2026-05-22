//! Cell + color + attribute types — the renderer-facing
//! representation of a terminal grid position.
//!
//! These mirror `alacritty_terminal`'s internal types but
//! re-exposed in Lattice's vocabulary so the rest of the
//! editor doesn't depend on alacritty_terminal's API surface
//! directly. A future libghostty swap (or another VT
//! substrate) can preserve this contract.

use std::fmt;

/// One grid cell. `Cell::default()` is "space char on default
/// fg/bg with no attributes" — what alacritty_terminal initialises
/// empty cells to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub attrs: CellAttrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: TerminalColor::Default,
            bg: TerminalColor::Default,
            attrs: CellAttrs::default(),
        }
    }
}

/// Per-cell text attributes (bold, italic, etc.). Bools rather
/// than bitflags so the renderer's adapter can `if attrs.bold
/// { … }` without bitwise math. Adding a new attribute is a
/// struct-field append.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CellAttrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub dim: bool,
    pub strikethrough: bool,
    pub blink: bool,
}

/// Terminal cell color. Covers every variant any VT/xterm
/// emulator emits:
///
/// - `Default` — "use the terminal's default fg/bg" (cell
///   carries no explicit color).
/// - `Named` — one of the 16 ANSI named palette entries.
/// - `Indexed` — 256-color palette index.
/// - `Rgb` — 24-bit truecolor.
///
/// TUI renderers degrade `Rgb` to nearest-`Indexed` when the
/// terminal lacks truecolor; GPUI consumes `Rgb` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColor {
    Default,
    Named(NamedColor),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// The 16 ANSI named palette colors (plus their bright
/// variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

/// Cursor shape (DECSCUSR + xterm SS3 style codes). Programs
/// like vim, ssh, modern shells issue these to differentiate
/// modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
    Hidden,
}

impl fmt::Display for CursorShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CursorShape::Block => f.write_str("block"),
            CursorShape::Underline => f.write_str("underline"),
            CursorShape::Bar => f.write_str("bar"),
            CursorShape::Hidden => f.write_str("hidden"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_default_is_space_with_default_colors() {
        let c = Cell::default();
        assert_eq!(c.ch, ' ');
        assert_eq!(c.fg, TerminalColor::Default);
        assert_eq!(c.bg, TerminalColor::Default);
        assert_eq!(c.attrs, CellAttrs::default());
    }

    #[test]
    fn cell_attrs_default_is_all_off() {
        let a = CellAttrs::default();
        assert!(!a.bold);
        assert!(!a.italic);
        assert!(!a.underline);
        assert!(!a.reverse);
        assert!(!a.dim);
        assert!(!a.strikethrough);
        assert!(!a.blink);
    }
}
