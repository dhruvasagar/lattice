//! Declarative contributions on [`crate::Mode`].
//!
//! `OptionOverrideSet` graduated to a real type in
//! `lattice_config::overrides` as of M.2.1. K.2.3 (2026-06-01)
//! promoted [`Keymap`] from a stub to a real contribution type
//! (see `keymap-architecture.md` §11.2). The remaining stubs:
//!
//! - [`Subscription`] -- when the typed event bus stabilises a
//!   mode-side subscription type (DESIGN.md §5.10).
//! - [`DecorationProvider`] -- M.4 / decoration registry.

use lattice_grammar::{CommandInvocation, SourceLocation};
use lattice_protocol::ChordPattern;

use crate::binding_mode::BindingMode;

/// One mode-contributed keymap binding.
///
/// Declarative: the host calls [`crate::Mode::keymap`] once at
/// registration time and translates each binding into a
/// `BoundCommand` inserted at `KeymapLayer::MinorMode(mode.id())`.
/// Re-translation only happens on dynamic
/// `ModeRegistry::register` after boot.
///
/// `source` is captured at the binding's own `file!()` +
/// `line!()` (via [`SourceLocation::builtin_file`]) so
/// `:describe-key` can name the contributing crate without
/// the host having to track provenance separately.
#[derive(Debug, Clone, PartialEq)]
pub struct KeymapBinding {
    /// Binding-mode the chord resolves in (Normal, Insert, …).
    pub mode: BindingMode,
    /// Registration path -- one [`ChordPattern`] per chord
    /// in the sequence (`gd` -> two `Literal` chords, `'a` ->
    /// one `Literal` + one `CharLiteral` for the mark name).
    pub chords: Vec<ChordPattern>,
    /// Typed invocation the dispatcher fires on match. Same
    /// shape host-registered bindings carry, so the matcher
    /// engine treats mode-contributed and host-registered
    /// bindings identically once translated.
    pub command: CommandInvocation,
    /// Where this binding was registered. Surfaces in
    /// `:describe-key` and the upcoming `:keymap` listing.
    pub source: SourceLocation,
}

impl KeymapBinding {
    /// Construct one mode-contributed binding. Modes use the
    /// `lattice_grammar::SourceLocation::builtin_file(file!(),
    /// line!())` idiom for `source` so provenance points at
    /// the binding declaration's own `file:line`.
    pub fn new(
        mode: BindingMode,
        chords: Vec<ChordPattern>,
        command: CommandInvocation,
        source: SourceLocation,
    ) -> Self {
        Self {
            mode,
            chords,
            command,
            source,
        }
    }
}

/// A mode's full keymap contribution.
///
/// `Keymap::default()` is the empty contribution -- modes that
/// don't ship bindings rely on the [`crate::Mode::keymap`] trait
/// default. Modes that do contribute build the binding list
/// imperatively (today) or via a macro (a thin layer on top of
/// this type can ship later without touching the substrate).
///
/// Layer placement is implicit at translation time: every
/// binding in this list lands at
/// `KeymapLayer::MinorMode(mode.id())` per K.1.b convention.
/// Per-binding layer is *not* exposed here -- letting a mode
/// inject into another layer would break the layer-priority
/// contract (a "minor mode" silently shadowing a builtin would
/// be invisible to `:describe-key`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keymap {
    /// Declarative list of bindings this mode contributes.
    pub bindings: Vec<KeymapBinding>,
}

impl Keymap {
    /// Empty keymap. Equivalent to `Keymap::default()`; kept
    /// for symmetry with builder-style construction.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one binding. Returns `self` so call sites can
    /// chain `Keymap::new().bind(...).bind(...)`.
    pub fn bind(mut self, binding: KeymapBinding) -> Self {
        self.bindings.push(binding);
        self
    }

    /// Append one binding parsed from a chord-sequence string.
    ///
    /// The recommended idiom for mode-contributed keymaps:
    ///
    /// ```ignore
    /// fn keymap(&self) -> Keymap {
    ///     Keymap::new()
    ///         .bind_chord(BindingMode::Normal, "]e", self.commands.excerpt_next)
    ///         .bind_chord(BindingMode::Normal, "[e", self.commands.excerpt_prev)
    /// }
    /// ```
    ///
    /// `#[track_caller]` propagates the binding row's own
    /// `file:line` into the resulting [`SourceLocation`]; no
    /// `SourceLocation::builtin_file(file!(), line!())`
    /// boilerplate per row. `:describe-key` shows the chord's
    /// declaration site directly.
    ///
    /// The chord string is parsed via
    /// [`lattice_protocol::parse_chord_sequence`] -- accepts
    /// the same notation the host's `keymap_entry!` macro
    /// catalog uses (`"j"`, `"gd"`, `"]e"`, `"<C-w>j"`,
    /// `"<Esc>"`, `"<C-S-x>"`, …). Wildcards (`'a`, `"a`,
    /// `fX`) are *not* expressible here; the rare mode that
    /// needs `ChordPattern::CharLiteral` calls [`Keymap::bind`]
    /// directly with an explicit `chords` vector.
    ///
    /// **Panics on parse error.** Mode bindings are declared
    /// at compile-time-static call sites with constant chord
    /// strings; a malformed string is a bug in the mode impl,
    /// not a runtime condition. The panic message names the
    /// chord string + the caller location so the fix is
    /// obvious. Same shape as host-side catalog drift: the
    /// editor refuses to boot rather than silently dropping
    /// the binding.
    #[track_caller]
    pub fn bind_chord(
        self,
        mode: BindingMode,
        chord: &str,
        command: CommandInvocation,
    ) -> Self {
        let chords = lattice_protocol::parse_chord_sequence(chord)
            .unwrap_or_else(|e| {
                panic!("bind_chord: chord {chord:?} failed to parse: {e}")
            })
            .into_iter()
            .map(ChordPattern::Literal)
            .collect();
        let loc = std::panic::Location::caller();
        let source = SourceLocation::builtin_file(loc.file(), loc.line());
        self.bind(KeymapBinding::new(mode, chords, command, source))
    }
}

/// Stub. Real type lands when the typed event bus stabilises
/// a subscription shape for modes.
#[derive(Debug, Clone)]
pub struct Subscription {
    _private: (),
}

/// Stub. M.4 replaces with the real decoration-provider type.
#[derive(Debug, Clone)]
pub struct DecorationProvider {
    _private: (),
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_grammar::SourceKind;
    use lattice_protocol::{CommandId, KeyChord};

    fn synthetic_invocation() -> CommandInvocation {
        CommandInvocation::of(CommandId::new(1))
    }

    #[test]
    fn default_keymap_has_no_bindings() {
        let km = Keymap::default();
        assert!(km.bindings.is_empty());
    }

    #[test]
    fn new_equals_default() {
        assert_eq!(Keymap::new(), Keymap::default());
    }

    #[test]
    fn bind_appends_and_preserves_order() {
        let a = KeymapBinding::new(
            BindingMode::Normal,
            vec![ChordPattern::Literal(KeyChord::char('a'))],
            synthetic_invocation(),
            SourceLocation::builtin_file(file!(), line!()),
        );
        let b = KeymapBinding::new(
            BindingMode::Normal,
            vec![ChordPattern::Literal(KeyChord::char('b'))],
            synthetic_invocation(),
            SourceLocation::builtin_file(file!(), line!()),
        );
        let km = Keymap::new().bind(a.clone()).bind(b.clone());
        assert_eq!(km.bindings, vec![a, b]);
    }

    #[test]
    fn equality_is_structural() {
        // PartialEq derives through chords, CommandInvocation,
        // and SourceLocation -- two structurally-identical
        // bindings compare equal. Capture `line` once so both
        // bindings carry the same source-location for the test.
        let line = line!();
        let make = || {
            KeymapBinding::new(
                BindingMode::Visual,
                vec![ChordPattern::Literal(KeyChord::char('v'))],
                synthetic_invocation(),
                SourceLocation::builtin_file(file!(), line),
            )
        };
        assert_eq!(make(), make());
    }

    #[test]
    fn source_location_captures_declaration_line() {
        // `file!()` + `line!()` evaluated at the binding's own
        // declaration site -- this is the contract `:describe-key`
        // depends on. Capture two SourceLocations at known
        // lines and assert they hold those exact lines.
        let line_a = line!();
        let loc_a = SourceLocation::builtin_file(file!(), line_a);
        let line_b = line!();
        let loc_b = SourceLocation::builtin_file(file!(), line_b);
        assert_ne!(loc_a, loc_b);
        match &loc_a.kind {
            SourceKind::File { line, .. } => assert_eq!(*line, Some(line_a)),
            other => panic!("expected SourceKind::File, got {other:?}"),
        }
        match &loc_b.kind {
            SourceKind::File { line, .. } => assert_eq!(*line, Some(line_b)),
            other => panic!("expected SourceKind::File, got {other:?}"),
        }
    }

    #[test]
    fn bind_chord_parses_chord_string_into_literal_pattern() {
        // `]e` parses to two Literal chords (the multibuffer
        // excerpt-next idiom). Ergonomic substitute for
        // building `vec![ChordPattern::Literal(...), ...]` by
        // hand.
        let km = Keymap::new().bind_chord(
            BindingMode::Normal,
            "]e",
            synthetic_invocation(),
        );
        assert_eq!(km.bindings.len(), 1);
        assert_eq!(km.bindings[0].mode, BindingMode::Normal);
        assert_eq!(
            km.bindings[0].chords,
            vec![
                ChordPattern::Literal(KeyChord::char(']')),
                ChordPattern::Literal(KeyChord::char('e')),
            ],
        );
    }

    #[test]
    fn bind_chord_parses_modifier_notation() {
        // `<C-w>j` -- modifier-bearing first chord plus a bare
        // second chord. Same shape `keymap_entry!` accepts.
        let km = Keymap::new().bind_chord(
            BindingMode::Normal,
            "<C-w>j",
            synthetic_invocation(),
        );
        assert_eq!(
            km.bindings[0].chords,
            vec![
                ChordPattern::Literal(KeyChord::ctrl('w')),
                ChordPattern::Literal(KeyChord::char('j')),
            ],
        );
    }

    #[test]
    fn bind_chord_parses_emacs_style_prefix_sequence() {
        // `<C-x>pp` -- modifier-bearing prefix followed by two
        // bare chord descents. The shape emacs's `C-x p p`
        // (project-switch-project) takes once mapped into
        // Lattice's keymap. The trie indexes three nodes:
        // ctrl('x') -> char('p') -> char('p') (terminal).
        let km = Keymap::new().bind_chord(
            BindingMode::Normal,
            "<C-x>pp",
            synthetic_invocation(),
        );
        assert_eq!(
            km.bindings[0].chords,
            vec![
                ChordPattern::Literal(KeyChord::ctrl('x')),
                ChordPattern::Literal(KeyChord::char('p')),
                ChordPattern::Literal(KeyChord::char('p')),
            ],
        );
    }

    #[test]
    fn bind_chord_captures_source_at_call_site() {
        // `#[track_caller]` -- the resulting binding's source
        // points at the line `bind_chord` was called on, not
        // at the inside of `bind_chord` itself. That's what
        // makes the API ergonomic for mode tables.
        let expected_line = line!() + 1;
        let km = Keymap::new().bind_chord(
            BindingMode::Normal,
            "j",
            synthetic_invocation(),
        );
        match &km.bindings[0].source.kind {
            SourceKind::File { line, path } => {
                assert_eq!(*line, Some(expected_line));
                assert!(
                    path.to_string_lossy().ends_with("contributions.rs"),
                    "source path = {}",
                    path.display(),
                );
            }
            other => panic!("expected SourceKind::File, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "bind_chord")]
    fn bind_chord_panics_on_invalid_chord_string() {
        // Malformed chord string at a binding declaration is a
        // mode-impl bug -- surface at boot, not silently drop.
        let _ = Keymap::new().bind_chord(
            BindingMode::Normal,
            "<Foo>",
            synthetic_invocation(),
        );
    }
}
