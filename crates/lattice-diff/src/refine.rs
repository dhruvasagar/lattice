//! DR.1 (2026-08-12): **intra-line refinement** — which *part* of a
//! changed line changed.
//!
//! Design: `docs/dev/architecture/diff-refinement.md`. Slice plan:
//! `docs/dev/operations/slice-plans/diff-refinement.md`.
//!
//! Every diff surface colours a changed line uniformly, so a
//! one-character change reads exactly like a rewritten one. This
//! computes the byte ranges that actually differ between a removed
//! line and the added line that replaced it; the presentation layers
//! (DR.2–DR.4) tint those ranges more strongly.
//!
//! Pure and consumer-agnostic. It lives here rather than in
//! `lattice-magit` because `diff-mode`'s side-by-side panes have the
//! identical gap — magit is the first consumer, not the owner.
//!
//! ## Word-level, deliberately
//!
//! Character-level diffing of source code produces confetti: matching
//! brackets and single letters scatter through a rename and read worse
//! than no refinement at all. Word-level is what magit, delta and
//! GitHub use.

use std::ops::Range;

use imara_diff::intern::{InternedInput, Interner, Token};
use imara_diff::{Algorithm, Sink};

/// Above this share of a line, refinement is noise rather than signal.
///
/// If nearly everything changed, the uniform row tint has already said
/// so, and marking it all just adds a second colour saying the same
/// thing. Both sides must come in under the threshold — a short line
/// replaced by a long one is "mostly changed" from the short line's
/// point of view even when the long one looks barely touched.
const MAX_REFINED_SHARE: f64 = 0.70;

/// The byte ranges that differ, on each side of one paired line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineRefinement {
    /// Ranges within the removed line.
    pub removed: Vec<Range<usize>>,
    /// Ranges within the added line.
    pub added: Vec<Range<usize>>,
}

impl LineRefinement {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty()
    }
}

/// One token of a line: a byte range plus its text.
///
/// A "word" is a maximal run of `[A-Za-z0-9_]`; every other character
/// is its own token. That keeps identifiers whole — the common rename
/// case — while still letting a punctuation-only change refine.
///
/// Tokenising on `char_indices` means every boundary is a character
/// boundary, so the ranges this produces can always be sliced from the
/// original string.
fn tokenize(line: &str) -> Vec<(Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if is_word_char(c) {
            let mut end = start + c.len_utf8();
            while let Some(&(i, next)) = chars.peek() {
                if is_word_char(next) {
                    end = i + next.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            out.push((start..end, &line[start..end]));
        } else {
            let end = start + c.len_utf8();
            out.push((start..end, &line[start..end]));
        }
    }
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Collects changed token index ranges from `imara-diff`.
struct TokenSink {
    before: Vec<Range<u32>>,
    after: Vec<Range<u32>>,
}

impl Sink for TokenSink {
    type Out = (Vec<Range<u32>>, Vec<Range<u32>>);

    fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
        if !before.is_empty() {
            self.before.push(before);
        }
        if !after.is_empty() {
            self.after.push(after);
        }
    }

    fn finish(self) -> Self::Out {
        (self.before, self.after)
    }
}

/// Merge token-index ranges into byte ranges over `tokens`,
/// coalescing runs that touch so adjacent changed tokens render as one
/// highlight rather than a dotted line.
fn to_byte_ranges(tokens: &[(Range<usize>, &str)], idx: &[Range<u32>]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in idx {
        let (Some(first), Some(last)) = (
            tokens.get(r.start as usize),
            tokens.get(r.end.saturating_sub(1) as usize),
        ) else {
            continue;
        };
        let span = first.0.start..last.0.end;
        match out.last_mut() {
            Some(prev) if prev.end >= span.start => prev.end = span.end,
            _ => out.push(span),
        }
    }
    out
}

/// Refine one removed / added line pair.
///
/// `None` when refinement would be noise rather than signal:
///
/// - the lines are identical (nothing to say);
/// - either side is wholly changed past [`MAX_REFINED_SHARE`] — the
///   uniform row tint already conveys "this line changed", and marking
///   nearly all of it adds a second colour saying the same thing.
///
/// Returning `None` degrades to exactly today's appearance, which is
/// the direction this feature must fail in.
pub fn refine_pair(removed: &str, added: &str) -> Option<LineRefinement> {
    if removed == added {
        return None;
    }
    let rm_tokens = tokenize(removed);
    let add_tokens = tokenize(added);
    if rm_tokens.is_empty() || add_tokens.is_empty() {
        return None;
    }

    // Intern by hand rather than through `TokenSource`: that trait is
    // implemented for whole-text sources (lines, chars), and our tokens
    // are already computed. `InternedInput`'s fields are public for
    // exactly this — "while you can intern tokens yourself" in its own
    // docs — and it avoids a wrapper type existing only to satisfy a
    // trait we do not otherwise need.
    let mut interner: Interner<&str> = Interner::new(rm_tokens.len() + add_tokens.len());
    let before: Vec<Token> = rm_tokens.iter().map(|(_, t)| interner.intern(*t)).collect();
    let after: Vec<Token> = add_tokens
        .iter()
        .map(|(_, t)| interner.intern(*t))
        .collect();
    let input = InternedInput {
        before,
        after,
        interner,
    };
    let (before_idx, after_idx) = imara_diff::diff(
        Algorithm::Histogram,
        &input,
        TokenSink {
            before: Vec::new(),
            after: Vec::new(),
        },
    );

    let refinement = LineRefinement {
        removed: to_byte_ranges(&rm_tokens, &before_idx),
        added: to_byte_ranges(&add_tokens, &after_idx),
    };
    if refinement.is_empty() {
        return None;
    }
    if over_threshold(&refinement.removed, removed.len())
        || over_threshold(&refinement.added, added.len())
    {
        return None;
    }
    Some(refinement)
}

fn over_threshold(ranges: &[Range<usize>], line_len: usize) -> bool {
    if line_len == 0 {
        return false;
    }
    let covered: usize = ranges.iter().map(|r| r.end - r.start).sum();
    (covered as f64) / (line_len as f64) > MAX_REFINED_SHARE
}

/// Pair the removed and added lines of one hunk and refine each pair.
///
/// Returns one entry per pair, in order, aligned with the input runs.
///
/// **Pairs positionally, and only when the two runs are the same
/// length.** Three removals against five additions gives no principled
/// answer to which addition replaced which removal, and a wrong guess
/// produces confident, incorrect emphasis — worse than none, because
/// the reader trusts it. Magit declines the same case. Similarity
/// scoring could do better and is deferred until the simple rule
/// proves insufficient in use.
pub fn refine_runs(removed: &[&str], added: &[&str]) -> Vec<Option<LineRefinement>> {
    if removed.len() != added.len() {
        return Vec::new();
    }
    removed
        .iter()
        .zip(added.iter())
        .map(|(r, a)| refine_pair(r, a))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice<'a>(line: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|r| &line[r.clone()]).collect()
    }

    #[test]
    fn a_one_word_change_refines_to_that_word() {
        let r = refine_pair("let x = compute(a);", "let x = derive(a);").unwrap();
        assert_eq!(slice("let x = compute(a);", &r.removed), vec!["compute"]);
        assert_eq!(slice("let x = derive(a);", &r.added), vec!["derive"]);
    }

    /// The rename case: only the identifier moves, not the punctuation
    /// around it. This is what word-level buys over character-level.
    #[test]
    fn a_rename_does_not_bleed_into_neighbours() {
        let before = "foo(bar, baz)";
        let after = "foo(qux, baz)";
        let r = refine_pair(before, after).unwrap();
        assert_eq!(slice(before, &r.removed), vec!["bar"]);
        assert_eq!(slice(after, &r.added), vec!["qux"]);
    }

    #[test]
    fn identical_lines_refine_to_nothing() {
        assert!(refine_pair("same", "same").is_none());
    }

    /// If nearly everything changed, the uniform tint already said so.
    #[test]
    fn a_wholly_different_line_declines_refinement() {
        assert!(refine_pair("alpha beta gamma", "one two three four").is_none());
    }

    /// A punctuation-only change still refines — the tokenizer gives
    /// each non-word char its own token precisely so this works.
    #[test]
    fn a_punctuation_only_change_refines() {
        let r = refine_pair("a[i]", "a(i)").unwrap();
        assert!(!r.removed.is_empty() && !r.added.is_empty());
    }

    /// Adjacent changed tokens coalesce into one range rather than a
    /// dotted line of separate highlights.
    #[test]
    fn adjacent_changed_tokens_coalesce() {
        let before = "let value = 1;";
        let after = "let other_name = 1;";
        let r = refine_pair(before, after).unwrap();
        assert_eq!(
            r.added.len(),
            1,
            "one contiguous highlight, got {:?}",
            r.added
        );
    }

    /// Ranges must be sliceable from the original string — a panic
    /// here would mean a boundary landed mid-codepoint.
    #[test]
    fn multibyte_ranges_land_on_char_boundaries() {
        let before = "let gruß = 1;";
        let after = "let grüße = 1;";
        let r = refine_pair(before, after).unwrap();
        // Slicing is the assertion: it panics on a bad boundary.
        let _ = slice(before, &r.removed);
        let _ = slice(after, &r.added);
    }

    #[test]
    fn an_empty_side_refines_to_nothing() {
        assert!(refine_pair("", "added").is_none());
        assert!(refine_pair("removed", "").is_none());
    }

    // ── pairing ──────────────────────────────────────────────────────

    #[test]
    fn equal_length_runs_pair_positionally() {
        let out = refine_runs(&["let a = 1;", "let b = 2;"], &["let a = 9;", "let b = 8;"]);
        assert_eq!(out.len(), 2);
        assert!(out[0].is_some() && out[1].is_some());
    }

    /// The case where a wrong guess would be confidently misleading.
    #[test]
    fn unequal_runs_refine_nothing() {
        assert!(refine_runs(&["a", "b", "c"], &["x", "y"]).is_empty());
    }

    /// A pure addition has no removed counterpart to compare against.
    #[test]
    fn a_pure_addition_refines_nothing() {
        assert!(refine_runs(&[], &["brand new line"]).is_empty());
    }

    /// A pair inside an otherwise-refinable run can decline on its own
    /// without taking the rest of the run with it.
    #[test]
    fn one_declining_pair_does_not_cancel_its_neighbours() {
        let out = refine_runs(
            &["let a = 1;", "alpha beta gamma"],
            &["let a = 9;", "one two three four"],
        );
        assert_eq!(out.len(), 2);
        assert!(out[0].is_some(), "the small change still refines");
        assert!(out[1].is_none(), "the wholly-different pair declines");
    }
}
