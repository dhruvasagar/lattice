//! Virtual rows: a sibling lane to [`crate::CellMatrix`] for
//! rows that **displace** content vertically without belonging
//! to the source rope.
//!
//! Examples of virtual rows: diff deletion blocks (D.3),
//! multibuffer excerpt headers and separators (M.2), LSP inlay
//! hints that occupy a row of their own, code-lens summaries
//! above a function declaration. Each is a row of [`Cell`]s
//! that appears at a specific anchor source line, either
//! immediately *above* or *below* that line.
//!
//! D.0a (this module) ships the primitive's data layer + an
//! interleaving [`crate::DisplaySliceIter`] over
//! [`CellMatrix`]. The first production consumer is D.3
//! (inline diff overlay) per
//! `docs/dev/architecture/diff-system.md`; the multibuffer
//! consumer is M.2 per `multibuffer-views.md`. D.0a itself
//! has no production renderer caller -- the iterator is
//! validated end-to-end by tests + bench against real
//! [`CellMatrix`] inputs.
//!
//! Design anchor:
//! `docs/dev/architecture/virtual-rows.md`.

use std::sync::Arc;

use crate::cell::Cell;

/// Where a virtual row sits relative to its anchor source
/// line.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AnchorPosition {
    /// The virtual row paints immediately above the anchor
    /// source line. Multiple `Above` rows at the same anchor
    /// paint in their `VirtualRowMatrix.rows` insertion
    /// order.
    Above,
    /// The virtual row paints immediately below the anchor
    /// source line. Multiple `Below` rows at the same anchor
    /// paint in their `VirtualRowMatrix.rows` insertion
    /// order.
    Below,
}

/// D.6.i (2026-05-31): which kind of virtual row this is,
/// for renderer-side backdrop / decoration discrimination.
///
/// Two production kinds today:
/// - `DeletionBlock` — a diff deletion-block row (D.3 inline
///   overlay) carrying baseline content that's gone from
///   the current side. Painted with the
///   `host_theme.diff_deletion_block_bg` backdrop (default:
///   faint dark red) so the user sees "this content
///   existed in baseline but is gone in current".
/// - `Filler` — a blank padding row (D.4.c / D.6.b
///   side-by-side alignment) on the shorter side of a hunk
///   so parallel rows line up across panes. Should paint
///   with **no backdrop** (or a neutral one) — fillers are
///   visual padding, not content; the deletion-block red
///   would mis-read them as "deleted lines."
///
/// `Generic` is the default for any other virtual-row
/// source (future code-lens, inlay-line, multibuffer
/// excerpt header). Renderers treat it like a deletion
/// block for backdrop purposes today; the variant exists
/// so future kinds can join the discriminator without a
/// breaking change.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum VirtualRowKind {
    /// Default — any virtual row not explicitly tagged.
    /// Painted with the deletion-block backdrop today.
    #[default]
    Generic,
    /// Diff deletion-block row (D.3). Baseline content
    /// that was removed from the current side; paints
    /// with the deletion-block backdrop.
    DeletionBlock,
    /// Side-by-side alignment filler (D.4.c / D.6.b).
    /// Blank padding; no backdrop.
    Filler,
    /// Sticky header row — always rendered at the top of the pane
    /// regardless of scroll position. Excluded from the per-line
    /// `virtual_rows_at` pass so it is never double-painted when its
    /// anchor line is in the viewport. Use `VirtualRow::bg` to supply
    /// a background colour; falls back to no backdrop if `bg` is `None`.
    Sticky,
    /// MG.26b: an annotation row — content that scrolls with its
    /// anchor and paints **no backdrop** of its own.
    ///
    /// Distinct from [`Generic`](Self::Generic), which carries the
    /// diff deletion-block backdrop: a blame chunk heading painted
    /// with that would read as a removed line. Distinct from
    /// [`Filler`](Self::Filler), which is blank alignment padding
    /// rather than something to read, and from
    /// [`Sticky`](Self::Sticky), which pins to the top of the pane —
    /// an annotation belongs *with* the lines it describes and has to
    /// scroll away with them. `VirtualRow::bg` still supplies a
    /// background where one is wanted.
    Annotation,
    /// Dashboard branding block (DB.4-gpui). A contiguous group of
    /// these rows carries the mark's block cells + the wordmark/tagline
    /// text; the **GPUI peer** intercepts the group and paints a 2-D
    /// composition instead of the flat cells — the mark as crisp square
    /// quads (corner cuts preserved) and the "Lattice" wordmark shaped
    /// large, vertically centred beside the mark. The **TUI peer** paints
    /// the cells normally (its terminal-art treatment). A *paint*
    /// discriminant only — never a motion/scroll/cursor branch.
    BrandingBlock,
    /// IM.3: an inline media block — an image drawn where it appears in the
    /// buffer. A contiguous group of these rows carries the alt text as
    /// ordinary cells; the **GPUI peer** intercepts the group and paints the
    /// image over the region, the **TUI peer** paints the cells it was given
    /// and needs no code of its own. The `BrandingBlock` treatment exactly,
    /// which is what keeps the TUI a first-class peer for a feature it
    /// cannot render.
    ///
    /// Scrolls with its anchor (an image belongs *with* the line that
    /// references it), so unlike `BrandingBlock` it is NOT pinned. A *paint*
    /// discriminant plus a height contribution — never a motion or cursor
    /// branch.
    MediaBlock,
}

impl VirtualRowKind {
    /// Whether rows of this kind are *pinned* to the top of the pane
    /// (rendered in the sticky pre-pass, excluded from the scrolling
    /// per-line pass, and reserved out of the visible window) rather than
    /// scrolling with the document.
    ///
    /// `Sticky` is the general headerline (multibuffer excerpt headers,
    /// async-status HUD). `BrandingBlock` — the dashboard logo — is pinned
    /// too: it is a masthead that should stay put while the sections
    /// beneath it scroll, and it keeps its 2-D paint treatment either way.
    pub fn is_pinned(self) -> bool {
        matches!(self, VirtualRowKind::Sticky | VirtualRowKind::BrandingBlock)
    }
}

/// One virtual row's anchor + content.
///
/// `anchor_line` is the 0-based source line this row attaches
/// to. `position` selects Above or Below the anchor.
/// `cells` is the rendered row content -- same `Arc<[Cell]>`
/// shape that backs a document [`crate::CellRow`], so
/// renderers can paint virtual rows through the same fast
/// path with no special casing.
///
/// `height` is the row's vertical span in matrix-row units
/// (`1` for the common case; values > 1 reserved for
/// multi-line code-lens / signature-preview blocks that paint
/// taller than one cell row).
///
/// `kind` (D.6.i) tags the row's provenance so renderers
/// pick the right backdrop / decoration treatment without
/// guessing from cell content.
///
/// `bg` overrides the kind-based default background when
/// `Some(rgb_u32)` (`0xRRGGBB`). `None` → renderer picks from
/// `kind` (deletion-block red for `DeletionBlock`/`Generic`,
/// transparent for `Filler`/`Sticky`).
#[derive(Clone, Debug)]
pub struct VirtualRow {
    pub anchor_line: u32,
    pub position: AnchorPosition,
    pub cells: Arc<[Cell]>,
    pub height: u16,
    pub kind: VirtualRowKind,
    pub bg: Option<u32>,
    /// F.3 (Thread F): per-display-column font scale in
    /// **hundredths** (`100` = 1.0×, the base size), parallel to
    /// [`Self::cells`] by display column. `None` ⇒ the whole row
    /// is base size (the common case — zero cost, no allocation).
    ///
    /// This extends the variable-font commitment (per-token
    /// scaling, the emacs markdown-heading model — only the title
    /// scales, not the leading markers) from document rows to
    /// virtual rows. A renderer coalesces contiguous equal scales
    /// into runs (mirroring how it coalesces per-cell `fg`), shapes
    /// each run at `font_size × scale/100` on a **shared baseline**,
    /// and grows the row height to the tallest run. The dashboard
    /// branding block (DB.4-gpui) is the first consumer: the
    /// "Lattice" wordmark scales while the mark blocks stay base.
    ///
    /// The GPUI peer honors it; the TUI peer ignores it (a terminal
    /// cell grid cannot vary font size). When present, its length
    /// matches `cells`; a shorter/absent entry defaults to `100`.
    pub scales: Option<Arc<[u16]>>,
    /// IM.3: the media block this row belongs to, when
    /// [`kind`](Self::kind) is
    /// [`MediaBlock`](VirtualRowKind::MediaBlock).
    ///
    /// Shared across every row of a group, so a renderer that meets any row
    /// of the block can paint the whole thing without reassembling it from
    /// the cells. `None` for every other kind — one `Option<Arc>` per row,
    /// which is what `scales` already costs.
    pub media: Option<crate::media::MediaBlockRef>,
    /// TC.8: the SOURCE line this row should show in its gutter (0-based;
    /// renderers add one, as they do for document rows). `None` — the default
    /// for every kind — paints the blank gutter virtual rows have always had.
    ///
    /// Deliberately separate from [`Self::anchor_line`]. Anchoring answers
    /// "where does this row sit"; this answers "what number does it show", and
    /// for most virtual rows the honest answer is *nothing*: a deletion block
    /// has no current-side line, and a filler row has no line at all. A sticky
    /// context row is the case where the two differ in the other direction —
    /// it is anchored above the viewport but shows its own place in the file.
    ///
    /// It is a field rather than a renderer-side `match vrow.kind` because the
    /// renderers must not branch on kind: any producer that knows a real line
    /// number can set this and get the document gutter for free.
    pub gutter_line: Option<u32>,
    /// TC.11: foreground for [`Self::gutter_line`]'s digits (`0xRRGGBB`).
    /// `None` — the default — leaves the renderer's own gutter colour, which
    /// is what document rows use.
    pub gutter_fg: Option<u32>,
}

/// F.3 (Thread F): one contiguous run of display columns rendered
/// at a single font scale, produced by [`coalesce_scales`]. The
/// renderer shapes each run at `font_size × scale/100`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ScaleRun {
    /// First display column of the run (0-based, into the row's
    /// cells).
    pub start_col: u32,
    /// Number of display columns the run spans.
    pub cols: u32,
    /// Font scale in hundredths (`100` = 1.0×).
    pub scale: u16,
}

/// Base font scale in hundredths (`100` = 1.0×). A column with this
/// scale (or no scale entry) renders at the base font size.
pub const BASE_SCALE: u16 = 100;

/// F.3 (Thread F): coalesce a per-column `scales` slice into
/// contiguous same-scale [`ScaleRun`]s across `total_cols` columns,
/// exactly as a renderer coalesces per-cell `fg` into text runs.
///
/// Columns beyond `scales.len()` (or when `scales` is empty) default
/// to [`BASE_SCALE`]. The returned runs cover `0..total_cols`
/// contiguously, in column order, and never split two adjacent
/// columns that share a scale. `total_cols == 0` ⇒ empty. Pure and
/// allocation-light (O(total_cols)); unit-testable without a
/// renderer.
pub fn coalesce_scales(scales: &[u16], total_cols: u32) -> Vec<ScaleRun> {
    let mut runs: Vec<ScaleRun> = Vec::new();
    if total_cols == 0 {
        return runs;
    }
    let scale_at = |col: u32| -> u16 {
        scales
            .get(col as usize)
            .copied()
            .filter(|s| *s != 0)
            .unwrap_or(BASE_SCALE)
    };
    let mut start = 0u32;
    let mut cur = scale_at(0);
    for col in 1..total_cols {
        let s = scale_at(col);
        if s != cur {
            runs.push(ScaleRun {
                start_col: start,
                cols: col - start,
                scale: cur,
            });
            start = col;
            cur = s;
        }
    }
    runs.push(ScaleRun {
        start_col: start,
        cols: total_cols - start,
        scale: cur,
    });
    runs
}

/// A monotonically-increasing counter; bumped by the
/// publisher whenever the [`VirtualRowMatrix`] is replaced.
///
/// Consumers compare versions across frames to invalidate
/// caches. A single `u64` is sufficient because virtual rows
/// have only one source of change (provider mutation); unlike
/// [`crate::MatrixVersion`], they don't need multiple axes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct VirtualRowVersion(pub u64);

impl VirtualRowVersion {
    pub const ZERO: Self = Self(0);

    /// Returns the next version (wrapping on overflow, which
    /// won't happen in any realistic session: 1 publish per
    /// frame at 240Hz for 2 billion years would be needed).
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// The published virtual-row lane for one document.
///
/// Immutable once built; the publisher replaces the `Arc<…>`
/// when providers mutate. Cheap to clone (Arc bump).
///
/// `rows` is sorted by `(anchor_line, position)` with
/// `Above` < `Below` at the same line. `line_index[i]` is the
/// index of the first row in `rows` whose `anchor_line >= i`;
/// length is `source_line_count + 1`. The line index turns
/// "how many virtual rows are anchored before line L" into a
/// constant-time array lookup, which the
/// [`crate::DisplaySliceIter`] uses to fast-forward past
/// scrolled-off virtual rows in O(1) instead of O(V).
#[derive(Clone, Debug)]
pub struct VirtualRowMatrix {
    pub rows: Arc<[VirtualRow]>,
    pub line_index: Arc<[u32]>,
    pub source_line_count: u32,
    pub version: VirtualRowVersion,
}

impl Default for VirtualRowMatrix {
    fn default() -> Self {
        Self::empty()
    }
}

impl VirtualRowMatrix {
    /// The empty matrix. The initial published value before
    /// any provider has emitted.
    pub fn empty() -> Self {
        Self {
            rows: Arc::from([] as [VirtualRow; 0]),
            line_index: Arc::from([0u32]),
            source_line_count: 0,
            version: VirtualRowVersion::ZERO,
        }
    }

    /// `true` when no virtual rows are present. The
    /// [`crate::CellMatrix::display_slice`] fast path detects
    /// this to skip interleaver overhead entirely.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Build a `VirtualRowMatrix` from an unsorted list. The
    /// rows are sorted by `(anchor_line, position)` and the
    /// `line_index` is computed.
    ///
    /// `source_line_count` should match the document's line
    /// count (so the line-index sentinel covers EOF). If a
    /// virtual row anchors past `source_line_count`, it is
    /// clamped to anchor at `source_line_count` (treated as
    /// "past EOF" by the interleaver, which emits it after
    /// the last document row).
    pub fn build(
        mut rows: Vec<VirtualRow>,
        source_line_count: u32,
        version: VirtualRowVersion,
    ) -> Self {
        for row in &mut rows {
            if row.anchor_line > source_line_count {
                row.anchor_line = source_line_count;
            }
        }
        rows.sort_by(|a, b| {
            a.anchor_line
                .cmp(&b.anchor_line)
                .then_with(|| position_rank(a.position).cmp(&position_rank(b.position)))
        });

        let line_index_len = source_line_count.saturating_add(1) as usize;
        let mut line_index = Vec::with_capacity(line_index_len);
        let mut row_idx: u32 = 0;
        for line in 0..line_index_len as u32 {
            while (row_idx as usize) < rows.len() && rows[row_idx as usize].anchor_line < line {
                row_idx += 1;
            }
            line_index.push(row_idx);
        }

        Self {
            rows: Arc::from(rows),
            line_index: Arc::from(line_index),
            source_line_count,
            version,
        }
    }

    /// Index of the first row in `rows` whose `anchor_line >=
    /// line`. Returns `rows.len() as u32` when every row
    /// anchors strictly below `line`.
    ///
    /// O(1) array lookup when `line <= source_line_count`;
    /// returns `rows.len()` for queries past EOF.
    pub fn first_row_at_or_after(&self, line: u32) -> u32 {
        let idx = (line as usize).min(self.line_index.len().saturating_sub(1));
        self.line_index[idx]
    }

    /// Number of virtual rows whose anchor sits in the inclusive
    /// document-line range `[lo, hi]`, regardless of
    /// [`AnchorPosition`]. Returns `0` when `lo > hi`.
    ///
    /// O(1) — two [`Self::first_row_at_or_after`] lookups. This is
    /// the geometry primitive the host's scroll model uses to
    /// answer "how many *display* rows does the document-line span
    /// `[lo, hi]` occupy", since each interleaved virtual row
    /// consumes a display row without being a document line. The
    /// count is position-agnostic on purpose: a bottom-anchored
    /// scroll over-reserves by at most the cursor line's own
    /// `Below` rows, which is the safe direction (the last line is
    /// guaranteed clear of the modeline rather than flush against
    /// it).
    ///
    /// [`VirtualRowKind::Sticky`] rows are excluded — they are
    /// rendered at the pane top outside the scroll window and do
    /// not displace content rows.
    pub fn virtual_rows_in_line_range(&self, lo: u32, hi: u32) -> u32 {
        if lo > hi {
            return 0;
        }
        let end = self.first_row_at_or_after(hi.saturating_add(1));
        let start = self.first_row_at_or_after(lo);
        let total = end.saturating_sub(start);
        let sticky = self.rows[start as usize..end as usize]
            .iter()
            .filter(|r| r.kind.is_pinned())
            .count() as u32;
        total.saturating_sub(sticky)
    }

    /// Iterator over all pinned rows in the matrix (see
    /// [`VirtualRowKind::is_pinned`] — `Sticky` headerlines + the
    /// `BrandingBlock` masthead). Used by renderers to paint the fixed top
    /// strip before the scrollable content window.
    pub fn sticky_rows(&self) -> impl Iterator<Item = &VirtualRow> {
        self.rows.iter().filter(|r| r.kind.is_pinned())
    }
}

/// Sort-order helper: `Above` < `Below` at the same anchor
/// line.
const fn position_rank(p: AnchorPosition) -> u8 {
    match p {
        AnchorPosition::Above => 0,
        AnchorPosition::Below => 1,
    }
}

/// Stable identity for a [`VirtualRowProvider`]. Issued by
/// the worker / subsystem that owns the provider registry.
pub type ProviderId = u64;

/// A producer of virtual rows.
///
/// Producers are registered with the (future) virtual-rows
/// worker, which calls [`Self::collect`] when rebuilding the
/// published [`VirtualRowMatrix`]. The worker merges the
/// outputs of all registered providers, sorts, and publishes
/// via `ArcSwap`.
///
/// D.0a ships the trait; the worker itself lands in D.0a.1
/// (or as part of D.3 when the first production consumer
/// appears, whichever ships first). Tests build
/// `VirtualRowMatrix` directly via [`VirtualRowMatrix::build`]
/// rather than through a provider registry.
pub trait VirtualRowProvider: Send + Sync + std::fmt::Debug {
    /// A stable id for this provider. Used by the worker to
    /// deduplicate registration + route mutation
    /// notifications.
    fn id(&self) -> ProviderId;

    /// Monotonic version counter — the provider bumps it
    /// whenever the rows [`Self::collect`] would emit have
    /// changed. The worker uses the combined fingerprint of all
    /// providers' versions to short-circuit on the cache-hit
    /// path without paying for the (potentially expensive)
    /// `collect` calls.
    ///
    /// D.0a.1 introduces this. Implementations whose row set is
    /// truly static may return `0` forever — the worker will
    /// then cache-hit unless some other provider's version
    /// changes or the document's line count changes.
    fn version(&self) -> u64;

    /// Emit the current set of virtual rows for the
    /// associated document.
    ///
    /// Called by the worker on its rebuild path. Providers
    /// must not block; non-trivial computation belongs in the
    /// provider's own background task with the result cached
    /// here.
    fn collect(&self) -> Vec<VirtualRow>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(anchor: u32, pos: AnchorPosition) -> VirtualRow {
        VirtualRow {
            media: None,
            anchor_line: anchor,
            position: pos,
            cells: Arc::from([] as [Cell; 0]),
            height: 1,
            kind: VirtualRowKind::Generic,
            bg: None,
            scales: None,
            gutter_line: None,
            gutter_fg: None,
        }
    }

    #[test]
    fn coalesce_scales_empty_is_empty() {
        assert!(coalesce_scales(&[], 0).is_empty());
        assert!(coalesce_scales(&[150, 150], 0).is_empty());
    }

    #[test]
    fn coalesce_scales_no_scales_is_one_base_run() {
        // No per-column scales ⇒ one base-size run spanning the row.
        let runs = coalesce_scales(&[], 5);
        assert_eq!(
            runs,
            vec![ScaleRun {
                start_col: 0,
                cols: 5,
                scale: BASE_SCALE
            }]
        );
    }

    #[test]
    fn coalesce_scales_splits_only_on_transition() {
        // The markdown-heading shape: base markers, scaled title —
        // "## " at base, the rest at 1.6×. Two runs, split at col 3.
        let scales = [100, 100, 100, 160, 160, 160];
        let runs = coalesce_scales(&scales, 6);
        assert_eq!(
            runs,
            vec![
                ScaleRun {
                    start_col: 0,
                    cols: 3,
                    scale: 100
                },
                ScaleRun {
                    start_col: 3,
                    cols: 3,
                    scale: 160
                },
            ]
        );
    }

    #[test]
    fn coalesce_scales_handles_multiple_runs_and_zero_sentinel() {
        // A general per-token row: base, scaled, base again — plus a
        // `0` sentinel column that defaults to base, and a trailing
        // column past `scales.len()` that also defaults to base.
        let scales = [100, 250, 250, 0];
        let runs = coalesce_scales(&scales, 6);
        assert_eq!(
            runs,
            vec![
                ScaleRun {
                    start_col: 0,
                    cols: 1,
                    scale: 100
                },
                ScaleRun {
                    start_col: 1,
                    cols: 2,
                    scale: 250
                },
                ScaleRun {
                    start_col: 3,
                    cols: 3,
                    scale: 100
                },
            ]
        );
    }

    #[test]
    fn empty_matrix_basics() {
        let m = VirtualRowMatrix::empty();
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
        assert_eq!(m.source_line_count, 0);
        assert_eq!(m.version, VirtualRowVersion::ZERO);
        // line_index has one sentinel entry.
        assert_eq!(m.line_index.len(), 1);
        assert_eq!(m.first_row_at_or_after(0), 0);
        assert_eq!(m.first_row_at_or_after(100), 0);
    }

    #[test]
    fn build_sorts_by_anchor_and_position() {
        // Insertion order: (5, Below), (3, Above), (5, Above), (3, Below).
        // Sorted: (3, Above), (3, Below), (5, Above), (5, Below).
        let rows = vec![
            row(5, AnchorPosition::Below),
            row(3, AnchorPosition::Above),
            row(5, AnchorPosition::Above),
            row(3, AnchorPosition::Below),
        ];
        let m = VirtualRowMatrix::build(rows, 10, VirtualRowVersion(1));
        assert_eq!(m.len(), 4);
        assert_eq!(m.rows[0].anchor_line, 3);
        assert_eq!(m.rows[0].position, AnchorPosition::Above);
        assert_eq!(m.rows[1].anchor_line, 3);
        assert_eq!(m.rows[1].position, AnchorPosition::Below);
        assert_eq!(m.rows[2].anchor_line, 5);
        assert_eq!(m.rows[2].position, AnchorPosition::Above);
        assert_eq!(m.rows[3].anchor_line, 5);
        assert_eq!(m.rows[3].position, AnchorPosition::Below);
    }

    #[test]
    fn line_index_locates_rows() {
        let rows = vec![
            row(2, AnchorPosition::Above),
            row(2, AnchorPosition::Below),
            row(5, AnchorPosition::Above),
            row(7, AnchorPosition::Below),
        ];
        let m = VirtualRowMatrix::build(rows, 10, VirtualRowVersion(1));

        // No rows anchor before line 0..2 ⇒ index 0 (first row
        // is at line 2).
        assert_eq!(m.first_row_at_or_after(0), 0);
        assert_eq!(m.first_row_at_or_after(2), 0);
        // Past line 2's two rows ⇒ index 2 (next is line 5).
        assert_eq!(m.first_row_at_or_after(3), 2);
        assert_eq!(m.first_row_at_or_after(5), 2);
        // Past line 5's row ⇒ index 3 (next is line 7).
        assert_eq!(m.first_row_at_or_after(6), 3);
        assert_eq!(m.first_row_at_or_after(7), 3);
        // Past everything ⇒ index 4 (= rows.len()).
        assert_eq!(m.first_row_at_or_after(8), 4);
    }

    #[test]
    fn anchor_past_eof_clamps_to_line_count() {
        let rows = vec![
            row(100, AnchorPosition::Above),
            row(5, AnchorPosition::Above),
        ];
        let m = VirtualRowMatrix::build(rows, 10, VirtualRowVersion(1));
        assert_eq!(m.len(), 2);
        // (100) clamped to 10; (5) stays. After sort: (5),
        // (10).
        assert_eq!(m.rows[0].anchor_line, 5);
        assert_eq!(m.rows[1].anchor_line, 10);
    }

    #[test]
    fn virtual_rows_in_line_range_counts_inclusive() {
        // anchors at lines 2 (x2), 5, 7.
        let rows = vec![
            row(2, AnchorPosition::Above),
            row(2, AnchorPosition::Below),
            row(5, AnchorPosition::Above),
            row(7, AnchorPosition::Below),
        ];
        let m = VirtualRowMatrix::build(rows, 10, VirtualRowVersion(1));

        // Empty / inverted ranges.
        assert_eq!(m.virtual_rows_in_line_range(3, 2), 0);
        // Range below every anchor.
        assert_eq!(m.virtual_rows_in_line_range(0, 1), 0);
        // Inclusive of both endpoints: [2, 7] covers all four.
        assert_eq!(m.virtual_rows_in_line_range(2, 7), 4);
        // Endpoint inclusivity: [2, 2] captures both line-2 rows.
        assert_eq!(m.virtual_rows_in_line_range(2, 2), 2);
        // Mid-range: [3, 5] captures only the line-5 row.
        assert_eq!(m.virtual_rows_in_line_range(3, 5), 1);
        // [6, 7] captures only the line-7 row.
        assert_eq!(m.virtual_rows_in_line_range(6, 7), 1);
        // Range past EOF is clamped, never panics.
        assert_eq!(m.virtual_rows_in_line_range(8, u32::MAX), 0);
        // The empty matrix reports zero for any range.
        assert_eq!(
            VirtualRowMatrix::empty().virtual_rows_in_line_range(0, u32::MAX),
            0
        );
    }

    #[test]
    fn version_next_increments() {
        let v = VirtualRowVersion::ZERO;
        assert_eq!(v.next(), VirtualRowVersion(1));
        assert_eq!(v.next().next(), VirtualRowVersion(2));
    }
}
