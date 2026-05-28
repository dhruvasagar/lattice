//! Data model for the diff subsystem.
//!
//! All types are pure (no actor / host references). The
//! `HunkIndex` is what `D.2`'s `DiffSubsystem` will publish via
//! `ArcSwap`; the `Hunk` and `LineRange` shapes are what every
//! consumer of `D.3+` reads against.

use smallvec::SmallVec;

/// Half-open line range `[start, end)` in a document.
///
/// Line indices are 0-based. `end == start` means the range is
/// empty (zero lines). The largest representable range covers
/// `u32::MAX - 1` lines, which is sufficient for files up to
/// ~4 billion lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineRange {
	pub start: u32,
	pub end: u32,
}

impl LineRange {
	/// Construct a new line range. Debug-asserts `start <= end`.
	pub fn new(start: u32, end: u32) -> Self {
		debug_assert!(
			start <= end,
			"LineRange::new: start {start} must be <= end {end}"
		);
		Self { start, end }
	}

	/// `true` if the range covers zero lines.
	pub fn is_empty(self) -> bool {
		self.start == self.end
	}

	/// Number of lines covered.
	pub fn len(self) -> u32 {
		self.end - self.start
	}
}

/// What kind of change a hunk represents.
///
/// Classification is relative to *one* "earlier" side. For
/// two-way diffs the earlier side is `ranges[0]` (the A rope);
/// for three-way diffs the earlier side is `ranges[0]` (the
/// base rope). `Conflict` is only produced by three-way diff
/// when both local and remote independently changed an
/// overlapping base region.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HunkKind {
	/// Lines present only in the later side(s). Earlier side
	/// range is empty.
	Add,
	/// Lines present only in the earlier side. Later side
	/// range(s) is/are empty.
	Remove,
	/// Lines present in both, with differing content.
	Change,
	/// Three-way only: both local and remote modified the
	/// same base region. Resolution is deferred to D.6 /
	/// user.
	Conflict,
}

/// Diff algorithm used by `imara-diff`.
///
/// All three are wired and behave identically with respect to
/// hunk *kinds*; they differ in how they choose where hunk
/// boundaries land for ambiguous edits.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DiffAlgorithm {
	/// Git's default since 2.7. Best general-purpose results
	/// on code; preferred for Lattice.
	Histogram,
	/// The classic Eugene Myers algorithm. Slightly weaker
	/// results on rebraced code; included for completeness.
	Myers,
	/// Myers run with the `--minimal` post-process — finds
	/// the smallest possible diff at extra CPU cost. Use when
	/// you want the tightest output and don't mind a slower
	/// recompute. Mapped to `imara_diff::Algorithm::MyersMinimal`.
	MyersMinimal,
}

impl Default for DiffAlgorithm {
	fn default() -> Self {
		Self::Histogram
	}
}

/// A single hunk: a contiguous region of change across the
/// participating documents.
///
/// `ranges.len()` matches the document arity:
///
/// - Two-way: `[a_range, b_range]`
/// - Three-way: `[base_range, local_range, remote_range]`
///
/// The `SmallVec` inline capacity of 3 covers both cases
/// without allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
	pub kind: HunkKind,
	pub ranges: SmallVec<[LineRange; 3]>,
}

/// Published hunk list with the algorithm used and a
/// revision tag.
///
/// `D.2`'s `DiffSubsystem` increments `revision` on each
/// recompute and publishes the new `Arc<HunkIndex>` via
/// `ArcSwap`. Consumers compare revisions to invalidate
/// caches (`DiffMap` row-translation, scroll-bind row
/// mapping, etc.).
#[derive(Clone, Debug)]
pub struct HunkIndex {
	pub hunks: Vec<Hunk>,
	pub algorithm: DiffAlgorithm,
	pub revision: u64,
}

impl HunkIndex {
	/// Construct an empty index with the given algorithm and
	/// revision 0.
	pub fn empty(algorithm: DiffAlgorithm) -> Self {
		Self {
			hunks: Vec::new(),
			algorithm,
			revision: 0,
		}
	}

	pub fn is_empty(&self) -> bool {
		self.hunks.is_empty()
	}

	pub fn len(&self) -> usize {
		self.hunks.len()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn line_range_basics() {
		let r = LineRange::new(3, 7);
		assert_eq!(r.start, 3);
		assert_eq!(r.end, 7);
		assert_eq!(r.len(), 4);
		assert!(!r.is_empty());

		let empty = LineRange::new(5, 5);
		assert!(empty.is_empty());
		assert_eq!(empty.len(), 0);
	}

	#[test]
	fn hunk_kind_default_for_diff_algorithm() {
		assert_eq!(DiffAlgorithm::default(), DiffAlgorithm::Histogram);
	}

	#[test]
	fn hunk_index_empty_helpers() {
		let idx = HunkIndex::empty(DiffAlgorithm::Histogram);
		assert!(idx.is_empty());
		assert_eq!(idx.len(), 0);
		assert_eq!(idx.algorithm, DiffAlgorithm::Histogram);
		assert_eq!(idx.revision, 0);
	}
}
