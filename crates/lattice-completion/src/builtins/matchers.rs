//! Built-in matchers (DESIGN.md §5.11.3).
#![allow(clippy::single_range_in_vec_init)]
//!
//! Three shipped:
//! - [`PrefixMatcher`] -- query is a prefix of the candidate text.
//!   Case-insensitive when `ctx.case_sensitive` is false (the
//!   default for cmdline use).
//! - [`SubstringMatcher`] -- query appears anywhere in the
//!   candidate text.
//! - [`FuzzyMatcher`] -- subsequence match (each char of the query
//!   appears in order in the candidate, possibly with skips). Score
//!   decays with the number of skipped chars; `match_ranges`
//!   records exactly which bytes the matcher consumed (so the
//!   renderer can paint them).

use std::ops::Range;

use crate::candidate::{MatchScore, RawCandidate};
use crate::traits::CandidateMatcher;

/// `match:prefix`. Default v1 matcher.
pub struct PrefixMatcher;

impl CandidateMatcher for PrefixMatcher {
    fn matches(
        &self,
        query: &str,
        candidate: &RawCandidate,
    ) -> Option<(MatchScore, Vec<Range<usize>>)> {
        if query.is_empty() {
            return Some((MatchScore::PREFIX, Vec::new()));
        }
        // Case-insensitive comparison via lowercase. Mirrors the
        // `:set ignorecase` semantics; users who want case-sensitive
        // can swap matchers or wrap.
        let qlow = query.to_ascii_lowercase();
        let tlow = candidate.text.to_ascii_lowercase();
        if tlow.starts_with(&qlow) {
            let score = if candidate.text == query {
                MatchScore::PERFECT
            } else {
                MatchScore::PREFIX
            };
            Some((score, vec![0..query.len()]))
        } else {
            None
        }
    }
}

/// `match:substring`. Returns lower score than prefix.
pub struct SubstringMatcher;

impl CandidateMatcher for SubstringMatcher {
    fn matches(
        &self,
        query: &str,
        candidate: &RawCandidate,
    ) -> Option<(MatchScore, Vec<Range<usize>>)> {
        if query.is_empty() {
            return Some((MatchScore::SUBSTRING, Vec::new()));
        }
        let qlow = query.to_ascii_lowercase();
        let tlow = candidate.text.to_ascii_lowercase();
        let pos = tlow.find(&qlow)?;
        let score = if pos == 0 {
            MatchScore::PREFIX
        } else {
            MatchScore::SUBSTRING
        };
        Some((score, vec![pos..pos + query.len()]))
    }
}

/// `match:fuzzy`. Five-tier scoring (Exact → Prefix → Word-
/// boundary subsequence → Substring → Fuzzy-subsequence with
/// skip-decay).
///
/// Slice 3c.cmdline-completion-fuzzy-shared follow-up: this
/// matcher used to carry its own single-tier subsequence-with-
/// gap-density algorithm. That diverged from the picker's filter
/// loop and from the insert-mode `FuzzyInsertMatcher`, both of
/// which delegated to the free function [`crate::fuzzy_match`]
/// in `insert.rs`. The divergence produced exactly the symptom
/// the user reported on the GPUI cmdline: `:desc<Tab>` returned
/// a noisy fuzzy net (all candidates containing `d-e-s-c` as a
/// subsequence) with no clear winner, because there was no
/// prefix tier to lift `describe-*` above unrelated matches.
///
/// Collapsing the two impls makes cmdline completion behave
/// identically to the picker's filter and the insert-mode
/// matcher: prefix matches dominate (Tier 2, score 800), with
/// fuzzy subsequence (Tier 5, score ≤200) as the last-resort
/// tier. The picker / insert / cmdline now share one algorithm,
/// one set of tests, one definition of "fuzzy".
pub struct FuzzyMatcher;

impl CandidateMatcher for FuzzyMatcher {
    fn matches(
        &self,
        query: &str,
        candidate: &RawCandidate,
    ) -> Option<(MatchScore, Vec<Range<usize>>)> {
        crate::fuzzy_match(query, &candidate.text)
    }
}

/// `match:fuzzy-display`. Same 5-tier algorithm as [`FuzzyMatcher`]
/// but matches against `candidate.display` instead of
/// `candidate.text`.
///
/// Slice `3c.unify.picker-via-pipeline`: picker rows have
/// `text` carrying a routing payload (e.g.
/// `"<server_id>\t<workspace>"`) the user never sees, while
/// `display` is the row's user-visible label. The picker has to
/// match on `display`. This split (cmdline matches `text`, picker
/// matches `display`) is now first-class: two matcher impls,
/// same underlying `fuzzy_match` algorithm.
pub struct FuzzyDisplayMatcher;

impl CandidateMatcher for FuzzyDisplayMatcher {
    fn matches(
        &self,
        query: &str,
        candidate: &RawCandidate,
    ) -> Option<(MatchScore, Vec<Range<usize>>)> {
        crate::fuzzy_match(query, &candidate.display)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::candidate::CandidateKind;

    fn cand(s: &str) -> RawCandidate {
        RawCandidate::plain(s, CandidateKind::Plain)
    }

    // ---- PrefixMatcher ----

    #[test]
    fn prefix_matches_exact_prefix() {
        let m = PrefixMatcher;
        let r = m.matches("alpha", &cand("alphabet"));
        assert!(r.is_some());
        let (score, ranges) = r.unwrap();
        assert_eq!(score, MatchScore::PREFIX);
        assert_eq!(ranges, vec![0..5]);
    }

    #[test]
    fn prefix_perfect_match_scores_higher() {
        let m = PrefixMatcher;
        let (score, _) = m.matches("alpha", &cand("alpha")).unwrap();
        assert_eq!(score, MatchScore::PERFECT);
    }

    #[test]
    fn prefix_is_case_insensitive_by_default() {
        let m = PrefixMatcher;
        assert!(m.matches("ALPHA", &cand("alphabet")).is_some());
        assert!(m.matches("alpha", &cand("ALPHABET")).is_some());
    }

    #[test]
    fn prefix_rejects_non_prefix() {
        let m = PrefixMatcher;
        assert!(m.matches("foo", &cand("bar")).is_none());
        assert!(m.matches("bet", &cand("alphabet")).is_none());
    }

    #[test]
    fn prefix_empty_query_matches_with_no_ranges() {
        let m = PrefixMatcher;
        let (_, ranges) = m.matches("", &cand("anything")).unwrap();
        assert!(ranges.is_empty());
    }

    // ---- SubstringMatcher ----

    #[test]
    fn substring_matches_anywhere() {
        let m = SubstringMatcher;
        let (score, ranges) = m.matches("hab", &cand("alphabet")).unwrap();
        assert_eq!(score, MatchScore::SUBSTRING);
        assert_eq!(ranges, vec![3..6]); // "hab" starts at byte 3
    }

    #[test]
    fn substring_at_start_scores_as_prefix() {
        let m = SubstringMatcher;
        let (score, _) = m.matches("alp", &cand("alphabet")).unwrap();
        assert_eq!(score, MatchScore::PREFIX);
    }

    #[test]
    fn substring_rejects_nonexistent() {
        let m = SubstringMatcher;
        assert!(m.matches("xyz", &cand("alphabet")).is_none());
    }

    // ---- FuzzyMatcher ----

    #[test]
    fn fuzzy_matches_subsequence() {
        let m = FuzzyMatcher;
        // "alh" in "alphabet": a(0), l(1), h(3) -- skips p(2)
        let (_, ranges) = m.matches("alh", &cand("alphabet")).unwrap();
        assert_eq!(ranges, vec![0..1, 1..2, 3..4]);
    }

    #[test]
    fn fuzzy_skips_chars_with_score_penalty() {
        let m = FuzzyMatcher;
        // Post-collapse (3c.cmdline-completion-fuzzy-shared): the
        // matcher delegates to the 5-tier `fuzzy_match`. Within
        // Tier 5 (subseq-with-skip-decay), the penalty key is
        // `target.len() - query.len()` (skip count), not
        // intra-target gap density. So "alh" vs "alt" in
        // "alphabet" both score identically — both fall in Tier 5
        // with the same `skipped = 5`. The within-tier ordering
        // signal is gone; the tier separation is what protects
        // the user from noisy fuzzy nets (prefix matches score
        // 800, subseq scores ≤200).
        //
        // To preserve a meaningful comparison the test now picks
        // candidates that fall in DIFFERENT tiers: a prefix-tier
        // hit must outscore a subseq-tier hit.
        let (prefix_hit, _) = m.matches("alp", &cand("alphabet")).unwrap();
        let (subseq_hit, _) = m.matches("alt", &cand("alphabet")).unwrap();
        assert!(
            prefix_hit > subseq_hit,
            "prefix-tier ({prefix_hit:?}) must outscore subseq-tier ({subseq_hit:?})",
        );
    }

    #[test]
    fn fuzzy_rejects_non_subsequence() {
        let m = FuzzyMatcher;
        // No `x` / `y` / `z` in "alphabet".
        assert!(m.matches("xyz", &cand("alphabet")).is_none());
        // No `z` after the prefix matches.
        assert!(m.matches("alz", &cand("alphabet")).is_none());
    }

    #[test]
    fn fuzzy_subsequence_order_matters() {
        // "lpa" in "alpha" -- IS a valid subsequence: l(1), p(2), a(4).
        // Confirms the matcher considers position-after-previous-match,
        // not arbitrary char presence.
        let m = FuzzyMatcher;
        assert!(m.matches("lpa", &cand("alpha")).is_some());
        // But "pal" can't match "alpha" -- p comes after a in the
        // candidate; a is consumed first; p has no remaining a after it.
        assert!(m.matches("pal", &cand("alpha")).is_none());
    }

    #[test]
    fn fuzzy_is_case_insensitive() {
        let m = FuzzyMatcher;
        assert!(m.matches("ALH", &cand("alphabet")).is_some());
    }

    #[test]
    fn fuzzy_empty_query_matches_anything() {
        // Post-collapse: empty query falls into `fuzzy_match`'s
        // empty-query branch (insert.rs), which uses a uniform
        // score of 100 so an empty query doesn't fight prefix /
        // substring tiers for ordering. The picker / insert /
        // cmdline all see the same value now.
        let m = FuzzyMatcher;
        let (score, ranges) = m.matches("", &cand("anything")).unwrap();
        assert_eq!(score, MatchScore(100));
        assert!(ranges.is_empty());
    }

    #[test]
    fn fuzzy_match_ranges_correspond_to_actual_matched_bytes() {
        let m = FuzzyMatcher;
        let (_, ranges) = m.matches("phb", &cand("alphabet")).unwrap();
        // p(2) h(3) b(5)
        assert_eq!(ranges, vec![2..3, 3..4, 5..6]);
    }
}
