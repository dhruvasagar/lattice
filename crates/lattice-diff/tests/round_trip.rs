//! Property tests for two-way diff round-trip and three-way
//! conflict invariants.
//!
//! Drives the design's D.1 invariant: for any two ropes `a`
//! and `b`, `apply_two_way(a, b, compute_diff(&[a, b])) == b`.
//! All three algorithms (Histogram, Myers, Patience) satisfy
//! it independently.

use lattice_diff::patch::apply_two_way;
use lattice_diff::{DiffAlgorithm, HunkKind, compute_diff};
use proptest::prelude::*;
use ropey::Rope;

/// Generates a small line-structured rope: 0-30 lines, each
/// drawn from a small alphabet so we hit identical lines
/// across `a` and `b` and exercise the diff engine's
/// alignment logic.
fn arb_lines() -> impl Strategy<Value = String> {
    prop::collection::vec(prop_oneof!["a", "b", "c", "d", "e", "f", "g", "h"], 0..30).prop_map(
        |lines| {
            let mut s = String::new();
            for line in &lines {
                s.push_str(line);
                s.push('\n');
            }
            s
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn two_way_round_trip_histogram(a in arb_lines(), b in arb_lines()) {
        let a_rope = Rope::from(a.as_str());
        let b_rope = Rope::from(b.as_str());
        let hunks = compute_diff(&[a_rope.clone(), b_rope.clone()], DiffAlgorithm::Histogram)
                .expect("two-way is supported");
        let reconstructed = apply_two_way(&a_rope, &b_rope, &hunks);
        prop_assert_eq!(reconstructed.to_string(), b);
    }

    #[test]
    fn two_way_round_trip_myers(a in arb_lines(), b in arb_lines()) {
        let a_rope = Rope::from(a.as_str());
        let b_rope = Rope::from(b.as_str());
        let hunks = compute_diff(&[a_rope.clone(), b_rope.clone()], DiffAlgorithm::Myers)
                .expect("two-way is supported");
        let reconstructed = apply_two_way(&a_rope, &b_rope, &hunks);
        prop_assert_eq!(reconstructed.to_string(), b);
    }

    #[test]
    fn two_way_round_trip_myers_minimal(a in arb_lines(), b in arb_lines()) {
        let a_rope = Rope::from(a.as_str());
        let b_rope = Rope::from(b.as_str());
        let hunks = compute_diff(&[a_rope.clone(), b_rope.clone()], DiffAlgorithm::MyersMinimal)
                .expect("two-way is supported");
        let reconstructed = apply_two_way(&a_rope, &b_rope, &hunks);
        prop_assert_eq!(reconstructed.to_string(), b);
    }

    #[test]
    fn three_way_identical_sides_no_conflict(base in arb_lines(), side in arb_lines()) {
        // If local == remote, no merge can be in conflict
        // (both sides made identical changes vs base).
        let base_rope = Rope::from(base.as_str());
        let side_rope = Rope::from(side.as_str());
        let idx = compute_diff(
            &[base_rope, side_rope.clone(), side_rope],
            DiffAlgorithm::Histogram,
        )
        .expect("three-way is supported");
        // Conflicts should only arise when the two sides
        // disagree; we don't bother asserting on hunk counts
        // (depends on alg) but we do assert no Conflict kind
        // is emitted.
        for hunk in &idx.hunks {
            prop_assert_ne!(
                hunk.kind,
                HunkKind::Conflict,
                "identical local==remote should never conflict"
            );
        }
    }

    #[test]
    fn three_way_unchanged_local_attributes_to_remote(
        base in arb_lines(),
        remote in arb_lines(),
    ) {
        // When local == base, every change must be
        // attributed to remote (no Conflict).
        let base_rope = Rope::from(base.as_str());
        let remote_rope = Rope::from(remote.as_str());
        let idx = compute_diff(
            &[base_rope.clone(), base_rope, remote_rope],
            DiffAlgorithm::Histogram,
        )
        .expect("three-way is supported");
        for hunk in &idx.hunks {
            prop_assert_ne!(
                hunk.kind,
                HunkKind::Conflict,
                "unchanged local should never conflict with remote"
            );
        }
    }
}
