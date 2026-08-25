//! IM.3 — the descriptor for an inline media block.
//!
//! An image (later: a LaTeX fragment, a chart) drawn where it appears in the
//! buffer. Design: `docs/dev/architecture/inline-media.md`.
//!
//! ## The shape is `BrandingBlock`'s, deliberately
//!
//! A media block is a contiguous group of virtual rows tagged
//! [`VirtualRowKind::MediaBlock`](crate::virtual_rows::VirtualRowKind::MediaBlock),
//! carrying ordinary cells that spell out the alt text. The GPUI peer
//! intercepts the group and paints an image over the region instead; the TUI
//! peer paints the cells it was given and needs **no code at all**.
//!
//! That is exactly how the dashboard's branding block already works, and it
//! is why the TUI stays a first-class peer for a feature it cannot render:
//! the fallback is not a special case bolted on afterwards, it is what the
//! rows literally contain.
//!
//! ## Why the descriptor names a path and not bytes
//!
//! Three reasons, and none of them is size alone:
//!
//! - **The UI thread must not decode.** A descriptor is cheap to build and
//!   cheap to publish; the read + decode happens off-thread (IM.4) and lands
//!   through the inbound primitive. Handing around bytes invites decoding
//!   wherever they are needed.
//! - **Capability gating stays host-side.** A plugin (IM.6) names a file and
//!   the *host* decides whether that plugin may read it. If the guest sent
//!   pixels, it could put anything on screen regardless of its `fs:read`
//!   grant.
//! - **The cache has a key.** `(path, mtime, target size)` is a cache key;
//!   an opaque buffer is not.
//!
//! ## Why `rows` is authoritative and `intrinsic` is not
//!
//! `rows` — the reserved display-row count — is what the core's scroll
//! arithmetic uses, and both peers agree on it. `intrinsic` is the image's
//! natural pixel size, known only once something has read the file header,
//! and only meaningful to a peer that draws pixels.
//!
//! Keeping the row count authoritative is what lets a document have the same
//! number of display rows on both peers while only one shows a picture. It is
//! also what stops a decoding image from reflowing the buffer: the block
//! reserves its space before its pixels exist.

use std::path::PathBuf;
use std::sync::Arc;

/// Where a block's pixels come from.
///
/// A single variant today. It is an enum rather than a bare `PathBuf` because
/// the producers already in view — an org `[[http://…]]` link, a generated
/// chart, a rendered LaTeX fragment — are not files on disk, and widening a
/// struct field later is a breaking change for every construction site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    /// A file on disk, already resolved to an absolute path by the producer.
    Path(PathBuf),
}

/// How the intrinsic size maps into the block's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaFit {
    /// Scale down to fit the box, preserving aspect ratio; never scale up.
    /// The default because upscaling a small diagram is worse than showing
    /// it small.
    #[default]
    Contain,
    /// Scale to the box's width, preserving aspect ratio, and let the row
    /// count follow from the result.
    Width,
}

/// An inline media block: what to draw, how big, and what to say instead.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaBlock {
    pub source: MediaSource,
    /// Natural size in pixels, once a header read has established it.
    /// `None` until then — and a block is perfectly usable meanwhile, which
    /// is the point: it reserves `rows` either way.
    pub intrinsic: Option<(u32, u32)>,
    pub fit: MediaFit,
    /// What the TUI shows and what a screen reader reads. Never empty in
    /// practice — [`MediaBlock::new`] falls back to the file name — because
    /// a blank box tells the user nothing about what they are missing.
    pub alt: String,
    /// IM.5: the block's drawn height in **line-heights**, once geometry is
    /// known — `None` until then.
    ///
    /// This is the number that makes the block genuinely variable-height
    /// rather than snapped to whole rows, and it is deliberately allowed to
    /// differ from the reserved row count. A 3.4-line-height image reserves
    /// 4 rows and draws 3.4 of them.
    ///
    /// Only the renderer that actually draws media populates it (through
    /// `RowWeights`), and only one renderer runs in a process — so there is
    /// no question of two peers disagreeing. What both peers *do* agree on is
    /// the row count, which is why `scroll` (a row index) still anchors the
    /// same line on each.
    pub height_lh: Option<f32>,
}

impl MediaBlock {
    /// A block for `path`, with `alt` defaulting to the file's name when the
    /// producer has nothing better.
    pub fn new(path: impl Into<PathBuf>, alt: Option<String>) -> Self {
        let path = path.into();
        let alt = alt.filter(|a| !a.trim().is_empty()).unwrap_or_else(|| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "image".to_string())
        });
        Self {
            source: MediaSource::Path(path),
            intrinsic: None,
            fit: MediaFit::default(),
            alt,
            height_lh: None,
        }
    }

    /// The path, when the source is one.
    pub fn path(&self) -> Option<&std::path::Path> {
        match &self.source {
            MediaSource::Path(p) => Some(p.as_path()),
        }
    }

    /// Height in **line-heights** — the drawn height once geometry is known,
    /// else the reserved row count.
    ///
    /// The fallback is what keeps a block harmless before its size is
    /// resolved and on any peer that does not draw media: it then costs the
    /// scroll walks exactly what its rows already cost, so introducing a
    /// block cannot move an existing scroll position.
    pub fn line_heights(&self, rows: u16) -> f32 {
        self.height_lh.unwrap_or(rows as f32)
    }
}

/// Shared handle — blocks are cloned into every row of their group.
pub type MediaBlockRef = Arc<MediaBlock>;

/// Build the virtual rows for a media block anchored below `anchor_line`.
///
/// The rows carry the alt text as ordinary cells, which is the whole of the
/// TUI's rendering: it paints what it is given and needs no media code. The
/// GPUI peer recognises the `kind` and paints an image over the region
/// instead — the `BrandingBlock` treatment.
///
/// `rows` is clamped to at least 1: a zero-row block would be invisible on
/// both peers while still occupying a slot in the matrix, which is a bug that
/// presents as "the image silently did nothing".
pub fn media_block_rows(
    block: MediaBlockRef,
    anchor_line: u32,
    rows: u16,
    width_cols: usize,
) -> Vec<crate::virtual_rows::VirtualRow> {
    use crate::virtual_rows::{AnchorPosition, VirtualRow, VirtualRowKind};
    let rows = rows.max(1);
    // The alt text goes on the block's middle row so it reads as a caption in
    // a box rather than a line of text with space under it.
    let label_row = (rows / 2) as usize;
    (0..rows as usize)
        .map(|i| {
            let text = if i == label_row {
                centred(&block.alt, width_cols)
            } else {
                String::new()
            };
            VirtualRow {
                anchor_line,
                position: AnchorPosition::Below,
                cells: text
                    .chars()
                    .map(|c| crate::cell::Cell::new(c as u32, 0, 0, 0))
                    .collect::<Vec<_>>()
                    .into(),
                height: 1,
                kind: VirtualRowKind::MediaBlock,
                bg: None,
                scales: None,
                // No gutter number: the block is not a source line, and a
                // repeated line number down the side of an image reads as
                // content that is not there.
                gutter_line: None,
                gutter_fg: None,
                media: Some(block.clone()),
            }
        })
        .collect()
}

/// Centre `text` in `width` columns, truncating rather than overflowing.
fn centred(text: &str, width: usize) -> String {
    let n = text.chars().count();
    if n >= width {
        return text.chars().take(width).collect();
    }
    let pad = (width - n) / 2;
    let mut out = " ".repeat(pad);
    out.push_str(text);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_falls_back_to_the_file_name() {
        let b = MediaBlock::new("/tmp/docs/diagram.png", None);
        assert_eq!(b.alt, "diagram.png");
        assert_eq!(
            b.path().unwrap(),
            std::path::Path::new("/tmp/docs/diagram.png")
        );
    }

    /// A blank alt is the same as no alt: an empty box tells the user nothing
    /// about what they cannot see.
    #[test]
    fn a_blank_alt_is_refused_in_favour_of_the_file_name() {
        assert_eq!(
            MediaBlock::new("/a/b/c.png", Some("   ".into())).alt,
            "c.png"
        );
        assert_eq!(
            MediaBlock::new("/a/b/c.png", Some(String::new())).alt,
            "c.png"
        );
        assert_eq!(
            MediaBlock::new("/a/b/c.png", Some("a wiring diagram".into())).alt,
            "a wiring diagram"
        );
    }

    /// Before IM.5 a block costs the scroll walks exactly what its rows cost,
    /// so introducing blocks cannot move any existing scroll position.
    #[test]
    fn a_block_costs_its_reserved_rows_until_natural_sizing_lands() {
        let b = MediaBlock::new("/x.png", None);
        for rows in [1u16, 4, 12] {
            assert_eq!(b.line_heights(rows), rows as f32);
        }
    }

    fn row_text(r: &crate::virtual_rows::VirtualRow) -> String {
        r.cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or(' '))
            .collect()
    }

    /// The block reserves exactly the rows it was asked for, and every one of
    /// them carries the shared descriptor — so a renderer meeting any row can
    /// paint the whole block without reassembling it.
    #[test]
    fn a_block_reserves_its_rows_and_every_row_knows_the_block() {
        let block = Arc::new(MediaBlock::new("/tmp/diagram.png", None));
        let rows = media_block_rows(block.clone(), 7, 5, 40);

        assert_eq!(rows.len(), 5, "five rows reserved");
        assert!(
            rows.iter().all(|r| r.anchor_line == 7
                && r.kind == crate::virtual_rows::VirtualRowKind::MediaBlock
                && r.media.as_deref() == Some(&*block)),
            "every row anchors to the source line and carries the descriptor"
        );
    }

    /// The TUI's entire rendering of a media block is the cells it is handed.
    /// This is what keeps it a first-class peer for a feature it cannot draw:
    /// the fallback is not a special case, it is the row content.
    #[test]
    fn the_alt_text_is_in_the_cells_so_the_tui_needs_no_media_code() {
        let block = Arc::new(MediaBlock::new("/tmp/x.png", Some("wiring diagram".into())));
        let rows = media_block_rows(block, 0, 3, 30);

        let joined: Vec<String> = rows.iter().map(row_text).collect();
        assert!(
            joined.iter().any(|t| t.contains("wiring diagram")),
            "alt text is painted as ordinary cells: {joined:?}"
        );
        // Centred on the middle row, so it reads as a caption in a box.
        assert!(
            joined[1].contains("wiring diagram"),
            "on the middle row, got {joined:?}"
        );
        assert!(joined[0].trim().is_empty() && joined[2].trim().is_empty());
    }

    /// A zero-row block would occupy a matrix slot while being invisible on
    /// both peers — a bug that presents as "the image silently did nothing".
    #[test]
    fn a_zero_row_block_is_clamped_rather_than_vanishing() {
        let block = Arc::new(MediaBlock::new("/tmp/x.png", None));
        assert_eq!(media_block_rows(block, 0, 0, 20).len(), 1);
    }

    /// The TUI gets a uniform map, so introducing media cannot move its
    /// scroll positions — it paints the alt-text cells in the reserved rows,
    /// and in a terminal those rows really are one line tall.
    #[test]
    fn a_non_drawing_renderer_gets_a_uniform_weight_map() {
        use crate::virtual_rows::{VirtualRowMatrix, VirtualRowVersion};
        let mut block = MediaBlock::new("/x.png", None);
        block.height_lh = Some(4.25);
        let rows = media_block_rows(Arc::new(block), 3, 5, 40);
        let m = VirtualRowMatrix::build(rows, 20, VirtualRowVersion::default());

        assert!(
            m.media_row_weights(false).is_uniform(),
            "a renderer that draws no pixels charges nothing extra"
        );
    }

    /// A drawing renderer charges the block's DRAWN height, which is
    /// fractional and differs from the rows it reserved — that is what makes
    /// the block variable-height rather than snapped.
    #[test]
    fn a_drawing_renderer_charges_the_blocks_drawn_height() {
        use crate::virtual_rows::{VirtualRowMatrix, VirtualRowVersion};
        let mut block = MediaBlock::new("/x.png", None);
        block.height_lh = Some(4.25);
        let rows = media_block_rows(Arc::new(block), 3, 5, 40);
        let m = VirtualRowMatrix::build(rows, 20, VirtualRowVersion::default());

        let w = m.media_row_weights(true);
        assert!(!w.is_uniform());
        // The anchor line costs its own row plus the drawn height, NOT the
        // five rows the block reserved.
        assert!(
            (w.cost(3, 6) - 5.25).abs() < 0.001,
            "expected 1 + 4.25, got {}",
            w.cost(3, 6)
        );
        assert_eq!(w.cost(4, 1), 1.0, "neighbouring lines are untouched");
    }

    /// Before its size is known a block costs its RESERVED rows, which is
    /// what stops a resolving image from reflowing the document under the
    /// reader.
    #[test]
    fn an_unresolved_block_costs_its_reserved_rows() {
        use crate::virtual_rows::{VirtualRowMatrix, VirtualRowVersion};
        let block = MediaBlock::new("/x.png", None); // height_lh: None
        let rows = media_block_rows(Arc::new(block), 2, 6, 40);
        let m = VirtualRowMatrix::build(rows, 20, VirtualRowVersion::default());

        let w = m.media_row_weights(true);
        assert!(
            (w.cost(2, 7) - 7.0).abs() < 0.001,
            "1 + 6 reserved rows, got {}",
            w.cost(2, 7)
        );
    }

    /// Alt text longer than the pane truncates rather than overflowing the
    /// row, which would corrupt the cell grid.
    #[test]
    fn long_alt_text_truncates_to_the_width() {
        let block = Arc::new(MediaBlock::new("/x.png", Some("a".repeat(80))));
        let rows = media_block_rows(block, 0, 1, 20);
        assert_eq!(row_text(&rows[0]).chars().count(), 20);
    }
}
