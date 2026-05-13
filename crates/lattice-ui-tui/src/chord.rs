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

use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ---------------------------------------------------------------
// Typed `KeyChord` (audit slice 8.a)
//
// The chord-string formatter / parser used to round-trip directly
// through `String`. The audit's M3 refactor needs a typed
// intermediate so the keymap registry's trie can index by
// `KeyChord` (stack-only, copyable, hashable) without per-lookup
// allocation.
//
// Wire format is unchanged -- `Display for KeyChord` produces the
// same canonical strings the existing `format_chord` produced and
// the existing `KeymapEntry` catalog uses verbatim. `format_chord`
// is now a thin shim over `KeyChord::from_event` + `to_string` so
// every call site keeps working through migration.
// ---------------------------------------------------------------

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

/// Named special keys. Mirrors crossterm's `KeyCode` non-char
/// variants except `BackTab` -- canonical form for shift+tab is
/// `Special(Tab) + KeyMods::SHIFT`, set up so the trie has one
/// entry for "shift-tab" rather than two ambiguous ones.
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
pub struct KeyMods(u8);

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
}

impl std::ops::BitOr for KeyMods {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Parse-side error variants. Detail-level so
/// `:bind`-style error messages can surface what was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordParseError {
    /// String was empty.
    Empty,
    /// `<...>` token had no closing `>`.
    UnterminatedAngle { at: usize },
    /// `<...>` token body was empty (`<>`).
    EmptyAngle { at: usize },
    /// `<...>` body referenced an unknown name (`<Foo>`,
    /// `<F99>`, `<C-S-X>` where the body chunk after modifiers
    /// is unrecognised).
    UnknownName { name: String, at: usize },
    /// Modifier prefix (`C-`, `S-`, `M-`) without a body
    /// (`<C->`).
    DanglingModifier { at: usize },
    /// The same modifier appeared twice in one token
    /// (`<C-C-x>`).
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
    /// internal callers; production callers usually go through
    /// `from_event`.
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

    /// Normalise a `crossterm::KeyEvent` into a canonical
    /// `KeyChord`. Returns `None` for events that have no chord
    /// representation (release events on terminals that emit
    /// them, modifier-only presses, key codes we don't recognise).
    ///
    /// Normalisation rules (match the format_chord behaviour
    /// retained in the old API):
    ///
    /// - **Letters with Ctrl / Alt**: case is folded to lowercase
    ///   so `Ctrl-c` and `Ctrl-C` map to the same chord.
    /// - **Letters without modifiers**: case is preserved (vim's
    ///   `A` is uppercase a, distinct from `a`).
    /// - **Letters with shift only**: shift is folded into the
    ///   case (the terminal already uppercased the letter; we
    ///   strip the redundant `KeyMods::SHIFT`). `Shift-a` and
    ///   `A` collapse.
    /// - **Non-letter chars**: shift is stripped (the terminal
    ///   reports the shifted symbol, e.g. `$` for shift-4; the
    ///   modifier would be redundant).
    /// - **Specials with shift**: shift is preserved (`<S-Tab>`
    ///   is distinct from `<Tab>`).
    /// - **`KeyCode::BackTab`**: canonicalised to
    ///   `Special(Tab) + KeyMods::SHIFT` so the keymap trie has
    ///   one entry rather than two for "shift-tab".
    pub fn from_event(event: &KeyEvent) -> Option<Self> {
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
                    // Plain space renders as a literal `' '`
                    // when un-modified; promote to `Special::Space`
                    // only if the chord carries a modifier
                    // (so `<C-Space>` is unambiguous).
                    KeyKind::Char(' ')
                } else if ctrl_or_alt && c.is_ascii_alphabetic() {
                    // Ctrl / Alt + letter normalises to lowercase.
                    // Shift on a ctrl-letter is preserved
                    // (`<C-S-c>` stays distinct from `<C-c>`).
                    KeyKind::Char(c.to_ascii_lowercase())
                } else if !ctrl_or_alt {
                    // Bare or shift-only printable. Strip
                    // shift -- the terminal already encoded it
                    // in the case (for letters) or in the
                    // shifted symbol (for non-letters).
                    if mods.shift() {
                        mods = KeyMods(mods.0 & !KeyMods::SHIFT.0);
                    }
                    KeyKind::Char(c)
                } else {
                    KeyKind::Char(c)
                }
            }
            _ => return None,
        };

        // Specials don't strip shift (it's meaningful for
        // `<S-Tab>`, `<S-F1>`, etc.); the strip-on-bare-printable
        // logic above handles only `KeyKind::Char`.
        Some(Self { key, mods })
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
                // (normalised in `from_event`); `<C-S-c>` is
                // distinct from `<C-c>` only by the explicit
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

/// Render a single key event as canonical chord notation.
///
/// Returns `None` for events that have no chord representation
/// (release events on terminals that emit them, modifier-only
/// presses, etc.) so the caller can ignore them.
///
/// Now a thin shim over `KeyChord::from_event` + `to_string`.
/// Existing callers (`input.rs` chord-capture, future
/// `:describe-key` lookup, macro recording) keep working without
/// change; new code should prefer the typed `KeyChord` path.
pub fn format_chord(event: &KeyEvent) -> Option<String> {
    KeyChord::from_event(event).map(|c| c.to_string())
}

/// Canonical name for a `SpecialKey`. Round-trips through
/// `parse_special`.
fn special_label(k: SpecialKey) -> &'static str {
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
        // lowercase to match `from_event`'s canonical form. Plain
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

// `special_name` and `wrap_with_modifiers` (the old string-only
// formatter helpers) are gone now -- their job lives on
// `KeyChord::Display` + `special_label`. Existing callers reach
// the same canonical strings through `format_chord` (now a thin
// shim) and `KeyChord::to_string()`.

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

    // ---------------------------------------------------------
    // Typed `KeyChord` (audit slice 8.a)
    //
    // These tests cover the new path: KeyEvent → KeyChord →
    // String, and String → KeyChord (single chord) /
    // [KeyChord] (sequence). The existing format_chord tests
    // above implicitly exercise KeyChord too -- format_chord
    // is now a thin shim over the typed path.
    // ---------------------------------------------------------

    #[test]
    fn keychord_from_event_normalises_ctrl_letter_lowercase() {
        let lower =
            KeyChord::from_event(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)).expect("ctrl-c");
        let upper =
            KeyChord::from_event(&ev(KeyCode::Char('C'), KeyModifiers::CONTROL)).expect("ctrl-C");
        assert_eq!(lower, upper);
        assert_eq!(lower, KeyChord::ctrl('c'));
    }

    #[test]
    fn keychord_from_event_strips_redundant_shift_on_bare_letter() {
        // Terminal reports `Char('A') + SHIFT`; canonical form
        // is just `Char('A')` (case encodes shift).
        let chord =
            KeyChord::from_event(&ev(KeyCode::Char('A'), KeyModifiers::SHIFT)).expect("shift-A");
        assert_eq!(chord, KeyChord::char('A'));
        assert!(!chord.mods.shift());
    }

    #[test]
    fn keychord_from_event_keeps_shift_on_special_keys() {
        // `<S-Tab>`, `<S-F1>`, `<S-Up>` are all distinct from
        // their unmodified counterparts.
        let stab = KeyChord::from_event(&ev(KeyCode::Tab, KeyModifiers::SHIFT)).expect("shift-tab");
        assert_eq!(stab.key, KeyKind::Special(SpecialKey::Tab));
        assert!(stab.mods.shift());
        let sf1 = KeyChord::from_event(&ev(KeyCode::F(1), KeyModifiers::SHIFT)).expect("shift-F1");
        assert!(sf1.mods.shift());
    }

    #[test]
    fn keychord_from_event_canonicalises_back_tab_to_tab_plus_shift() {
        // `KeyCode::BackTab` IS shift-tab; canonical form is
        // `Tab + SHIFT` so the keymap trie has one entry.
        let chord =
            KeyChord::from_event(&ev(KeyCode::BackTab, KeyModifiers::NONE)).expect("back-tab");
        assert_eq!(chord.key, KeyKind::Special(SpecialKey::Tab));
        assert!(chord.mods.shift());
    }

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
    fn keyevent_to_keychord_to_string_matches_format_chord() {
        // For every key event format_chord handles, the typed
        // path produces the same string. The shim does this by
        // construction; this test guards against regression if
        // the shim ever diverges.
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
            let via_typed = KeyChord::from_event(e).map(|c| c.to_string());
            assert_eq!(via_shim, via_typed, "mismatch for {e:?}");
        }
    }
}
