//! Built-in rankers (DESIGN.md §5.11.3).

use crate::candidate::ScoredCandidate;
use crate::traits::CandidateRanker;

/// `rank:score`. Default v1 ranker. Sorts by descending score with
/// alphabetical tie-break on candidate text.
pub struct ScoreRanker;

impl CandidateRanker for ScoreRanker {
    fn rank(&self, scored: &mut Vec<ScoredCandidate>) {
        scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.raw.text.cmp(&b.raw.text)));
    }
}

/// `rank:alphabetical`. Plain A-Z sort on text. Useful when score
/// information isn't trustworthy (e.g. uniform-score generators
/// like `gen:files`).
pub struct AlphabeticalRanker;

impl CandidateRanker for AlphabeticalRanker {
    fn rank(&self, scored: &mut Vec<ScoredCandidate>) {
        scored.sort_by(|a, b| a.raw.text.cmp(&b.raw.text));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::candidate::{CandidateKind, MatchScore, RawCandidate};

    fn s(text: &str, score: u32) -> ScoredCandidate {
        ScoredCandidate {
            raw: RawCandidate::plain(text, CandidateKind::Plain),
            score: MatchScore(score),
            match_ranges: Vec::new(),
        }
    }

    #[test]
    fn score_ranker_orders_descending() {
        let mut v = vec![s("a", 100), s("b", 500), s("c", 300)];
        ScoreRanker.rank(&mut v);
        assert_eq!(v[0].raw.text, "b"); // 500
        assert_eq!(v[1].raw.text, "c"); // 300
        assert_eq!(v[2].raw.text, "a"); // 100
    }

    #[test]
    fn score_ranker_breaks_ties_alphabetically() {
        let mut v = vec![s("zebra", 500), s("apple", 500), s("mango", 500)];
        ScoreRanker.rank(&mut v);
        assert_eq!(v[0].raw.text, "apple");
        assert_eq!(v[1].raw.text, "mango");
        assert_eq!(v[2].raw.text, "zebra");
    }

    #[test]
    fn alphabetical_ranker_ignores_score() {
        let mut v = vec![s("zebra", 999), s("apple", 1)];
        AlphabeticalRanker.rank(&mut v);
        assert_eq!(v[0].raw.text, "apple");
        assert_eq!(v[1].raw.text, "zebra");
    }

    #[test]
    fn ranker_is_stable_for_empty_vec() {
        let mut v: Vec<ScoredCandidate> = Vec::new();
        ScoreRanker.rank(&mut v);
        AlphabeticalRanker.rank(&mut v);
        assert!(v.is_empty());
    }
}
