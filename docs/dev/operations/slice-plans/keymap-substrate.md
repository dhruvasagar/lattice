# Slice plan: K.2 — keymap substrate (mode-owned bindings)

**Design:** [keymap-architecture.md §11](../../architecture/keymap-architecture.md#11-mode-owned-keymap-contributions-substrate-gap).

**Status:** 🗒 spec'd, paused on M.6.4+. Critical path: blocks
MO.1–MO.4 cleanup and the `multibuffer_keymap.rs` deletion.

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

### K.2.1 — Move chord primitives to `lattice-protocol`

Move `KeyChord`, `KeyKind`, `KeyMods`, `ChordPattern` from
`crates/lattice-host/src/chord.rs` → `lattice-protocol`.
`lattice-host::chord` becomes a `pub use lattice_protocol::*`
shim for one release cycle to avoid downstream churn (TUI /
GPUI renderers, dispatcher).

- ~730 LOC moved.
- Host re-exports for stability.
- Test coverage: the existing chord unit tests move with the
  types.
- Bench: no perf-relevant change (data types only).

### K.2.2 — Move `BindingMode` to `lattice-mode`

Move `BindingMode` enum from `crates/lattice-host/src/keymap.rs`
→ `lattice-mode::binding_mode` (or alongside the `Mode` trait).
Host re-exports.

- Verify nothing outside host uses `BindingMode` today — host
  is currently the only consumer; renderers receive resolved
  `BoundCommand`s, not `BindingMode`.

### K.2.3 — Make `Keymap` real

Add `lattice-grammar` dep to `lattice-mode`
(verify no cycle: `lattice-grammar` does not depend on
`lattice-mode`).

Replace the `Keymap` stub in
`crates/lattice-mode/src/contributions.rs` with the real
type per [keymap-architecture.md §11.2](../../architecture/keymap-architecture.md#112-the-real-keymap-contribution-type):

```rust
pub struct Keymap {
	pub bindings: Vec<KeymapBinding>,
}
pub struct KeymapBinding {
	pub mode: BindingMode,
	pub chords: Vec<ChordPattern>,
	pub command: CommandInvocation,
	pub source: SourceLocation,
}
```

Trait method default stays `Keymap::default()` (empty). Add
unit tests in `lattice-mode` for the contribution type
(equality, default-empty, source-location capture via
`file!()` / `line!()`).

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
