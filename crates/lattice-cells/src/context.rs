//! Structural context scopes and the resolver that turns them into the
//! header lines a pane pins above its text.
//!
//! A [`ContextScope`] is a structural range plus the line span that names
//! it — `impl Renderer for TuiRenderer {` naming the impl block, `fn
//! paint(…) {` naming the function. It is a **pure function of the parse
//! tree**: no viewport, no cursor, no fold state, no user option. That is
//! what makes it correct to compute once per parse (off-thread, in a
//! plugin) and resolve against any anchor line afterwards.
//!
//! [`resolve_context`] is the resolution half, and it lives here rather
//! than in either renderer for the same reason `IndentBlock::paints_on`
//! does: one implementation means a bug is a failing test here instead of
//! a wrong strip in one peer and not the other. The host calls it when it
//! publishes pane inputs, so the scroll model reserves exactly the rows
//! that get painted.
//!
//! Design anchor: `docs/dev/architecture/treesitter-context.md`.

/// One structural scope: the range it spans, and the lines that name it.
///
/// `header_start ..= header_end` is normally a single line; it spans
/// several when a signature wraps. Both ends are inclusive, 0-based
/// source lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextScope {
    pub scope_start: u32,
    pub scope_end: u32,
    pub header_start: u32,
    pub header_end: u32,
}

/// Which end of the stack to drop when there are more context rows than
/// the budget allows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrimScope {
    /// Drop the outermost scopes first. The default: the innermost scope
    /// is the one you are actually in, so it is the last to go.
    #[default]
    Outer,
    /// Drop the innermost scopes first.
    Inner,
}

/// The knobs [`resolve_context`] reads. Mirrors the `context.*` options
/// the plugin registers; the host resolves them per buffer and passes
/// them in, so this stays a pure function.
#[derive(Clone, Copy, Debug)]
pub struct ContextOptions {
    /// Maximum context **rows** (not scopes — a multi-line header spends
    /// more than one).
    pub max_lines: u32,
    /// Which end to drop when over budget.
    pub trim: TrimScope,
    /// Maximum rows a single scope's header may contribute.
    pub multiline_threshold: u32,
    /// Percent of the pane height the whole sticky strip may occupy.
    pub max_viewport_fraction: u32,
    /// The pane's height in rows, for the fraction guard.
    pub viewport_height: u32,
    /// First source line the pane is showing. A scope whose header is at
    /// or below this line is still on screen, so pinning it would spend a
    /// row duplicating a visible line — the resolver drops it.
    pub viewport_top: u32,
    /// Rows the headerline already occupies. Context stacks *under* it
    /// and never displaces it, so those rows come out of the same
    /// viewport budget.
    pub reserved_rows: u32,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            max_lines: 3,
            trim: TrimScope::Outer,
            multiline_threshold: 1,
            max_viewport_fraction: 33,
            viewport_height: 40,
            viewport_top: 0,
            reserved_rows: 0,
        }
    }
}

/// Resolve the context header lines for `anchor`.
///
/// Returns source line numbers, outermost scope first, ready for the
/// cells worker to build rows from. Empty when nothing encloses the
/// anchor or the budget leaves no room.
pub fn resolve_context(scopes: &[ContextScope], anchor: u32, opts: &ContextOptions) -> Vec<u32> {
    let mut enclosing: Vec<&ContextScope> = scopes
        .iter()
        .filter(|s| s.scope_start <= anchor && anchor <= s.scope_end)
        // Only what actually scrolled away: a header still on screen
        // would cost a row to duplicate a line the user can already read.
        .filter(|s| s.header_end < opts.viewport_top)
        .collect();
    enclosing.sort_by_key(|s| s.scope_start);

    // Expand each scope to the header rows it contributes, capped by
    // `multiline_threshold`. A wrapped signature names its scope across
    // several lines and spends several rows.
    let cap = opts.multiline_threshold.max(1);
    let groups: Vec<Vec<u32>> = enclosing
        .iter()
        .map(|s| {
            let last = s.header_end.min(s.header_start.saturating_add(cap - 1));
            (s.header_start..=last).collect()
        })
        .collect();

    // Trim to the ROW budget from whichever end `trim` names. A group
    // that does not fit stops the walk rather than being skipped over:
    // keeping a further-out scope after dropping a nearer one would
    // render a stack with a hole in it, which reads as simply wrong.
    // The budget is the tighter of `max_lines` and this pane's share of
    // the viewport, minus whatever the headerline already holds — context
    // stacks under the headerline and never displaces it.
    let share = (opts.viewport_height as u64 * opts.max_viewport_fraction as u64 / 100) as u32;
    let budget = opts.max_lines.min(share.saturating_sub(opts.reserved_rows)) as usize;
    let mut rows: Vec<u32> = Vec::with_capacity(budget);
    match opts.trim {
        // Keep the innermost — walk inward-out, building the strip
        // backwards, so the survivors are the scopes you are actually in.
        TrimScope::Outer => {
            for group in groups.iter().rev() {
                if rows.len() + group.len() > budget {
                    break;
                }
                rows.splice(0..0, group.iter().copied());
            }
        }
        TrimScope::Inner => {
            for group in &groups {
                if rows.len() + group.len() > budget {
                    break;
                }
                rows.extend(group.iter().copied());
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scope at `start..=end` whose header is its first line.
    fn scope(start: u32, end: u32) -> ContextScope {
        ContextScope {
            scope_start: start,
            scope_end: end,
            header_start: start,
            header_end: start,
        }
    }

    /// The whole point of the strip is to show what scrolled away. A
    /// scope whose header is still on screen must NOT be pinned — doing
    /// so spends a row duplicating a line the user can already read, and
    /// on a short pane that is a row the innermost scope needed.
    ///
    /// This is why the resolver needs `viewport_top` and not just the
    /// anchor: with the cursor at 30 and the impl header at 10, whether
    /// line 10 is visible depends entirely on where the view starts.
    #[test]
    fn a_scope_whose_header_is_still_visible_is_not_pinned() {
        let scopes = [scope(10, 99), scope(20, 40)];

        // View starts at 5: the impl header (10) and the fn header (20)
        // are both on screen, so there is nothing to pin.
        let opts = ContextOptions {
            viewport_top: 5,
            ..ContextOptions::default()
        };
        assert_eq!(
            resolve_context(&scopes, 30, &opts),
            Vec::<u32>::new(),
            "both headers are visible — pinning either duplicates a line \
             already on screen"
        );

        // View starts at 15 — between the two headers. The impl header
        // (10) has scrolled off; the fn header (20) is still on screen.
        let opts = ContextOptions {
            viewport_top: 15,
            ..ContextOptions::default()
        };
        assert_eq!(
            resolve_context(&scopes, 30, &opts),
            vec![10],
            "only the header that actually scrolled away is pinned"
        );
    }

    /// `max_lines` is a budget in ROWS, and `trim_scope` picks which end
    /// loses. Default `Outer`: the innermost scope is the one you are
    /// actually in, so it survives longest.
    #[test]
    fn over_budget_drops_the_end_trim_scope_names() {
        // mod 5.., impl 10.., fn 20.., loop 25.. — four deep, cursor at 30.
        let scopes = [scope(5, 99), scope(10, 90), scope(20, 40), scope(25, 35)];
        let base = ContextOptions {
            viewport_top: 28,
            max_lines: 2,
            ..ContextOptions::default()
        };

        assert_eq!(
            resolve_context(&scopes, 30, &base),
            vec![20, 25],
            "trim outer keeps the innermost scopes — the ones you are in"
        );

        let inner = ContextOptions {
            trim: TrimScope::Inner,
            ..base
        };
        assert_eq!(
            resolve_context(&scopes, 30, &inner),
            vec![5, 10],
            "trim inner keeps the outermost, and STILL emits them \
             outermost-first — trimming picks which scopes survive, never \
             the order they paint in"
        );
    }

    /// A wrapped signature names its scope across several lines.
    /// `multiline_threshold` caps how many of them a single scope may
    /// spend, and those rows come out of the SAME `max_lines` budget —
    /// which is the whole reason the budget counts rows and not scopes.
    #[test]
    fn a_multiline_header_spends_rows_from_the_shared_budget() {
        let outer = scope(10, 99);
        // `fn long_signature(` at 20, wrapping through 22.
        let inner = ContextScope {
            scope_start: 20,
            scope_end: 40,
            header_start: 20,
            header_end: 22,
        };
        let scopes = [outer, inner];
        let base = ContextOptions {
            viewport_top: 28,
            max_lines: 3,
            ..ContextOptions::default()
        };

        assert_eq!(
            resolve_context(&scopes, 30, &base),
            vec![10, 20],
            "threshold 1 (the default) shows only the signature's first \
             line, so both scopes fit in the budget"
        );

        let full = ContextOptions {
            multiline_threshold: 3,
            ..base
        };
        assert_eq!(
            resolve_context(&scopes, 30, &full),
            vec![20, 21, 22],
            "the full 3-line signature spends the whole budget, so the \
             outer scope is trimmed — rows, not scopes"
        );
    }

    /// A sticky strip that eats the pane is worse than no strip. The
    /// guard is a FRACTION rather than a row count so it scales with the
    /// split — a 6-row pane and a 60-row pane want very different limits
    /// and neither wants to be told a constant.
    #[test]
    fn the_strip_never_outgrows_its_share_of_the_pane() {
        let scopes = [scope(5, 99), scope(10, 90), scope(20, 40)];
        let at = |viewport_height| ContextOptions {
            viewport_top: 28,
            viewport_height,
            ..ContextOptions::default()
        };

        // Roomy: `max_lines` (3) is what binds, not the fraction (33).
        assert_eq!(resolve_context(&scopes, 30, &at(100)), vec![5, 10, 20]);

        // 10 rows × 33% = 3 — the two limits coincide.
        assert_eq!(resolve_context(&scopes, 30, &at(10)), vec![5, 10, 20]);

        // 3 rows × 33% = 0. A pane this short has no room to spare, and
        // showing nothing is the honest answer.
        assert_eq!(
            resolve_context(&scopes, 30, &at(3)),
            Vec::<u32>::new(),
            "a pane too short for even one context row shows none"
        );
    }

    /// The headerline is never displaced — context stacks under it — so
    /// the rows it already occupies come out of the same viewport share.
    #[test]
    fn the_headerline_rows_come_out_of_the_same_budget() {
        let scopes = [scope(5, 99), scope(10, 90), scope(20, 40)];
        let opts = ContextOptions {
            viewport_top: 28,
            viewport_height: 12, // 12 × 33% = 3 rows for the whole strip
            reserved_rows: 2,    // headerline already took two of them
            ..ContextOptions::default()
        };

        assert_eq!(
            resolve_context(&scopes, 30, &opts),
            vec![20],
            "one row left after the headerline, and it goes to the \
             innermost scope"
        );
    }

    /// Standing ON a scope's header line still counts as being inside
    /// that scope — which is what makes `[u` terminate. The jump lands
    /// the cursor on the header, and the next `[u` looks for a header
    /// STRICTLY above, so it finds the parent instead of sticking.
    #[test]
    fn a_scope_encloses_its_own_header_line() {
        let scopes = [scope(10, 99), scope(20, 40)];

        let scrolled_past = ContextOptions {
            viewport_top: 22,
            ..ContextOptions::default()
        };
        assert_eq!(
            resolve_context(&scopes, 20, &scrolled_past),
            vec![10, 20],
            "the cursor sits on the fn header; both headers are above the \
             view, so both pin"
        );

        let still_visible = ContextOptions {
            viewport_top: 15,
            ..ContextOptions::default()
        };
        assert_eq!(
            resolve_context(&scopes, 20, &still_visible),
            vec![10],
            "the fn header is the cursor's own line and plainly on screen"
        );
    }

    // ── Degenerate input. These passed on arrival; they are regression
    // guards for a resolver that is about to be called at keystroke rate
    // from the host, where a panic is a crashed editor and a wrong order
    // is a strip that reads backwards.

    #[test]
    fn no_scopes_and_no_enclosing_scopes_resolve_to_nothing() {
        let opts = ContextOptions {
            viewport_top: 50,
            ..ContextOptions::default()
        };
        assert_eq!(resolve_context(&[], 30, &opts), Vec::<u32>::new());
        // Scopes exist but none contain the anchor.
        let elsewhere = [scope(60, 80)];
        assert_eq!(
            resolve_context(&elsewhere, 30, &opts),
            Vec::<u32>::new(),
            "a scope the cursor is not inside contributes nothing"
        );
    }

    #[test]
    fn scope_order_comes_from_the_data_not_the_input_ordering() {
        // A query returns captures in tree-walk order, which is not
        // guaranteed to be outermost-first.
        let jumbled = [scope(20, 40), scope(5, 99), scope(10, 90)];
        let opts = ContextOptions {
            viewport_top: 28,
            ..ContextOptions::default()
        };
        assert_eq!(resolve_context(&jumbled, 30, &opts), vec![5, 10, 20]);
    }

    #[test]
    fn overlapping_and_malformed_scopes_do_not_panic() {
        let opts = ContextOptions {
            viewport_top: 50,
            ..ContextOptions::default()
        };
        // Partially overlapping without nesting — impossible from a real
        // tree, reachable from a hand-written query.
        let overlapping = [scope(10, 50), scope(30, 70)];
        assert_eq!(resolve_context(&overlapping, 40, &opts), vec![10, 30]);

        // A header span that ends before it starts.
        let inverted = [ContextScope {
            scope_start: 10,
            scope_end: 60,
            header_start: 20,
            header_end: 15,
        }];
        assert_eq!(
            resolve_context(&inverted, 40, &opts),
            Vec::<u32>::new(),
            "an inverted header span contributes no rows rather than \
             panicking on the range"
        );
    }

    #[test]
    fn nested_scopes_resolve_outermost_first() {
        // impl at 10..=99, fn at 20..=40, cursor at 30.
        let scopes = [scope(10, 99), scope(20, 40)];
        // View starts below both headers, so both are pinnable and the
        // test is about ORDER and nothing else.
        let opts = ContextOptions {
            viewport_top: 25,
            ..ContextOptions::default()
        };

        let rows = resolve_context(&scopes, 30, &opts);

        assert_eq!(
            rows,
            vec![10, 20],
            "outermost first — the row nearest the text must be the \
             nearest enclosing scope, so the strip reads as a continuation \
             of the code"
        );
    }
}
