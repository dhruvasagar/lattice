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
#[derive(Clone, Debug)]
pub struct VirtualRow {
	pub anchor_line: u32,
	pub position: AnchorPosition,
	pub cells: Arc<[Cell]>,
	pub height: u16,
	pub kind: VirtualRowKind,
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
			while (row_idx as usize) < rows.len()
				&& rows[row_idx as usize].anchor_line < line
			{
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
	pub fn virtual_rows_in_line_range(&self, lo: u32, hi: u32) -> u32 {
		if lo > hi {
			return 0;
		}
		let end = self.first_row_at_or_after(hi.saturating_add(1));
		let start = self.first_row_at_or_after(lo);
		end.saturating_sub(start)
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
			anchor_line: anchor,
			position: pos,
			cells: Arc::from([] as [Cell; 0]),
			height: 1,
			kind: VirtualRowKind::Generic,
		}
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
