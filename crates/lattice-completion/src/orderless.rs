//! Orderless matching — a query is a *set* of independent components,
//! not one string.
//!
//! [`crate::fuzzy_match`] treats the whole query as a single token, so
//! `"pic ref"` only matches a candidate that literally contains
//! `"pic ref"`. That is the limit users hit on a file picker, where the
//! two memorable fragments of a path are usually in the wrong order
//! (`refilter` lives under `lattice-picker`, so the natural query is
//! "picker" *then* "refilter", but "ref pic" should work just as well).
//!
//! Orderless splits the query on unescaped whitespace and requires
//! **every** component to match, **in any order**. Each component runs
//! the full 5-tier [`crate::fuzzy_match`] ladder, so the tier scores keep
//! prefix hits ranked above subsequence hits — prefix preference is
//! expressed in the *ranking*, not as a *filter*. That is the deliberate
//! divergence from emacs' `orderless-prefixes` style, which drops
//! non-prefix matches outright: the symptom this exists to fix is "too
//! few matches", and a stricter style would narrow the result set
//! further.
//!
//! Syntax, in full:
//!
//! | Written | Means |
//! |---|---|
//! | `foo bar` | both `foo` and `bar` must match, either order |
//! | `!foo` | candidates containing `foo` are excluded |
//! | `foo\ bar` | one component containing a literal space |
//! | `\!foo` | one component whose first character is a literal `!` |
//!
//! Scoring: the candidate's score is the **mean** of its positive
//! components' tier scores, plus [`ORDER_BONUS`] when those components
//! happen to match left-to-right. The mean (rather than the sum) keeps
//! the result inside the same 0..1000 band single-token matching already
//! produces, so a two-word query does not outrank a one-word query
//! purely by having more components — the picker's MRU bonus stays
//! calibrated against the same scale.
//!
//! A single positive component with no negations delegates verbatim to
//! [`crate::fuzzy_match`], so the overwhelmingly common case is
//! bit-for-bit identical to the pre-orderless behaviour (same score,
//! same ranges, no order bonus).

use std::ops::Range;

use crate::candidate::MatchScore;

/// Added to a multi-component match whose components land in the order
/// the user typed them. Set below the 200-point gap between adjacent
/// match tiers so it acts as a within-tier tie-breaker and can never
/// promote a subsequence match above a substring one.
pub const ORDER_BONUS: u32 = 50;

/// Uniform score for a query that filters nothing — empty, or purely
/// negative with no exclusion hit. Matches [`crate::fuzzy_match`]'s
/// empty-query score so the two agree on "everything passes".
const UNIFORM_SCORE: u32 = 100;

/// One whitespace-separated piece of an orderless query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderlessComponent {
    /// The component text with escapes resolved (`foo\ bar` → `foo bar`).
    pub text: String,
    /// `true` when written with a leading unescaped `!`: candidates
    /// containing `text` are excluded.
    pub negated: bool,
}

/// Split `query` into components on unescaped whitespace, resolving
/// `\<char>` escapes and the leading-`!` negation marker.
///
/// A trailing lone backslash is treated as a literal backslash rather
/// than an error — the user is mid-keystroke, and a picker that emptied
/// its result list on every half-typed escape would be unusable.
pub fn parse_orderless_query(query: &str) -> Vec<OrderlessComponent> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut negated = false;
    let mut started = false;
    let mut escaped = false;

    for c in query.chars() {
        if escaped {
            text.push(c);
            escaped = false;
            started = true;
            continue;
        }
        match c {
            '\\' => {
                escaped = true;
                // A component that begins with a backslash has started
                // even if the escape resolves to nothing yet.
                started = true;
            }
            c if c.is_whitespace() => {
                if started {
                    out.push(OrderlessComponent {
                        text: std::mem::take(&mut text),
                        negated,
                    });
                }
                negated = false;
                started = false;
            }
            '!' if !started => {
                // Leading `!` is the negation marker; a `!` anywhere
                // else in the component is a literal character.
                negated = true;
                started = true;
            }
            c => {
                text.push(c);
                started = true;
            }
        }
    }
    if escaped {
        text.push('\\');
        started = true;
    }
    if started {
        out.push(OrderlessComponent { text, negated });
    }
    // `!` alone excludes nothing; drop it rather than excluding every
    // candidate (an empty `contains` is always true).
    out.retain(|c| !c.text.is_empty());
    out
}

/// Match `target` against an orderless `query`.
///
/// Returns `None` when any positive component fails to match or any
/// negated component matches. Returned byte ranges are into `target`,
/// sorted and merged, so a renderer can highlight every component's hit
/// without handling overlaps.
///
/// See the module docs for syntax and scoring.
pub fn orderless_match(query: &str, target: &str) -> Option<(MatchScore, Vec<Range<usize>>)> {
    let components = parse_orderless_query(query);

    // Fast path: the common single-token query is the pre-orderless
    // algorithm, unchanged. Also covers the empty query (no components
    // → `fuzzy_match`'s own uniform score).
    match components.as_slice() {
        [] => return crate::fuzzy_match("", target),
        [only] if !only.negated => return crate::fuzzy_match(&only.text, target),
        _ => {}
    }

    let target_lower = target.to_lowercase();
    let mut total = 0u64;
    let mut positives = 0u32;
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut prev_start: Option<usize> = None;
    let mut in_order = true;

    for component in &components {
        if component.negated {
            // Negation is literal substring, not fuzzy: `!test` should
            // exclude what a user means by "test", and a fuzzy negation
            // would silently exclude nearly everything (every path
            // contains t-e-s-t as a subsequence somewhere).
            if target_lower.contains(&component.text.to_lowercase()) {
                return None;
            }
            continue;
        }
        let (score, component_ranges) = crate::fuzzy_match(&component.text, target)?;
        total += u64::from(score.0);
        positives += 1;
        if let Some(start) = component_ranges.first().map(|r| r.start) {
            if prev_start.is_some_and(|prev| start < prev) {
                in_order = false;
            }
            prev_start = Some(start);
        }
        ranges.extend(component_ranges);
    }

    if positives == 0 {
        // Purely negative query that excluded nothing: everything
        // passes, uniformly, with no highlight.
        return Some((MatchScore(UNIFORM_SCORE), Vec::new()));
    }

    let mut score = (total / u64::from(positives)) as u32;
    if positives >= 2 && in_order {
        score = score.saturating_add(ORDER_BONUS);
    }
    Some((MatchScore(score), merge_ranges(ranges)))
}

/// Sort and coalesce overlapping / touching byte ranges so the renderer
/// sees each highlighted span once. Components can legitimately overlap
/// (`"fo oo"` against `"foo"`), and a renderer painting the same byte
/// twice double-applies its emphasis attribute.
fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    if ranges.len() < 2 {
        return ranges;
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => {
                last.end = last.end.max(range.end);
            }
            _ => merged.push(range),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn comp(text: &str, negated: bool) -> OrderlessComponent {
        OrderlessComponent {
            text: text.to_string(),
            negated,
        }
    }

    // ---- parsing ----

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(
            parse_orderless_query("pic ref"),
            vec![comp("pic", false), comp("ref", false)]
        );
    }

    #[test]
    fn collapses_runs_of_whitespace_and_ignores_edges() {
        assert_eq!(
            parse_orderless_query("  pic   ref  "),
            vec![comp("pic", false), comp("ref", false)]
        );
    }

    #[test]
    fn leading_bang_negates_but_an_inner_bang_is_literal() {
        assert_eq!(
            parse_orderless_query("!test wat!"),
            vec![comp("test", true), comp("wat!", false)]
        );
    }

    #[test]
    fn backslash_space_joins_one_component() {
        assert_eq!(
            parse_orderless_query(r"my\ file rs"),
            vec![comp("my file", false), comp("rs", false)]
        );
    }

    #[test]
    fn backslash_escapes_a_leading_bang() {
        assert_eq!(
            parse_orderless_query(r"\!important"),
            vec![comp("!important", false)]
        );
    }

    /// A half-typed escape must not empty the result list — the user is
    /// still typing, and a picker that blanks mid-keystroke is unusable.
    #[test]
    fn a_trailing_backslash_is_literal_not_an_error() {
        assert_eq!(parse_orderless_query(r"foo\"), vec![comp(r"foo\", false)]);
    }

    #[test]
    fn a_bare_bang_excludes_nothing() {
        assert!(parse_orderless_query("!").is_empty());
    }

    // ---- matching ----

    /// The single-component case must be bit-for-bit the old behaviour:
    /// same score, same ranges. Anything else silently re-ranks every
    /// existing picker.
    #[test]
    fn a_single_component_delegates_verbatim_to_fuzzy_match() {
        for (query, target) in [
            ("file", "file"),
            ("fil", "file_12.rs"),
            ("fb", "foo_bar"),
            ("oo_b", "foo_bar"),
            ("fr", "foo_bar"),
            ("zzz", "foo_bar"),
            ("", "foo_bar"),
        ] {
            assert_eq!(
                orderless_match(query, target),
                crate::fuzzy_match(query, target),
                "query {query:?} against {target:?}"
            );
        }
    }

    #[test]
    fn all_components_must_match() {
        assert!(orderless_match("pic ref", "lattice-picker/src/refilter.rs").is_some());
        assert!(orderless_match("pic nope", "lattice-picker/src/refilter.rs").is_none());
    }

    /// The whole point: the components' order is not the candidate's.
    #[test]
    fn components_match_in_any_order() {
        let target = "lattice-picker/src/refilter.rs";
        let forward = orderless_match("pic ref", target).unwrap();
        let backward = orderless_match("ref pic", target).unwrap();
        assert!(
            forward.0 > backward.0,
            "typed-in-order should score higher ({:?} vs {:?})",
            forward.0,
            backward.0
        );
        assert_eq!(
            forward.0.0 - backward.0.0,
            ORDER_BONUS,
            "the only difference between the two is the order bonus"
        );
    }

    #[test]
    fn negation_excludes_a_matching_candidate() {
        assert!(orderless_match("parse !test", "src/parse.rs").is_some());
        assert!(orderless_match("parse !test", "src/parse_test.rs").is_none());
    }

    /// A purely negative query filters but does not rank: everything
    /// that survives is equally good.
    #[test]
    fn a_purely_negative_query_passes_everything_else_uniformly() {
        let (score, ranges) = orderless_match("!test", "src/parse.rs").unwrap();
        assert_eq!(score, MatchScore(UNIFORM_SCORE));
        assert!(ranges.is_empty());
        assert!(orderless_match("!test", "src/parse_test.rs").is_none());
    }

    #[test]
    fn an_escaped_space_matches_a_literal_space() {
        assert!(orderless_match(r"my\ file", "docs/my file.md").is_some());
        assert!(orderless_match(r"my\ file", "docs/myfile.md").is_none());
    }

    /// Score stays inside the single-token band so the picker's MRU
    /// bonus (0..~110, calibrated as a within-tier tie-break) keeps
    /// meaning the same thing under a multi-word query.
    #[test]
    fn score_is_the_mean_of_component_scores_not_the_sum() {
        let (score, _) = orderless_match("foo bar", "foo_bar").unwrap();
        assert!(
            score.0 <= 1000 + ORDER_BONUS,
            "score {score:?} escaped the single-token band"
        );
    }

    /// Prefix preference survives the split. This is the property that
    /// makes the permissive-per-component choice safe: both candidates
    /// match, but the one whose component lands on the prefix tier
    /// ranks above the one that only found a mid-word substring — so
    /// widening the match set does not scramble the ordering.
    #[test]
    fn prefix_hits_still_outrank_weaker_tiers() {
        let strong = orderless_match("pic ref", "picker_refilter.rs").unwrap().0;
        let weak = orderless_match("pic ref", "topical_reference.rs")
            .unwrap()
            .0;
        assert!(
            strong > weak,
            "prefix-tier component {strong:?} must beat substring-only {weak:?}"
        );
    }

    #[test]
    fn ranges_are_sorted_and_non_overlapping() {
        let (_, ranges) = orderless_match("ref pic", "lattice-picker/src/refilter.rs").unwrap();
        assert!(!ranges.is_empty());
        for pair in ranges.windows(2) {
            assert!(
                pair[0].end <= pair[1].start,
                "ranges must be sorted and disjoint: {ranges:?}"
            );
        }
    }

    #[test]
    fn overlapping_component_hits_merge_into_one_range() {
        let (_, ranges) = orderless_match("fo oo", "foo").unwrap();
        assert_eq!(ranges, vec![0..3]);
    }

    /// Every returned range must index real bytes of `target` — a range
    /// past the end panics the renderer's slice.
    #[test]
    fn ranges_are_valid_byte_offsets_into_the_target() {
        let target = "crates/lattice-picker/src/refilter.rs";
        let (_, ranges) = orderless_match("pic ref rs", target).unwrap();
        for r in &ranges {
            assert!(r.end <= target.len(), "range {r:?} past end of {target:?}");
            assert!(target.is_char_boundary(r.start) && target.is_char_boundary(r.end));
        }
    }

    /// Multi-byte targets must not produce ranges that split a
    /// codepoint — the picker renders arbitrary file names.
    #[test]
    fn non_ascii_targets_yield_char_boundary_ranges() {
        let target = "docs/日本語/naïve_pick.md";
        let (_, ranges) = orderless_match("pick md", target).unwrap();
        for r in &ranges {
            assert!(
                target.is_char_boundary(r.start) && target.is_char_boundary(r.end),
                "range {r:?} splits a codepoint in {target:?}"
            );
        }
    }

    #[test]
    fn matching_is_case_insensitive_across_components() {
        assert!(orderless_match("PIC ref", "lattice-picker/src/Refilter.rs").is_some());
        assert!(orderless_match("parse !TEST", "src/parse_test.rs").is_none());
    }
}
