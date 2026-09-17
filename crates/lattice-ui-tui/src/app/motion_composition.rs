//! VM.3: chord-level coverage for motions that compose with an operator.
//!
//! Driven through `test_helpers::press_chars` — the REAL keystroke path —
//! for the reason `edit.rs`'s `reported_vim_grammar_2026_09_11` module
//! already states: `Editor::dispatch_chord` does not compose operator+motion.
//! The absorb effect pushes the operator's prefix into
//! `Editor::partial_chord`, while `dispatch_chord` resolves against the
//! `&mut Vec` its caller threads; they are different vectors, so `d%` driven
//! that way fires a bare `%` and the test measures nothing.
//!
//! That is not a hypothetical. It is what a host-side harness for these
//! exact assertions did while VM.3b was being written: `d%` "passed" the
//! motion and left the buffer untouched, and `d}` recorded a jump it should
//! not have — both because the operator never armed.

#[cfg(test)]
mod tests {
    use crate::app::test_helpers::*;

    fn body(app: &crate::app::App) -> String {
        app.editor.document.snapshot().buffer.as_string()
    }

    /// `%` composing with an operator is the whole point of VM.3b. As
    /// `action:match-bracket` it could not: an action takes no operator, so
    /// `d%` was unbound — in vim it is one of the first things anyone tries
    /// on a bracketed expression.
    ///
    /// Inclusive, so BOTH brackets go. That is `exclusive: false` on the
    /// `MotionSpec` doing its job, and it is the assertion that would fail if
    /// someone "tidied" it to match the exclusive motions around it.
    #[test]
    fn d_percent_deletes_the_bracketed_span_inclusively() {
        let mut a = app_with("x(abc)y\n", 10);
        press_chars(&mut a, "ld%");
        assert_eq!(body(&a), "xy\n");
    }

    /// From the CLOSING bracket, the same span — `%` scans backwards and the
    /// operator range still covers both ends. Worth its own test because the
    /// backward scan is a separate loop with its own `i == 0` termination.
    #[test]
    fn d_percent_works_from_the_closing_bracket() {
        let mut a = app_with("x(abc)y\n", 10);
        press_chars(&mut a, "lllll");
        press_chars(&mut a, "d%");
        assert_eq!(body(&a), "xy\n");
    }

    /// Yank composes too, and leaves the buffer alone.
    #[test]
    fn y_percent_leaves_the_buffer_alone() {
        let mut a = app_with("x(abc)y\n", 10);
        press_chars(&mut a, "ly%");
        assert_eq!(body(&a), "x(abc)y\n");
    }

    /// A `%` that finds no bracket must not make `d%` delete something
    /// surprising. The motion returns the cursor unmoved, so the operator's
    /// range is empty.
    #[test]
    fn d_percent_with_no_bracket_deletes_nothing() {
        let mut a = app_with("no brackets here\n", 10);
        press_chars(&mut a, "d%");
        assert_eq!(body(&a), "no brackets here\n");
    }

    /// The three motions whose `exclusive` flag VM.3b had to correct, pinned
    /// through real keys so the correction is provably behaviour-preserving.
    ///
    /// `F` and `T` were registered INCLUSIVE and rendered EXCLUSIVE by a
    /// backward branch that dropped the cursor character — two errors that
    /// cancelled, right up until `%` arrived as the first genuinely-inclusive
    /// bidirectional motion and `d%` from the closing bracket left the `)`
    /// behind. Fixing the branch without fixing these flags would have moved
    /// `dF` / `dT` / `dk` instead.
    #[test]
    fn the_reclassified_backward_motions_delete_exactly_what_they_did() {
        // `dFc` from the `f`: back to the `c`, cursor char excluded (vim).
        let mut a = app_with("abcdef\n", 10);
        press_chars(&mut a, "lllll");
        press_chars(&mut a, "dFc");
        assert_eq!(body(&a), "abf\n", "`dF` is exclusive of the cursor char");

        // `dTa` from the `e`: `T` lands one past the `a` (on the `b`), and
        // exclusive drops the cursor char — so `bcd` goes and `ae` remains.
        let mut a = app_with("abcde\n", 10);
        press_chars(&mut a, "llll");
        press_chars(&mut a, "dTa");
        assert_eq!(body(&a), "ae\n", "`dT` is exclusive of the cursor char");

        // `dk` — unchanged by VM.3b, and linewise since VM.3L closed the gap
        // this test used to pin: both lines go, as in vim.
        let mut a = app_with("abcd\nefgh\n", 10);
        press_chars(&mut a, "jll");
        press_chars(&mut a, "dk");
        assert_eq!(body(&a), "\n", "`dk` deletes both lines, linewise");
    }

    /// VM.3c: `;` composes. `d;` under an operator is the reason `;` had to be
    /// a motion — as `action:find-repeat-forward` it took no operator at all.
    #[test]
    fn d_semicolon_deletes_to_the_repeated_find() {
        let mut a = app_with("a.b.c.d\n", 10);
        press_chars(&mut a, "f.");
        press_chars(&mut a, "d;");
        // `f.` sits on the dot at 1; `;` targets the dot at 3. `;` repeating
        // `f` is INCLUSIVE, so both dots and the `b` between them go.
        assert_eq!(body(&a), "ac.d\n");
    }

    /// And `,` under an operator, the other direction — the case that forced
    /// `MotionResult::exclusive` to exist.
    ///
    /// `,` after an `f` acts as `F`, which vim calls EXCLUSIVE, so the
    /// character under the cursor survives. With exclusivity read from the
    /// `;` spec instead of from the repeat, this deleted one character too
    /// many and left `a.bd`.
    #[test]
    fn d_comma_takes_the_exclusivity_of_the_motion_it_repeats() {
        let mut a = app_with("a.b.c.d\n", 10);
        press_chars(&mut a, "f.;;");
        // Now on the dot at 5. `,` targets the dot at 3; exclusive, so the
        // dot at 5 stays.
        press_chars(&mut a, "d,");
        assert_eq!(body(&a), "a.b.d\n");
    }

    /// VM.3a's negative half, on the path where the operator actually arms:
    /// `d}` is an edit, not a jump. The invocation carries the OPERATOR's id,
    /// so `motion_is_jump` answers `false` with no special-casing — but only
    /// if the operator armed, which is precisely what a host-side harness
    /// cannot arrange.
    #[test]
    fn an_operator_targeting_a_jump_motion_records_no_jump() {
        let mut a = app_with("alpha\nbeta\n\ngamma\ndelta\n", 10);
        let before = a.editor.position_history.len();
        press_chars(&mut a, "d}");
        assert_ne!(
            body(&a),
            "alpha\nbeta\n\ngamma\ndelta\n",
            "test premise: `d}}` must have edited"
        );
        assert_eq!(
            a.editor.position_history.len(),
            before,
            "`d}}` is an edit, not a jump"
        );
    }

    /// And the positive half on the same path: a bare `}` still records.
    #[test]
    fn a_bare_jump_motion_still_records() {
        let mut a = app_with("alpha\nbeta\n\ngamma\ndelta\n", 10);
        let before = a.editor.position_history.len();
        press_chars(&mut a, "}");
        assert_eq!(a.editor.position_history.len(), before + 1);
    }

    // ── VM.3L: linewise operator targets (vim 9.2, `vimcheck_linewise*.vim`) ──

    const SIX: &str = "  one a\n  two b\n\n  four d\n  five e\n  six f";
    const THREE: &str = "  one a\n  two b\n  three c";

    fn at(text: &str, line: u32, byte: u32) -> crate::app::App {
        let mut a = app_with(text, 20);
        a.editor.cursor = lattice_protocol::position::Position::new(line, byte);
        a
    }

    fn cursor(a: &crate::app::App) -> (u32, u32) {
        (a.editor.cursor.line, a.editor.cursor.byte)
    }

    fn register(a: &crate::app::App) -> Option<(String, lattice_grammar::YankKind)> {
        a.editor
            .unnamed_register
            .as_ref()
            .map(|r| (r.content.clone(), r.kind))
    }

    const LINEWISE: lattice_grammar::YankKind = lattice_grammar::YankKind::Linewise;
    const CHARWISE: lattice_grammar::YankKind = lattice_grammar::YankKind::Charwise;

    /// vim: `dj` from 1,5 deletes lines 1–2 whole; register `V`.
    #[test]
    fn dj_deletes_two_whole_lines() {
        let mut a = at(SIX, 0, 4);
        press_chars(&mut a, "dj");
        assert_eq!(body(&a), "\n  four d\n  five e\n  six f");
        assert_eq!(register(&a), Some(("  one a\n  two b\n".into(), LINEWISE)));
        assert_eq!(cursor(&a), (0, 0));
    }

    /// vim: `yj` yanks lines 1–2 linewise and leaves the cursor where it was.
    #[test]
    fn yj_yanks_two_lines_linewise() {
        let mut a = at(SIX, 0, 4);
        press_chars(&mut a, "yj");
        assert_eq!(body(&a), SIX);
        assert_eq!(register(&a), Some(("  one a\n  two b\n".into(), LINEWISE)));
        assert_eq!(cursor(&a), (0, 4));
    }

    /// vim: `dk` from 2,5 deletes the same two lines as `dj` from 1,5.
    #[test]
    fn dk_deletes_the_line_above_and_this_one() {
        let mut a = at(SIX, 1, 4);
        press_chars(&mut a, "dk");
        assert_eq!(body(&a), "\n  four d\n  five e\n  six f");
        assert_eq!(register(&a), Some(("  one a\n  two b\n".into(), LINEWISE)));
        assert_eq!(cursor(&a), (0, 0));
    }

    /// vim: `dG` from 4,5 deletes lines 4–6 whole; cursor on the blank line 3.
    ///
    /// On a buffer that ends in a newline. Without one, vim's result
    /// `['  one a', '  two b', '']` is the string `"  one a\n  two b\n"`, which
    /// lattice (like any editor that treats a final newline as a terminator)
    /// reads as two lines — so there is no blank line 3 for the cursor to sit on.
    #[test]
    fn d_upper_g_deletes_to_the_end_linewise() {
        let mut a = at(&format!("{SIX}\n"), 3, 4);
        press_chars(&mut a, "dG");
        assert_eq!(body(&a), "  one a\n  two b\n\n");
        assert_eq!(register(&a).map(|r| r.1), Some(LINEWISE));
        assert_eq!(cursor(&a), (2, 0));
    }

    /// vim: `dgg` from 2,5 deletes lines 1–2 whole.
    #[test]
    fn dgg_deletes_to_the_top_linewise() {
        let mut a = at(SIX, 1, 4);
        press_chars(&mut a, "dgg");
        assert_eq!(body(&a), "\n  four d\n  five e\n  six f");
        assert_eq!(register(&a).map(|r| r.1), Some(LINEWISE));
    }

    /// vim: `2dj` from 1,5 deletes three lines; cursor on the first non-blank
    /// of the line that replaces them (VM.3m).
    #[test]
    fn a_count_on_dj_deletes_more_lines() {
        let mut a = at(SIX, 0, 4);
        press_chars(&mut a, "2dj");
        assert_eq!(body(&a), "  four d\n  five e\n  six f");
        assert_eq!(register(&a).map(|r| r.1), Some(LINEWISE));
        assert_eq!(cursor(&a), (0, 2));
    }

    /// vim: `d}` from 1,1 becomes linewise (`:h exclusive-linewise`): lines 1–2
    /// go, the blank line stays.
    #[test]
    fn d_brace_from_the_line_start_is_linewise() {
        let mut a = at(SIX, 0, 0);
        press_chars(&mut a, "d}");
        assert_eq!(body(&a), "\n  four d\n  five e\n  six f");
        assert_eq!(register(&a).map(|r| r.1), Some(LINEWISE));
    }

    /// vim: `d}` from 1,5 ends at the end of line 2, keeping its newline.
    #[test]
    fn d_brace_from_mid_line_keeps_the_last_newline() {
        let mut a = at(SIX, 0, 4);
        press_chars(&mut a, "d}");
        assert_eq!(body(&a), "  on\n\n  four d\n  five e\n  six f");
        assert_eq!(register(&a), Some(("e a\n  two b".into(), CHARWISE)));
    }

    /// vim: `dj` on the last line does nothing, not even to the register.
    #[test]
    fn dj_on_the_last_line_does_nothing() {
        let mut a = at(THREE, 2, 3);
        press_chars(&mut a, "dj");
        assert_eq!(body(&a), THREE);
        assert_eq!(register(&a), None);
    }

    /// vim: `5dj` two lines from the end clamps to the end.
    #[test]
    fn a_count_past_the_end_clamps() {
        let mut a = at(THREE, 1, 3);
        press_chars(&mut a, "5dj");
        assert_eq!(body(&a), "  one a");
        assert_eq!(register(&a).map(|r| r.1), Some(LINEWISE));
    }

    /// vim: `yk` from 2,6 yanks lines 1–2 linewise and moves the cursor to the
    /// start line, keeping the column (VM.3m).
    #[test]
    fn yk_yanks_the_line_above_and_this_one() {
        let mut a = at(THREE, 1, 5);
        press_chars(&mut a, "yk");
        assert_eq!(register(&a), Some(("  one a\n  two b\n".into(), LINEWISE)));
        assert_eq!(cursor(&a), (0, 5));
    }

    /// vim: `cjX` replaces two lines with one holding `X`.
    #[test]
    fn cj_replaces_two_lines_with_one() {
        let mut a = at(THREE, 0, 3);
        press_chars(&mut a, "cjX");
        assert_eq!(body(&a), "X\n  three c");
    }

    // ── VM.3d-2: `n` / `N` / `*` / `#` are motions (vim 9.2, `vimcheck_n.vim`) ──

    const FOO: &str = "alpha foo one\nbeta two\ngamma foo three\ndelta four";

    fn searching(pattern: &str, line: u32, byte: u32) -> crate::app::App {
        let mut a = app_with(FOO, 20);
        a.editor.last_search = Some(lattice_host::state::LastSearch {
            pattern: pattern.into(),
            direction: lattice_grammar::SearchDirection::Forward,
        });
        a.editor.refresh_hlsearch_from_last();
        a.editor.cursor = lattice_protocol::position::Position::new(line, byte);
        a
    }

    /// vim: `dn` from 1,1 deletes `alpha ` (charwise, exclusive).
    #[test]
    fn dn_deletes_up_to_the_next_match() {
        let mut a = searching("foo", 0, 0);
        press_chars(&mut a, "dn");
        assert_eq!(body(&a), "foo one\nbeta two\ngamma foo three\ndelta four");
    }

    /// vim: `dn` from 3,10 wraps to the match at 1,7 and deletes back to it.
    #[test]
    fn dn_across_a_wrap_deletes_back_to_the_wrapped_match() {
        let mut a = searching("foo", 2, 9);
        press_chars(&mut a, "dn");
        assert_eq!(body(&a), "alpha  three\ndelta four");
        assert_eq!(
            a.editor.cursor,
            lattice_protocol::position::Position::new(0, 6)
        );
    }

    /// vim: `yN` from 3,10 yanks the `foo` behind the cursor.
    #[test]
    fn y_upper_n_yanks_back_to_the_previous_match() {
        let mut a = searching("foo", 2, 9);
        press_chars(&mut a, "yN");
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "foo");
    }

    /// vim: `dn` with no match deletes nothing and says E486.
    #[test]
    fn dn_with_no_match_deletes_nothing_and_echoes_e486() {
        let mut a = searching("zzz", 0, 0);
        press_chars(&mut a, "dn");
        assert_eq!(body(&a), FOO);
        let msg = a.editor.last_message.as_ref().expect("an echo");
        assert_eq!(msg.text, "E486: Pattern not found: zzz");
    }

    /// vim: `vny` from 1,1 yanks `alpha f` (Visual includes the match start).
    #[test]
    fn vn_extends_the_selection_to_the_match() {
        let mut a = searching("foo", 0, 0);
        press_chars(&mut a, "vny");
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "alpha f");
    }

    /// vim: `d*` on `foo` at 1,7 deletes to the next `foo`, and `*` recorded
    /// the word as the search.
    #[test]
    fn d_star_deletes_to_the_next_occurrence_of_the_word() {
        let mut a = searching("unrelated", 0, 6);
        press_chars(&mut a, "d*");
        assert_eq!(body(&a), "alpha foo three\ndelta four");
        // Whole-word, as vim records `\<foo\>` (c3b4bfca).
        assert_eq!(a.editor.last_search.as_ref().unwrap().pattern, r"\bfoo\b");
    }

    /// vim: `n` past the last match wraps and says so.
    #[test]
    fn a_wrapping_n_echoes_search_hit_bottom() {
        let mut a = searching("foo", 2, 9);
        press_chars(&mut a, "n");
        assert_eq!(
            a.editor.cursor,
            lattice_protocol::position::Position::new(0, 6)
        );
        let msg = a.editor.last_message.as_ref().expect("an echo");
        assert_eq!(msg.text, "search hit BOTTOM, continuing at TOP");
    }

    /// A successful `n` records a jump; a failed one (E486) records none.
    #[test]
    fn only_a_successful_n_records_a_jump() {
        let mut a = searching("foo", 0, 0);
        let before = a.editor.position_history.len();
        press_chars(&mut a, "n");
        assert_eq!(a.editor.position_history.len(), before + 1);

        let mut b = searching("zzz", 0, 0);
        let before = b.editor.position_history.len();
        press_chars(&mut b, "n");
        assert_eq!(
            b.editor.position_history.len(),
            before,
            "E486 is not a jump"
        );
    }

    // ── VM.3e: `'x` / `` `x `` are motions (vim 9.2, `vimcheck_marks*.vim`) ──

    const MARKED: &str = "  one a\n  two b\n\n  four d\n  five e\n  six f";

    fn marked(mark: (u32, u32), cursor: (u32, u32)) -> crate::app::App {
        use lattice_protocol::position::Position;
        let mut a = app_with(MARKED, 20);
        a.editor.marks.insert('a', Position::new(mark.0, mark.1));
        a.editor.cursor = Position::new(cursor.0, cursor.1);
        a
    }

    /// vim: `d'a` from 1,5 (mark 5,3) deletes lines 1–5, linewise.
    #[test]
    fn d_quote_mark_deletes_whole_lines_to_the_mark() {
        let mut a = marked((4, 2), (0, 4));
        press_chars(&mut a, "d'a");
        assert_eq!(body(&a), "  six f");
        assert_eq!(
            register(&a).map(|r| r.0).as_deref(),
            Some("  one a\n  two b\n\n  four d\n  five e\n")
        );
    }

    /// vim: `` d`a `` from 1,5 (mark 5,3) is charwise and exclusive.
    #[test]
    fn d_backtick_mark_deletes_up_to_the_mark() {
        let mut a = marked((4, 2), (0, 4));
        press_chars(&mut a, "d`a");
        assert_eq!(body(&a), "  onfive e\n  six f");
        assert_eq!(
            register(&a).map(|r| r.0).as_deref(),
            Some("e a\n  two b\n\n  four d\n  ")
        );
    }

    /// vim: `y'a` from 5,3 back to a mark at 2,5 yanks lines 2–5.
    #[test]
    fn y_quote_mark_backward_yanks_whole_lines() {
        let mut a = marked((1, 4), (4, 2));
        press_chars(&mut a, "y'a");
        assert_eq!(body(&a), MARKED);
        assert_eq!(
            register(&a).map(|r| r.0).as_deref(),
            Some("  two b\n\n  four d\n  five e\n")
        );
    }

    /// vim: `v'ay` from 1,5 (mark 5,3) extends a charwise selection to the
    /// first non-blank of the mark's line, inclusive.
    #[test]
    fn v_quote_mark_extends_the_selection_to_the_mark_line() {
        let mut a = marked((4, 2), (0, 4));
        press_chars(&mut a, "v'ay");
        assert_eq!(
            register(&a).map(|r| r.0).as_deref(),
            Some("e a\n  two b\n\n  four d\n  f")
        );
    }

    /// vim: `c'aX` from 1,5 (mark 2,5) changes lines 1–2 linewise.
    #[test]
    fn c_quote_mark_changes_whole_lines() {
        let mut a = marked((1, 4), (0, 4));
        press_chars(&mut a, "c'aX");
        assert_eq!(body(&a), "X\n\n  four d\n  five e\n  six f");
    }

    /// vim: `d'a` with no mark says E20 and deletes nothing; `'a` records no
    /// jump.
    #[test]
    fn an_unset_mark_echoes_e20_and_changes_nothing() {
        let mut a = app_with(MARKED, 20);
        press_chars(&mut a, "d'a");
        assert_eq!(body(&a), MARKED);
        assert_eq!(
            a.editor.last_message.as_ref().expect("an echo").text,
            "E20: Mark not set"
        );

        let mut b = app_with(MARKED, 20);
        let before = b.editor.position_history.len();
        press_chars(&mut b, "'a");
        assert_eq!(b.editor.position_history.len(), before, "E20 is not a jump");
    }

    /// vim: `3'a` ignores the count; a successful jump records one entry.
    #[test]
    fn a_count_before_a_mark_is_ignored_and_the_jump_is_recorded() {
        let mut a = marked((4, 2), (0, 4));
        let before = a.editor.position_history.len();
        press_chars(&mut a, "3'a");
        assert_eq!(
            a.editor.cursor,
            lattice_protocol::position::Position::new(4, 2)
        );
        assert_eq!(a.editor.position_history.len(), before + 1);
    }

    // ── VM.3f: `H` / `M` / `L` are motions (vim 9.2, `vimcheck_hml4.vim`) ──

    /// Forty `"  line N"` lines, a 21-row window showing lines 6–26 (1-based),
    /// cursor on 8,6 — the layout of the vim check.
    fn windowed() -> crate::app::App {
        let text = (1..=40)
            .map(|n| format!("  line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = app_with(&text, 21);
        a.editor.scroll = 5;
        a.editor.cursor = lattice_protocol::position::Position::new(7, 5);
        a
    }

    fn lines(from: u32, to: u32) -> String {
        (from..=to).map(|n| format!("  line {n}\n")).collect()
    }

    /// vim: `H` 6,3 · `M` 16,3 · `L` 26,3 · `3H` 8,3 · `3L` 24,3.
    #[test]
    fn h_m_and_l_land_on_the_first_non_blank_of_window_lines() {
        for (keys, line) in [("H", 5), ("M", 15), ("L", 25), ("3H", 7), ("3L", 23)] {
            let mut a = windowed();
            press_chars(&mut a, keys);
            assert_eq!(
                a.editor.cursor,
                lattice_protocol::position::Position::new(line, 2),
                "{keys}"
            );
        }
    }

    /// vim: `dL` from 8,6 deletes lines 8–26, linewise.
    #[test]
    fn d_l_deletes_whole_lines_to_the_bottom_of_the_window() {
        let mut a = windowed();
        press_chars(&mut a, "dL");
        assert_eq!(register(&a).map(|r| r.0), Some(lines(8, 26)));
        assert_eq!(body(&a).lines().count(), 21);
    }

    /// vim: `yH` from 8,6 yanks lines 6–8, linewise.
    #[test]
    fn y_h_yanks_whole_lines_up_to_the_top_of_the_window() {
        let mut a = windowed();
        press_chars(&mut a, "yH");
        assert_eq!(register(&a).map(|r| r.0), Some(lines(6, 8)));
    }

    /// vim: with `nostartofline`, `L` from 8,6 keeps column 6.
    #[test]
    fn nostartofline_keeps_the_column() {
        let mut a = windowed();
        a.editor.option_cache.startofline = false;
        press_chars(&mut a, "L");
        assert_eq!(
            a.editor.cursor,
            lattice_protocol::position::Position::new(25, 5)
        );
    }

    // ── VM.3m: where an operator leaves the cursor (vim 9.2,
    // `vimcheck_opcursor.vim`) ──

    /// The buffer of the vim check: line 2 is indented four, so "first
    /// non-blank" and "column 0" can't be confused.
    const SIXI: &str = "  one a\n    two b\n\n  four d\n  five e\n  six f";

    /// vim: `dd` from 1,6 → 1,5, the first non-blank of the line that moved up.
    #[test]
    fn dd_lands_on_the_first_non_blank_of_the_line_that_moved_up() {
        let mut a = at(SIXI, 0, 5);
        press_chars(&mut a, "dd");
        assert_eq!(body(&a), "    two b\n\n  four d\n  five e\n  six f");
        assert_eq!(cursor(&a), (0, 4));
    }

    /// vim: `dk` from 5,5 → 4,3, likewise the first non-blank.
    #[test]
    fn dk_lands_on_the_first_non_blank() {
        let mut a = at(SIXI, 4, 4);
        press_chars(&mut a, "dk");
        assert_eq!(body(&a), "  one a\n    two b\n\n  six f");
        assert_eq!(cursor(&a), (3, 2));
    }

    /// vim: `yy` and `2yy` leave the cursor exactly where it was.
    #[test]
    fn a_linewise_yank_of_this_line_does_not_move_the_cursor() {
        let mut a = at(SIXI, 1, 7);
        press_chars(&mut a, "yy");
        assert_eq!(cursor(&a), (1, 7));

        let mut a = at(SIXI, 1, 7);
        press_chars(&mut a, "2yy");
        assert_eq!(cursor(&a), (1, 7));
    }

    /// vim: `yb` from 2,8 → 2,5, the start of the yanked text.
    #[test]
    fn yb_lands_on_the_start_of_the_yanked_text() {
        let mut a = at(SIXI, 1, 7);
        press_chars(&mut a, "yb");
        assert_eq!(register(&a).map(|r| r.0), Some("two".into()));
        assert_eq!(cursor(&a), (1, 4));
    }

    /// vim: `y{` from 5,5 → 3,1 — the start of the range, not the cursor.
    #[test]
    fn y_brace_lands_on_the_start_of_the_range() {
        let mut a = at(SIXI, 4, 4);
        press_chars(&mut a, "y{");
        assert_eq!(cursor(&a), (2, 0));
    }

    /// vim: `yip` from 5,5 → 4,1 — the start of the object.
    #[test]
    fn yip_lands_on_the_start_of_the_object() {
        let mut a = at(SIXI, 4, 4);
        press_chars(&mut a, "yip");
        assert_eq!(cursor(&a), (3, 0));
    }

    // ── VM.3j-1: `gg` / `G` / `<C-f>` follow `startofline` (vim 9.2,
    // `vimcheck_sol_pages.vim`) ──

    /// vim: `gg` and `G` land on the first non-blank; `nostartofline` keeps the
    /// column. Lattice put them at column 0, which is neither.
    #[test]
    fn gg_and_g_follow_startofline() {
        let mut a = at(SIXI, 1, 7);
        press_chars(&mut a, "gg");
        assert_eq!(cursor(&a), (0, 2));

        let mut a = at(SIXI, 0, 5);
        press_chars(&mut a, "G");
        assert_eq!(cursor(&a), (5, 2));

        let mut a = at(SIXI, 1, 5);
        a.editor.option_cache.startofline = false;
        press_chars(&mut a, "gg");
        assert_eq!(cursor(&a), (0, 5), "column kept, not the first non-blank");
    }

    /// vim: a page scroll lands on the first non-blank too. Only the column is
    /// this slice's business — the line is whatever the page walk already gave.
    #[test]
    fn a_page_scroll_follows_startofline() {
        let mut a = at(SIXI, 0, 5);
        a.editor.do_page(true);
        let landed = a.editor.cursor.line;
        assert_eq!(a.editor.cursor.byte, 2, "first non-blank of line {landed}");

        let mut b = at(SIXI, 0, 5);
        b.editor.option_cache.startofline = false;
        b.editor.do_page(true);
        assert_eq!(b.editor.cursor.line, landed, "same line either way");
        assert_eq!(b.editor.cursor.byte, 5, "column kept");
    }

    // ── VM.3g-1: the goal column survives a short line (vim 9.2,
    // `vimcheck_curswant2.vim`) ──

    /// `['abcdefghijkl', 'ab', 'abcdefghijkl']` — a short line between two long
    /// ones, which is the only shape that can tell a remembered goal column
    /// from a re-read cursor column.
    const RAGGED: &str = "abcdefghijkl\nab\nabcdefghijkl";

    /// `at()` plants the cursor by ASSIGNMENT, outside any dispatch — and the
    /// goal column is maintained BY dispatch, so it would still hold the column
    /// boot left it on (0) and every `j` would aim there. Real input cannot do
    /// that: every cursor move is a dispatch, which sets the goal on its way
    /// out. Clearing it here is the harness admitting it skipped that step;
    /// `None` then means "the cursor's own column", which is where the user
    /// would be.
    fn at_ragged(line: u32, byte: u32) -> crate::app::App {
        let mut a = at(RAGGED, line, byte);
        a.editor.curswant = None;
        a
    }

    /// vim: `jj` from column 9 comes back to column 9, though the line between
    /// is two columns wide. Lattice used to land on column 2 and stay there.
    #[test]
    fn jj_across_a_short_line_returns_to_the_goal_column() {
        let mut a = at_ragged(0, 8);
        press_chars(&mut a, "j");
        assert_eq!(cursor(&a), (1, 2), "clamped to the short line");
        press_chars(&mut a, "j");
        assert_eq!(cursor(&a), (2, 8), "and back to the column we started in");
    }

    /// vim: and upward too — `k` shares the goal with `j`.
    #[test]
    fn kk_across_a_short_line_returns_to_the_goal_column() {
        let mut a = at_ragged(2, 8);
        press_chars(&mut a, "kk");
        assert_eq!(cursor(&a), (0, 8));
    }

    /// vim: any horizontal move re-sets the goal — `j h j` lands on `h`'s
    /// column, not on the one `j` was aiming at.
    #[test]
    fn a_horizontal_motion_resets_the_goal_column() {
        let mut a = at_ragged(0, 8);
        press_chars(&mut a, "jhj");
        assert_eq!(cursor(&a), (2, 1));
    }
}
