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

### K.2.4 — Host translation pass ✅ (commit `ff9f9bf`)

Landed at `crates/lattice-host/src/keymap_mode_contributions.rs`
(the slice plan's tentative `keymap_trie/mode_contribution.rs`
home wasn't viable — `keymap_trie.rs` is a flat file, not a
directory — so the pass lives as a sibling module).

- Public entry `translate_mode_keymaps(handle, registry,
  command_registry)` (boot-path bulk walk) and
  `translate_mode_keymap(handle, mode_id, mode,
  command_registry)` (single-mode pass for future dynamic
  registration). The third `&CommandRegistry` arg added in
  K.2.4.A.0.3 supports table-form entry resolution; chain
  form alone ignores it.
- `ModeRegistry::iter() -> impl Iterator<Item = (ModeId,
  Arc<dyn DynMode>)>` added to `lattice-mode` to give the
  pass the live trait object alongside the `ModeId`.
- `lattice_mode::KeymapBinding` re-exported at crate root.
- Per-mode the pass: calls `mode.keymap()`; if empty (both
  `bindings` and `entries`), skip; otherwise concatenate
  chain-form bindings with resolved table-form entries,
  group by `BindingMode` into one `KeymapTrie` per mode,
  call `handle.push_layer(MinorMode(mode_id), label, map)`.
  One layer-rebuild per mode (idempotent on `mode_id` per
  K.1.b).
- Boot call site wired in `editor_boot.rs` right after the
  existing diff / multibuffer / project-search `push_layer`
  block; `mode_registry` and `registry` cloned at the
  struct-field assignment so the `keymap: { ... }` block
  borrows them after the moves.
- 9 unit tests (5 chain-form from K.2.4 + 3 entry-form from
  K.2.4.A.0.3 + 1 composability from K.2.4.A.0.4):
  empty-keymap-skips-layer, single-binding round-trip,
  binding-mode grouping, emacs-style `<C-x>pp` chord,
  bulk-vs-single parity, table-form name resolution via
  registry, synthetic-entry silent skip, unresolvable-name
  warn-and-skip, chain + table composability.

Bench row (sub-100µs single-mode translation) deferred to
K.2.4.A.5 alongside the K.2.4.A.0.5 doc carry-through —
benchmarks block on the polish arc landing first because the
describe-key tightening (K.2.4.A.1-A.4) churns the same code
path.

### K.2.4.A — Tighten `:describe-key` output 🚧

User-testing surfaced that `:describe-key`'s current output
enumerates all sources but doesn't make the layered-keymap-
resolution model legible. Four pieces of polish + a substrate-
cleanup sub-arc and the user docs that describe the polished
output factually. Insertion between K.2.4 and K.2.5 — K.2.5's
multibuffer / project-search migration consumes the polished
output and the unified catalog/registry presentation, so the
polish has to land first.

#### K.2.4.A.0 — `keymap_entry!` substrate consolidation 🚧

K.2.1's substrate-floor move stopped at the chord primitives;
`KeymapEntry` and `keymap_entry!` stayed in host. K.2.4.A.0
finishes the job so the macro is reachable from mode crates
and the table form becomes a real contribution path.
Composed of five sub-slices:

- **K.2.4.A.0.1 ✅ (commit `4f763d5`)** — `KeymapEntry`,
  `keymap_entry!` macro, `__builtin_source`, and the
  `default_keymap` / `lookup` / `entries` accessors moved
  from `lattice-host::keymap` to
  `lattice-mode::keymap_entry`. Forgery-prevention preserved
  via `KeymapEntry::__new` constructor + private `source`
  field. `lattice-host::keymap` is a re-export shim so
  `:describe-key`, `:keymap`, the TUI drift test, and
  every `keymap_normal`/`visual`/`insert`/`replace.rs`
  consumer keep resolving verbatim. Macro re-exported at
  `lattice_host` crate root so the `lattice_host::keymap_entry!`
  path used by `lattice-ui-tui` still resolves. Test path
  assertions updated `keymap.rs` → `keymap_entry.rs`.
- **K.2.4.A.0.2 ✅ (commit `6461f56`)** — `Keymap`
  contribution shape extended with `entries: Vec<&'static
  KeymapEntry>` alongside the chain-form `bindings`.
  `Keymap::from_entries(&'static [KeymapEntry])` +
  `extend_with_entries(...)` builders. `KeymapBinding` grew
  `pub doc: Option<&'static str>` + `with_doc(...)` builder
  so the entry-path's docstring survives into the runtime
  binding for `:describe-key` and `:keymap`. `KeymapEntry`
  gained `PartialEq + Eq` derives. 4 unit tests cover
  default-empty, from_entries-collects-slice,
  extend-appends-in-order, with_doc-sets-doc.
- **K.2.4.A.0.3 ✅ (commit `81c4600`)** —
  `translate_mode_keymaps` walks `keymap.entries`, resolves
  each entry's canonical command-name string against the
  `CommandRegistry`, parses the chord string, builds one
  `KeymapBinding` per resolvable entry carrying the entry's
  doc + source. Per-entry resolution: `command == None` →
  silent skip (synthetic catalog row); registry miss →
  `tracing::warn!` and skip (catalog drift);
  `parse_chord_sequence` failure → `tracing::warn!` and skip
  (defensive). Boot call site updated to pass
  `&command_registry`. 3 unit tests + 5 existing tests
  updated to thread the new `&CommandRegistry` arg.
- **K.2.4.A.0.4 ✅ (commit `eaf9e33`)** — single unit test
  closing the entry-form arc: a mode whose
  `Mode::keymap()` returns `Keymap::from_entries(&CAT)
  .bind_chord(Normal, "<C-r>", typed_cmd)` —
  composability case proving both paths land at the same
  `MinorMode(mode_id)` layer. Shape K.2.5's multibuffer
  migration will adopt.
- **K.2.4.A.0.5 🚧** — docs (this slice): `keymap-architecture.md`
  §11.2.2 entry-form contribution; this slice plan +
  ledger refresh; brief mention in user docs deferred to
  K.2.4.A.5 alongside the describe-key user-docs arc.

#### K.2.4.A.1 — Resolved-binding indicator 🗒

Add a "Resolved binding (under current active modes)" line
at the top of `:describe-key` output per binding-mode where
the chord is bound. Computed by replaying the K.1.c
precedence fold for the active buffer's mode set
(builtin/major < user/buffer < minors-in-activation-order;
last write wins). Shows: chord, resolved command, the winning
layer, the resolved binding's source via `as_link()`. If no
layer fires (chord bound only in inactive minors):
`"Not resolved here — bound in {inactive minor list}"`.
~80 LOC + tests.

#### K.2.4.A.2 — Friendly layer labels 🗒

Replace `{layer:?}` debug formatting in the runtime-registry
section with a labeller: `Built-in` / `Major: {major_name}`
/ `Minor: {minor_name}` / `User config` / `Buffer-local`.
Reuse the existing `KeymapHandle::layer_label(LayerId)` where
informative; fall back to the layer-kind name + mode-id
where not. ~30 LOC + tests.

#### K.2.4.A.3 — Source rendering via `as_link()` 🗒

Replace `format!("{source:?}")` with `source.as_link()` so
file:line entries render as clickable markdown links in the
help buffer. `SourceLocation::as_link()` already exists; the
follow-link handler routes `file:` links. ~15 LOC + tests.

#### K.2.4.A.4 — Catalog/registry unification 🗒

After K.2.4.A.0 lands, the static catalog
(`lattice_mode::keymap_entry::default_keymap()`) is the same
shape as the runtime registry's content. Drop the static-
catalog section from `:describe-key` output; render one
unified section per binding-mode. Eliminates the duality
users see today (the same `j` showing twice — once from the
informational catalog, once from the registry). ~50 LOC +
tests.

#### K.2.4.A.5 — User docs + bench row 🗒

Now that the polish (.A.1-.A.4) makes the layered resolution
visible, write the user-doc section in `docs/user/modes.md`
covering: how mode-contributed bindings appear in
`:describe-key` / `:keymap`, the K.1.c precedence model, how
to discover what a mode contributes. Plus the mode-author
guide at `docs/dev/notes/mode-keymap-authoring.md` covering
both the chain form (`bind_chord`) and table form
(`keymap_entry!`/`from_entries`). BENCHMARKS row for
single-mode translation deferred from K.2.4 lands here too,
once the post-polish translation cost is stable.

### K.2.5 — Migrate multibuffer + project-search bindings ✅ (commit `7719e27`)

Landed. `crates/lattice-host/src/multibuffer_keymap.rs`
deleted entirely (-292 LOC, 23 files modified in the K.2 arc
total). The contents split as planned:

- **Keymap bindings** → `Mode::keymap()` table form on
  `MultibufferMode` and `ProjectSearchMultibufferMode` in
  `lattice-multibuffer`. Both return
  `Keymap::from_entries(&STATIC_CATALOG)` with one
  `keymap_entry!` row per chord. Names resolve at host
  translation time against the `CommandRegistry`
  (`multibuffer.next-excerpt-start`, `action:search-jump-to-source`,
  etc.). Macro-captured source per row now points at the
  owning crate's catalog file, so `:describe-key ]e` jumps
  to `crates/lattice-multibuffer/src/mode.rs`.
- **Ex-commands** (`register_multibuffer_ex_commands`,
  `register_search_ex_command`) → `lattice-multibuffer` via
  `pub fn`s on `mode.rs` and `providers::search` respectively.
  Re-exported at the crate root.

Boot path in `editor_boot.rs` calls
`lattice_multibuffer::register_multibuffer_ex_commands` /
`lattice_multibuffer::providers::search::register_search_ex_command`
directly. The explicit `push_layer(MinorMode(multibuffer-mode))`
and `push_layer(MinorMode(project-search-multibuffer-mode))`
calls retire — the K.2.4 translation pass handles those via
`Mode::keymap()` now. `multibuffer_motion_ids` binding renamed
to `_` since the typed `MotionIds` return is no longer needed
(the keymap references motions by canonical name string; the
registration side-effect is what keeps name lookup successful).

Diff-mode's helper (`diff_mode_layer_bindings`) still uses the
older host-side push_layer pattern — migration is tracked
under [`mode-ownership-cleanup.md`](./mode-ownership-cleanup.md)
as the MO.x diff-mode-keymap-migration slice, sequenced after
K.2.7.

Verification: 641 lattice-host lib tests, 60
lattice-multibuffer lib tests, 12 describe-key tests all
pass. Workspace `--all-targets` clean.

### K.2.6 — Doc artefacts ✅

Architecture + slice plan + ledger updates carrying the K.2.5
landing through:

- ✅ Design fragment landed: [keymap-architecture.md §11](../../architecture/keymap-architecture.md#11-mode-owned-keymap-contributions-substrate-gap),
  with §11.2.1 (chain form, K.2.3) and §11.2.2 (table form,
  K.2.4.A.0) added in earlier doc commits.
- ✅ [mode-architecture.md §13](../../architecture/mode-architecture.md#13-mode-owned-keymaps--contribution-debt-2026-06-01)
  rewritten to point at the now-real `Mode::keymap()` (K.2.3 +
  K.2.4) instead of the "long-term ideal" framing. "Patterns
  already correct" updated: `MultibufferMode` +
  `ProjectSearchMultibufferMode` shown as table-form
  `Keymap::from_entries(...)` per K.2.5; diff-mode noted as
  still using the host-side helper pending the MO.x
  diff-mode-keymap-migration slice. "Convention for new mode
  work" rewritten — `Mode::keymap()` is THE convention now,
  not the long-term ideal.
- ✅ Slice plan + ledger refresh (this slice's commit) marks
  K.2.4 / K.2.4.A / K.2.5 / K.2.6 / K.2.7 with final status
  and commit hashes.
- 🗒 BENCHMARKS row deferred to a measured profiling sweep
  (see K.2.4.A.5 note) — the existing benchmarks.md rows are
  criterion-derived, and adding a stub without measured data
  would be misleading.

### K.2.7 — Unblock MO.1–MO.4 ✅ (symbolic — substrate now in place)

K.2.5 retired the last of the host-side `multibuffer_keymap`
glue, and the K.2.4.A.0 table form is the recommended path
for the LSP / Oil / Snippet keymap migrations. The
[`mode-ownership-cleanup`](./mode-ownership-cleanup.md)
slices that K.2 was blocking are now unblocked:

- **MO.1** LSP bindings → `Mode::keymap()` (table form) on
  `LspMode` / `LspReferencesMode` / `LspHoverMode` etc. in
  `lattice-lsp`. Per the §13 cluster-size note, ~7 chords
  split across logical sub-modes.
- **MO.2** Oil bindings → `Mode::keymap()` on `OilMode`. 1
  chord; trivial chain-form migration.
- **MO.3** Snippet bindings → `Mode::keymap()` on
  `SnippetMode` / `ActiveSnippetMode`. 4 chords; the
  runtime `is_snippet_active` check standing in for what
  should be a keymap-layer scope retires.
- **MO.4** Broader audit: every chord registered at
  `KeymapLayer::Builtin` that's actually mode-scoped moves
  via the same pattern.
- **MO.x diff-mode** (added during K.2.5 review): the
  host-side `diff_mode_layer_bindings` helper migrates to
  `DiffMode::keymap()` — same shape as the multibuffer
  migration K.2.5 landed.

MO.x sequencing tracked in
[`mode-ownership-cleanup.md`](./mode-ownership-cleanup.md);
this slice plan's responsibility ends with the substrate
being usable.

## K.2 sub-arc summary

Closed (2026-06-01 → 2026-06-02):

| Slice | Commit | Outcome |
|---|---|---|
| K.2.1 | `d075a66` | Chord primitives → lattice-protocol |
| K.2.2 | `d3dbe87` | BindingMode → lattice-mode |
| K.2.3 | `c6c3ffe` | Real Keymap contribution + chain form (`bind_chord`) |
| K.2.4 | `ff9f9bf` | Host translation pass (`translate_mode_keymaps`) |
| K.2.4.A.0.1 | `4f763d5` | keymap_entry! macro relocated to lattice-mode |
| K.2.4.A.0.2 | `6461f56` | Keymap::from_entries() + KeymapBinding.doc |
| K.2.4.A.0.3 | `81c4600` | Translation pass entry resolution |
| K.2.4.A.0.4 | `eaf9e33` | Chain + table composability test |
| K.2.4.A.0.5 | `f9a15cd` | Sub-arc docs (§11.2.2) |
| K.2.4.A.1 | `174cffd` | Resolved-binding indicator on :describe-key |
| K.2.4.A.2 | `90be78c` | Friendly layer labels |
| K.2.4.A.3 | `9e01634` | Source rendering via as_link() |
| K.2.4.A.4 | `28562c5` | Canonical names in runtime registry |
| K.2.4.A.5 | `3764c7d` | User docs + mode-author guide |
| K.2.5 | `7719e27` | Multibuffer + project-search migrated; multibuffer_keymap.rs deleted |
| K.2.6 | this | Doc artefacts (§13 + slice plan + ledger) |
| K.2.7 | this | Symbolic unblock for MO.x |

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
