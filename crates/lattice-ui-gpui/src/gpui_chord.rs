//! GPUI `Keystroke` → [`KeyChord`] adapter.
//!
//! Phase 5.7.B.3: mirrors the role of `lattice-ui-tui::chord`
//! for the crossterm side — turns the renderer's native key
//! event shape into the canonical, renderer-neutral
//! [`KeyChord`] that the host's keymap trie + translate path
//! both consume.
//!
//! ## Why a string-typed adapter (no gpui dep)
//!
//! The lib of this crate must build (and its tests must run)
//! in headless CI without the `window` Cargo feature so the
//! host-substrate-reusable claim from 5.7's scaffold slice
//! remains provable on every host. Taking a `&gpui::Keystroke`
//! would link the whole `lattice-ui-gpui` lib against
//! `gpui = "0.2.2"` and the X11 / Wayland / Cocoa / Windows
//! display libs it pulls in transitively.
//!
//! Instead [`from_keystroke`] takes the **shape** of GPUI's
//! `Keystroke` as primitives: `key: &str` for the key id,
//! plus four `bool`s for the modifier set. The binary's GPUI
//! event handler is the thin glue that destructures
//! `KeyDownEvent.keystroke` into those primitives and calls
//! this adapter; the adapter itself is pure data.
//!
//! ## Normalisation rules
//!
//! Match what `lattice-ui-tui::chord::from_event` produces so
//! the keymap trie sees identical [`KeyChord`]s regardless of
//! which renderer originated the keystroke:
//!
//! - **Letters with Ctrl / Alt**: case folded to lowercase
//!   (`Ctrl-C` and `Ctrl-c` collapse).
//! - **Letters without modifiers**: case preserved (`a` and
//!   `A` are distinct chords by vim convention).
//! - **Letters with shift only**: shift folded into the case
//!   when GPUI reports the lowercase letter (some backends
//!   do); the redundant `KeyMods::SHIFT` is stripped.
//! - **Non-letter chars with shift only**: shift stripped (the
//!   key string already encodes the shifted symbol —
//!   `Shift-4` arrives as `"$"`).
//! - **Specials with shift**: shift preserved (`<S-Tab>` is
//!   distinct from `<Tab>`).
//! - **Space**: bare space becomes `KeyKind::Char(' ')`; with
//!   a modifier (`<C-Space>`) it stays a [`SpecialKey::Space`]
//!   so the trie has a distinct entry. GPUI's `"space"` key
//!   string is accepted alongside the literal `" "` char.
//!
//! Key-string vocabulary (case-insensitive, follows GPUI's
//! lowercase convention):
//!
//! - Specials: `"escape" | "esc"`, `"enter" | "return"`,
//!   `"tab"`, `"backspace"`, `"space"`, `"up"`, `"down"`,
//!   `"left"`, `"right"`, `"home"`, `"end"`, `"pageup"`,
//!   `"pagedown"`, `"insert"`, `"delete"`.
//! - Function keys: `"f1"`..`"f24"` (out-of-range returns
//!   `None`).
//! - Anything else of length 1 is treated as a printable
//!   character; longer strings that don't match a special key
//!   id return `None` (GPUI may report compound names this
//!   adapter doesn't recognise yet).

use lattice_host::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};

/// Normalise a GPUI [`Keystroke`]-shaped input into a canonical
/// [`KeyChord`].
///
/// `key` is GPUI's `Keystroke::key` string id (lowercase by
/// convention, but matched case-insensitively for robustness).
/// `control` / `alt` / `shift` / `platform` are the four
/// modifier bits of `Keystroke::modifiers`; `platform` maps to
/// [`KeyMods::SUPER`] so it joins the same modifier bitfield
/// the rest of the host uses.
///
/// Returns `None` for inputs with no chord representation:
/// empty key string, multi-character key strings that aren't
/// recognised special names, or `"f"`-prefixed function keys
/// outside the `1..=24` range.
///
/// [`Keystroke`]: https://docs.rs/gpui/latest/gpui/struct.Keystroke.html
pub fn from_keystroke(
    key: &str,
    control: bool,
    alt: bool,
    shift: bool,
    platform: bool,
) -> Option<KeyChord> {
    let mut mods = KeyMods::NONE;
    if control {
        mods = mods | KeyMods::CTRL;
    }
    if shift {
        mods = mods | KeyMods::SHIFT;
    }
    if alt {
        mods = mods | KeyMods::ALT;
    }
    if platform {
        mods = mods | KeyMods::SUPER;
    }

    if key.is_empty() {
        return None;
    }

    // Special-key id match first (case-insensitive). GPUI uses
    // lowercase by convention but the adapter is tolerant.
    let lower = key.to_ascii_lowercase();
    let special = match lower.as_str() {
        "escape" | "esc" => Some(SpecialKey::Esc),
        "enter" | "return" => Some(SpecialKey::Enter),
        "tab" => Some(SpecialKey::Tab),
        "backspace" => Some(SpecialKey::Backspace),
        "space" => Some(SpecialKey::Space),
        "up" => Some(SpecialKey::Up),
        "down" => Some(SpecialKey::Down),
        "left" => Some(SpecialKey::Left),
        "right" => Some(SpecialKey::Right),
        "home" => Some(SpecialKey::Home),
        "end" => Some(SpecialKey::End),
        "pageup" => Some(SpecialKey::PageUp),
        "pagedown" => Some(SpecialKey::PageDown),
        "insert" => Some(SpecialKey::Insert),
        "delete" => Some(SpecialKey::Delete),
        s if s.starts_with('f') && s.len() >= 2 && s.len() <= 3 => s[1..]
            .parse::<u8>()
            .ok()
            .filter(|&n| (1..=24).contains(&n))
            .map(SpecialKey::F),
        _ => None,
    };

    if let Some(sk) = special {
        // Specials preserve shift -- `<S-Tab>` is distinct from
        // `<Tab>`. Other modifiers ride through unchanged.
        // Bare space collapses to `Char(' ')` so the keymap
        // trie sees a single canonical entry shared with TUI's
        // adapter; with any modifier it stays the Special form.
        if matches!(sk, SpecialKey::Space) && mods.is_empty() {
            return Some(KeyChord::new(KeyKind::Char(' '), mods));
        }
        return Some(KeyChord::new(KeyKind::Special(sk), mods));
    }

    // Printable character. Must be exactly one char.
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        // Multi-char key string we don't recognise as special.
        return None;
    }

    let ctrl_or_alt = mods.ctrl() || mods.alt();
    let key_kind = if c == ' ' && !ctrl_or_alt {
        KeyKind::Char(' ')
    } else if ctrl_or_alt && c.is_ascii_alphabetic() {
        // Ctrl / Alt + letter normalises to lowercase.
        // Shift on a ctrl-letter is preserved.
        KeyKind::Char(c.to_ascii_lowercase())
    } else if !ctrl_or_alt && mods.shift() && c.is_ascii_lowercase() {
        // Shift + bare lowercase letter: GPUI may report the
        // unshifted letter when shift is held; fold to
        // uppercase + strip the redundant SHIFT bit (vim
        // convention: `A` is shift-a; no `<S-a>` chord).
        mods = mods.without(KeyMods::SHIFT);
        KeyKind::Char(c.to_ascii_uppercase())
    } else if !ctrl_or_alt && mods.shift() {
        // Shift on a non-letter or already-uppercase char.
        // The key string already encodes the shifted form
        // (terminal-like backends do this); strip the bit.
        mods = mods.without(KeyMods::SHIFT);
        KeyKind::Char(c)
    } else if !ctrl_or_alt {
        // Bare printable, no modifier work needed.
        KeyKind::Char(c)
    } else {
        // Ctrl / Alt + non-letter char: keep verbatim.
        KeyKind::Char(c)
    };

    Some(KeyChord::new(key_kind, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_returns_none() {
        assert_eq!(from_keystroke("", false, false, false, false), None);
    }

    #[test]
    fn plain_lowercase_letter_no_mods() {
        let chord = from_keystroke("a", false, false, false, false).unwrap();
        assert_eq!(chord.key, KeyKind::Char('a'));
        assert!(chord.mods.is_empty());
    }

    #[test]
    fn uppercase_letter_no_mods_preserves_case() {
        // Some backends report the post-shift uppercase letter
        // with the shift modifier already consumed. The case is
        // load-bearing (vim distinguishes `a` and `A`); leave it.
        let chord = from_keystroke("A", false, false, false, false).unwrap();
        assert_eq!(chord.key, KeyKind::Char('A'));
        assert!(chord.mods.is_empty());
    }

    #[test]
    fn shift_lowercase_letter_folds_into_uppercase_and_strips_shift() {
        // Backends that report the unshifted letter + shift
        // modifier: fold so the trie key matches `A` above.
        let chord = from_keystroke("a", false, false, true, false).unwrap();
        assert_eq!(chord.key, KeyKind::Char('A'));
        assert!(chord.mods.is_empty());
    }

    #[test]
    fn ctrl_letter_normalises_to_lowercase() {
        // Whether the backend reports `c` or `C`, ctrl-c should
        // produce the same chord (`<C-c>`).
        let a = from_keystroke("c", true, false, false, false).unwrap();
        let b = from_keystroke("C", true, false, false, false).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.key, KeyKind::Char('c'));
        assert!(a.mods.ctrl());
        assert!(!a.mods.shift());
    }

    #[test]
    fn ctrl_shift_letter_preserves_shift() {
        // `<C-S-c>` must stay distinct from `<C-c>`.
        let chord = from_keystroke("c", true, false, true, false).unwrap();
        assert_eq!(chord.key, KeyKind::Char('c'));
        assert!(chord.mods.ctrl());
        assert!(chord.mods.shift());
    }

    #[test]
    fn alt_letter_records_alt_modifier() {
        let chord = from_keystroke("x", false, true, false, false).unwrap();
        assert_eq!(chord.key, KeyKind::Char('x'));
        assert!(chord.mods.alt());
        assert!(!chord.mods.ctrl());
    }

    #[test]
    fn platform_maps_to_super() {
        let chord = from_keystroke("a", false, false, false, true).unwrap();
        assert!(chord.mods.super_());
    }

    #[test]
    fn shift_on_non_letter_stripped() {
        // `Shift-4` arrives as `"$"` — the key string already
        // encodes the shifted symbol; shift modifier is redundant.
        let chord = from_keystroke("$", false, false, true, false).unwrap();
        assert_eq!(chord.key, KeyKind::Char('$'));
        assert!(!chord.mods.shift());
    }

    #[test]
    fn escape_special_accepts_aliases_and_case() {
        for name in ["escape", "esc", "Escape", "ESC"] {
            let chord = from_keystroke(name, false, false, false, false).unwrap();
            assert_eq!(chord.key, KeyKind::Special(SpecialKey::Esc));
        }
    }

    #[test]
    fn enter_special_accepts_return_alias() {
        let a = from_keystroke("enter", false, false, false, false).unwrap();
        let b = from_keystroke("return", false, false, false, false).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.key, KeyKind::Special(SpecialKey::Enter));
    }

    #[test]
    fn tab_with_shift_preserves_shift() {
        // `<S-Tab>` is a distinct chord; shift survives on specials.
        let chord = from_keystroke("tab", false, false, true, false).unwrap();
        assert_eq!(chord.key, KeyKind::Special(SpecialKey::Tab));
        assert!(chord.mods.shift());
    }

    #[test]
    fn function_keys_in_range() {
        for n in 1u8..=12 {
            let s = format!("f{n}");
            let chord = from_keystroke(&s, false, false, false, false).unwrap();
            assert_eq!(chord.key, KeyKind::Special(SpecialKey::F(n)));
        }
    }

    #[test]
    fn function_keys_out_of_range_return_none() {
        assert_eq!(from_keystroke("f0", false, false, false, false), None);
        assert_eq!(from_keystroke("f25", false, false, false, false), None);
        assert_eq!(from_keystroke("f99", false, false, false, false), None);
    }

    #[test]
    fn space_bare_renders_as_char_space() {
        // `"space"` un-modified collapses to `Char(' ')` so the
        // trie has a single entry shared with TUI's `from_event`.
        let chord = from_keystroke("space", false, false, false, false).unwrap();
        assert_eq!(chord.key, KeyKind::Char(' '));
    }

    #[test]
    fn space_with_ctrl_keeps_special_form() {
        // With a modifier, the `Space` special variant carries
        // the chord — `<C-Space>` is unambiguous either way, but
        // matching trie shape with TUI is the goal.
        let chord = from_keystroke("space", true, false, false, false).unwrap();
        assert_eq!(chord.key, KeyKind::Special(SpecialKey::Space));
        assert!(chord.mods.ctrl());
    }

    #[test]
    fn unrecognised_multichar_key_returns_none() {
        // Anything multi-char that isn't a known special id is a
        // chord we can't represent yet — better to return None
        // than fabricate a bogus chord.
        assert_eq!(from_keystroke("hyper", false, false, false, false), None);
        assert_eq!(from_keystroke("xyz", false, false, false, false), None);
    }

    #[test]
    fn lt_char_passes_through_unchanged() {
        // Renderer-neutral storage; `<lt>` notation is a
        // *display* concern handled by `KeyChord::Display`.
        let chord = from_keystroke("<", false, false, false, false).unwrap();
        assert_eq!(chord.key, KeyKind::Char('<'));
    }
}
