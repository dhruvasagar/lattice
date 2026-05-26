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
}
