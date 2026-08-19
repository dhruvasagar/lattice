//! Chord-level coverage for `surround-mode`.
//!
//! Same blind spot `magit_bindings` was written for, in a different
//! feature: SU.3b's eleven tests drive `grammar_execute()` directly, so
//! they prove the *operators* work and say nothing about whether a
//! keypress ever reaches one. The seam between them — press key →
//! `input::translate` → chain-form minor binding with
//! `ChordPattern::CharLiteral` wildcards → dispatch → operator — was
//! uncovered, which is how `S` in Visual mode could read as bound in
//! `:describe-key` and do nothing.
//!
//! Every test here presses real keys through `test_helpers::press`, the
//! same path the terminal drives. A test that called the operator would
//! pass against a completely dead keymap.

#[cfg(test)]
mod tests {
    use crate::app::test_helpers::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn press_chars(app: &mut crate::app::App, s: &str) {
        for c in s.chars() {
            press(app, key(c));
        }
    }

    fn body(app: &crate::app::App) -> String {
        app.editor.document.snapshot().buffer.as_string()
    }

    /// Guard against a vacuous suite: if surround-mode is not active on
    /// an ordinary buffer, every assertion below would be measuring an
    /// unbound key rather than a broken one.
    #[test]
    fn surround_mode_is_active_on_an_ordinary_buffer() {
        let app = app_with("hello\n", 20);
        let modes = app
            .editor
            .active_modes
            .get(&app.editor.document_buffer_id)
            .cloned()
            .unwrap_or_default();
        assert!(
            modes.minors().iter().any(|m| m.as_str() == "surround-mode"),
            "surround-mode is `ActivationPolicy::Global`; it must be active \
             here or nothing below tests what it claims to. minors: {:?}",
            modes
                .minors()
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
        );
    }

    /// SU.3e: `[y, s, s]` was bound at the Builtin layer by
    /// `register_operator_bindings`' doubled-operator block, so the fourth
    /// keystroke never had a path to walk to.
    #[test]
    fn yss_wraps_the_line() {
        let mut app = app_with("hello\n", 20);
        press_chars(&mut app, "yss\"");
        assert_eq!(body(&app), "\"hello\"\n");
    }

    /// Cursor deliberately inside the pair — see
    /// `ds_with_the_cursor_on_the_delimiter` for why that matters.
    #[test]
    fn ds_deletes_the_pair() {
        let mut app = app_with("\"hello\"\n", 20);
        press_chars(&mut app, "lds\"");
        assert_eq!(body(&app), "hello\n");
    }

    #[test]
    fn cs_changes_the_pair() {
        let mut app = app_with("\"hello\"\n", 20);
        press_chars(&mut app, "lcs\"'");
        assert_eq!(body(&app), "'hello'\n");
    }

    /// The reported defect: bound, listed by `:describe-key`, inert.
    /// surround-mode's own table-form catalog row bound `[S]` at one chord
    /// and shadowed its own `[S, CharLiteral]`.
    #[test]
    fn visual_s_wraps_the_selection() {
        let mut app = app_with("hello world\n", 20);
        press_chars(&mut app, "ve");
        press_chars(&mut app, "S\"");
        assert_eq!(body(&app), "\"hello\" world\n");
    }

    /// SU.4 (`ys{motion}{char}`) is wired by `register_operator_bindings`
    /// with `post_motion_char: true` and works — whatever the slice plan's
    /// ⛔ says. Pinned here because the doubled-operator fix edits that same
    /// function, and the doubled form and the motion form must not drift.
    #[test]
    fn ysiw_wraps_the_word() {
        let mut app = app_with("hello world\n", 20);
        press_chars(&mut app, "ysiw\"");
        assert_eq!(body(&app), "\"hello\" world\n");
    }

    /// The three deleted Normal catalog rows wrote their chords
    /// space-separated (`"d s"`), and `parse_chord_sequence` reads a space
    /// as a literal Space chord — so they bound `d<Space>s`, `c<Space>s`
    /// and `y<Space>s<Space>s`. Nothing ever resolved those paths; they were
    /// three stray bindings sitting in the trie. Pin that they are gone,
    /// because re-adding a catalog row is exactly how they would come back.
    #[test]
    fn a_space_separated_chord_is_not_bound() {
        let mut app = app_with("\"hello\"\n", 20);
        press_chars(&mut app, "d s\"");
        assert_eq!(
            body(&app),
            "\"hello\"\n",
            "`d<Space>s\"` must do nothing; it was a typo'd catalog chord"
        );
    }

    /// SU.3f: cursor sitting ON the opening delimiter. The pair finder
    /// skipped an opener under the cursor, so this did nothing where vim
    /// deletes the pair. Kept at the chord level as well as in the finder's
    /// own unit tests, because this is the shape a user actually types —
    /// landing on a quote and pressing `ds"`.
    #[test]
    fn ds_with_the_cursor_on_the_delimiter() {
        let mut app = app_with("\"hello\"\n", 20);
        press_chars(&mut app, "ds\"");
        assert_eq!(body(&app), "hello\n");
    }

    #[test]
    fn cs_with_the_cursor_on_the_delimiter() {
        let mut app = app_with("\"hello\"\n", 20);
        press_chars(&mut app, "cs\"'");
        assert_eq!(body(&app), "'hello'\n");
    }
}
