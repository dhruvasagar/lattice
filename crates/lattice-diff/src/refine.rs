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

/// Above this share of a **line**, that line's refinement is noise
/// rather than signal.
///
/// If nearly all of a line changed, the uniform row tint has already
/// said so, and marking almost all of it adds a second colour saying
/// the same thing. Applied per line and per side — see
/// [`drop_noisy_lines`] for why the pair-coupled form DR.1 used cannot
/// survive region refinement.
const MAX_REFINED_SHARE: f64 = 0.70;

/// Above this many bytes on either side, refinement is skipped.
///
/// DR.5 diffs whole regions rather than line pairs, so a single
/// enormous hunk is now one token diff instead of many small ones. The
/// cap keeps that bounded. It costs nothing in practice: a hunk this
/// size is a wholesale rewrite, which [`MAX_REFINED_SHARE`] would
/// almost always decline anyway — this just declines it without doing
/// the work first.
const MAX_REGION_BYTES: usize = 64 * 1024;

/// The byte ranges that differ, per line, on each side of one hunk.
///
/// **Per side, not per pair** (DR.5). A hunk that removes one line and
/// adds twelve has no line pairing to speak of, so the two sides carry
/// independent per-line range lists: `removed[i]` describes the *i*-th
/// removed line, `added[j]` the *j*-th added one, and neither implies
/// the other's length.
///
/// This replaced a `Vec<Option<LineRefinement>>` that was indexed by
/// pair. That shape could not represent *n* removed against *m* added
/// at all, which is why the old code declined those hunks outright
/// rather than rendering them badly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegionRefinement {
    /// One entry per removed line, in order.
    pub removed: Vec<Vec<Range<usize>>>,
    /// One entry per added line, in order.
    pub added: Vec<Vec<Range<usize>>>,
}

impl RegionRefinement {
    /// True when neither side has a single refined range — the value a
    /// declined region carries, and the one that renders exactly as it
    /// did before refinement existed.
    pub fn is_empty(&self) -> bool {
        self.removed.iter().all(Vec::is_empty) && self.added.iter().all(Vec::is_empty)
    }

    /// Refined ranges on the *i*-th removed line. Empty — never a
    /// panic — for an index past the end, so a consumer walking a
    /// baseline range that outruns the refinement degrades to "no
    /// refinement here" instead of falling over.
    pub fn removed_line(&self, i: usize) -> &[Range<usize>] {
        self.removed.get(i).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Refined ranges on the *j*-th added line. Same tolerance as
    /// [`Self::removed_line`].
    pub fn added_line(&self, j: usize) -> &[Range<usize>] {
        self.added.get(j).map(Vec::as_slice).unwrap_or(&[])
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

/// One token of a whole *region* — a run of lines — remembering which
/// line inside the region it came from.
///
/// Diffing the region as one token stream is the whole of DR.5: it is
/// what lets an *n*-removed / *m*-added hunk refine without anyone
/// having to decide which added line "replaced" which removed one.
struct RegionToken<'a> {
    /// Index into the region's line slice.
    line: usize,
    /// Byte range within *that* line.
    range: Range<usize>,
    text: &'a str,
    /// A synthetic line break between two lines.
    ///
    /// Present so the matcher sees the line structure — without it, the
    /// last word of one line and the first of the next are adjacent
    /// tokens and can match across the boundary. Skipped when ranges
    /// are mapped back, since a line break is not a byte anyone tints.
    separator: bool,
}

/// Tokenise a region: every line's tokens in order, separated by a
/// synthetic line break.
fn tokenize_region<'a>(lines: &[&'a str]) -> Vec<RegionToken<'a>> {
    let mut out = Vec::new();
    for (line, text) in lines.iter().enumerate() {
        if line > 0 {
            out.push(RegionToken {
                line,
                range: 0..0,
                text: "\n",
                separator: true,
            });
        }
        for (range, tok) in tokenize(text) {
            out.push(RegionToken {
                line,
                range,
                text: tok,
                separator: false,
            });
        }
    }
    out
}

/// Scatter changed token-index ranges back onto their lines as byte
/// ranges, coalescing tokens that touch *within a line* so adjacent
/// changed tokens render as one highlight rather than a dotted line.
///
/// A changed range that spans a line break simply contributes to both
/// lines: each token knows its own line, so no range ever straddles
/// one.
fn to_line_ranges(
    tokens: &[RegionToken<'_>],
    idx: &[Range<u32>],
    line_count: usize,
) -> Vec<Vec<Range<usize>>> {
    let mut out: Vec<Vec<Range<usize>>> = vec![Vec::new(); line_count];
    for r in idx {
        let lo = r.start as usize;
        let hi = (r.end as usize).min(tokens.len());
        let Some(slice) = tokens.get(lo..hi) else {
            continue;
        };
        for token in slice {
            if token.separator {
                continue;
            }
            let Some(dst) = out.get_mut(token.line) else {
                continue;
            };
            match dst.last_mut() {
                Some(prev) if prev.end >= token.range.start => {
                    prev.end = prev.end.max(token.range.end)
                }
                _ => dst.push(token.range.clone()),
            }
        }
    }
    out
}

/// Refine one hunk's removed region against its added region.
///
/// **Region-to-region, not line-paired** (DR.5). The two runs are
/// tokenised whole, diffed as single token streams, and the changed
/// ranges scattered back onto whichever lines they fell on. Nothing
/// decides which added line "replaced" which removed one, because
/// nothing has to — which is exactly why an *n*-removed / *m*-added
/// hunk refines here and declined under the old pairing rule.
///
/// This is what the reference implementation does:
/// `magit-diff-update-hunk-refinement` hands the hunk's whole removed
/// and added regions to `smerge-refine-regions`. The predecessor's
/// claim that "magit declines the same case" was simply wrong.
///
/// Returns an empty [`RegionRefinement`] — which renders exactly as it
/// did before refinement existed, the direction this feature must fail
/// in — when refinement would be noise rather than signal:
///
/// - either side is absent (a pure `Add` or `Remove` has nothing to
///   compare against);
/// - the two regions are identical (nothing to say);
/// - either side is wholly changed past [`MAX_REFINED_SHARE`] — the
///   uniform row tint already conveys "this changed", and marking
///   nearly all of it adds a second colour saying the same thing;
/// - either side exceeds [`MAX_REGION_BYTES`] (see that constant).
pub fn refine_regions(removed: &[&str], added: &[&str]) -> RegionRefinement {
    if removed.is_empty() || added.is_empty() || removed == added {
        return RegionRefinement::default();
    }
    let rm_bytes: usize = removed.iter().map(|l| l.len()).sum();
    let add_bytes: usize = added.iter().map(|l| l.len()).sum();
    if rm_bytes > MAX_REGION_BYTES || add_bytes > MAX_REGION_BYTES {
        return RegionRefinement::default();
    }

    let rm_tokens = tokenize_region(removed);
    let add_tokens = tokenize_region(added);
    if rm_tokens.is_empty() || add_tokens.is_empty() {
        return RegionRefinement::default();
    }

    // Intern by hand rather than through `TokenSource`: that trait is
    // implemented for whole-text sources (lines, chars), and our tokens
    // are already computed. `InternedInput`'s fields are public for
    // exactly this — "while you can intern tokens yourself" in its own
    // docs — and it avoids a wrapper type existing only to satisfy a
    // trait we do not otherwise need.
    let mut interner: Interner<&str> = Interner::new(rm_tokens.len() + add_tokens.len());
    let before: Vec<Token> = rm_tokens.iter().map(|t| interner.intern(t.text)).collect();
    let after: Vec<Token> = add_tokens.iter().map(|t| interner.intern(t.text)).collect();
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

    let mut refinement = RegionRefinement {
        removed: to_line_ranges(&rm_tokens, &before_idx, removed.len()),
        added: to_line_ranges(&add_tokens, &after_idx, added.len()),
    };
    drop_noisy_lines(&mut refinement.removed, removed);
    drop_noisy_lines(&mut refinement.added, added);
    if refinement.is_empty() {
        return RegionRefinement::default();
    }
    refinement
}

/// Clear the refinement of any line more than [`MAX_REFINED_SHARE`]
/// changed, leaving its neighbours alone.
///
/// **Per line, and per side** — DR.1 applied this per *pair* and
/// required BOTH sides to come in under the bar, declining the pair
/// outright otherwise. That coupling was an artifact of the pair being
/// its unit, and carrying it into DR.5 actively breaks the case DR.5
/// exists to fix: in an *n*-removed / *m*-added hunk the surplus added
/// lines are wholly new **by definition**, so any region-wide or
/// cross-side measure is dragged over the bar by lines that were never
/// candidates for refinement in the first place.
///
/// Per line is also simply the right question. Refinement is *rendered*
/// per line, so "does this emphasis tell the reader anything?" is asked
/// of one line at a time: a wholly-new line is already fully tinted by
/// its row, and whether some other line on the opposite side is mostly
/// changed has no bearing on the line in front of you.
fn drop_noisy_lines(per_line: &mut [Vec<Range<usize>>], lines: &[&str]) {
    for (i, ranges) in per_line.iter_mut().enumerate() {
        let len = lines.get(i).map(|l| l.len()).unwrap_or(0);
        if len == 0 {
            ranges.clear();
            continue;
        }
        let covered: usize = ranges.iter().map(|r| r.end - r.start).sum();
        if (covered as f64) / (len as f64) > MAX_REFINED_SHARE {
            ranges.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice<'a>(line: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|r| &line[r.clone()]).collect()
    }

    /// Refine a one-line-against-one-line region and read both sides.
    /// The balanced case is now just the degenerate region.
    fn pair(removed: &str, added: &str) -> RegionRefinement {
        refine_regions(&[removed], &[added])
    }

    // ── the single-line cases DR.1 established ───────────────────────
    //
    // Kept verbatim in intent: DR.5 must be a SUPERSET, so every
    // balanced result these pinned has to survive the algorithm change.

    #[test]
    fn a_one_word_change_refines_to_that_word() {
        let r = pair("let x = compute(a);", "let x = derive(a);");
        assert_eq!(
            slice("let x = compute(a);", r.removed_line(0)),
            vec!["compute"]
        );
        assert_eq!(slice("let x = derive(a);", r.added_line(0)), vec!["derive"]);
    }

    /// The rename case: only the identifier moves, not the punctuation
    /// around it. This is what word-level buys over character-level.
    #[test]
    fn a_rename_does_not_bleed_into_neighbours() {
        let before = "foo(bar, baz)";
        let after = "foo(qux, baz)";
        let r = pair(before, after);
        assert_eq!(slice(before, r.removed_line(0)), vec!["bar"]);
        assert_eq!(slice(after, r.added_line(0)), vec!["qux"]);
    }

    #[test]
    fn identical_lines_refine_to_nothing() {
        assert!(pair("same", "same").is_empty());
    }

    /// If nearly everything changed, the uniform tint already said so.
    #[test]
    fn a_wholly_different_line_declines_refinement() {
        assert!(pair("alpha beta gamma", "one two three four").is_empty());
    }

    /// A punctuation-only change still refines — the tokenizer gives
    /// each non-word char its own token precisely so this works.
    #[test]
    fn a_punctuation_only_change_refines() {
        let r = pair("a[i]", "a(i)");
        assert!(!r.removed_line(0).is_empty() && !r.added_line(0).is_empty());
    }

    /// Adjacent changed tokens coalesce into one range rather than a
    /// dotted line of separate highlights.
    #[test]
    fn adjacent_changed_tokens_coalesce() {
        let r = pair("let value = 1;", "let other_name = 1;");
        assert_eq!(
            r.added_line(0).len(),
            1,
            "one contiguous highlight, got {:?}",
            r.added_line(0)
        );
    }

    /// Ranges must be sliceable from the original string — a panic
    /// here would mean a boundary landed mid-codepoint.
    #[test]
    fn multibyte_ranges_land_on_char_boundaries() {
        let before = "let gruß = 1;";
        let after = "let grüße = 1;";
        let r = pair(before, after);
        // Slicing is the assertion: it panics on a bad boundary.
        let _ = slice(before, r.removed_line(0));
        let _ = slice(after, r.added_line(0));
    }

    #[test]
    fn a_pure_addition_or_removal_refines_nothing() {
        assert!(refine_regions(&[], &["brand new line"]).is_empty());
        assert!(refine_regions(&["deleted line"], &[]).is_empty());
    }

    // ── DR.5: unbalanced regions ─────────────────────────────────────

    /// **The reported case, verbatim.** One line rewritten with a
    /// doc-comment block added above it — 1 removed against 12 added.
    /// The old pairing rule declined this outright; it is the shape
    /// "rewrite a line and document it" produces every time.
    #[test]
    fn one_removed_against_twelve_added_still_refines() {
        let removed = ["#[derive(Debug, Clone, PartialEq, Eq, Serialize)]"];
        let added = [
            "/// Doc line one.",
            "///",
            "/// Doc line two.",
            "/// Doc line three.",
            "/// Doc line four.",
            "/// Doc line five.",
            "/// Doc line six.",
            "/// Doc line seven.",
            "/// Doc line eight.",
            "/// Doc line nine.",
            "/// Doc line ten.",
            "#[derive(Debug, Clone, PartialEq, Serialize)]",
        ];
        let r = refine_regions(&removed, &added);
        assert!(
            !r.is_empty(),
            "an unbalanced hunk must refine — this is the DR.5 bug"
        );
        // Which side of the comma the matcher attributes the deletion
        // to (`Eq, ` vs `, Eq`) is a legitimate tokenisation choice and
        // not worth pinning; that it marks the dropped derive, and only
        // a few bytes of the line, is the claim.
        let marked = slice(removed[0], r.removed_line(0)).concat();
        assert!(
            marked.contains("Eq"),
            "the removed side marks the dropped derive, got {marked:?}"
        );
        assert!(
            marked.len() <= 6,
            "and marks only it, not the whole derive: {marked:?}"
        );
    }

    /// The other captured case: 6 removed against 2 added still
    /// refines, on both sides.
    #[test]
    fn six_removed_against_two_added_refines_on_both_sides() {
        let removed = [
            "/// Build the sources + excerpts for a set of changed files.",
            "///",
            "/// `files` is `(path, baseline_text)`; the working-tree text is read",
            "/// from disk. Returns `None` when there is nothing to show — no",
            "/// changed files, or every one unreadable.",
            "///",
        ];
        let added = [
            "/// One changed file, read and diffed: the working-tree text plus the",
            "/// post-image ranges its hunks occupy.",
        ];
        let r = refine_regions(&removed, &added);
        assert!(!r.is_empty(), "unbalanced region refines");
        assert!(
            r.removed.iter().any(|l| !l.is_empty()),
            "the removed side carries ranges"
        );
        assert!(
            r.added.iter().any(|l| !l.is_empty()),
            "the added side carries ranges"
        );
    }

    /// Every side's vec is exactly as long as its own line count —
    /// consumers index by `line - range.start`, so a short vec would
    /// silently drop the tail's refinement.
    #[test]
    fn each_side_is_indexed_by_its_own_line_count() {
        let r = refine_regions(&["a = 1;"], &["a = 2;", "b = 3;", "c = 4;"]);
        assert_eq!(r.removed.len(), 1);
        assert_eq!(r.added.len(), 3);
    }

    /// A range must never straddle a line break: each token carries its
    /// own line, and the synthetic separator is dropped on the way out.
    #[test]
    fn ranges_never_straddle_a_line_boundary() {
        let removed = ["alpha one;", "beta two;"];
        let added = ["alpha ONE;", "beta TWO;"];
        let r = refine_regions(&removed, &added);
        for (i, line) in removed.iter().enumerate() {
            for range in r.removed_line(i) {
                assert!(
                    range.end <= line.len(),
                    "range {range:?} runs past line {i} ({line:?})"
                );
            }
        }
        for (i, line) in added.iter().enumerate() {
            for range in r.added_line(i) {
                assert!(range.end <= line.len(), "range {range:?} past line {i}");
            }
        }
    }

    /// Indexing past either side is empty, not a panic — a consumer
    /// walking a baseline range wider than the refinement degrades.
    #[test]
    fn indexing_past_the_end_is_empty_not_a_panic() {
        let r = refine_regions(&["a = 1;"], &["a = 2;"]);
        assert!(r.removed_line(99).is_empty());
        assert!(r.added_line(99).is_empty());
    }

    /// A wholly-rewritten region still declines, measured over the
    /// region rather than per line.
    #[test]
    fn a_wholly_rewritten_region_declines() {
        let r = refine_regions(
            &["alpha beta gamma", "delta epsilon zeta"],
            &["one two three", "four five six"],
        );
        assert!(r.is_empty());
    }

    /// A line barely touched keeps its refinement even when a
    /// neighbour in the same region changed a lot — the region-level
    /// threshold must not be an all-or-nothing per-line gate.
    #[test]
    fn a_small_change_survives_beside_a_larger_one() {
        let removed = [
            "let alpha = compute(a);",
            "let beta = compute(b);",
            "let gamma = compute(c);",
        ];
        let added = [
            "let alpha = derive(a);",
            "let beta = compute(b);",
            "let gamma = compute(c);",
        ];
        let r = refine_regions(&removed, &added);
        assert_eq!(slice(removed[0], r.removed_line(0)), vec!["compute"]);
    }

    /// The size cap declines rather than diffing an enormous region.
    #[test]
    fn an_enormous_region_declines_without_diffing() {
        let huge = "x".repeat(MAX_REGION_BYTES + 1);
        let r = refine_regions(&[huge.as_str()], &["small"]);
        assert!(r.is_empty());
    }
}
