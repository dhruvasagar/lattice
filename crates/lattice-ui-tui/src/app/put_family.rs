//! Vim's put family beyond plain `p` / `P`: a count, `gp` / `gP`, `]p` /
//! `[p`, `p` / `P` over a Visual selection, and `:put`.
//!
//! Only `p` and `P` in Normal mode existed. The rest were unbound: `3p`
//! put once, `gp` and `]p` did nothing, `p` in Visual did nothing, and
//! `:put` was not a command.
//!
//! Put is host code, so the window gets all of this from the same place.
//! The tests are here because they need a real key path: counts and
//! operator-pending state are carried by a peer's input loop, and the
//! host's own `Editor::dispatch_chord` resolves each chord without them.

#![allow(clippy::unwrap_used, clippy::panic)]

use super::App;
use super::test_helpers::app_with;

fn boot(text: &str) -> App {
    app_with(text, 20)
}

/// Press a vim-notation key sequence (`"yiwjgp"`, `":put!<CR>"`) through
/// `App::press_chord`, the entry the input loop calls.
fn press(app: &mut App, keys: &str) {
    lattice_host::peer_conformance::press(app, keys);
}

fn text(app: &App) -> String {
    app.editor.document.snapshot().buffer.as_string()
}

// ---- count ----

#[test]
fn a_count_puts_charwise_text_that_many_times() {
    let mut e = boot("ab cd\n");
    press(&mut e, "yiw$3p");
    assert_eq!(text(&e), "ab cdababab\n");
}

#[test]
fn a_count_puts_lines_that_many_times() {
    let mut e = boot("one\ntwo\n");
    press(&mut e, "yy2p");
    assert_eq!(text(&e), "one\none\none\ntwo\n");
    press(&mut e, "u");
    assert_eq!(text(&e), "one\ntwo\n", "a counted put is one undo step");
}

// ---- gp / gP ----

#[test]
fn gp_leaves_the_cursor_after_the_text_it_put() {
    let mut e = boot("one\ntwo\nthree\n");
    press(&mut e, "yygp");
    assert_eq!(text(&e), "one\none\ntwo\nthree\n");
    assert_eq!(e.editor.cursor.line, 2, "`gp`: the line after the one put");

    let mut e = boot("one\ntwo\nthree\n");
    press(&mut e, "yyp");
    assert_eq!(e.editor.cursor.line, 1, "`p`: on the line put");
}

#[test]
fn g_capital_p_puts_above_and_lands_below_it() {
    let mut e = boot("one\ntwo\n");
    press(&mut e, "jyygP");
    assert_eq!(text(&e), "one\ntwo\ntwo\n");
    assert_eq!(e.editor.cursor.line, 2);
}

#[test]
fn gp_with_charwise_text_lands_just_past_it() {
    let mut e = boot("ab cd\n");
    press(&mut e, "yiwgP");
    assert_eq!(text(&e), "abab cd\n");
    assert_eq!(
        e.editor.cursor.byte, 2,
        "after the put `ab`, not on its `b`"
    );
}

// ---- ]p / [p ----

#[test]
fn bracket_p_reindents_the_lines_to_the_current_line() {
    // Yank a two-line block indented by 8 and 12; put it under a line
    // indented by 2. The block keeps its shape at the new indent.
    let mut e = boot("        deep\n            deeper\n  here\n");
    press(&mut e, "yjjj]p");
    assert_eq!(
        text(&e),
        "        deep\n            deeper\n  here\n  deep\n      deeper\n"
    );
}

#[test]
fn open_bracket_p_puts_above_at_the_current_indent() {
    let mut e = boot("flush\n    inner\n");
    press(&mut e, "yyj[p");
    assert_eq!(text(&e), "flush\n    flush\n    inner\n");
}

#[test]
fn bracket_p_with_charwise_text_is_plain_p() {
    let mut e = boot("    ab cd\n");
    press(&mut e, "^yiw]p");
    assert_eq!(text(&e), "    aabb cd\n");
}

// ---- Visual p / P ----

#[test]
fn visual_p_replaces_the_selection_and_takes_what_it_replaced() {
    let mut e = boot("one two three\n");
    press(&mut e, "yiwwviwp");
    assert_eq!(text(&e), "one one three\n");
    // The replaced word is now the unnamed register.
    press(&mut e, "$p");
    assert_eq!(text(&e), "one one threetwo\n");
}

#[test]
fn visual_capital_p_replaces_the_selection_and_keeps_the_register() {
    let mut e = boot("one two three\n");
    press(&mut e, "yiwwviwP");
    assert_eq!(text(&e), "one one three\n");
    press(&mut e, "$p");
    assert_eq!(
        text(&e),
        "one one threeone\n",
        "`P` left the register alone"
    );
}

#[test]
fn visual_p_is_one_undo_step() {
    let mut e = boot("one two three\n");
    press(&mut e, "yiwwviwp");
    press(&mut e, "u");
    assert_eq!(text(&e), "one two three\n");
}

#[test]
fn visual_p_over_the_end_of_a_line_puts_at_the_end() {
    let mut e = boot("one two\n");
    press(&mut e, "yiwwviwp");
    assert_eq!(text(&e), "one one\n");
}

#[test]
fn linewise_visual_p_replaces_whole_lines() {
    let mut e = boot("one\ntwo\nthree\n");
    press(&mut e, "yyjVp");
    assert_eq!(text(&e), "one\none\nthree\n");

    // The last line: the text is appended, not put above a line that is
    // no longer there.
    let mut e = boot("one\ntwo\nthree\n");
    press(&mut e, "yyGVp");
    assert_eq!(text(&e), "one\ntwo\none\n");
}

#[test]
fn linewise_text_over_a_charwise_selection_gets_lines_of_its_own() {
    let mut e = boot("LINE\nab cd ef\n");
    press(&mut e, "yyjwviwp");
    assert_eq!(text(&e), "LINE\nab \nLINE\n ef\n");
}

// ---- :put ----

#[test]
fn ex_put_puts_the_register_below_the_cursor_line_as_a_line() {
    // A charwise yank still lands on a line of its own.
    let mut e = boot("ab cd\nlast\n");
    press(&mut e, "yiw:put<CR>");
    assert_eq!(text(&e), "ab cd\nab\nlast\n");
}

#[test]
fn ex_put_bang_puts_above() {
    let mut e = boot("one\ntwo\n");
    press(&mut e, "yyj:put!<CR>");
    assert_eq!(text(&e), "one\none\ntwo\n");
}

#[test]
fn ex_put_takes_a_line_and_a_register() {
    let mut e = boot("one\ntwo\nthree\n");
    press(&mut e, "\"ayyG:1put a<CR>");
    assert_eq!(text(&e), "one\none\ntwo\nthree\n");
}

#[test]
fn ex_put_of_an_unknown_register_changes_nothing() {
    let mut e = boot("one\n");
    press(&mut e, "yy:put =<CR>");
    assert_eq!(text(&e), "one\n");
}

// ---- the last line ----

/// A linewise put below the last line of a file that ends in a newline
/// used to leave a blank line after the text it put.
#[test]
fn a_linewise_put_on_the_last_line_adds_no_blank_line() {
    let mut e = boot("one\ntwo\n");
    press(&mut e, "yyGp");
    assert_eq!(text(&e), "one\ntwo\none\n");
}

/// A file with no final newline still gets its line break first.
#[test]
fn a_linewise_put_after_an_unterminated_last_line_starts_a_new_line() {
    let mut e = boot("one");
    press(&mut e, "yyp");
    assert_eq!(text(&e), "one\none\n");
}
