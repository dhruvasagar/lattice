//! Keystroke → ANSI byte encoder for Terminal-Insert mode.
//!
//! Terminal-mode T2.a (2026-05-25): the minimum table that lets a
//! user type at a shell prompt and run commands:
//!
//! - printable chars (ASCII + UTF-8) → their bytes.
//! - `Enter` → `\r`.
//! - `Tab` → `\t`; `Shift-Tab` → `\x1b[Z` (backtab CSI Z).
//! - `Backspace` → `\x7f` (DEL — what xterm sends by default).
//! - `Esc` → `\x1b`.
//! - `Ctrl-a..Ctrl-z` → `\x01..\x1a`.
//! - `Ctrl-[` / `Ctrl-\` / `Ctrl-]` / `Ctrl-^` / `Ctrl-_` /
//!   `Ctrl-Space` / `Ctrl-@` → their canonical bytes per the
//!   VT/xterm table.
//!
//! Terminal-mode T2.b (2026-05-25): full encoder.
//! - Arrows / Home / End / PgUp / PgDn / Insert / Delete →
//!   CSI sequences (DECCKM-OFF; the application-cursor-keys
//!   variant lands when the alacritty_terminal swap tracks the
//!   mode bit).
//! - F1–F4 → SS3 `ESC O P/Q/R/S`.
//! - F5–F12 → CSI `ESC [ <n> ~`.
//! - Alt + key → ESC-prefix encoding (`\x1b` then the key's
//!   own bytes).
//! - Shift / Ctrl on arrows / F-keys use the xterm
//!   modifyOtherKeys-style `;<n>` parameter.
//!
//! DECCKM (application-cursor-keys) tracking is deferred to T2.c
//! once `lattice-terminal::reader` swaps to alacritty_terminal,
//! which surfaces the mode bit per terminal. The encoder takes
//! `cursor_keys_application_mode: bool` so the wiring is
//! one-call-site when the substrate lands; today the only caller
//! passes `false` (xterm-default cursor keys).
//!
//! Lives here in `lattice-host` because [`crate::chord::KeyChord`]
//! is host-owned and the substrate crate (`lattice-terminal`)
//! can't depend on the host without a cycle. If `KeyChord` ever
//! moves down to `lattice-core` this module can relocate to
//! `lattice-terminal::encode` without changing the surface.

use crate::chord::{KeyChord, KeyKind, SpecialKey};

/// Encode a single key chord into PTY-stdin bytes. Returns `None`
/// for chords with no Terminal-Insert meaning yet (modifier-only
/// releases, unmapped F-keys beyond F12). Callers treat `None`
/// as a no-op rather than a translation error.
///
/// Backwards-compat wrapper over [`key_to_ansi_with_mode`] that
/// pins DECCKM to `false` (xterm-default normal cursor keys).
/// Production callers route through this; tests opting into the
/// application-cursor-keys variant call the lower-level helper.
pub fn key_to_ansi(chord: &KeyChord) -> Option<Vec<u8>> {
    key_to_ansi_with_mode(chord, false)
}

/// Lower-level encoder with explicit cursor-key mode. When
/// `cursor_keys_application_mode` is `true`, bare arrow keys
/// encode as `ESC O <letter>` (SS3) instead of `ESC [ <letter>`
/// (CSI). The rest of the table is independent of the mode bit.
pub fn key_to_ansi_with_mode(
    chord: &KeyChord,
    cursor_keys_application_mode: bool,
) -> Option<Vec<u8>> {
    let mods = chord.mods;

    // Alt-prefix: ESC-prefix encoding. Compute the no-Alt payload
    // via a recursive call with the Alt bit cleared and splice
    // `\x1b` in front. Captures Alt-arrow (yields
    // `ESC ESC [ A`, bash/zsh's "argument word-back" idiom) and
    // Alt-letter (`ESC <c>`, the "meta key" convention).
    if mods.alt() {
        let stripped = KeyChord {
            key: chord.key,
            mods: mods.without(crate::chord::KeyMods::ALT),
        };
        return key_to_ansi_with_mode(&stripped, cursor_keys_application_mode).map(|mut bytes| {
            let mut out = Vec::with_capacity(1 + bytes.len());
            out.push(0x1b);
            out.append(&mut bytes);
            out
        });
    }

    // Ctrl-bearing first: `Ctrl-letter` is the densest part of
    // the table; short-circuit before the bare-char fall-through.
    // Capital ASCII letters yield the same control byte as
    // lower-case (Ctrl-A == Ctrl-a == \x01); xterm matches this
    // even on Caps-Lock.
    if mods.ctrl() {
        if let KeyKind::Char(c) = chord.key {
            if let Some(b) = ctrl_char_byte(c) {
                return Some(vec![b]);
            }
        }
    }

    match chord.key {
        KeyKind::Char(c) => {
            // Bare printable + Shift'd printables both serialise
            // to the upstream byte (the chord layer already
            // reflects shift state in `c`).
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            Some(s.as_bytes().to_vec())
        }
        KeyKind::Special(SpecialKey::Enter) => Some(vec![b'\r']),
        KeyKind::Special(SpecialKey::Tab) => {
            if mods.shift() {
                Some(b"\x1b[Z".to_vec())
            } else {
                Some(vec![b'\t'])
            }
        }
        KeyKind::Special(SpecialKey::Backspace) => Some(vec![0x7f]),
        KeyKind::Special(SpecialKey::Esc) => Some(vec![0x1b]),
        // Space arrives as `Special::Space` only when modifiers
        // are present (the chord layer keeps bare space as
        // `Char(' ')`). Encode as a literal space byte; modifier
        // combinations like Ctrl-Space hit the `ctrl_char_byte`
        // table above before reaching this arm.
        KeyKind::Special(SpecialKey::Space) => Some(vec![b' ']),
        // ---- Arrows + Home/End — cursor-key family.
        KeyKind::Special(SpecialKey::Up) => {
            Some(cursor_key(b'A', mods, cursor_keys_application_mode))
        }
        KeyKind::Special(SpecialKey::Down) => {
            Some(cursor_key(b'B', mods, cursor_keys_application_mode))
        }
        KeyKind::Special(SpecialKey::Right) => {
            Some(cursor_key(b'C', mods, cursor_keys_application_mode))
        }
        KeyKind::Special(SpecialKey::Left) => {
            Some(cursor_key(b'D', mods, cursor_keys_application_mode))
        }
        KeyKind::Special(SpecialKey::Home) => {
            Some(cursor_key(b'H', mods, cursor_keys_application_mode))
        }
        KeyKind::Special(SpecialKey::End) => {
            Some(cursor_key(b'F', mods, cursor_keys_application_mode))
        }
        // ---- Tilde-terminated family.
        KeyKind::Special(SpecialKey::PageUp) => Some(tilde_key(5, mods)),
        KeyKind::Special(SpecialKey::PageDown) => Some(tilde_key(6, mods)),
        KeyKind::Special(SpecialKey::Insert) => Some(tilde_key(2, mods)),
        KeyKind::Special(SpecialKey::Delete) => Some(tilde_key(3, mods)),
        // ---- Function keys: SS3 for F1–F4, tilde form for F5+.
        KeyKind::Special(SpecialKey::F(n)) => fn_key(n, mods),
    }
}

/// Build the CSI / SS3 encoding for an arrow or Home/End key.
/// Bare in app-mode: `ESC O <letter>`. Bare in normal mode:
/// `ESC [ <letter>`. Modified (any of shift / ctrl / alt-in-
/// inner-call): `ESC [ 1 ; <mod> <letter>` — modifiers always
/// stay on the CSI variant per xterm.
fn cursor_key(letter: u8, mods: crate::chord::KeyMods, app_mode: bool) -> Vec<u8> {
    let mod_param = modifier_param(mods);
    if mod_param == 1 {
        if app_mode {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    } else {
        let mut out = Vec::with_capacity(8);
        out.extend_from_slice(b"\x1b[1;");
        out.extend_from_slice(mod_param.to_string().as_bytes());
        out.push(letter);
        out
    }
}

/// Build a `ESC [ <n> ~` (or `ESC [ <n> ; <mod> ~` when
/// modified) encoding for keys that follow the tilde-terminator
/// convention: Insert (2), Delete (3), PageUp (5), PageDown (6),
/// and F5+ function keys.
fn tilde_key(n: u16, mods: crate::chord::KeyMods) -> Vec<u8> {
    let mod_param = modifier_param(mods);
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(n.to_string().as_bytes());
    if mod_param != 1 {
        out.push(b';');
        out.extend_from_slice(mod_param.to_string().as_bytes());
    }
    out.push(b'~');
    out
}

/// Encode F1–F12 per the xterm convention: F1–F4 use SS3
/// (`ESC O P/Q/R/S`), F5–F12 use the tilde form with explicit
/// numeric parameters (15, 17–21, 23–24). Higher F-keys
/// (F13–F24) ride the same scheme but are rarely needed; the
/// match below stops at F12 and returns `None` for the rest so
/// upstream sees the omission instead of fabricated bytes.
fn fn_key(n: u8, mods: crate::chord::KeyMods) -> Option<Vec<u8>> {
    let mod_param = modifier_param(mods);
    // F1–F4: SS3 form. Modified variants fall back to the CSI
    // shape `ESC [ 1 ; <mod> P/Q/R/S` per xterm.
    let ss3_letter = match n {
        1 => Some(b'P'),
        2 => Some(b'Q'),
        3 => Some(b'R'),
        4 => Some(b'S'),
        _ => None,
    };
    if let Some(letter) = ss3_letter {
        if mod_param == 1 {
            return Some(vec![0x1b, b'O', letter]);
        }
        let mut out = Vec::with_capacity(8);
        out.extend_from_slice(b"\x1b[1;");
        out.extend_from_slice(mod_param.to_string().as_bytes());
        out.push(letter);
        return Some(out);
    }
    // F5–F12 → tilde form with xterm's canonical numeric ids.
    // The non-sequential gap at 16 and 22 matches the spec.
    let param = match n {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(tilde_key(param, mods))
}

/// xterm modifier-parameter encoding: `1` = none, `2` = Shift,
/// `3` = Alt, `5` = Ctrl, `4` = Shift+Alt, `6` = Shift+Ctrl,
/// `7` = Ctrl+Alt, `8` = Shift+Ctrl+Alt. Built from the bitmap
/// `(shift << 0) | (alt << 1) | (ctrl << 2) + 1`. The Alt bit
/// is never observed here in practice — `key_to_ansi_with_mode`
/// strips Alt at its entry and re-prefixes with `\x1b` — but the
/// bit stays in the table so callers that bypass the wrapper
/// still get the right xterm parameter.
fn modifier_param(mods: crate::chord::KeyMods) -> u8 {
    let mut bits: u8 = 0;
    if mods.shift() {
        bits |= 0b001;
    }
    if mods.alt() {
        bits |= 0b010;
    }
    if mods.ctrl() {
        bits |= 0b100;
    }
    bits + 1
}

/// Map a printable ASCII character to its Ctrl-modified byte. Per
/// the VT/xterm table:
///
/// | Char           | Byte    |
/// |----------------|---------|
/// | `a..z` / `A..Z`| `01..1a`|
/// | `[`            | `1b`    |
/// | `\`            | `1c`    |
/// | `]`            | `1d`    |
/// | `^` / `~`      | `1e`    |
/// | `_` / `?`      | `1f`    |
/// | ` ` / `@`      | `00`    |
///
/// Returns `None` for chars with no Ctrl-mapping (digits, most
/// punctuation); callers fall through to the bare byte.
fn ctrl_char_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' | '~' => Some(0x1e),
        '_' | '?' => Some(0x1f),
        ' ' | '@' => Some(0x00),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::KeyMods;

    fn bare(key: KeyKind) -> KeyChord {
        KeyChord {
            key,
            mods: KeyMods::NONE,
        }
    }

    fn ctrl(c: char) -> KeyChord {
        KeyChord::ctrl(c)
    }

    #[test]
    fn printable_ascii_passes_through_as_one_byte() {
        assert_eq!(key_to_ansi(&bare(KeyKind::Char('a'))), Some(b"a".to_vec()));
        assert_eq!(key_to_ansi(&bare(KeyKind::Char('Z'))), Some(b"Z".to_vec()));
        assert_eq!(key_to_ansi(&bare(KeyKind::Char('0'))), Some(b"0".to_vec()));
        assert_eq!(key_to_ansi(&bare(KeyKind::Char(' '))), Some(b" ".to_vec()));
        assert_eq!(key_to_ansi(&bare(KeyKind::Char('-'))), Some(b"-".to_vec()));
    }

    #[test]
    fn enter_tab_backspace_esc_map_to_canonical_bytes() {
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Enter))),
            Some(vec![b'\r']),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Tab))),
            Some(vec![b'\t']),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Backspace))),
            Some(vec![0x7f]),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Esc))),
            Some(vec![0x1b]),
        );
    }

    #[test]
    fn ctrl_letters_map_to_low_control_bytes() {
        assert_eq!(key_to_ansi(&ctrl('a')), Some(vec![0x01]));
        assert_eq!(key_to_ansi(&ctrl('c')), Some(vec![0x03])); // SIGINT
        assert_eq!(key_to_ansi(&ctrl('d')), Some(vec![0x04])); // EOF
        assert_eq!(key_to_ansi(&ctrl('w')), Some(vec![0x17])); // WERASE
        assert_eq!(key_to_ansi(&ctrl('z')), Some(vec![0x1a])); // SIGTSTP
        // Case-insensitive: Ctrl-A == Ctrl-a.
        assert_eq!(key_to_ansi(&ctrl('A')), Some(vec![0x01]));
    }

    #[test]
    fn ctrl_punctuation_maps_per_xterm_table() {
        assert_eq!(key_to_ansi(&ctrl('[')), Some(vec![0x1b])); // ESC
        assert_eq!(key_to_ansi(&ctrl('\\')), Some(vec![0x1c]));
        assert_eq!(key_to_ansi(&ctrl(']')), Some(vec![0x1d]));
        assert_eq!(key_to_ansi(&ctrl('^')), Some(vec![0x1e]));
        assert_eq!(key_to_ansi(&ctrl('_')), Some(vec![0x1f]));
        assert_eq!(key_to_ansi(&ctrl(' ')), Some(vec![0x00]));
    }

    #[test]
    fn shift_tab_becomes_backtab_csi_z() {
        let backtab = KeyChord {
            key: KeyKind::Special(SpecialKey::Tab),
            mods: KeyMods::SHIFT,
        };
        assert_eq!(key_to_ansi(&backtab), Some(b"\x1b[Z".to_vec()));
    }

    // ---- T2.b (2026-05-25) — full encoder coverage ----

    fn special(k: SpecialKey, mods: KeyMods) -> KeyChord {
        KeyChord {
            key: KeyKind::Special(k),
            mods,
        }
    }

    #[test]
    fn bare_arrows_emit_csi_letter_in_normal_mode() {
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Up))),
            Some(b"\x1b[A".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Down))),
            Some(b"\x1b[B".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Right))),
            Some(b"\x1b[C".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Left))),
            Some(b"\x1b[D".to_vec()),
        );
    }

    #[test]
    fn bare_arrows_emit_ss3_letter_in_application_mode() {
        // DECCKM-on (program issued `ESC [ ? 1 h`): bare arrows
        // flip to SS3. Modifiers stay on CSI.
        assert_eq!(
            key_to_ansi_with_mode(&bare(KeyKind::Special(SpecialKey::Up)), true),
            Some(b"\x1bOA".to_vec()),
        );
        assert_eq!(
            key_to_ansi_with_mode(&bare(KeyKind::Special(SpecialKey::Left)), true),
            Some(b"\x1bOD".to_vec()),
        );
    }

    #[test]
    fn shifted_arrows_use_xterm_modifier_param() {
        assert_eq!(
            key_to_ansi(&special(SpecialKey::Up, KeyMods::SHIFT)),
            Some(b"\x1b[1;2A".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&special(SpecialKey::Right, KeyMods::CTRL)),
            Some(b"\x1b[1;5C".to_vec()),
        );
        // Ctrl+Shift = parameter `6` (bits 0b101 + 1).
        assert_eq!(
            key_to_ansi(&special(SpecialKey::Down, KeyMods::CTRL | KeyMods::SHIFT,)),
            Some(b"\x1b[1;6B".to_vec()),
        );
    }

    #[test]
    fn home_end_follow_cursor_key_family() {
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Home))),
            Some(b"\x1b[H".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::End))),
            Some(b"\x1b[F".to_vec()),
        );
    }

    #[test]
    fn insert_delete_page_keys_emit_tilde_form() {
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Insert))),
            Some(b"\x1b[2~".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Delete))),
            Some(b"\x1b[3~".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::PageUp))),
            Some(b"\x1b[5~".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::PageDown))),
            Some(b"\x1b[6~".to_vec()),
        );
    }

    #[test]
    fn modified_tilde_keys_carry_xterm_param() {
        assert_eq!(
            key_to_ansi(&special(SpecialKey::Delete, KeyMods::SHIFT)),
            Some(b"\x1b[3;2~".to_vec()),
        );
        assert_eq!(
            key_to_ansi(&special(SpecialKey::PageUp, KeyMods::CTRL)),
            Some(b"\x1b[5;5~".to_vec()),
        );
    }

    #[test]
    fn f1_to_f4_use_ss3_form() {
        for (n, letter) in [(1u8, b'P'), (2, b'Q'), (3, b'R'), (4, b'S')] {
            assert_eq!(
                key_to_ansi(&bare(KeyKind::Special(SpecialKey::F(n)))),
                Some(vec![0x1b, b'O', letter]),
                "F{n} should emit ESC O {}",
                letter as char,
            );
        }
    }

    #[test]
    fn f5_to_f12_use_tilde_form_with_xterm_ids() {
        let cases = [
            (5u8, 15u16),
            (6, 17),
            (7, 18),
            (8, 19),
            (9, 20),
            (10, 21),
            (11, 23),
            (12, 24),
        ];
        for (n, param) in cases {
            let expected = format!("\x1b[{param}~");
            assert_eq!(
                key_to_ansi(&bare(KeyKind::Special(SpecialKey::F(n)))),
                Some(expected.into_bytes()),
                "F{n} should emit ESC [ {param} ~",
            );
        }
    }

    #[test]
    fn unmapped_fn_returns_none() {
        // F13+ aren't in the T2.b table; the encoder rejects them
        // rather than fabricating bytes.
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::F(13)))),
            None,
        );
        assert_eq!(key_to_ansi(&bare(KeyKind::Special(SpecialKey::F(0)))), None,);
    }

    #[test]
    fn alt_letter_uses_esc_prefix_meta_convention() {
        // Alt-x → ESC x (the "meta key" convention; readline reads
        // this as M-x). Letters take the upstream byte; the Alt
        // bit clears before recursion.
        let alt_x = KeyChord {
            key: KeyKind::Char('x'),
            mods: KeyMods::ALT,
        };
        assert_eq!(key_to_ansi(&alt_x), Some(b"\x1bx".to_vec()));
    }

    #[test]
    fn alt_arrow_emits_double_esc_csi_idiom() {
        // Alt-Left: bash/zsh's "word backward" — `ESC ESC [ D`.
        let alt_left = special(SpecialKey::Left, KeyMods::ALT);
        assert_eq!(key_to_ansi(&alt_left), Some(b"\x1b\x1b[D".to_vec()));
    }
}
