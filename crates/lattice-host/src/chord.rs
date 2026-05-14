//! Renderer-neutral chord representation -- the typed canonical
//! form the keymap trie indexes by.
//!
//! Phase 5.4 split: every type + parser + formatter here is pure
//! data. The crossterm-coupled side (`KeyEvent → KeyChord`
//! conversion, `format_chord` for ratatui-driven describe-key
//! output) lives in `lattice-ui-tui::chord` and reaches into the
//! neutral types defined here. The future `lattice-ui-gpui` ships
//! its own adapter from GPUI's key event type into the same
//! [`KeyChord`] without coordinating with the TUI's adapter.
//!
//! ## Notation conventions
//!
//! Match the strings the keymap registry catalog uses:
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

use std::fmt;
use std::str::FromStr;

/// Canonical, stack-allocated representation of one chord.
///
/// `Copy` so call sites can pass it freely without lifetime
/// gymnastics; `Hash` + `Eq` so it works as a `HashMap` key in
/// the keymap trie. Memory: 8 bytes (1-byte mod bitfield + 1-byte
/// discriminant + a 4-byte char or 1-byte SpecialKey, padded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: KeyKind,
    pub mods: KeyMods,
}

/// What kind of key the chord represents.
///
/// `Char` covers printable characters and Ctrl/Alt-modified chars
/// (the modifier lives in `mods`). For *letters* the case
/// encodes shift (vim convention: `A` is shift+a; we don't carry
/// `KeyMods::SHIFT` for plain letters). For non-letter chars
/// (`$`, `0`, `<`, ...) the case is the only valid form.
///
/// `Special` covers named keys (`Esc`, `Enter`, `Tab`, `Up`, ...)
/// where shift IS carried separately because `<S-Tab>` is a
/// genuinely different chord from `<Tab>`.
///
/// Function keys live in `Special::F(u8)` rather than as their
/// own enum variant to keep the type compact; `1..=24` covers
/// every reasonable terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyKind {
    Char(char),
    Special(SpecialKey),
}

/// Named special keys. Renderer-neutral; crossterm's
/// `KeyCode::BackTab` is normalised away by the TUI adapter
/// (`from_crossterm`) into `Special(Tab) + KeyMods::SHIFT` so the
/// trie has one entry for "shift-tab" rather than two ambiguous
/// ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecialKey {
    Esc,
    Enter,
    Tab,
    Backspace,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// Function keys F1..=F24. `F(0)` is reserved (invalid).
    F(u8),
}

/// Modifier bitfield. `Copy + Eq + Hash` so the whole `KeyChord`
/// fits in a CPU register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyMods(pub u8);

impl KeyMods {
    pub const NONE: Self = Self(0);
    pub const CTRL: Self = Self(1 << 0);
    pub const SHIFT: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);
    pub const SUPER: Self = Self(1 << 3);

    #[inline]
    pub const fn ctrl(self) -> bool {
        self.0 & Self::CTRL.0 != 0
    }
    #[inline]
    pub const fn shift(self) -> bool {
        self.0 & Self::SHIFT.0 != 0
    }
    #[inline]
    pub const fn alt(self) -> bool {
        self.0 & Self::ALT.0 != 0
    }
    #[inline]
    pub const fn super_(self) -> bool {
        self.0 & Self::SUPER.0 != 0
    }
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Strip a modifier (used by renderer adapters to clear the
    /// redundant SHIFT bit on bare printable chars where the
    /// terminal already encoded shift in the case / shifted
    /// symbol).
    #[inline]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl std::ops::BitOr for KeyMods {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Parse-side error variants. Detail-level so `:bind`-style error
/// messages can surface what was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordParseError {
    /// String was empty.
    Empty,
    /// `<...>` token had no closing `>`.
    UnterminatedAngle { at: usize },
    /// `<...>` token body was empty (`<>`).
    EmptyAngle { at: usize },
    /// `<...>` body referenced an unknown name (`<Foo>`, `<F99>`,
    /// `<C-S-X>` where the body chunk after modifiers is
    /// unrecognised).
    UnknownName { name: String, at: usize },
    /// Modifier prefix (`C-`, `S-`, `M-`) without a body (`<C->`).
    DanglingModifier { at: usize },
    /// The same modifier appeared twice in one token (`<C-C-x>`).
    DuplicateModifier { at: usize },
    /// `<...>` body chunk after modifiers was longer than one
    /// chord (e.g. `<C-foo>`).
    BodyTooLong { name: String, at: usize },
    /// Sequence parser saw a stray `>` (no matching `<`).
    StrayClose { at: usize },
}

impl fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty chord string"),
            Self::UnterminatedAngle { at } => {
                write!(f, "unterminated `<...>` at byte {at}")
            }
            Self::EmptyAngle { at } => {
                write!(f, "empty `<>` at byte {at}")
            }
            Self::UnknownName { name, at } => {
                write!(f, "unknown chord name `{name}` at byte {at}")
            }
            Self::DanglingModifier { at } => {
                write!(f, "dangling modifier prefix at byte {at}")
            }
            Self::DuplicateModifier { at } => {
                write!(f, "duplicate modifier at byte {at}")
            }
            Self::BodyTooLong { name, at } => {
                write!(
                    f,
                    "body `{name}` after modifiers is not a single chord at byte {at}"
                )
            }
            Self::StrayClose { at } => {
                write!(f, "stray `>` at byte {at}")
            }
        }
    }
}

impl std::error::Error for ChordParseError {}

impl KeyChord {
    /// Build directly from kind + mods. Useful for tests +
    /// internal callers; production callers go through the
    /// renderer-specific adapter (`lattice_ui_tui::chord::from_event`
    /// for the TUI, the analogous function in the future GPUI
    /// adapter).
    #[inline]
    pub const fn new(key: KeyKind, mods: KeyMods) -> Self {
        Self { key, mods }
    }

    /// Plain printable character with no modifiers.
    #[inline]
    pub const fn char(c: char) -> Self {
        Self {
            key: KeyKind::Char(c),
            mods: KeyMods::NONE,
        }
    }

    /// Ctrl-modified character. Letter case is normalised by the
    /// caller; convention is lowercase (`<C-c>`, not `<C-C>`).
    #[inline]
    pub const fn ctrl(c: char) -> Self {
        Self {
            key: KeyKind::Char(c),
            mods: KeyMods::CTRL,
        }
    }

    /// Special key with no modifiers.
    #[inline]
    pub const fn special(k: SpecialKey) -> Self {
        Self {
            key: KeyKind::Special(k),
            mods: KeyMods::NONE,
        }
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Plain printable chars with no modifiers render bare,
        // except `<` which escapes as `<lt>` so the parser can
        // round-trip without ambiguity.
        if self.mods.is_empty()
            && let KeyKind::Char(c) = self.key
        {
            if c == '<' {
                return f.write_str("<lt>");
            }
            return write!(f, "{c}");
        }

        f.write_str("<")?;
        if self.mods.ctrl() {
            f.write_str("C-")?;
        }
        if self.mods.shift() {
            f.write_str("S-")?;
        }
        if self.mods.alt() {
            f.write_str("M-")?;
        }
        if self.mods.super_() {
            f.write_str("D-")?;
        }
        match self.key {
            KeyKind::Char(c) => {
                // Ctrl/Alt-letter is rendered lowercase
                // (normalised by the renderer adapter); `<C-S-c>`
                // is distinct from `<C-c>` only by the explicit
                // S- prefix.
                write!(f, "{c}")?;
            }
            KeyKind::Special(s) => {
                f.write_str(special_label(s))?;
            }
        }
        f.write_str(">")
    }
}

/// Canonical name for a `SpecialKey`. Round-trips through
/// `parse_special`. Renderer-neutral text; both the TUI's
/// `format_chord` and any future GPUI describe-key renderer use
/// this label.
pub fn special_label(k: SpecialKey) -> &'static str {
    match k {
        SpecialKey::Esc => "Esc",
        SpecialKey::Enter => "CR",
        SpecialKey::Tab => "Tab",
        SpecialKey::Backspace => "BS",
        SpecialKey::Space => "Space",
        SpecialKey::Up => "Up",
        SpecialKey::Down => "Down",
        SpecialKey::Left => "Left",
        SpecialKey::Right => "Right",
        SpecialKey::Home => "Home",
        SpecialKey::End => "End",
        SpecialKey::PageUp => "PageUp",
        SpecialKey::PageDown => "PageDown",
        SpecialKey::Insert => "Insert",
        SpecialKey::Delete => "Delete",
        SpecialKey::F(1) => "F1",
        SpecialKey::F(2) => "F2",
        SpecialKey::F(3) => "F3",
        SpecialKey::F(4) => "F4",
        SpecialKey::F(5) => "F5",
        SpecialKey::F(6) => "F6",
        SpecialKey::F(7) => "F7",
        SpecialKey::F(8) => "F8",
        SpecialKey::F(9) => "F9",
        SpecialKey::F(10) => "F10",
        SpecialKey::F(11) => "F11",
        SpecialKey::F(12) => "F12",
        SpecialKey::F(_) => "F?", // 13..24; renderer-only fallback
    }
}

/// Inverse of `special_label`. Used by the parser when an
/// `<...>` body's modifier-stripped chunk is more than one
/// char (`<Esc>`, `<F12>`, etc.).
fn parse_special(name: &str) -> Option<SpecialKey> {
    Some(match name {
        "Esc" | "Escape" => SpecialKey::Esc,
        "CR" | "Enter" | "Return" => SpecialKey::Enter,
        "Tab" => SpecialKey::Tab,
        "BS" | "Backspace" => SpecialKey::Backspace,
        "Space" => SpecialKey::Space,
        "Up" => SpecialKey::Up,
        "Down" => SpecialKey::Down,
        "Left" => SpecialKey::Left,
        "Right" => SpecialKey::Right,
        "Home" => SpecialKey::Home,
        "End" => SpecialKey::End,
        "PageUp" => SpecialKey::PageUp,
        "PageDown" => SpecialKey::PageDown,
        "Insert" | "Ins" => SpecialKey::Insert,
        "Delete" | "Del" => SpecialKey::Delete,
        n if n.starts_with('F') => {
            let num: u8 = n[1..].parse().ok()?;
            if (1..=24).contains(&num) {
                SpecialKey::F(num)
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

impl FromStr for KeyChord {
    type Err = ChordParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Single-chord parse: either one bare char or one
        // `<...>` token.
        let mut iter = s.char_indices();
        let (start, first) = iter.next().ok_or(ChordParseError::Empty)?;
        if first == '<' {
            let close_rel = s[start + 1..]
                .find('>')
                .ok_or(ChordParseError::UnterminatedAngle { at: start })?;
            let body_end = start + 1 + close_rel;
            let body = &s[start + 1..body_end];
            // Single-chord parse rejects trailing input -- the
            // caller is asking for ONE chord, not a sequence.
            if body_end + 1 != s.len() {
                return Err(ChordParseError::BodyTooLong {
                    name: s.to_string(),
                    at: start,
                });
            }
            return parse_angle_body(body, start);
        }
        // Bare char. Reject trailing input for the same reason.
        if iter.next().is_some() {
            return Err(ChordParseError::BodyTooLong {
                name: s.to_string(),
                at: start,
            });
        }
        Ok(KeyChord::char(first))
    }
}

/// Parse the body of an `<...>` token (without the angles).
/// Handles modifier prefixes (`C-`, `S-`, `M-`, `D-`), the
/// `lt` literal, special-key names, and bare letters.
fn parse_angle_body(body: &str, at: usize) -> Result<KeyChord, ChordParseError> {
    if body.is_empty() {
        return Err(ChordParseError::EmptyAngle { at });
    }
    if body == "lt" {
        return Ok(KeyChord::char('<'));
    }
    let mut mods = KeyMods::NONE;
    let mut rest = body;
    loop {
        // Each modifier prefix is exactly two bytes (`C-`,
        // `S-`, `M-`, `D-`). Walk them off the front.
        if rest.len() < 2 || rest.as_bytes()[1] != b'-' {
            break;
        }
        let prefix = rest.as_bytes()[0];
        let m = match prefix {
            b'C' => KeyMods::CTRL,
            b'S' => KeyMods::SHIFT,
            b'M' | b'A' => KeyMods::ALT,
            b'D' => KeyMods::SUPER,
            _ => break,
        };
        if mods.0 & m.0 != 0 {
            return Err(ChordParseError::DuplicateModifier { at });
        }
        mods = mods | m;
        rest = &rest[2..];
    }
    if rest.is_empty() {
        return Err(ChordParseError::DanglingModifier { at });
    }
    // After modifiers: either a single char or a special-key name.
    let mut chars = rest.chars();
    let first = chars.next().expect("rest non-empty checked above");
    if chars.next().is_none() {
        // Single char body. Letters with Ctrl / Alt normalise to
        // lowercase to match the adapter's canonical form. Plain
        // shift on a letter folds into the case (`<S-a>` -> `A`).
        if mods.ctrl() || mods.alt() {
            return Ok(KeyChord {
                key: KeyKind::Char(first.to_ascii_lowercase()),
                mods,
            });
        }
        if mods.shift() && first.is_ascii_alphabetic() {
            // <S-a> = `A`; strip shift.
            mods = KeyMods(mods.0 & !KeyMods::SHIFT.0);
            return Ok(KeyChord {
                key: KeyKind::Char(first.to_ascii_uppercase()),
                mods,
            });
        }
        return Ok(KeyChord {
            key: KeyKind::Char(first),
            mods,
        });
    }
    // Multi-char body -- must be a special-key name.
    let special = parse_special(rest).ok_or_else(|| ChordParseError::UnknownName {
        name: rest.to_string(),
        at,
    })?;
    Ok(KeyChord {
        key: KeyKind::Special(special),
        mods,
    })
}

/// Parse a chord-sequence string into the canonical
/// `Vec<KeyChord>` the keymap trie indexes by.
///
/// Examples:
///
/// - `"j"` -> `[char('j')]`
/// - `"gg"` -> `[char('g'), char('g')]`
/// - `"<C-w>j"` -> `[ctrl('w'), char('j')]`
/// - `"dw"` -> `[char('d'), char('w')]`
/// - `"<lt>"` -> `[char('<')]`
///
/// Used by:
/// - The `keymap_entry!` macro at startup to convert the
///   catalog's `&'static str` chord into the trie's typed key.
/// - `:bind` user / plugin invocations that take a
///   chord-string at runtime.
pub fn parse_chord_sequence(s: &str) -> Result<Vec<KeyChord>, ChordParseError> {
    if s.is_empty() {
        return Err(ChordParseError::Empty);
    }
    let mut out = Vec::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Walk to matching `>`. Brackets don't nest in
            // chord notation.
            let close = match s[i + 1..].find('>') {
                Some(rel) => i + 1 + rel,
                None => return Err(ChordParseError::UnterminatedAngle { at: i }),
            };
            let body = &s[i + 1..close];
            out.push(parse_angle_body(body, i)?);
            i = close + 1;
        } else if bytes[i] == b'>' {
            return Err(ChordParseError::StrayClose { at: i });
        } else {
            // One UTF-8 char. Walk to the next char boundary.
            let ch_len = utf8_char_len(bytes[i]);
            let end = i + ch_len;
            let ch = s[i..end].chars().next().expect("valid utf-8 boundary");
            out.push(KeyChord::char(ch));
            i = end;
        }
    }
    Ok(out)
}

/// UTF-8 byte length of the leading char given its first byte.
/// Returns 1 for ASCII / continuation bytes (defensive; the
/// caller has already checked it's a leading byte).
#[inline]
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b & 0b1110_0000 == 0b1100_0000 {
        2
    } else if b & 0b1111_0000 == 0b1110_0000 {
        3
    } else if b & 0b1111_1000 == 0b1111_0000 {
        4
    } else {
        1
    }
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

    #[test]
    fn keychord_display_round_trips_through_from_str() {
        // Pick representative chords across every shape:
        // bare char, Ctrl/Alt letters, multi-modifier, specials,
        // shift-special, function keys, the `<lt>` literal.
        let cases: &[KeyChord] = &[
            KeyChord::char('a'),
            KeyChord::char('$'),
            KeyChord::char('A'),
            KeyChord::ctrl('c'),
            KeyChord {
                key: KeyKind::Char('x'),
                mods: KeyMods::CTRL | KeyMods::SHIFT,
            },
            KeyChord {
                key: KeyKind::Char('q'),
                mods: KeyMods::ALT,
            },
            KeyChord::special(SpecialKey::Esc),
            KeyChord::special(SpecialKey::Enter),
            KeyChord::special(SpecialKey::Backspace),
            KeyChord {
                key: KeyKind::Special(SpecialKey::Tab),
                mods: KeyMods::SHIFT,
            },
            KeyChord::special(SpecialKey::F(1)),
            KeyChord::special(SpecialKey::F(12)),
            KeyChord::char('<'),
            KeyChord::char('>'),
        ];
        for c in cases {
            let s = c.to_string();
            let parsed: KeyChord = s
                .parse()
                .unwrap_or_else(|e| panic!("re-parse {s:?}: {e:?}"));
            assert_eq!(parsed, *c, "round-trip differs for {s:?}");
        }
    }

    #[test]
    fn parse_chord_sequence_walks_mixed_tokens() {
        let seq = parse_chord_sequence("<C-w>j").unwrap();
        assert_eq!(seq, vec![KeyChord::ctrl('w'), KeyChord::char('j')]);
    }

    #[test]
    fn parse_chord_sequence_handles_multi_key_built_in_chords() {
        assert_eq!(
            parse_chord_sequence("gg").unwrap(),
            vec![KeyChord::char('g'), KeyChord::char('g')]
        );
        assert_eq!(
            parse_chord_sequence("dw").unwrap(),
            vec![KeyChord::char('d'), KeyChord::char('w')]
        );
        assert_eq!(
            parse_chord_sequence("zt").unwrap(),
            vec![KeyChord::char('z'), KeyChord::char('t')]
        );
    }

    #[test]
    fn parse_chord_sequence_handles_lt_literal() {
        assert_eq!(
            parse_chord_sequence("<lt>").unwrap(),
            vec![KeyChord::char('<')]
        );
        // Mid-sequence too.
        assert_eq!(
            parse_chord_sequence("a<lt>b").unwrap(),
            vec![
                KeyChord::char('a'),
                KeyChord::char('<'),
                KeyChord::char('b')
            ]
        );
    }

    #[test]
    fn parse_chord_sequence_rejects_unterminated_angle() {
        assert!(matches!(
            parse_chord_sequence("<C-x"),
            Err(ChordParseError::UnterminatedAngle { .. })
        ));
    }

    #[test]
    fn parse_chord_sequence_rejects_stray_close() {
        assert!(matches!(
            parse_chord_sequence("a>b"),
            Err(ChordParseError::StrayClose { .. })
        ));
    }

    #[test]
    fn parse_chord_sequence_rejects_unknown_special() {
        assert!(matches!(
            parse_chord_sequence("<Foo>"),
            Err(ChordParseError::UnknownName { .. })
        ));
        assert!(matches!(
            parse_chord_sequence("<F99>"),
            Err(ChordParseError::UnknownName { .. })
        ));
    }

    #[test]
    fn parse_chord_sequence_rejects_dangling_modifier() {
        assert!(matches!(
            parse_chord_sequence("<C->"),
            Err(ChordParseError::DanglingModifier { .. })
        ));
    }

    #[test]
    fn parse_chord_sequence_rejects_duplicate_modifier() {
        assert!(matches!(
            parse_chord_sequence("<C-C-x>"),
            Err(ChordParseError::DuplicateModifier { .. })
        ));
    }

    #[test]
    fn parse_chord_sequence_accepts_uppercase_shifted_letter_via_s_prefix() {
        // `<S-a>` is a legacy form; canonicalises to `Char('A')`
        // (vim does the same).
        assert_eq!(
            parse_chord_sequence("<S-a>").unwrap(),
            vec![KeyChord::char('A')]
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
