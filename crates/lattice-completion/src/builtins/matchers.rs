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

/// `match:fuzzy`. Subsequence with score-by-density.
///
/// Algorithm:
/// 1. Walk `candidate.text` bytes; for each `query` byte (in order),
///    advance until a (case-insensitively-) matching byte in
///    candidate.
/// 2. Each matched byte produces a one-byte range.
/// 3. Score = base - (gaps * penalty), where `gaps` is the total
///    number of unmatched bytes between consecutive matches.
///
/// Returns `None` if the query can't be matched as a subsequence.
pub struct FuzzyMatcher;

impl CandidateMatcher for FuzzyMatcher {
    fn matches(
        &self,
        query: &str,
        candidate: &RawCandidate,
    ) -> Option<(MatchScore, Vec<Range<usize>>)> {
        if query.is_empty() {
            return Some((MatchScore::FUZZY_HIGH, Vec::new()));
        }
        let q = query.as_bytes();
        let t = candidate.text.as_bytes();
        let mut q_i = 0;
        let mut ranges: Vec<Range<usize>> = Vec::with_capacity(q.len());
        let mut gaps: usize = 0;
        let mut last_match: Option<usize> = None;
        for (i, b) in t.iter().enumerate() {
            if q_i >= q.len() {
                break;
            }
            if b.eq_ignore_ascii_case(&q[q_i]) {
                if let Some(prev) = last_match {
                    gaps += i.saturating_sub(prev + 1);
                }
                ranges.push(i..i + 1);
                last_match = Some(i);
                q_i += 1;
            }
        }
        if q_i < q.len() {
            return None;
        }
        // Score:
        //   base = FUZZY_HIGH (700)
        //   penalty = min(gaps * 5, FUZZY_HIGH - FUZZY_LOW)
        //   bonus if matches start at byte 0 (prefix-like)
        let penalty = (gaps as u32).saturating_mul(5);
        let mut score = MatchScore::FUZZY_HIGH.0.saturating_sub(penalty);
        score = score.max(MatchScore::FUZZY_LOW.0);
        if ranges.first().is_some_and(|r| r.start == 0) {
            score = score.saturating_add(50);
        }
        Some((MatchScore(score), ranges))
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
        let (close, _) = m.matches("alh", &cand("alphabet")).unwrap();
        // "alh" in "alphabet": a(0) l(1) h(3) -> gap of 1
        // "alt" in "alphabet": a(0) l(1) t(7) -> gap of 5
        let (far, _) = m.matches("alt", &cand("alphabet")).unwrap();
        assert!(close > far, "close-skip should outscore far-skip");
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
        let m = FuzzyMatcher;
        let (score, ranges) = m.matches("", &cand("anything")).unwrap();
        assert_eq!(score, MatchScore::FUZZY_HIGH);
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
