//! Two-way and three-way hunk computation.
//!
//! Both entry points materialise the input ropes to strings,
//! tokenise by line, and run `imara-diff`. The two-way path is
//! a thin wrapper around the engine; the three-way path
//! composes two two-way diffs against the base and merges them
//! by overlapping base ranges, producing `Conflict` hunks
//! where both sides independently touched the same base
//! lines.
//!
//! See `docs/dev/architecture/diff-system.md` §3 and §4.

use std::ops::Range;

use imara_diff::intern::InternedInput;
use imara_diff::{Algorithm, Sink};
use ropey::Rope;
use smallvec::smallvec;

use crate::types::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};

fn algorithm_to_imara(alg: DiffAlgorithm) -> Algorithm {
	match alg {
		DiffAlgorithm::Histogram => Algorithm::Histogram,
		DiffAlgorithm::Myers => Algorithm::Myers,
		DiffAlgorithm::MyersMinimal => Algorithm::MyersMinimal,
	}
}

/// `imara_diff::Sink` impl that collects each `process_change`
/// callback into a two-way `Hunk`.
struct TwoWaySink {
	hunks: Vec<Hunk>,
}

impl Sink for TwoWaySink {
	type Out = Vec<Hunk>;

	fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
		let a = LineRange::new(before.start, before.end);
		let b = LineRange::new(after.start, after.end);
		let kind = classify_two_way(a, b);
		self.hunks.push(Hunk {
			kind,
			ranges: smallvec![a, b],
		});
	}

	fn finish(self) -> Self::Out {
		self.hunks
	}
}

fn classify_two_way(a: LineRange, b: LineRange) -> HunkKind {
	match (a.is_empty(), b.is_empty()) {
		// imara-diff does not emit an empty/empty hunk; fall
		// back to `Change` defensively rather than panic.
		(true, true) => HunkKind::Change,
		(true, false) => HunkKind::Add,
		(false, true) => HunkKind::Remove,
		(false, false) => HunkKind::Change,
	}
}

/// Compute the two-way hunk list between ropes `a` and `b`.
///
/// Each hunk's `ranges` is `[a_range, b_range]`. The
/// classification (Add / Remove / Change) is relative to `a`
/// being the "earlier" side.
///
/// Allocates the full text of both ropes once each via
/// `Rope::to_string()` for the engine's interner. The cost is
/// O(N + M) bytes for ropes of sizes N and M; bench gated in
/// `benches/recompute.rs`.
pub fn compute_two_way(a: &Rope, b: &Rope, algorithm: DiffAlgorithm) -> HunkIndex {
	let a_str = a.to_string();
	let b_str = b.to_string();
	let hunks = compute_two_way_str(&a_str, &b_str, algorithm);
	HunkIndex {
		hunks,
		algorithm,
		revision: 0,
	}
}

/// Internal: two-way diff over `&str` inputs. Used by both
/// the public `compute_two_way` and by `compute_three_way`
/// (which materialises ropes once and reuses the strings for
/// the two base-vs-side diffs).
///
/// Tokenises with `imara_diff::sources::lines_with_terminator`
/// so trailing-newline differences are preserved (the default
/// `&str` tokenisation uses `str::lines()` which strips
/// terminators and treats `"x"` and `"x\n"` as identical).
fn compute_two_way_str(a: &str, b: &str, algorithm: DiffAlgorithm) -> Vec<Hunk> {
	let input = InternedInput::new(
		imara_diff::sources::lines_with_terminator(a),
		imara_diff::sources::lines_with_terminator(b),
	);
	imara_diff::diff(
		algorithm_to_imara(algorithm),
		&input,
		TwoWaySink { hunks: Vec::new() },
	)
}

/// Compute a three-way hunk list between `base`, `local`, and
/// `remote`.
///
/// Each hunk's `ranges` is `[base_range, local_range,
/// remote_range]`. A hunk is `Conflict` iff both `local` and
/// `remote` independently modified an overlapping base region.
/// Otherwise the hunk is classified relative to whichever side
/// changed (the other side's range covers the corresponding
/// untouched region in that side's coordinate system, computed
/// via running offset).
///
/// Adjacent (touching but not overlapping) hunks from
/// different sides are kept separate — they don't conflict.
/// Strict overlap (`a.start < b.end && b.start < a.end`) is
/// the conflict predicate.
pub fn compute_three_way(
	base: &Rope,
	local: &Rope,
	remote: &Rope,
	algorithm: DiffAlgorithm,
) -> HunkIndex {
	let base_str = base.to_string();
	let local_str = local.to_string();
	let remote_str = remote.to_string();

	let local_hunks = compute_two_way_str(&base_str, &local_str, algorithm);
	let remote_hunks = compute_two_way_str(&base_str, &remote_str, algorithm);

	let merged = merge_three_way(&local_hunks, &remote_hunks, &local_str, &remote_str);

	HunkIndex {
		hunks: merged,
		algorithm,
		revision: 0,
	}
}

/// Precompute byte offsets of every line start in `s`.
///
/// `offsets[i]` is the byte index in `s` where line `i`
/// starts. `offsets[len_lines]` is `s.len()` (past-the-end
/// sentinel). Used by [`line_slice`] for O(1) line-range
/// indexing during three-way merge content comparison.
fn line_offsets(s: &str) -> Vec<usize> {
	let mut offsets = Vec::with_capacity(s.len() / 32 + 2);
	offsets.push(0);
	for (i, byte) in s.bytes().enumerate() {
		if byte == b'\n' {
			offsets.push(i + 1);
		}
	}
	if offsets.last().copied() != Some(s.len()) {
		offsets.push(s.len());
	}
	offsets
}

/// Slice `s` by line range, using precomputed offsets.
/// Returns an empty slice if the range is empty or out of
/// bounds.
fn line_slice<'a>(s: &'a str, offsets: &[usize], range: LineRange) -> &'a str {
	let start = offsets
		.get(range.start as usize)
		.copied()
		.unwrap_or(s.len());
	let end = offsets.get(range.end as usize).copied().unwrap_or(s.len());
	if start > end || start > s.len() {
		return "";
	}
	&s[start..end.min(s.len())]
}

/// Merge two sorted two-way hunk lists (each `[base, side]`)
/// into a unified three-way list `[base, local, remote]`.
///
/// Walks both lists in tandem in base-ascending order. Picks
/// the earliest unprocessed hunk as the seed of a new union
/// region, then takes every subsequent hunk from either side
/// that strictly overlaps the growing union. A union that
/// took hunks from both sides is classified `Conflict`; a
/// union touched by only one side is attributed to that
/// side's change kind.
fn merge_three_way(
	local_hunks: &[Hunk],
	remote_hunks: &[Hunk],
	local_str: &str,
	remote_str: &str,
) -> Vec<Hunk> {
	let local_offsets = line_offsets(local_str);
	let remote_offsets = line_offsets(remote_str);

	let mut merged = Vec::new();
	let mut li = 0;
	let mut ri = 0;

	// Running net deltas: how many lines local/remote has
	// gained (positive) or lost (negative) vs base by the time
	// we reach the current base position. Used to project an
	// untouched base range into the side's coordinate space.
	let mut local_delta: i64 = 0;
	let mut remote_delta: i64 = 0;

	while li < local_hunks.len() || ri < remote_hunks.len() {
		let l_pos = local_hunks.get(li).map(|h| h.ranges[0].start);
		let r_pos = remote_hunks.get(ri).map(|h| h.ranges[0].start);

		// Pick the earliest unprocessed hunk as the seed.
		let take_local_first = match (l_pos, r_pos) {
			(Some(l), Some(r)) => l <= r,
			(Some(_), None) => true,
			(None, Some(_)) => false,
			(None, None) => break,
		};

		let mut taken_local: Vec<usize> = Vec::new();
		let mut taken_remote: Vec<usize> = Vec::new();
		let mut union_base;

		if take_local_first {
			union_base = local_hunks[li].ranges[0];
			taken_local.push(li);
			li += 1;
		} else {
			union_base = remote_hunks[ri].ranges[0];
			taken_remote.push(ri);
			ri += 1;
		}

		// Extend the union while either side has a strictly-
		// overlapping next hunk.
		loop {
			let mut extended = false;
			if let Some(h) = local_hunks.get(li) {
				if h.ranges[0].start < union_base.end {
					union_base = LineRange::new(
						union_base.start,
						union_base.end.max(h.ranges[0].end),
					);
					taken_local.push(li);
					li += 1;
					extended = true;
				}
			}
			if let Some(h) = remote_hunks.get(ri) {
				if h.ranges[0].start < union_base.end {
					union_base = LineRange::new(
						union_base.start,
						union_base.end.max(h.ranges[0].end),
					);
					taken_remote.push(ri);
					ri += 1;
					extended = true;
				}
			}
			if !extended {
				break;
			}
		}

		let local_range = side_range(&taken_local, local_hunks, union_base, local_delta);
		let remote_range = side_range(&taken_remote, remote_hunks, union_base, remote_delta);

		// Advance running deltas past the taken hunks.
		for &idx in &taken_local {
			let h = &local_hunks[idx];
			local_delta += h.ranges[1].len() as i64 - h.ranges[0].len() as i64;
		}
		for &idx in &taken_remote {
			let h = &remote_hunks[idx];
			remote_delta += h.ranges[1].len() as i64 - h.ranges[0].len() as i64;
		}

		let kind = if !taken_local.is_empty() && !taken_remote.is_empty() {
			// Both sides touched this region. If the resulting
			// content is identical, this is a "soft" merge
			// where both sides made the same change — not a
			// conflict. Compare the actual line content rather
			// than just the ranges, since equivalent edits can
			// land at different line indices in their side's
			// coordinate space.
			let local_text = line_slice(local_str, &local_offsets, local_range);
			let remote_text = line_slice(remote_str, &remote_offsets, remote_range);
			if local_text == remote_text {
				classify_three_way_attributed(union_base, local_range)
			} else {
				HunkKind::Conflict
			}
		} else if !taken_local.is_empty() {
			classify_three_way_attributed(union_base, local_range)
		} else {
			classify_three_way_attributed(union_base, remote_range)
		};

		merged.push(Hunk {
			kind,
			ranges: smallvec![union_base, local_range, remote_range],
		});
	}

	merged
}

/// Compute one side's (local *or* remote) range corresponding
/// to the union base range.
///
/// - If the side touched the union (one or more hunks taken),
///   the range spans from the first taken hunk's side-start
///   (extended by the base-prefix preceding it) to the last
///   taken hunk's side-end (extended by the base-suffix
///   following it).
/// - If the side did not touch the union, project the base
///   range into the side's coordinate space via the running
///   delta.
fn side_range(
	taken: &[usize],
	hunks: &[Hunk],
	union_base: LineRange,
	side_delta: i64,
) -> LineRange {
	if taken.is_empty() {
		let start = (union_base.start as i64 + side_delta).max(0) as u32;
		let end = (union_base.end as i64 + side_delta).max(0) as u32;
		LineRange::new(start, end)
	} else {
		let first = &hunks[taken[0]];
		let last = &hunks[*taken.last().expect("taken non-empty")];
		// How much base extends before the first taken hunk
		// (untouched prefix that we count as side-untouched).
		let prefix = first.ranges[0].start.saturating_sub(union_base.start);
		let suffix = union_base.end.saturating_sub(last.ranges[0].end);
		let start = first.ranges[1].start.saturating_sub(prefix);
		let end = last.ranges[1].end + suffix;
		LineRange::new(start, end)
	}
}

fn classify_three_way_attributed(base: LineRange, side: LineRange) -> HunkKind {
	match (base.is_empty(), side.is_empty()) {
		(true, false) => HunkKind::Add,
		(false, true) => HunkKind::Remove,
		(false, false) => HunkKind::Change,
		// An empty/empty union shouldn't occur after a
		// hunk-taking iteration; classify defensively.
		(true, true) => HunkKind::Change,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn empty_inputs_have_no_hunks() {
		let a = Rope::new();
		let b = Rope::new();
		let idx = compute_two_way(&a, &b, DiffAlgorithm::Histogram);
		assert!(idx.is_empty());
	}

	#[test]
	fn identical_inputs_have_no_hunks() {
		let a = Rope::from("alpha\nbeta\ngamma\n");
		let b = Rope::from("alpha\nbeta\ngamma\n");
		let idx = compute_two_way(&a, &b, DiffAlgorithm::Histogram);
		assert!(idx.is_empty());
	}

	#[test]
	fn pure_add_classifies_as_add() {
		let a = Rope::from("alpha\ngamma\n");
		let b = Rope::from("alpha\nbeta\ngamma\n");
		let idx = compute_two_way(&a, &b, DiffAlgorithm::Histogram);
		assert_eq!(idx.len(), 1);
		assert_eq!(idx.hunks[0].kind, HunkKind::Add);
	}

	#[test]
	fn pure_remove_classifies_as_remove() {
		let a = Rope::from("alpha\nbeta\ngamma\n");
		let b = Rope::from("alpha\ngamma\n");
		let idx = compute_two_way(&a, &b, DiffAlgorithm::Histogram);
		assert_eq!(idx.len(), 1);
		assert_eq!(idx.hunks[0].kind, HunkKind::Remove);
	}

	#[test]
	fn change_classifies_as_change() {
		let a = Rope::from("alpha\nbeta\ngamma\n");
		let b = Rope::from("alpha\nBETA\ngamma\n");
		let idx = compute_two_way(&a, &b, DiffAlgorithm::Histogram);
		assert_eq!(idx.len(), 1);
		assert_eq!(idx.hunks[0].kind, HunkKind::Change);
	}

	#[test]
	fn three_way_non_overlapping_changes_no_conflict() {
		let base = Rope::from("a\nb\nc\nd\ne\nf\n");
		let local = Rope::from("a\nB\nc\nd\ne\nf\n"); // changed line 1
		let remote = Rope::from("a\nb\nc\nd\nE\nf\n"); // changed line 4
		let idx = compute_three_way(&base, &local, &remote, DiffAlgorithm::Histogram);
		// Two separate non-conflict hunks (one per side).
		assert_eq!(idx.len(), 2);
		assert!(idx.hunks.iter().all(|h| h.kind != HunkKind::Conflict));
	}

	#[test]
	fn three_way_overlapping_changes_yield_conflict() {
		let base = Rope::from("a\nb\nc\n");
		let local = Rope::from("a\nLOCAL\nc\n"); // changed line 1
		let remote = Rope::from("a\nREMOTE\nc\n"); // changed line 1 differently
		let idx = compute_three_way(&base, &local, &remote, DiffAlgorithm::Histogram);
		assert_eq!(idx.len(), 1);
		assert_eq!(idx.hunks[0].kind, HunkKind::Conflict);
	}

	#[test]
	fn three_way_no_changes_no_hunks() {
		let base = Rope::from("a\nb\nc\n");
		let local = base.clone();
		let remote = base.clone();
		let idx = compute_three_way(&base, &local, &remote, DiffAlgorithm::Histogram);
		assert!(idx.is_empty());
	}

	#[test]
	fn all_algorithms_agree_on_simple_change() {
		let a = Rope::from("alpha\nbeta\ngamma\n");
		let b = Rope::from("alpha\nBETA\ngamma\n");
		for alg in [
			DiffAlgorithm::Histogram,
			DiffAlgorithm::Myers,
			DiffAlgorithm::MyersMinimal,
		] {
			let idx = compute_two_way(&a, &b, alg);
			assert_eq!(idx.len(), 1, "algorithm {alg:?}");
			assert_eq!(idx.hunks[0].kind, HunkKind::Change, "algorithm {alg:?}");
			assert_eq!(idx.algorithm, alg);
		}
	}
}
