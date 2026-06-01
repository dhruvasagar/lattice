# Slice plan: K.2 — keymap substrate (mode-owned bindings)

**Design:** [keymap-architecture.md §11](../../architecture/keymap-architecture.md#11-mode-owned-keymap-contributions-substrate-gap).

**Status:** 🚧 in progress. K.2.1 / K.2.2 / K.2.3 landed
(2026-06-01); K.2.4 (host translation pass) is the active
sub-slice. Critical path: blocks MO.1–MO.4 cleanup and the
`multibuffer_keymap.rs` deletion.

**Why:** `Mode::keymap()` exists on the trait
(`crates/lattice-mode/src/mode.rs:178`) but returns a
`_private: ()` stub
(`crates/lattice-mode/src/contributions.rs:19`). Every mode
that ships bindings today (LSP, Oil, Snippet, Multibuffer,
ProjectSearch, …) registers them in `lattice-host` via
hand-rolled glue, contradicting the mode-owns-its-surface
convention. K.2 closes the substrate gap so the trait method
is real and modes contribute their own bindings from their
own crates.

## Sequencing

### K.2.1 — Move chord primitives to `lattice-protocol` ✅ (commit `d075a66`)

Moved `KeyChord`, `KeyKind`, `KeyMods`, `SpecialKey`,
`ChordParseError`, `special_label`, `parse_chord_sequence`,
`last_chord_token_byte_len` (the full
`crates/lattice-host/src/chord.rs` surface) to
`crates/lattice-protocol/src/chord.rs`. `ChordPattern`
relocated from `crates/lattice-host/src/keymap_trie.rs` into
the same `lattice-protocol` module — it pairs with `KeyChord`
and is consumed by the `Keymap` contribution type that K.2.3
landed.

- ~730 LOC moved; 14 chord round-trip / parser tests moved
  with the types and pass against `lattice-protocol`.
- `lattice-host::chord` retained as a re-export shim
  (`pub use lattice_protocol::chord::*`) so the existing
  `lattice_host::chord::{KeyChord, …}` import paths used by
  the TUI / GPUI adapters and host internals keep resolving
  verbatim.
- `lattice-host::keymap_trie` re-exports `ChordPattern` from
  protocol so the matcher engine's internal callers don't
  churn.
- Bench: no perf-relevant change (data types only).
- Matcher engine (`KeymapTrie`, `KeymapLayer`, `BoundCommand`)
  stays in host — it owns the lookup hot path, not the wire
  shape.

### K.2.2 — Move `BindingMode` to `lattice-mode` ✅ (commit `d3dbe87`)

Moved the `BindingMode` enum + `label()` impl from
`crates/lattice-host/src/keymap.rs` (lines 29-122) to
`crates/lattice-mode/src/binding_mode.rs`. Variants +
label byte-identical to the host copy. `lattice-mode`
re-exports as `lattice_mode::BindingMode`;
`lattice-host::keymap` re-exports as
`pub use lattice_mode::BindingMode;` so the existing
matcher / dispatcher / TUI input / GPUI peer call sites keep
resolving (incl. `lattice_ui_tui::keymap::BindingMode` which
re-exports `lattice_host::keymap` transitively).

- Verified pre-move: host is the only consumer modulo the
  TUI / GPUI re-export chain; renderers receive resolved
  `BoundCommand`s, not `BindingMode`.
- 83 lattice-mode lib tests + 641 lattice-host lib tests
  green after the move.

### K.2.3 — Make `Keymap` real ✅ (commit `c6c3ffe`)

Added `lattice-grammar` dep to `lattice-mode`. Cycle-free:
`lattice-grammar`'s deps are
`lattice-protocol`/`lattice-core`/`thiserror`/`serde`/`tracing`
— none transitively reach `lattice-mode`. Resolves the
stub-deferral reason cited in §11.

Replaced the `_private: ()` `Keymap` stub in
`crates/lattice-mode/src/contributions.rs` with the real type
per [keymap-architecture.md §11.2](../../architecture/keymap-architecture.md#112-the-real-keymap-contribution-type):

```rust
pub struct Keymap { pub bindings: Vec<KeymapBinding> }
pub struct KeymapBinding {
	pub mode: BindingMode,
	pub chords: Vec<ChordPattern>,
	pub command: CommandInvocation,
	pub source: SourceLocation,
}
```

Trait default `Keymap::default()` stays the empty
contribution; every existing `Mode` impl across multibuffer /
LSP / oil / snippet / help / file-tree / terminal / syntax
keeps working unchanged. The substrate is opt-in — K.2.5
onward migrates one mode at a time to return a populated
`Keymap`.

**Ergonomic surface added beyond the original §11.2 design.**
`Keymap::new().bind_chord(mode, chord_str, command)` is the
recommended idiom: `#[track_caller]` auto-captures the
binding row's `file:line` into the `SourceLocation`
(zero-boilerplate provenance), and the chord string parses
via `lattice_protocol::parse_chord_sequence` so emacs-style
prefix sequences (`<C-x>pp`, `<C-x><C-s>`), vim window-prefix
(`<C-w>gd`), and arbitrary multi-chord paths declare in one
row. Wildcards (`'a`, `"a`, `fX`) deliberately not
expressible via `bind_chord`; the rare mode that needs
`ChordPattern::CharLiteral` falls back to `Keymap::bind` with
an explicit chord vector. Malformed chord strings panic at
boot (compile-time-static call sites; bug in the mode impl,
not a recoverable runtime condition). See
[keymap-architecture.md §11.2.1](../../architecture/keymap-architecture.md#1121-ergonomic-surface-keymapbind_chord)
for the full design rationale + the chord-notation table.

10 unit tests landed in `contributions.rs`: default-empty,
new-equals-default, bind-append-order, structural equality,
source-location capture, chord-string parsing, modifier
notation, emacs-style multi-chord prefix (`<C-x>pp`
explicitly), track-caller source capture, panic-on-malformed.

### K.2.4 — Host translation pass

Host gains an internal helper
(`crates/lattice-host/src/keymap_trie/mode_contribution.rs`
or similar) that walks `ModeRegistry`, calls `mode.keymap()`,
and translates each `KeymapBinding` into a
`BoundCommand::from_invocation(...)` insert at
`KeymapLayer::MinorMode(mode.id())`. The pass runs:

- At boot, after `ModeRegistry` is fully populated.
- On every `ModeRegistry::register` after boot (for plugin /
  dynamic mode loads).

Test coverage: round-trip — register a mode that returns one
binding via `Keymap`, assert the trie matches the chord at the
right layer. Cover both boot-time and post-boot registration
paths.

Bench coverage: add a row to `BENCHMARKS.md` for *"mode keymap
translation at activation"* — single-mode translation should
be sub-100µs for realistic binding counts (<50 per mode).

### K.2.5 — Migrate multibuffer + project-search bindings

Delete `crates/lattice-host/src/multibuffer_keymap.rs`. The
contents split:

- **Keymap bindings** (`multibuffer_mode_layer_bindings`,
  `project_search_mode_layer_bindings`) → `Mode::keymap()`
  on `MultibufferMode` / `ProjectSearchMultibufferMode` in
  `crates/lattice-multibuffer/`.
- **Ex-commands** (`register_multibuffer_ex_commands`,
  `register_search_ex_command`) → `lattice-multibuffer`
  via a new `register_<provider>_ex_commands(&mut CommandRegistry)`
  helper on the provider module. Ex-command registration is
  independent of keymap contribution; it stays explicit boot
  glue, but moves to the owning crate.

Boot path in `editor_boot.rs` calls
`lattice_multibuffer::register_multibuffer_ex_commands` /
`register_search_ex_commands` directly. No more host-side
keymap glue for multibuffer.

### K.2.6 — Doc artefacts

- ✅ Design fragment landed: [keymap-architecture.md §11](../../architecture/keymap-architecture.md#11-mode-owned-keymap-contributions-substrate-gap).
- Update [mode-architecture.md](../../architecture/mode-architecture.md) §13
  to point at the now-real `Mode::keymap()` instead of the
  stub.
- BENCHMARKS row added in K.2.4.

### K.2.7 — Unblock MO.1–MO.4

Once K.2.5 lands, [mode-ownership-cleanup](./mode-ownership-cleanup.md)
slices reduce to:

- **MO.1** LSP bindings → `Mode::keymap()` on `LspMode` /
  `LspReferencesMode` etc. in `lattice-lsp`.
- **MO.2** Oil bindings → `Mode::keymap()` on `OilMode`.
- **MO.3** Snippet bindings → `Mode::keymap()` on
  `SnippetMode` / `ActiveSnippetMode`.
- **MO.4** Broader audit: every chord registered at
  `KeymapLayer::Builtin` that's actually mode-scoped moves.

## Risk + roll-back

- **Risk:** translation pass walks every mode at boot. For
  hundreds of modes this is O(N) per boot. Mitigation: the
  pass is sub-millisecond per mode; total stays under the
  K.1.a registration budget. Benched in K.2.4.
- **Risk:** `lattice-mode` depending on `lattice-grammar` is
  a new edge; verify no graph cycle (`lattice-grammar` deps
  audited at K.2.3 — must not transitively reach
  `lattice-mode`).
- **Roll-back:** the K.2 translation pass is additive. If
  K.2.5 is reverted, the pass finds no `Keymap::bindings` to
  translate (all modes return `Keymap::default()`) and the
  hand-rolled host glue continues to work. No flag-day.

## Cross-references

- Design: [keymap-architecture.md §11](../../architecture/keymap-architecture.md#11-mode-owned-keymap-contributions-substrate-gap).
- Blocks: [mode-ownership-cleanup.md](./mode-ownership-cleanup.md) (MO.1–MO.4).
- Touches: [multibuffer-views.md](./multibuffer-views.md) (`multibuffer_keymap.rs` deletion at K.2.5).
