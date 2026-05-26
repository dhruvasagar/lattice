//! One renderable row of cells.
//!
//! A `CellRow` corresponds to one *visible* row in the matrix.
//! When the buffer has folds, folded source lines do **not**
//! produce a row — the chunk's row vector skips them. Use
//! [`CellRow::source_line`] to recover the logical buffer line.

use std::sync::Arc;

use crate::cell::Cell;

/// Inlay-hint position record. `(orig_byte, char_width)`:
///
/// - `orig_byte` — utf-8 byte offset INTO THE ORIGINAL (pre-splice)
///   line where the inlay was inserted.
/// - `char_width` — number of cells the inlay occupies in the
///   spliced row.
///
/// Overlays (cursor, selection, diagnostic underline) take byte
/// positions in the source rope. To map a source byte → cell
/// column, the overlay walker scans this list and accumulates
/// `char_width` for every entry with `orig_byte ≤ target`. The
/// representation mirrors the TUI's existing inlay offset arrays
/// so S3's cutover is a structural swap, not a logic change.
pub type InlayOffset = (u32, u32);

/// One row of cells in the matrix. Immutable once built; cheap to
/// share via `Arc<CellRow>` across panes and frames.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CellRow {
    /// Body cells in left-to-right order, including any spliced
    /// inlay cells. Length is the rendered column count for this
    /// row (post-inlay).
    pub cells: Arc<[Cell]>,
    /// Logical line in the source buffer (0-based, pre-fold).
    /// Stable across edits *within* the line; rebuilt when the
    /// line itself changes.
    pub source_line: u32,
    /// Inlay positions for byte↔column remap (see [`InlayOffset`]).
    /// Empty when the row has no inlays.
    pub inlay_offsets: Arc<[InlayOffset]>,
}

impl CellRow {
    /// Construct a row from already-built cells + metadata. The
    /// cell-builder worker (S2) is the production caller; tests
    /// use this directly.
    pub fn new(
        cells: impl Into<Arc<[Cell]>>,
        source_line: u32,
        inlay_offsets: impl Into<Arc<[InlayOffset]>>,
    ) -> Self {
        Self {
            cells: cells.into(),
            source_line,
            inlay_offsets: inlay_offsets.into(),
        }
    }

    /// Empty row at a given source line. Used for visually-empty
    /// lines (truly-empty source lines, or fold-marker placeholders).
    pub fn empty(source_line: u32) -> Self {
        Self {
            cells: Arc::from([] as [Cell; 0]),
            source_line,
            inlay_offsets: Arc::from([] as [InlayOffset; 0]),
        }
    }

    /// Column count = post-inlay cell count.
    pub fn col_count(&self) -> u32 {
        self.cells.len() as u32
    }

    /// `true` when the row has no cells. Visually-empty source
    /// lines produce empty rows.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Map a source-byte (already char-resolved, see note) →
    /// combined cell column for this row. Used by overlay
    /// decoration computation to position cursor / selection /
    /// diagnostic underline quads. Returns the column *after* any
    /// inlays spliced before `byte`.
    ///
    /// Walks `inlay_offsets` linearly — fine for the typical
    /// 0–3 inlays per row. Pre-sort assumption: `inlay_offsets`
    /// must be sorted ascending by `orig_byte`. Construction in
    /// S2 maintains that invariant.
    ///
    /// Note on byte vs char: the cell-grid renderer's design
    /// target is ASCII source code where byte == char-column. For
    /// non-ASCII content the cell-builder pre-resolves byte → char
    /// position before calling overlays, so `byte` here is
    /// effectively a char-column count. This fn only adds inlay
    /// shifts.
    pub fn byte_to_combined_col(&self, byte: u32) -> u32 {
        let mut col = byte;
        for (orig_byte, width) in self.inlay_offsets.iter() {
            if *orig_byte <= byte {
                col = col.saturating_add(*width);
            } else {
                break;
            }
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii(b: u8) -> Cell {
        Cell::with_codepoint(b as u32)
    }

    #[test]
    fn empty_row_at_source_line() {
        let r = CellRow::empty(42);
        assert_eq!(r.source_line, 42);
        assert!(r.is_empty());
        assert_eq!(r.col_count(), 0);
        assert!(r.inlay_offsets.is_empty());
    }

    #[test]
    fn new_builds_from_slices() {
        let cells = vec![ascii(b'a'), ascii(b'b'), ascii(b'c')];
        let inlays = vec![(2u32, 1u32)];
        let r = CellRow::new(cells.clone(), 7, inlays.clone());
        assert_eq!(r.source_line, 7);
        assert_eq!(r.col_count(), 3);
        assert_eq!(r.cells.as_ref(), cells.as_slice());
        assert_eq!(r.inlay_offsets.as_ref(), inlays.as_slice());
    }

    #[test]
    fn byte_to_col_identity_without_inlays() {
        let r = CellRow::new(
            vec![ascii(b'h'), ascii(b'i')],
            0,
            Vec::<InlayOffset>::new(),
        );
        assert_eq!(r.byte_to_combined_col(0), 0);
        assert_eq!(r.byte_to_combined_col(1), 1);
        assert_eq!(r.byte_to_combined_col(2), 2);
    }

    #[test]
    fn byte_to_col_shifts_by_inlay_width() {
        // Inlay of width 3 inserted before byte 2.
        let r = CellRow::new(
            vec![
                ascii(b'a'),
                ascii(b'b'),
                ascii(b'?'),
                ascii(b'?'),
                ascii(b'?'),
                ascii(b'c'),
            ],
            0,
            vec![(2u32, 3u32)],
        );
        // Bytes before the inlay are unshifted.
        assert_eq!(r.byte_to_combined_col(0), 0);
        assert_eq!(r.byte_to_combined_col(1), 1);
        // Byte at the inlay position is shifted by the inlay width.
        assert_eq!(r.byte_to_combined_col(2), 5);
        assert_eq!(r.byte_to_combined_col(3), 6);
    }

    #[test]
    fn byte_to_col_handles_multiple_inlays() {
        let r = CellRow::new(
            vec![ascii(b'x'); 10],
            0,
            vec![(1u32, 2u32), (3u32, 1u32)],
        );
        assert_eq!(r.byte_to_combined_col(0), 0);
        assert_eq!(r.byte_to_combined_col(1), 3); // +2 from first inlay
        assert_eq!(r.byte_to_combined_col(2), 4); // still +2
        assert_eq!(r.byte_to_combined_col(3), 6); // +3 (both)
        assert_eq!(r.byte_to_combined_col(5), 8);
    }
}
