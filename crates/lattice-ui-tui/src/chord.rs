//! Crossterm → `KeyChord` adapter for the TUI renderer.
//!
//! Phase 5.4 split: the renderer-neutral chord types (`KeyChord`,
//! `KeyKind`, `SpecialKey`, `KeyMods`, `ChordParseError`,
//! `parse_chord_sequence`, `last_chord_token_byte_len`, and the
//! `FromStr` / `Display` impls) live in
//! [`lattice_host::chord`]. This module is the TUI-side adapter
//! that turns a `crossterm::KeyEvent` into the canonical
//! [`KeyChord`] the keymap trie indexes by.
//!
//! `from_event` (the canonical crossterm-side entry) lives here
//! as a **free function** rather than an `impl KeyChord` method
//! because orphan rules forbid extending a foreign type from
//! this crate. Existing call sites change from
//! `KeyChord::from_event(&ev)` to `chord::from_event(&ev)`; the
//! re-export below means `crate::chord::KeyChord` (and every
//! other neutral type) still resolves unchanged.
//!
//! The future `lattice-ui-gpui` ships its own analogous
//! `gpui_chord::from_event(&GpuiKeyEvent) -> Option<KeyChord>`
//! adapter; both renderers feed the same `KeyChord` into
//! `lattice_host`'s dispatch.

// Re-export every neutral type from the host. Callers that
// import `crate::chord::KeyChord` etc. continue to resolve
// without source changes.
pub use lattice_host::chord::*;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Normalise a `crossterm::KeyEvent` into a canonical
/// [`KeyChord`]. Returns `None` for events that have no chord
/// representation (release events on terminals that emit them,
/// modifier-only presses, key codes we don't recognise).
///
/// Normalisation rules (canonical form that the keymap trie
/// indexes by):
///
/// - **Letters with Ctrl / Alt**: case is folded to lowercase
///   so `Ctrl-c` and `Ctrl-C` map to the same chord.
/// - **Letters without modifiers**: case is preserved (vim's
///   `A` is uppercase a, distinct from `a`).
/// - **Letters with shift only**: shift is folded into the
///   case (the terminal already uppercased the letter; we
///   strip the redundant `KeyMods::SHIFT`). `Shift-a` and `A`
///   collapse.
/// - **Non-letter chars**: shift is stripped (the terminal
///   reports the shifted symbol, e.g. `$` for shift-4; the
///   modifier would be redundant).
/// - **Specials with shift**: shift is preserved (`<S-Tab>` is
///   distinct from `<Tab>`).
/// - **`KeyCode::BackTab`**: canonicalised to `Special(Tab) +
///   KeyMods::SHIFT` so the keymap trie has one entry rather
///   than two for "shift-tab".
pub fn from_event(event: &KeyEvent) -> Option<KeyChord> {
    let mut mods = KeyMods::NONE;
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        mods = mods | KeyMods::CTRL;
    }
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        mods = mods | KeyMods::SHIFT;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        mods = mods | KeyMods::ALT;
    }
    if event.modifiers.contains(KeyModifiers::SUPER) {
        mods = mods | KeyMods::SUPER;
    }

    let key = match event.code {
        KeyCode::Esc => KeyKind::Special(SpecialKey::Esc),
        KeyCode::Enter => KeyKind::Special(SpecialKey::Enter),
        KeyCode::Tab => KeyKind::Special(SpecialKey::Tab),
        KeyCode::BackTab => {
            // BackTab IS shift-tab; canonicalise.
            mods = mods | KeyMods::SHIFT;
            KeyKind::Special(SpecialKey::Tab)
        }
        KeyCode::Backspace => KeyKind::Special(SpecialKey::Backspace),
        KeyCode::Up => KeyKind::Special(SpecialKey::Up),
        KeyCode::Down => KeyKind::Special(SpecialKey::Down),
        KeyCode::Left => KeyKind::Special(SpecialKey::Left),
        KeyCode::Right => KeyKind::Special(SpecialKey::Right),
        KeyCode::Home => KeyKind::Special(SpecialKey::Home),
        KeyCode::End => KeyKind::Special(SpecialKey::End),
        KeyCode::PageUp => KeyKind::Special(SpecialKey::PageUp),
        KeyCode::PageDown => KeyKind::Special(SpecialKey::PageDown),
        KeyCode::Insert => KeyKind::Special(SpecialKey::Insert),
        KeyCode::Delete => KeyKind::Special(SpecialKey::Delete),
        KeyCode::F(n) if (1..=24).contains(&n) => KeyKind::Special(SpecialKey::F(n)),
        KeyCode::Char(c) => {
            let ctrl_or_alt = mods.ctrl() || mods.alt();
            if c == ' ' && !ctrl_or_alt {
                // Plain space renders as a literal `' '` when
                // un-modified; promote to `Special::Space` only
                // if the chord carries a modifier (so `<C-Space>`
                // is unambiguous).
                KeyKind::Char(' ')
            } else if ctrl_or_alt && c.is_ascii_alphabetic() {
                // Ctrl / Alt + letter normalises to lowercase.
                // Shift on a ctrl-letter is preserved (`<C-S-c>`
                // stays distinct from `<C-c>`).
                KeyKind::Char(c.to_ascii_lowercase())
            } else if !ctrl_or_alt {
                // Bare or shift-only printable. Strip shift --
                // the terminal already encoded it in the case
                // (for letters) or in the shifted symbol (for
                // non-letters).
                if mods.shift() {
                    mods = mods.without(KeyMods::SHIFT);
                }
                KeyKind::Char(c)
            } else {
                KeyKind::Char(c)
            }
        }
        _ => return None,
    };

    // Specials don't strip shift (it's meaningful for `<S-Tab>`,
    // `<S-F1>`, etc.); the strip-on-bare-printable logic above
    // handles only `KeyKind::Char`.
    Some(KeyChord::new(key, mods))
}

/// Render a single crossterm key event as canonical chord
/// notation. Returns `None` for events that have no chord
/// representation. Thin shim over `from_event` + `to_string`.
pub fn format_chord(event: &KeyEvent) -> Option<String> {
    from_event(event).map(|c| c.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn plain_char_renders_unwrapped() {
        assert_eq!(
            format_chord(&ev(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some("a".into())
        );
        assert_eq!(
            format_chord(&ev(KeyCode::Char('$'), KeyModifiers::NONE)),
            Some("$".into())
        );
    }

    #[test]
    fn ctrl_letter_renders_with_c_prefix_lowercase() {
        // Ctrl-c -- key terminals may report 'c' or 'C'; either
        // way the output normalises to lowercase.
        assert_eq!(
            format_chord(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some("<C-c>".into())
        );
        assert_eq!(
            format_chord(&ev(KeyCode::Char('C'), KeyModifiers::CONTROL)),
            Some("<C-c>".into())
        );
    }

    #[test]
    fn alt_letter_renders_with_m_prefix() {
        assert_eq!(
            format_chord(&ev(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some("<M-x>".into())
        );
    }

    #[test]
    fn ctrl_shift_letter_canonical_order() {
        assert_eq!(
            format_chord(&ev(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some("<C-S-c>".into())
        );
    }

    #[test]
    fn special_keys_render_with_canonical_names() {
        assert_eq!(
            format_chord(&ev(KeyCode::Esc, KeyModifiers::NONE)),
            Some("<Esc>".into())
        );
        assert_eq!(
            format_chord(&ev(KeyCode::Tab, KeyModifiers::NONE)),
            Some("<Tab>".into())
        );
        assert_eq!(
            format_chord(&ev(KeyCode::Enter, KeyModifiers::NONE)),
            Some("<CR>".into())
        );
        assert_eq!(
            format_chord(&ev(KeyCode::Backspace, KeyModifiers::NONE)),
            Some("<BS>".into())
        );
        assert_eq!(
            format_chord(&ev(KeyCode::Left, KeyModifiers::NONE)),
            Some("<Left>".into())
        );
    }

    #[test]
    fn ctrl_special_key_carries_modifier() {
        assert_eq!(
            format_chord(&ev(KeyCode::Up, KeyModifiers::CONTROL)),
            Some("<C-Up>".into())
        );
    }

    #[test]
    fn back_tab_encodes_shift_without_double_prefix() {
        assert_eq!(
            format_chord(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some("<S-Tab>".into())
        );
    }

    #[test]
    fn function_keys_render_as_fn() {
        assert_eq!(
            format_chord(&ev(KeyCode::F(1), KeyModifiers::NONE)),
            Some("<F1>".into())
        );
        assert_eq!(
            format_chord(&ev(KeyCode::F(12), KeyModifiers::NONE)),
            Some("<F12>".into())
        );
    }

    #[test]
    fn literal_lt_escapes_as_lt_token() {
        assert_eq!(
            format_chord(&ev(KeyCode::Char('<'), KeyModifiers::NONE)),
            Some("<lt>".into())
        );
    }

    #[test]
    fn from_event_normalises_ctrl_letter_lowercase() {
        let lower =
            from_event(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)).expect("ctrl-c");
        let upper =
            from_event(&ev(KeyCode::Char('C'), KeyModifiers::CONTROL)).expect("ctrl-C");
        assert_eq!(lower, upper);
        assert_eq!(lower, KeyChord::ctrl('c'));
    }

    #[test]
    fn from_event_strips_redundant_shift_on_bare_letter() {
        // Terminal reports `Char('A') + SHIFT`; canonical form
        // is just `Char('A')` (case encodes shift).
        let chord =
            from_event(&ev(KeyCode::Char('A'), KeyModifiers::SHIFT)).expect("shift-A");
        assert_eq!(chord, KeyChord::char('A'));
        assert!(!chord.mods.shift());
    }

    #[test]
    fn from_event_keeps_shift_on_special_keys() {
        let stab = from_event(&ev(KeyCode::Tab, KeyModifiers::SHIFT)).expect("shift-tab");
        assert_eq!(stab.key, KeyKind::Special(SpecialKey::Tab));
        assert!(stab.mods.shift());
        let sf1 = from_event(&ev(KeyCode::F(1), KeyModifiers::SHIFT)).expect("shift-F1");
        assert!(sf1.mods.shift());
    }

    #[test]
    fn from_event_canonicalises_back_tab_to_tab_plus_shift() {
        let chord = from_event(&ev(KeyCode::BackTab, KeyModifiers::NONE)).expect("back-tab");
        assert_eq!(chord.key, KeyKind::Special(SpecialKey::Tab));
        assert!(chord.mods.shift());
    }

    #[test]
    fn keyevent_to_keychord_to_string_matches_format_chord() {
        // The shim and the typed path produce the same string
        // by construction; this test guards against regression
        // if the shim diverges.
        let cases = &[
            ev(KeyCode::Char('a'), KeyModifiers::NONE),
            ev(KeyCode::Char('A'), KeyModifiers::NONE),
            ev(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ev(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            ev(KeyCode::Char('x'), KeyModifiers::ALT),
            ev(KeyCode::Esc, KeyModifiers::NONE),
            ev(KeyCode::Tab, KeyModifiers::NONE),
            ev(KeyCode::BackTab, KeyModifiers::NONE),
            ev(KeyCode::Tab, KeyModifiers::SHIFT),
            ev(KeyCode::F(7), KeyModifiers::NONE),
            ev(KeyCode::Char('<'), KeyModifiers::NONE),
        ];
        for e in cases {
            let via_shim = format_chord(e);
            let via_typed = from_event(e).map(|c| c.to_string());
            assert_eq!(via_shim, via_typed, "mismatch for {e:?}");
        }
    }
}
