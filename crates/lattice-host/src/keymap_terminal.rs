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
//! T2.b extends with arrows, function keys, Home/End/PgUp/PgDn,
//! Insert/Delete, Alt-prefix sequences, and DECCKM-mode-aware
//! cursor-key variants. The interface here is locked at T2.a so
//! the upgrade is body-only.
//!
//! Lives here in `lattice-host` because [`crate::chord::KeyChord`]
//! is host-owned and the substrate crate (`lattice-terminal`)
//! can't depend on the host without a cycle. If `KeyChord` ever
//! moves down to `lattice-core` this module can relocate to
//! `lattice-terminal::encode` without changing the surface.

use crate::chord::{KeyChord, KeyKind, SpecialKey};

/// Encode a single key chord into PTY-stdin bytes. Returns `None`
/// for chords with no Terminal-Insert meaning yet (unmapped
/// special keys, modifier-only releases). Callers treat `None`
/// as a no-op rather than a translation error.
pub fn key_to_ansi(chord: &KeyChord) -> Option<Vec<u8>> {
    let mods = chord.mods;

    // Ctrl-bearing first: `Ctrl-letter` is the densest part of
    // the table; short-circuit before the bare-char fall-through.
    // Capital ASCII letters yield the same control byte as
    // lower-case (Ctrl-A == Ctrl-a == \x01); xterm matches this
    // even on Caps-Lock.
    if mods.ctrl() && !mods.alt() {
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
        // T2.b expands the rest. Returning None here keeps the
        // surface honest — the translate layer can decide a
        // fallback (Normal-in-terminal motion lookup, or no-op)
        // rather than silently dropping bytes.
        _ => None,
    }
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

    #[test]
    fn unmapped_specials_return_none_for_t2a() {
        // Arrows / F-keys / Home / End / PgUp / PgDn ship in T2.b.
        assert_eq!(
            key_to_ansi(&bare(KeyKind::Special(SpecialKey::Up))),
            None,
        );
    }
}
