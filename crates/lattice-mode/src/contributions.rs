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
use crate::keymap_entry::KeymapEntry;

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
    /// Human-readable one-line doc surfaced by `:describe-key`
    /// and the `:keymap` listing. K.2.4.A.0.2: populated when
    /// the binding originates from a `keymap_entry!`-driven
    /// entry (every entry carries a doc) and translated by the
    /// host pass into a `KeymapBinding`. `None` when the
    /// binding came via `bind_chord` (the terse chain form,
    /// optimized for ergonomics) or via `KeymapBinding::new`
    /// directly. Plugin and runtime `:bind` callers can attach
    /// docs via [`Self::with_doc`].
    pub doc: Option<&'static str>,
}

impl KeymapBinding {
    /// Construct one mode-contributed binding. Modes use the
    /// `lattice_grammar::SourceLocation::builtin_file(file!(),
    /// line!())` idiom for `source` so provenance points at
    /// the binding declaration's own `file:line`. `doc`
    /// defaults to `None`; attach a doc string via
    /// [`Self::with_doc`].
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
            doc: None,
        }
    }

    /// Attach a human-readable one-line doc. Returned via
    /// `:describe-key` and `:keymap`. Builder shape so call
    /// sites can chain `KeymapBinding::new(...).with_doc("...")`.
    pub fn with_doc(mut self, doc: &'static str) -> Self {
        self.doc = Some(doc);
        self
    }
}

/// A mode's full keymap contribution.
///
/// `Keymap::default()` is the empty contribution -- modes that
/// don't ship bindings rely on the [`crate::Mode::keymap`] trait
/// default.
///
/// Two declaration paths share the same contribution shape:
///
/// 1. **Chain form** — `Keymap::new().bind_chord(...)` /
///    `.bind(...)`. Terse; ergonomic for 1-5 bindings; populates
///    [`Keymap::bindings`] with fully-typed [`KeymapBinding`]s.
///    Source-location auto-captured via `#[track_caller]`. No
///    docstring per binding (use `.bind(KeymapBinding::new(...)
///    .with_doc(...))` if needed).
/// 2. **Table form** — `Keymap::from_entries(&MY_TABLE)` /
///    `.extend_with_entries(&...)`. Static-catalog-style;
///    ergonomic for 5-20+ bindings; references a
///    `&'static [KeymapEntry]` built with the [`keymap_entry!`]
///    macro. Each entry carries a docstring; the host
///    translation pass (K.2.4.A.0.3) resolves the entry's
///    canonical command-name string against the
///    `CommandRegistry` at registration time, building one
///    [`KeymapBinding`] per resolvable entry. Mode authors
///    declare entries in a `static` slice next to the impl;
///    macro-captured `file!()` + `line!()` give per-row
///    provenance.
///
/// The two paths compose:
///
/// ```ignore
/// fn keymap(&self) -> Keymap {
///     Keymap::from_entries(&MULTIBUFFER_KEYMAP)
///         .bind_chord(BindingMode::Normal, "<C-r>", self.cmd.refresh)
/// }
/// ```
///
/// Layer placement is implicit at translation time: every
/// binding / entry contributed by `Mode X` lands at
/// `KeymapLayer::MinorMode(x.id())` per K.1.b convention.
/// Per-binding layer is *not* exposed here -- letting a mode
/// inject into another layer would break the layer-priority
/// contract (a "minor mode" silently shadowing a builtin would
/// be invisible to `:describe-key`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keymap {
    /// Declarative list of bindings this mode contributes.
    /// Populated by the chain form (`bind` / `bind_chord`) and
    /// by the host translation pass when it resolves entries.
    pub bindings: Vec<KeymapBinding>,
    /// Static-catalog-style entries this mode contributes.
    /// Populated by [`Self::from_entries`] /
    /// [`Self::extend_with_entries`]. The host translation
    /// pass (K.2.4.A.0.3) walks both `bindings` and `entries`;
    /// entries get name→`CommandId` resolved via the
    /// `CommandRegistry` and the resulting [`KeymapBinding`]s
    /// (carrying the entry's `doc`) flow into the trie
    /// alongside the explicit `bindings`.
    pub entries: Vec<&'static KeymapEntry>,
}

impl Keymap {
    /// Empty keymap. Equivalent to `Keymap::default()`; kept
    /// for symmetry with builder-style construction.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a keymap from a static slice of `keymap_entry!`-
    /// constructed entries. The host translation pass resolves
    /// each entry's canonical command-name string against the
    /// `CommandRegistry` at registration time; unresolvable
    /// names log a `tracing::warn!` and skip the binding
    /// (matches the existing catalog-drift convention).
    ///
    /// Returns a keymap with [`Self::entries`] populated and
    /// [`Self::bindings`] empty. Compose with the chain form
    /// (`.bind_chord(...)`) to add typed bindings on top.
    pub fn from_entries(entries: &'static [KeymapEntry]) -> Self {
        Self {
            bindings: Vec::new(),
            entries: entries.iter().collect(),
        }
    }

    /// Append a static slice of `keymap_entry!`-constructed
    /// entries to an existing keymap. Returns `self` so call
    /// sites can chain
    /// `Keymap::new().bind_chord(...).extend_with_entries(&TBL)`
    /// or
    /// `Keymap::from_entries(&BASE).extend_with_entries(&MORE)`.
    pub fn extend_with_entries(mut self, entries: &'static [KeymapEntry]) -> Self {
        self.entries.extend(entries.iter());
        self
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

    // ---- K.2.4.A.0.2: table-form contribution (`from_entries`) ----

    #[test]
    fn default_keymap_has_no_entries() {
        // Sibling of `default_keymap_has_no_bindings` for the
        // new `entries` field. Modes that don't ship table-form
        // entries leave it empty; the host translation pass
        // walks it without finding work.
        let km = Keymap::default();
        assert!(km.entries.is_empty());
    }

    #[test]
    fn from_entries_collects_slice() {
        // Use the built-in vim default keymap (the static
        // catalog moved to lattice-mode in K.2.4.A.0.1) as a
        // realistic fixture — confirms the type plumbing
        // accepts the same shape modes will return from
        // `Mode::keymap()`.
        let catalog = crate::keymap_entry::default_keymap();
        let km = Keymap::from_entries(catalog);
        assert!(
            km.bindings.is_empty(),
            "from_entries leaves the bindings list empty"
        );
        assert_eq!(km.entries.len(), catalog.len());
    }

    #[test]
    fn extend_with_entries_appends_in_order() {
        // Split the static catalog in half and feed it through
        // the chain form. Resulting entries should be the
        // catalog's concatenation, with order preserved across
        // both halves.
        let catalog = crate::keymap_entry::default_keymap();
        let mid = catalog.len() / 2;
        let first = &catalog[..mid];
        let second = &catalog[mid..];
        let km = Keymap::from_entries(first).extend_with_entries(second);
        assert_eq!(km.entries.len(), catalog.len());
        // Pointer-equality on the borrowed entries: the first
        // collected entry IS the catalog's first entry; the
        // entry at `mid` IS the second slice's first entry.
        assert!(std::ptr::eq(km.entries[0], &first[0]));
        assert!(std::ptr::eq(km.entries[mid], &second[0]));
    }

    // ---- K.2.4.A.0.2: KeymapBinding::with_doc ----

    #[test]
    fn with_doc_sets_doc() {
        let binding = KeymapBinding::new(
            BindingMode::Normal,
            vec![ChordPattern::Literal(KeyChord::char('q'))],
            synthetic_invocation(),
            SourceLocation::builtin_file(file!(), line!()),
        );
        assert_eq!(binding.doc, None, "KeymapBinding::new defaults doc to None");
        let binding = binding.with_doc("Quit");
        assert_eq!(binding.doc, Some("Quit"));
    }
}
