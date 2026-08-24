//! Org headline structure, as pure line logic.
//!
//! ## Why text and not the parse tree
//!
//! `apply-action` receives `option<borrow<tree-snapshot>>` — this plugin's own
//! org tree — so walking it for `(headline (stars))` was the obvious route. It
//! is the wrong one here, for two reasons that are both paramount-goal
//! arguments rather than convenience ones:
//!
//! * **The tree can be absent.** `none` when the parse is pending. A chord that
//!   silently no-ops right after a paste, and works a moment later, is the
//!   worst kind of bug to report. Line logic has no such state (goal #2).
//! * **A query on the keystroke path costs more than the answer.** Recognising
//!   a headline is `^\*+\s` on one line. Compiling and running a query to learn
//!   the same fact is strictly more work inside the grammar seam's budget
//!   (goal #1).
//!
//! And it is not an approximation: a line whose first non-`*` character is a
//! space, at column 0, **is** a headline in org — including inside
//! `#+BEGIN_SRC`, which is precisely why org makes you escape such lines as
//! `,*`. Matching on text matches org's own rule.
//!
//! The tree stays valuable for what it is actually better at — tables, links,
//! blocks — and later slices use it there.
//!
//! ## Why these take a line accessor and not a `&[String]`
//!
//! Reading a line crosses the WASM boundary (`document.line(n)`), so
//! materialising the buffer to find one headline costs one guest→host call per
//! line — 10,000 of them on a 10,000-line org file, on every press of a key
//! that ends up editing three of them. At the seam's own budget that is
//! milliseconds, which is a missed frame (paramount goal #1).
//!
//! So every function here takes `line: impl Fn(u32) -> Option<String>` and
//! reads only what it needs: upward until a headline appears, downward until
//! the subtree ends. A headline operation touches a handful of lines whatever
//! the file size; a subtree operation touches its subtree, which it must read
//! anyway in order to rewrite it.

/// The level of `line` if it is a headline, else `None`.
///
/// A headline is one or more `*` at column 0 followed by a space. The trailing
/// space matters: `**bold**` at the start of a line is not a level-2 headline,
/// and neither is a lone `*` on its own line (org requires the space, and a
/// bare `*` is a list bullet).
pub fn headline_level(line: &str) -> Option<usize> {
    let stars = line.bytes().take_while(|b| *b == b'*').count();
    if stars == 0 {
        return None;
    }
    match line.as_bytes().get(stars) {
        Some(b' ') => Some(stars),
        _ => None,
    }
}

/// The headline at or above `from`, as `(line_index, level)`.
///
/// Walks backwards, so it answers "which headline am I under" from anywhere in
/// a subtree — the cursor is rarely sitting on the headline itself when you
/// promote it. `None` when nothing above the cursor is a headline (the preamble
/// before a file's first headline).
///
/// Reads one line at a time and stops at the first headline, so the cost is the
/// distance to it, not the size of the file.
pub fn enclosing_headline(line: impl Fn(u32) -> Option<String>, from: u32) -> Option<(u32, usize)> {
    (0..=from)
        .rev()
        .find_map(|i| headline_level(&line(i)?).map(|lvl| (i, lvl)))
}

/// The last line of the subtree rooted at headline line `start` (inclusive).
///
/// A subtree runs to just before the next headline of the same level or
/// shallower, so a level-2 headline's subtree swallows its level-3 children and
/// stops at the next level-2 or level-1. Runs to the last line when no such
/// headline follows.
///
/// Scans forward one line at a time and stops at the subtree's end, so a
/// headline near the top of a long file does not read the rest of it.
pub fn subtree_end(line: impl Fn(u32) -> Option<String>, start: u32, line_count: u32) -> u32 {
    let Some(level) = line(start).as_deref().and_then(headline_level) else {
        return start;
    };
    (start + 1..line_count)
        .find(|i| {
            line(*i)
                .as_deref()
                .and_then(headline_level)
                .is_some_and(|lvl| lvl <= level)
        })
        .map(|i| i - 1)
        .unwrap_or(line_count.saturating_sub(1))
}

/// Re-star a headline by `delta` levels, or `None` if `line` is not a headline
/// or the shift is refused.
///
/// Refused at level 1 going up: org has no level-0 headline, and turning `*
/// Title` into `Title` would silently convert a headline into body text of the
/// headline above — a destructive surprise from a key that means "promote".
/// Demotion has no ceiling; org files nest as deep as you like and only the
/// theme's size ramp runs out (it holds at level 6, see `doc/org.md`).
pub fn restar(line: &str, delta: isize) -> Option<String> {
    let level = headline_level(line)?;
    let new_level = level as isize + delta;
    if new_level < 1 {
        return None;
    }
    let rest = &line[level..];
    Some(format!("{}{}", "*".repeat(new_level as usize), rest))
}

/// Rewrite `lines[start..=end]`, shifting every headline among them by `delta`.
///
/// `start` must be the ROOT headline of the span, and if it cannot shift the
/// whole operation is refused — `None`, no edit at all.
///
/// That all-or-nothing rule is org's, and it matters. Promoting a level-1
/// subtree by moving only the descendants that *can* move would turn
///
/// ```org
/// * One
/// ** Child
/// ```
///
/// into two level-1 siblings: the child escapes its parent. Emacs refuses the
/// whole promote ("Cannot promote to level 0") for exactly this reason, and a
/// key that silently restructures a document is worse than one that declines.
///
/// Returning `None` rather than an unchanged string also lets the caller
/// decline the chord instead of pushing a no-op edit onto the undo stack.
///
/// Non-headline lines are copied verbatim, so a subtree's body text is
/// untouched even though the rewritten span covers it. The whole span is
/// replaced as ONE edit deliberately: N separate edits would be N undo steps,
/// and `u` after demoting a subtree must put every star back at once, not one
/// headline at a time.
pub fn shift_headlines(
    line: impl Fn(u32) -> Option<String>,
    start: u32,
    end: u32,
    delta: isize,
) -> Option<(String, u32)> {
    // The root gates the whole span. Every descendant is deeper, so if the
    // root can move they all can.
    restar(&line(start)?, delta)?;
    let mut out = String::new();
    let mut last_len = 0u32;
    for i in start..=end {
        let text = line(i)?;
        if i > start {
            out.push('\n');
        }
        // The ORIGINAL length: the edit's range is expressed in the document
        // as it stands, so the span ends at the old last line's end, not the
        // rewritten one. Capturing it here saves reading that line across the
        // boundary a second time.
        last_len = text.len() as u32;
        out.push_str(&restar(&text, delta).unwrap_or(text));
    }
    Some((out, last_len))
}

/// The next headline strictly after `from`, or `None` at the last one.
///
/// Any level — `]]` in org walks every headline, not only siblings. Stops at
/// the first hit, so cost is the distance to it.
pub fn next_headline(line: impl Fn(u32) -> Option<String>, from: u32, line_count: u32) -> Option<u32> {
    (from + 1..line_count).find(|i| line(*i).as_deref().and_then(headline_level).is_some())
}

/// The previous headline strictly before `from`, or `None` at the first one.
pub fn prev_headline(line: impl Fn(u32) -> Option<String>, from: u32) -> Option<u32> {
    (0..from)
        .rev()
        .find(|i| line(*i).as_deref().and_then(headline_level).is_some())
}

/// The parent of the headline enclosing `from` — the nearest headline above it
/// at a strictly shallower level.
///
/// `None` when the enclosing headline is already level 1, or when there is no
/// enclosing headline at all. Emacs's `outline-up-heading`, and the reason it
/// is a separate walk rather than "the previous headline": from a level-3
/// headline, the previous headline may be a level-3 sibling, and `g{` must skip
/// it to reach the level-2 parent.
pub fn parent_headline(line: impl Fn(u32) -> Option<String>, from: u32) -> Option<u32> {
    let (start, level) = enclosing_headline(&line, from)?;
    if level <= 1 {
        return None;
    }
    (0..start)
        .rev()
        .find(|i| {
            line(*i)
                .as_deref()
                .and_then(headline_level)
                .is_some_and(|lvl| lvl < level)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accessor the real callers pass, over an in-memory buffer.
    fn buf(text: &str) -> (impl Fn(u32) -> Option<String> + use<>, u32) {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let count = lines.len() as u32;
        (
            move |i: u32| lines.get(i as usize).cloned(),
            count,
        )
    }

    #[test]
    fn recognises_headlines_and_rejects_look_alikes() {
        assert_eq!(headline_level("* Top"), Some(1));
        assert_eq!(headline_level("*** Third"), Some(3));
        // No space ⇒ not a headline. `**bold**` at line start is the case that
        // would otherwise corrupt markup-heavy files.
        assert_eq!(headline_level("**bold** text"), None);
        assert_eq!(headline_level("*"), None);
        assert_eq!(headline_level(" * indented"), None);
        assert_eq!(headline_level("body text"), None);
        assert_eq!(headline_level(""), None);
        // A headline with no title is still a headline.
        assert_eq!(headline_level("** "), Some(2));
    }

    #[test]
    fn finds_the_enclosing_headline_from_inside_a_subtree() {
        let (l, _) = buf("* One\nbody\n** Two\nmore body\n");
        assert_eq!(enclosing_headline(&l, 0), Some((0, 1)));
        assert_eq!(enclosing_headline(&l, 1), Some((0, 1)));
        assert_eq!(enclosing_headline(&l, 3), Some((2, 2)));
    }

    /// The reason these take an accessor: the walk must stop at the headline,
    /// not read the file. Each read crosses the WASM boundary.
    #[test]
    fn the_upward_walk_stops_at_the_first_headline() {
        use std::cell::Cell;
        let reads = Cell::new(0u32);
        let lines: Vec<String> = std::iter::once("* Top".to_string())
            .chain((0..10_000).map(|i| format!("body {i}")))
            .collect();
        let accessor = |i: u32| {
            reads.set(reads.get() + 1);
            lines.get(i as usize).cloned()
        };
        // From the very bottom of a 10k-line file, one line above a headline.
        assert_eq!(enclosing_headline(&accessor, 5), Some((0, 1)));
        assert!(
            reads.get() <= 6,
            "read {} lines to find a headline 5 lines up",
            reads.get()
        );
    }

    #[test]
    fn a_preamble_has_no_enclosing_headline() {
        let (l, _) = buf("#+TITLE: Notes\n\n* First\n");
        assert_eq!(enclosing_headline(&l, 0), None);
        assert_eq!(enclosing_headline(&l, 1), None);
    }

    #[test]
    fn a_subtree_swallows_children_and_stops_at_a_peer() {
        let (l, n) = buf("* One\nbody\n** Child\nkid body\n* Two\ntail\n");
        assert_eq!(subtree_end(&l, 0, n), 3, "stops before the next level-1");
        assert_eq!(
            subtree_end(&l, 2, n),
            3,
            "the child ends where its parent does"
        );
        assert_eq!(subtree_end(&l, 4, n), 5, "the last subtree runs to the end");
    }

    #[test]
    fn a_shallower_headline_also_terminates_a_subtree() {
        let (l, n) = buf("* One\n*** Deep\nbody\n** Mid\n");
        assert_eq!(
            subtree_end(&l, 1, n),
            2,
            "level 3 stops at the level 2 below it"
        );
    }

    #[test]
    fn restar_shifts_and_refuses_to_promote_past_level_one() {
        assert_eq!(restar("** Two", -1).as_deref(), Some("* Two"));
        assert_eq!(restar("* One", 1).as_deref(), Some("** One"));
        assert_eq!(
            restar("* One", -1),
            None,
            "org has no level 0; promoting would turn a headline into body text"
        );
        assert_eq!(restar("body", 1), None);
    }

    #[test]
    fn shifting_a_subtree_leaves_body_text_alone() {
        let (l, _) = buf("* One\nbody\n** Child\n");
        let (out, last_len) = shift_headlines(&l, 0, 2, 1).expect("something changed");
        assert_eq!(out, "** One\nbody\n*** Child");
        assert_eq!(
            last_len,
            "** Child".len() as u32,
            "the ORIGINAL last line's length, since the edit range is in \
             current-document coordinates"
        );
    }

    #[test]
    fn shifting_reports_no_change_rather_than_a_no_op_edit() {
        let (l, _) = buf("* One\nbody\n");
        assert_eq!(
            shift_headlines(&l, 0, 1, -1),
            None,
            "a level-1 root that cannot promote yields no edit at all"
        );
        // A span whose root is not a headline at all is refused too.
        let (not_a_headline, _) = buf("body\n* Later\n");
        assert_eq!(shift_headlines(&not_a_headline, 0, 1, 1), None);
        let (body_only, _) = buf("body\nmore\n");
        assert_eq!(shift_headlines(&body_only, 0, 1, 1), None);
    }

    #[test]
    fn headline_motions_walk_every_level() {
        let (l, n) = buf("* One\nbody\n** Child\nkid\n* Two\n");
        assert_eq!(next_headline(&l, 0, n), Some(2), "]] from a headline");
        assert_eq!(next_headline(&l, 1, n), Some(2), "]] from body text");
        assert_eq!(next_headline(&l, 2, n), Some(4));
        assert_eq!(next_headline(&l, 4, n), None, "no headline after the last");

        assert_eq!(prev_headline(&l, 4), Some(2));
        assert_eq!(prev_headline(&l, 2), Some(0));
        assert_eq!(prev_headline(&l, 0), None, "nothing before the first");
        assert_eq!(prev_headline(&l, 1), Some(0), "[[ from body text");
    }

    #[test]
    fn parent_skips_siblings_to_reach_a_shallower_headline() {
        // From the second level-3, the PREVIOUS headline is its level-3
        // sibling; the parent is the level-2 above both. This is the whole
        // reason `g{` is not just `[[`.
        let (l, _) = buf("* One\n** Two\n*** A\n*** B\nbody\n");
        assert_eq!(parent_headline(&l, 3), Some(1));
        assert_eq!(parent_headline(&l, 4), Some(1), "from body under B");
        assert_eq!(parent_headline(&l, 1), Some(0));
        assert_eq!(parent_headline(&l, 0), None, "a level-1 has no parent");
    }

    #[test]
    fn parent_is_none_outside_any_headline() {
        let (l, _) = buf("#+TITLE: Notes\nprose\n");
        assert_eq!(parent_headline(&l, 1), None);
    }

    #[test]
    fn a_subtree_promote_is_all_or_nothing() {
        // If the root cannot promote, nothing moves. Shifting only the
        // children would make `** Child` a level-1 sibling of `* One` — the
        // child escapes its parent, which is document corruption from a key
        // that means "promote".
        let (l, _) = buf("* One\n** Child\n*** Grand\n");
        assert_eq!(
            shift_headlines(&l, 0, 2, -1),
            None,
            "org refuses the whole promote rather than restructuring"
        );
        // Demote has no such ceiling; everything moves together.
        let (out, _) = shift_headlines(&l, 0, 2, 1).expect("demote always fits");
        assert_eq!(out, "** One\n*** Child\n**** Grand");
    }
}
