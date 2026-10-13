//! GD.4 — which keys the window dispatches itself, and which it leaves for
//! the platform's text input to turn into text.
//!
//! A key-down is not always a character. `Option-e` then `e` is `é`; a held
//! `e` is an accent picker; an input method turns several keys into one
//! word. The operating system does that work, and it hands the result to
//! the window's input handler — but only for keys the key-down handler did
//! not consume. Before this module the window consumed every key, so the
//! platform never composed anything: a dead key typed its base letter.
//!
//! The rule, decided with Dhruva (2026-10-13):
//!
//! - **Composition applies where text is entered** — Insert, Replace, the
//!   `:` line, search, a prompt, a picker's query. Normal, Visual and
//!   operator-pending keep receiving keys as pressed, so an input method
//!   never swallows `j` (paramount #3: the grammar does not pass through
//!   composition).
//! - **`Option` + a printable key runs a binding if one claims it as
//!   pressed, and types what the platform says otherwise.** The host's
//!   keymap already answers "is `<M-x>` bound here"; the unbound case used
//!   to fall back to the bare letter, and now falls to the platform.
//! - **In-progress text lives in the window** ([`Composition`]). Nothing
//!   crosses to the editor until text is committed, so composing costs no
//!   edit, undo entry, reparse or LSP change (paramount #1).
//!
//! This module is the pure half: no GPUI types, so it builds and is tested
//! without the `window` feature. `window.rs` holds the handler GPUI calls.

use lattice_grammar::ModalState;
use lattice_host::chord::{KeyChord, KeyKind, KeyMods, SpecialKey};

/// Where a key-down goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRoute {
    /// The window resolves it against the keymap, as every key was before.
    Dispatch,
    /// The window leaves it unhandled; the platform composes it and calls
    /// the input handler with the text it types.
    Platform,
}

/// What [`route_key`] needs to know about the editor when a key arrives.
#[derive(Debug, Clone, Copy, Default)]
pub struct InputState {
    /// Text is being entered — see [`accepts_text`].
    pub text_entry: bool,
    /// The focused pane is a terminal taking keys. A shell wants `Option`
    /// as Meta (`M-b`, `M-f`), never as a way to type `∫`.
    pub terminal: bool,
    /// The next key is being captured (`:describe-key`), so it must arrive
    /// exactly as pressed.
    pub capture: bool,
    /// This platform's text input is wired up — see [`PLATFORM_COMPOSES`].
    pub platform_composes: bool,
}

/// Whether the window hands text keys to the platform on this OS.
///
/// **macOS only, deliberately.** The routing was designed from a capture
/// taken on a Mac and has been seen nowhere else, and GPUI treats an
/// unhandled key differently per platform: on Linux an unhandled `Alt`
/// chord types nothing, on Windows it falls to the system menu. Until
/// Linux and Windows each get their own capture, every key there is
/// dispatched as pressed, exactly as before this module existed, and the
/// window registers no input handler.
pub const PLATFORM_COMPOSES: bool = cfg!(target_os = "macos");

/// Whether `modal` is a state in which typed keys are text.
///
/// A picker's query is text too, but a picker is not a modal state; the
/// caller ORs that in.
pub fn accepts_text(modal: ModalState) -> bool {
    matches!(
        modal,
        ModalState::Insert
            | ModalState::Replace
            | ModalState::Command
            | ModalState::Search(_)
            | ModalState::Prompt
    )
}

/// Decide a key-down's route.
///
/// `key` and `key_char` are GPUI's: the key's name, and what the platform
/// says it types (`None` for a key that types nothing). `bound_as_pressed`
/// is asked only for an `Option` chord in a text-entry state, and answers
/// whether the keymap claims the chord with its modifier.
///
/// Only a key whose NAME is a single character takes the platform route.
/// `enter`, `tab`, `space` and `backspace` type something too, but they
/// are keys the keymap binds (completion accept, snippet jump, auto-pair
/// delete), and while a composition is open GPUI offers every key to the
/// platform first regardless.
pub fn route_key(
    state: InputState,
    key: &str,
    key_char: Option<&str>,
    control: bool,
    alt: bool,
    platform: bool,
    bound_as_pressed: impl FnOnce() -> bool,
) -> KeyRoute {
    if !state.platform_composes || !state.text_entry || state.capture || control || platform {
        return KeyRoute::Dispatch;
    }
    if key_char.is_none_or(str::is_empty) || key.chars().count() != 1 {
        return KeyRoute::Dispatch;
    }
    if alt && (state.terminal || bound_as_pressed()) {
        return KeyRoute::Dispatch;
    }
    KeyRoute::Platform
}

/// The chords that typing `text` is: one per character, each exactly what
/// the key path would have produced for that character.
///
/// Going through chords rather than an insert action is what makes
/// committed text behave as typed text in every state — it reaches the
/// `:` line and a search as well as a buffer, and Insert-mode bindings on
/// characters (auto-pairs, completion triggers) still see it.
pub fn chords_for_text(text: &str) -> Vec<KeyChord> {
    text.chars()
        .filter_map(|c| {
            let kind = match c {
                '\r' | '\n' => KeyKind::Special(SpecialKey::Enter),
                '\t' => KeyKind::Special(SpecialKey::Tab),
                ' ' => KeyKind::Special(SpecialKey::Space),
                // A no-break space is what macOS appends to a dead key's
                // spacing form (`´\u{a0}`); a cancelled dead key commits
                // the accent alone.
                '\u{a0}' => return None,
                c if c.is_control() => return None,
                c => KeyKind::Char(c),
            };
            Some(KeyChord::new(kind, KeyMods::NONE))
        })
        .collect()
}

/// Text the platform is still composing: shown underlined at the caret,
/// not yet in any buffer.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Composition {
    marked: String,
}

impl Composition {
    /// The in-progress text, empty when nothing is being composed.
    pub fn marked(&self) -> &str {
        &self.marked
    }

    /// Whether a composition is open.
    pub fn is_active(&self) -> bool {
        !self.marked.is_empty()
    }

    /// Replace the in-progress text. An empty string ends the composition.
    pub fn set(&mut self, text: &str) {
        self.marked.clear();
        self.marked.push_str(text);
    }

    /// End the composition without committing. Returns whether there was
    /// one, so the caller repaints only when something changed.
    pub fn clear(&mut self) -> bool {
        let had = !self.marked.is_empty();
        self.marked.clear();
        had
    }

    /// Length in UTF-16 code units, the unit the platform's ranges use.
    pub fn utf16_len(&self) -> usize {
        self.marked.encode_utf16().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSERT: InputState = InputState {
        text_entry: true,
        terminal: false,
        capture: false,
        platform_composes: true,
    };
    const NORMAL: InputState = InputState {
        text_entry: false,
        terminal: false,
        capture: false,
        platform_composes: true,
    };

    fn never() -> bool {
        panic!("the keymap must not be asked for this key")
    }

    /// The cases below are the 2026-10-13 capture from a Mac, key for key.
    #[test]
    fn plain_text_keys_go_to_the_platform_only_where_text_is_entered() {
        assert_eq!(
            route_key(INSERT, "a", Some("a"), false, false, false, never),
            KeyRoute::Platform
        );
        // Shifted: GPUI names the key by its base and reports the result.
        assert_eq!(
            route_key(INSERT, "a", Some("A"), false, false, false, never),
            KeyRoute::Platform
        );
        assert_eq!(
            route_key(INSERT, "~", Some("~"), false, false, false, never),
            KeyRoute::Platform
        );
        // Normal mode: `j` is a command, and stays one under an input method.
        assert_eq!(
            route_key(NORMAL, "j", Some("j"), false, false, false, never),
            KeyRoute::Dispatch
        );
    }

    #[test]
    fn keys_the_keymap_binds_are_dispatched_even_while_typing() {
        for (key, ch) in [
            ("enter", Some("\n")),
            ("tab", Some("\t")),
            ("space", Some(" ")),
            ("backspace", None),
            ("escape", None),
            ("left", None),
            ("f5", None),
        ] {
            assert_eq!(
                route_key(INSERT, key, ch, false, false, false, never),
                KeyRoute::Dispatch,
                "{key}"
            );
        }
        // Control and Cmd chords are never text.
        assert_eq!(
            route_key(INSERT, "a", None, true, false, false, never),
            KeyRoute::Dispatch
        );
        assert_eq!(
            route_key(INSERT, "v", Some("v"), false, false, true, never),
            KeyRoute::Dispatch
        );
    }

    #[test]
    fn option_types_unless_a_binding_claims_the_chord() {
        // `Option-e`: a dead key. Nothing binds `<M-e>`, so the platform
        // gets it and starts composing.
        assert_eq!(
            route_key(INSERT, "e", Some("´\u{a0}"), false, true, false, || false),
            KeyRoute::Platform
        );
        // `Option-3` types `£` the same way.
        assert_eq!(
            route_key(INSERT, "3", Some("£"), false, true, false, || false),
            KeyRoute::Platform
        );
        // A mode that binds `<M-e>` keeps it.
        assert_eq!(
            route_key(INSERT, "e", Some("´\u{a0}"), false, true, false, || true),
            KeyRoute::Dispatch
        );
        // Outside text entry `Option` is a modifier, and the keymap is not
        // consulted here at all — the dispatch does that.
        assert_eq!(
            route_key(NORMAL, "e", Some("´\u{a0}"), false, true, false, never),
            KeyRoute::Dispatch
        );
    }

    #[test]
    fn a_terminal_and_a_key_capture_take_keys_as_pressed() {
        let term = InputState {
            terminal: true,
            ..INSERT
        };
        // `M-b` is "back a word" to a shell.
        assert_eq!(
            route_key(term, "b", Some("˘\u{a0}"), false, true, false, never),
            KeyRoute::Dispatch
        );
        // Plain text still composes in a terminal.
        assert_eq!(
            route_key(term, "b", Some("b"), false, false, false, never),
            KeyRoute::Platform
        );
        let capture = InputState {
            capture: true,
            ..INSERT
        };
        assert_eq!(
            route_key(capture, "a", Some("a"), false, false, false, never),
            KeyRoute::Dispatch
        );
    }

    /// Off macOS nothing is routed to the platform: every key is dispatched
    /// as pressed, as it was before this module, and the keymap is never
    /// asked the Option question.
    #[test]
    fn a_platform_without_wired_text_input_dispatches_every_key() {
        let other = InputState {
            platform_composes: false,
            ..INSERT
        };
        assert_eq!(
            route_key(other, "a", Some("a"), false, false, false, never),
            KeyRoute::Dispatch
        );
        assert_eq!(
            route_key(other, "e", Some("e"), false, true, false, never),
            KeyRoute::Dispatch
        );
    }

    #[test]
    fn committed_text_is_the_chords_typing_it_would_be() {
        let c = |ch| KeyChord::new(KeyKind::Char(ch), KeyMods::NONE);
        assert_eq!(chords_for_text("é"), vec![c('é')]);
        assert_eq!(chords_for_text("日本"), vec![c('日'), c('本')]);
        assert_eq!(
            chords_for_text("a b"),
            vec![
                c('a'),
                KeyChord::new(KeyKind::Special(SpecialKey::Space), KeyMods::NONE),
                c('b'),
            ]
        );
        // A cancelled dead key commits its spacing form; the no-break
        // space macOS pads it with is not text the user typed.
        assert_eq!(chords_for_text("´\u{a0}"), vec![c('´')]);
    }

    #[test]
    fn a_composition_counts_in_utf16_and_reports_when_it_ends() {
        let mut comp = Composition::default();
        assert!(!comp.is_active());
        assert!(!comp.clear(), "nothing to end");
        comp.set("´");
        assert!(comp.is_active());
        assert_eq!(comp.utf16_len(), 1);
        comp.set("😀");
        assert_eq!(comp.utf16_len(), 2, "one emoji is two UTF-16 units");
        assert!(comp.clear());
        assert_eq!(comp.marked(), "");
    }
}
