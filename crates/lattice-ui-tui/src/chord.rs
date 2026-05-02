//! Chord-notation formatter (DESIGN.md §5.11, §B.1).
//!
//! Converts a single `crossterm::KeyEvent` into the canonical chord
//! string used everywhere else in the UI: `<C-c>`, `<Esc>`, `<Tab>`,
//! `gg`, `<C-S-x>`. The keymap entries (`keymap.rs`) are written by
//! hand in this same notation; this module is the runtime inverse,
//! consumed by:
//!
//! - **Chord-capture in the cmdline** (`ArgKind::Chord` slots --
//!   `:describe-key` types itself). Raw key events get rendered
//!   into the cmdline as one chord token per keypress.
//! - **`:describe-key` lookup** (eventually): the same canonical
//!   string that the keymap was registered under.
//! - **Macro recording / replay** (later): tokens, not raw key
//!   events.
//!
//! Notation conventions match the keymap entries:
//!
//! - Bare printable char: `"a"`, `"$"`, `"0"`. No angles.
//! - Modifier-only-Shift on a printable char: folded into the char
//!   (`"A"`, not `"<S-a>"`). Shift on a non-character key keeps the
//!   prefix: `"<S-Tab>"`, `"<S-F1>"`.
//! - Ctrl: `"<C-x>"` -- always lowercase letter, even if the
//!   keyboard reports it uppercase.
//! - Alt / Meta: `"<M-x>"`.
//! - Combined modifiers in canonical order `C, S, M`: `"<C-S-x>"`,
//!   `"<C-M-x>"`.
//! - Named special keys: `<Esc>`, `<Tab>`, `<CR>`, `<BS>`, `<Up>`,
//!   `<Down>`, `<Left>`, `<Right>`, `<Home>`, `<End>`, `<PageUp>`,
//!   `<PageDown>`, `<Insert>`, `<Delete>`, `<F1>`-`<F12>`, `<Space>`.
//! - Literal `<` types as `<lt>` (vim convention) so the parser
//!   reading these strings can disambiguate.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Render a single key event as canonical chord notation.
///
/// Returns `None` for events that have no chord representation
/// (release events on terminals that emit them, modifier-only
/// presses, etc.) so the caller can ignore them.
pub fn format_chord(event: &KeyEvent) -> Option<String> {
    let mods = event.modifiers;
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);

    // Special keys go inside `< >` always, with explicit modifier
    // prefixes (Shift kept because `<Tab>` and `<S-Tab>` are
    // genuinely different chords in terminals that report it).
    let special = special_name(event.code);

    if let Some(name) = special {
        return Some(wrap_with_modifiers(name, ctrl, shift, alt));
    }

    // Character keys.
    match event.code {
        KeyCode::Char(c) => {
            if c == '<' && !ctrl && !alt {
                // `<` literal -- escape so the chord parser doesn't
                // confuse it with the start of a `<…>` token.
                return Some("<lt>".to_string());
            }

            if !ctrl && !alt {
                // Plain char (Shift folded into uppercase by the
                // terminal already). Single-char tokens with no
                // angle brackets.
                return Some(c.to_string());
            }

            // Ctrl / Alt-modified chars normalise the letter to
            // lowercase: `<C-c>` not `<C-C>`. Terminals that emit
            // the modifier may also have already uppercased the
            // letter; we strip that. Shift on a Ctrl-letter is
            // preserved (`<C-S-c>` is distinct from `<C-c>` on
            // terminals that report it).
            let body = c.to_ascii_lowercase().to_string();
            Some(wrap_with_modifiers(&body, ctrl, shift, alt))
        }
        _ => None,
    }
}

/// Map crossterm's `KeyCode` to the canonical name of a special
/// (non-character) key, or `None` if the code is a character /
/// unrepresentable.
fn special_name(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::Esc => "Esc",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => "S-Tab",
        KeyCode::Backspace => "BS",
        KeyCode::Enter => "CR",
        KeyCode::Up => "Up",
        KeyCode::Down => "Down",
        KeyCode::Left => "Left",
        KeyCode::Right => "Right",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::Insert => "Insert",
        KeyCode::Delete => "Delete",
        KeyCode::F(1) => "F1",
        KeyCode::F(2) => "F2",
        KeyCode::F(3) => "F3",
        KeyCode::F(4) => "F4",
        KeyCode::F(5) => "F5",
        KeyCode::F(6) => "F6",
        KeyCode::F(7) => "F7",
        KeyCode::F(8) => "F8",
        KeyCode::F(9) => "F9",
        KeyCode::F(10) => "F10",
        KeyCode::F(11) => "F11",
        KeyCode::F(12) => "F12",
        _ => return None,
    })
}

/// Wrap a body string with `<…>` angle brackets and any C/S/M
/// modifier prefixes the event carries. Order of prefixes is fixed
/// at `C, S, M` so the canonical string is stable across runs.
///
/// `BackTab` is a special case: its name is already `"S-Tab"`,
/// which encodes the Shift; we don't double-prefix.
fn wrap_with_modifiers(body: &str, ctrl: bool, shift: bool, alt: bool) -> String {
    let mut out = String::with_capacity(body.len() + 6);
    out.push('<');
    if ctrl {
        out.push_str("C-");
    }
    // BackTab already encodes shift in its name; skip the S- prefix
    // to avoid `<S-S-Tab>`.
    if shift && body != "S-Tab" {
        out.push_str("S-");
    }
    if alt {
        out.push_str("M-");
    }
    out.push_str(body);
    out.push('>');
    out
}

/// Number of bytes the last chord token occupies at the end of
/// `text`, treating it as a sequence of chord tokens (one chord
/// per logical keypress). Used by chord-capture's backspace
/// handler to remove a whole token instead of a single char.
///
/// A token is either:
/// - A `<…>` group (matched balanced angle brackets).
/// - A single character.
///
/// Returns 0 if `text` is empty.
pub fn last_chord_token_byte_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let n = bytes.len();
    if n == 0 {
        return 0;
    }
    if bytes[n - 1] == b'>' {
        // Walk back to the matching `<`. Brackets don't nest in
        // chord notation (no `<<…>>`), so a simple scan suffices.
        let mut i = n;
        while i > 0 {
            i -= 1;
            if bytes[i] == b'<' {
                return n - i;
            }
        }
        // Unbalanced `>` -- treat as a single-byte token.
        return 1;
    }
    // Plain char token. UTF-8 safe: walk back to a char boundary.
    let mut i = n - 1;
    while i > 0 && (bytes[i] & 0b1100_0000) == 0b1000_0000 {
        i -= 1;
    }
    n - i
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
    fn last_chord_token_treats_angle_group_as_one_unit() {
        assert_eq!(last_chord_token_byte_len("<C-c>"), 5);
        assert_eq!(last_chord_token_byte_len("a<C-c>"), 5);
        assert_eq!(last_chord_token_byte_len("<Esc>"), 5);
    }

    #[test]
    fn last_chord_token_handles_plain_char() {
        assert_eq!(last_chord_token_byte_len("abc"), 1);
        assert_eq!(last_chord_token_byte_len("a"), 1);
    }

    #[test]
    fn last_chord_token_handles_empty() {
        assert_eq!(last_chord_token_byte_len(""), 0);
    }

    #[test]
    fn last_chord_token_handles_utf8_char() {
        // Two-byte UTF-8 char (é) should pop as one unit.
        assert_eq!(last_chord_token_byte_len("aé"), 2);
    }
}
