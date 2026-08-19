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

    #[test]
    #[ignore = "FAILS: surround chords are inert through the key path. Two \
                defects — the catalog entry shadows the wildcard binding, and \
                the operator no-ops even when the chord resolves. See the \
                module docs; un-ignore when fixed."]
    fn yss_wraps_the_line() {
        let mut app = app_with("hello\n", 20);
        press_chars(&mut app, "yss\"");
        assert_eq!(body(&app), "\"hello\"\n");
    }

    #[test]
    #[ignore = "FAILS: see yss_wraps_the_line."]
    fn ds_deletes_the_pair() {
        let mut app = app_with("\"hello\"\n", 20);
        press_chars(&mut app, "ds\"");
        assert_eq!(body(&app), "hello\n");
    }

    #[test]
    #[ignore = "FAILS: see yss_wraps_the_line."]
    fn cs_changes_the_pair() {
        let mut app = app_with("\"hello\"\n", 20);
        press_chars(&mut app, "cs\"'");
        assert_eq!(body(&app), "'hello'\n");
    }

    /// The reported defect: bound, listed by `:describe-key`, inert.
    #[test]
    #[ignore = "FAILS: see yss_wraps_the_line."]
    fn visual_s_wraps_the_selection() {
        let mut app = app_with("hello world\n", 20);
        press_chars(&mut app, "ve");
        press_chars(&mut app, "S\"");
        assert_eq!(body(&app), "\"hello\" world\n");
    }
}
