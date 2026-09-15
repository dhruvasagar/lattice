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

        // `dk` — charwise today (the linewise gap), and unchanged by VM.3b.
        let mut a = app_with("abcd\nefgh\n", 10);
        press_chars(&mut a, "jll");
        press_chars(&mut a, "dk");
        assert_eq!(body(&a), "abgh\n", "`dk` must delete exactly what it did");
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
}
