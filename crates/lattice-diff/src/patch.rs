//! Patch application for round-trip testing.
//!
//! Given a `HunkIndex` produced by [`compute_two_way(a, b)`]
//! and the original ropes `a` + `b`, [`apply_two_way`] applies
//! the hunks to `a` to produce a result equivalent to `b`.
//! This is the primitive that drives the property test
//! `apply(diff(a, b), a) == b` for randomly-generated rope
//! pairs.
//!
//! Not the public surface for editing — production hunk
//! transfer (`do` / `dp`) lands in D.5 and routes through the
//! standard edit pipeline rather than this helper.

use ropey::Rope;

use crate::types::HunkIndex;

/// Apply a two-way `HunkIndex` produced by
/// `compute_two_way(a, b)` to `a`, reading replacement
/// content from `b`. The result equals `b` (this is the
/// round-trip invariant the property tests assert).
///
/// Walks hunks in reverse so earlier indices stay valid as
/// we mutate later ones first.
pub fn apply_two_way(a: &Rope, b: &Rope, hunks: &HunkIndex) -> Rope {
	let mut result = a.clone();
	for hunk in hunks.hunks.iter().rev() {
		let a_range = hunk.ranges[0];
		let b_range = hunk.ranges[1];

		let a_char_start = line_to_char_clamped(&result, a_range.start);
		let a_char_end = line_to_char_clamped(&result, a_range.end);

		let b_text = if b_range.is_empty() {
			String::new()
		} else {
			let b_char_start = line_to_char_clamped(b, b_range.start);
			let b_char_end = line_to_char_clamped(b, b_range.end);
			b.slice(b_char_start..b_char_end).to_string()
		};

		if a_char_start < a_char_end {
			result.remove(a_char_start..a_char_end);
		}
		if !b_text.is_empty() {
			result.insert(a_char_start, &b_text);
		}
	}
	result
}

/// `Rope::line_to_char` panics if `line > rope.len_lines()`;
/// clamp to `len_lines()` for safety. Ropey treats
/// `line == len_lines()` as past-the-end (returns
/// `len_chars()`), which is what we want for the "end of
/// range" case.
fn line_to_char_clamped(rope: &Rope, line: u32) -> usize {
	let max = rope.len_lines();
	let clamped = (line as usize).min(max);
	rope.line_to_char(clamped)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::compute::two_way;
	use crate::DiffAlgorithm;

	/// Convenience: diff then apply. Reconstructs `b` from
	/// `a` via the round-trip path.
	fn round_trip(a: &Rope, b: &Rope, algorithm: DiffAlgorithm) -> Rope {
		let hunks = two_way(a, b, algorithm);
		apply_two_way(a, b, &hunks)
	}

	fn rt_eq(a: &str, b: &str) {
		for alg in [
			DiffAlgorithm::Histogram,
			DiffAlgorithm::Myers,
			DiffAlgorithm::MyersMinimal,
		] {
			let a_rope = Rope::from(a);
			let b_rope = Rope::from(b);
			let result = round_trip(&a_rope, &b_rope, alg);
			assert_eq!(
				result.to_string(),
				b,
				"round trip failed for algorithm {alg:?}\n  a = {a:?}\n  b = {b:?}"
			);
		}
	}

	#[test]
	fn round_trip_identical() {
		rt_eq("alpha\nbeta\ngamma\n", "alpha\nbeta\ngamma\n");
	}

	#[test]
	fn round_trip_empty_both() {
		rt_eq("", "");
	}

	#[test]
	fn round_trip_empty_to_content() {
		rt_eq("", "alpha\nbeta\n");
	}

	#[test]
	fn round_trip_content_to_empty() {
		rt_eq("alpha\nbeta\n", "");
	}

	#[test]
	fn round_trip_single_change() {
		rt_eq("alpha\nbeta\ngamma\n", "alpha\nBETA\ngamma\n");
	}

	#[test]
	fn round_trip_insert_middle() {
		rt_eq("alpha\ngamma\n", "alpha\nbeta\ngamma\n");
	}

	#[test]
	fn round_trip_delete_middle() {
		rt_eq("alpha\nbeta\ngamma\n", "alpha\ngamma\n");
	}

	#[test]
	fn round_trip_multiple_hunks() {
		rt_eq(
			"alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\n",
			"alpha\nBETA\ngamma\nDELTA\nepsilon\nZETA\n",
		);
	}

	#[test]
	fn round_trip_block_replacement() {
		rt_eq(
			"alpha\nbeta\ngamma\ndelta\n",
			"alpha\nfoo\nbar\nbaz\nquux\ndelta\n",
		);
	}

	#[test]
	fn round_trip_no_trailing_newline() {
		rt_eq("alpha\nbeta", "alpha\nBETA");
	}

	#[test]
	fn round_trip_mixed_trailing_newline() {
		rt_eq("alpha\nbeta", "alpha\nbeta\n");
	}
}
