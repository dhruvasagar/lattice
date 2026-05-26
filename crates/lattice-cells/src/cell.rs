//! 16-byte cell type — the atom of the cell-grid renderer.
//!
//! Cells are cursor-invariant: anything that changes layout or
//! glyph content lives here; anything cursor-coupled lives in
//! `OverlayState` (S2). See
//! `docs/dev/architecture/cell-grid-renderer.md` § Decoration
//! assignment for the full rule.

/// Bit positions inside `Cell::flags`. Use the named constants;
/// raw bit math here would be a future maintenance hazard.
pub mod flags {
    /// Cell came from an inlay-hint splice rather than source
    /// text. The body codepoint is still the inlay's character;
    /// this bit only marks provenance so byte↔column remap can
    /// distinguish inlay positions from source bytes.
    pub const INLAY: u16 = 1 << 0;
    /// Whitespace-marker cell (e.g. tab indicator `→`, EOL `·`).
    /// Source text at this position was whitespace; the renderer
    /// substituted a marker glyph. Used by `listchars`-style
    /// rendering.
    pub const WS_MARKER: u16 = 1 << 1;
    /// S3.a (2026-05-26): text-attribute modifier — bold glyph.
    /// Set by the cell-builder from
    /// `host::Theme::syntax_style(style).modifiers.bold` so the
    /// renderer paints the cell with its font's bold weight.
    pub const BOLD: u16 = 1 << 2;
    /// Text-attribute modifier — italic glyph. From
    /// `host::Theme::syntax_style(style).modifiers.italic`.
    pub const ITALIC: u16 = 1 << 3;
    /// Text-attribute modifier — underlined glyph. From
    /// `host::Theme::syntax_style(style).modifiers.underline`.
    /// The renderer is responsible for the underline geometry
    /// (font baseline + 1px, etc.).
    pub const UNDERLINE: u16 = 1 << 4;
    /// Text-attribute modifier — dimmed cell. From
    /// `host::Theme::syntax_style(style).modifiers.dim`. The
    /// renderer typically blends fg toward the pane background.
    pub const DIM: u16 = 1 << 5;
    /// Text-attribute modifier — reverse video (swap fg/bg).
    /// From `host::Theme::syntax_style(style).modifiers.reverse`.
    pub const REVERSE: u16 = 1 << 6;
}

/// One renderable cell. Exactly 16 bytes: codepoint (4) + fg (4) +
/// bg (4) + flags (2) + padding (2). 4 cells per 64-byte cache
/// line, which makes row walks cache-friendly.
///
/// Construction is direct (`Cell { codepoint, fg, bg, flags, .. }`)
/// or via the [`Cell::blank`] / [`Cell::with_codepoint`]
/// constructors below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Cell {
    /// Unicode scalar value. `0` is the empty/blank cell sentinel.
    pub codepoint: u32,
    /// `0xRRGGBB`. Theme-resolved foreground colour.
    pub fg: u32,
    /// `0xRRGGBB`. Theme-resolved background colour; `0` is
    /// "transparent" → renderer uses the pane background.
    pub bg: u32,
    /// Bit flags from [`flags`].
    pub flags: u16,
    /// Padding so the struct is 16 bytes. Never read.
    _padding: u16,
}

impl Cell {
    /// All-zero cell: blank codepoint, transparent fg/bg, no
    /// flags. The default value for unused trailing cells in a
    /// shorter-than-row source line.
    pub const BLANK: Self = Self {
        codepoint: 0,
        fg: 0,
        bg: 0,
        flags: 0,
        _padding: 0,
    };

    /// Construct a default-coloured cell at `codepoint`. fg/bg
    /// default to 0 (renderer fills from theme); call sites that
    /// know colours should construct the struct literal directly.
    pub const fn with_codepoint(codepoint: u32) -> Self {
        Self {
            codepoint,
            fg: 0,
            bg: 0,
            flags: 0,
            _padding: 0,
        }
    }

    /// Convenience builder used in tests + cell-builder fixtures.
    /// Direct struct construction is the normal path.
    pub const fn new(codepoint: u32, fg: u32, bg: u32, flags: u16) -> Self {
        Self {
            codepoint,
            fg,
            bg,
            flags,
            _padding: 0,
        }
    }

    /// `true` when the cell has the blank-sentinel codepoint. Does
    /// not consider fg/bg/flags — a blank-codepoint cell with a
    /// non-zero bg is still considered blank for content purposes
    /// (it's used for trailing-cell padding within a row).
    pub fn is_blank(&self) -> bool {
        self.codepoint == 0
    }

    /// `true` when this cell came from an inlay-hint splice.
    pub fn is_inlay(&self) -> bool {
        self.flags & flags::INLAY != 0
    }

    /// `true` when this cell is a whitespace marker glyph.
    pub fn is_ws_marker(&self) -> bool {
        self.flags & flags::WS_MARKER != 0
    }

    /// S3.a: `true` iff the bold modifier bit is set.
    pub fn is_bold(&self) -> bool {
        self.flags & flags::BOLD != 0
    }

    /// S3.a: `true` iff the italic modifier bit is set.
    pub fn is_italic(&self) -> bool {
        self.flags & flags::ITALIC != 0
    }

    /// S3.a: `true` iff the underline modifier bit is set.
    pub fn is_underline(&self) -> bool {
        self.flags & flags::UNDERLINE != 0
    }

    /// S3.a: `true` iff the dim modifier bit is set.
    pub fn is_dim(&self) -> bool {
        self.flags & flags::DIM != 0
    }

    /// S3.a: `true` iff the reverse modifier bit is set.
    pub fn is_reverse(&self) -> bool {
        self.flags & flags::REVERSE != 0
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::BLANK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Load-bearing: `Cell` must be 16 bytes for the cache-line
    /// argument in `cell-grid-renderer.md` to hold. If this assert
    /// fires, adjust padding before doing anything else.
    #[test]
    fn cell_is_16_bytes() {
        assert_eq!(std::mem::size_of::<Cell>(), 16);
        assert_eq!(std::mem::align_of::<Cell>(), 4);
    }

    #[test]
    fn blank_has_zero_codepoint() {
        let c = Cell::BLANK;
        assert!(c.is_blank());
        assert_eq!(c.codepoint, 0);
        assert_eq!(c.fg, 0);
        assert_eq!(c.bg, 0);
        assert_eq!(c.flags, 0);
    }

    #[test]
    fn default_equals_blank() {
        assert_eq!(Cell::default(), Cell::BLANK);
    }

    #[test]
    fn with_codepoint_only_sets_codepoint() {
        let c = Cell::with_codepoint(b'x' as u32);
        assert_eq!(c.codepoint, b'x' as u32);
        assert_eq!(c.fg, 0);
        assert_eq!(c.bg, 0);
        assert_eq!(c.flags, 0);
        assert!(!c.is_blank());
    }

    #[test]
    fn new_with_colors_and_flags() {
        let c = Cell::new(b'a' as u32, 0xcdd6f4, 0x1e1e2e, flags::INLAY);
        assert_eq!(c.codepoint, b'a' as u32);
        assert_eq!(c.fg, 0xcdd6f4);
        assert_eq!(c.bg, 0x1e1e2e);
        assert!(c.is_inlay());
        assert!(!c.is_ws_marker());
        assert!(!c.is_blank());
    }

    #[test]
    fn ws_marker_flag_independent_of_inlay() {
        let c = Cell::new(b'.' as u32, 0, 0, flags::WS_MARKER);
        assert!(c.is_ws_marker());
        assert!(!c.is_inlay());
        let c2 = Cell::new(b':' as u32, 0, 0, flags::INLAY | flags::WS_MARKER);
        assert!(c2.is_ws_marker());
        assert!(c2.is_inlay());
    }

    /// S3.a: each modifier-flag bit toggles independently and its
    /// query helper returns the right answer in isolation + when
    /// combined with other flags.
    #[test]
    fn modifier_flag_bits_compose_independently() {
        // Each modifier alone.
        let bold = Cell::new(b'a' as u32, 0, 0, flags::BOLD);
        assert!(bold.is_bold());
        assert!(!bold.is_italic());
        assert!(!bold.is_underline());
        assert!(!bold.is_dim());
        assert!(!bold.is_reverse());

        let italic = Cell::new(b'a' as u32, 0, 0, flags::ITALIC);
        assert!(italic.is_italic());
        assert!(!italic.is_bold());

        let under = Cell::new(b'a' as u32, 0, 0, flags::UNDERLINE);
        assert!(under.is_underline());

        let dim = Cell::new(b'a' as u32, 0, 0, flags::DIM);
        assert!(dim.is_dim());

        let rev = Cell::new(b'a' as u32, 0, 0, flags::REVERSE);
        assert!(rev.is_reverse());

        // Composition: bold + italic + underline + INLAY all set.
        let all = Cell::new(
            b'a' as u32,
            0,
            0,
            flags::BOLD | flags::ITALIC | flags::UNDERLINE | flags::INLAY,
        );
        assert!(all.is_bold());
        assert!(all.is_italic());
        assert!(all.is_underline());
        assert!(all.is_inlay());
        assert!(!all.is_dim());
        assert!(!all.is_reverse());
        assert!(!all.is_ws_marker());
    }

    /// Modifier bits don't collide with the INLAY / WS_MARKER bits
    /// (sanity check for future flag additions).
    #[test]
    fn flag_bits_dont_overlap() {
        let all = [
            flags::INLAY,
            flags::WS_MARKER,
            flags::BOLD,
            flags::ITALIC,
            flags::UNDERLINE,
            flags::DIM,
            flags::REVERSE,
        ];
        let mut seen: u16 = 0;
        for f in all {
            assert!(
                seen & f == 0,
                "flag {f:#06x} overlaps an earlier flag in {seen:#06x}"
            );
            seen |= f;
        }
    }
}
