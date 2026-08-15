# `directory-listing-mode` — slice plan (CV.6)

> Sequencing for `docs/dev/architecture/directory-listing-mode.md`.
> That fragment owns the *what* and *why*; this file owns the *when* and
> *in what order*. Opened 2026-08-13 out of CV.5
> (`cursor-visibility.md`).

## Status

| Slice | Title | Status |
|---|---|---|
| DL.0 | Merge `lattice-oil` + `lattice-file-tree` into one crate | ✅ |
| DL.1 | `Style::Element(ElementId)` — spans can name a registered element | ✅ |
| DL.2 | `directory-listing-mode` skeleton + `listing.*` theme vocabulary | ✅ |
| DL.3a | Inlay runs carry a style (retires a hardcoded colour) | ✅ |
| DL.3b | The mode publishes entry icons as leading inlays | ✅ |
| DL.4 | File tree → `DocumentEntry`, both bespoke renderers deleted | ✅ |
| DL.5 | Oil → `DocumentEntry`, both bespoke renderers deleted | ✅ |
| DL.6 | Retire `ext_color`'s runtime lookup; benches + parity audit | 🚧 |

## Shape of the sequence

DL.0 is a **pure move**: two crates become one, deps rewired, not a line
of behaviour changed. It lands first because it makes DL.2's placement
question disappear and lets DL.4/DL.5 converge the two duplicated
directory readers and rope renderers *inside* one crate instead of
across a boundary.

DL.1–DL.3b are **invisible**: they add substrate and a mode that
nothing consumes yet. Nothing about the painted frame changes, which is
what makes them safe to land ahead of the risky part. (DL.3a is the one
exception in spirit — it makes LSP inlay hints resolve through the
theme instead of a hardcoded colour, which is a fix, not a no-op.)

DL.4 and DL.5 are each **atomic per kind**. A buffer cannot be
half-rendered, so one kind's storage migration, its switch to the shared
compose path, and the wiring of its icons/spans all land together —
otherwise there is a commit where that pane paints without icons, which
is a visible regression and a bad bisect point. They are separated from
each other because the tree is read-only and oil is writable: oil's `:w`
rename derivation is the real risk in this plan and deserves its own
commit and its own bisect slot.

## DL.0 — merge the two listing crates ✅

**Landed 2026-08-13** as `lattice-listing`. **Depends on:** nothing.
**Behaviour change: none.**

`lattice-oil` and `lattice-file-tree` become one crate (~1,240 lines,
existing module trees preserved). Rationale is in the design fragment's
"Where it lives" — briefly: no consumer takes one without the other
(`lattice-host`, `lattice-ui-tui`, `lattice-ui-gpui` all take both), and
the two already duplicate their directory readers, their entries→rope
renderers, and their entry models, which is the duplication the rest of
this plan exists to remove.

- Move both module trees into the merged crate; keep `oil` /
  `file_tree` as separate modules.
- Rewire `lattice-host`, `lattice-ui-tui`, `lattice-ui-gpui` Cargo
  manifests and the `register_oil_modes` / `register_file_tree_modes`
  boot calls in `lattice-host/src/modes.rs` + `editor_boot.rs`.
- **Do not converge anything yet.** The duplicated readers/renderers
  and the two entry models stay exactly as they are — unifying them is
  DL.2/DL.4/DL.5 work, and mixing it into a move commit destroys the
  "pure move" property that makes this one reviewable.

**Named `lattice-listing`** — the `oil` / `file_tree` modules inside
disambiguate, so the longer `lattice-directory-listing` bought nothing.
Consumers moved from `lattice_oil::X` / `lattice_file_tree::X` to
`lattice_listing::oil::X` / `lattice_listing::file_tree::X`; the module
namespacing is required rather than cosmetic, since both trees export a
`render_to_buffer` and a `modes` module.

**Verification.** The whole workspace builds and tests green, and the
diff is moves + manifest edits only. `git log --follow` should still
reach the pre-merge history of both trees.

## DL.1 — `Style::Element(ElementId)` ✅

**Landed 2026-08-13.** **Depends on:** nothing.

Add the variant to `lattice_cells::style::Style`, and one arm to
`lattice_syntax::theme_style::syntax_element_id` returning the id
unchanged. `ElementId` is a `u32` newtype, so `Style` stays `Copy`.

Every existing producer and consumer is untouched — this is purely
additive.

**Tests.** A span carrying a mode-registered element resolves to that
element's style through `resolve_syntax_style`; retuning the element in
the registry changes what the span resolves to. Both renderers' style
adaptation must handle the new variant — check for a non-exhaustive
`match` on `Style` in each peer.

**Note.** This is substrate, not listing-specific (design §5): it is
what any mode or WASM plugin needs to own a themed span vocabulary. It
lands here because this is the first consumer, but it is not
conditional on the rest of the plan and should not be reverted if a
later slice is deferred.

**As landed.** `lattice-cells` — previously a zero-dependency leaf —
now depends on `lattice-theme` so the variant carries a real
`ElementId` rather than a bare `u32`. The edge is safe: `lattice-theme`
is itself a deliberately minimal leaf (arc-swap + tracing), kept that
way because it sits on the renderer hot path.

One consumer broke, usefully: `extra_spans_version` folded a style into
a cache hash with `style as u64`, which the compiler permitted only
while every variant was field-less. It is now `Style::fingerprint()`,
which **includes the payload** — two spans naming different registered
elements must not collide, or retuning one element would leave a stale
matrix painted.

## DL.2 — mode skeleton + `listing.*` vocabulary ✅

**Landed 2026-08-14.**

**Depends on:** DL.1.

**Depends on:** DL.0, DL.1.

`DirectoryListingMode` (a **minor**) lives in the merged crate from
DL.0, alongside the two majors it activates on. `ActivationPolicy` names
`oil-mode` and `file-tree-mode`.

It also defines the shared **`ListingEntries`** buffer-local that both
majors write — path + is-dir per row, the data the icon and colour
lookup needs. This is the coupling that made placement matter:
activation alone needs no dependency (the policy names mode ids as
strings), but the entry data does, so the majors and the minor must
share one type. DL.0 is what gives that type an obvious home.

`on_activate` registers the `listing.*` elements through the
`ThemeRegistryHandle` service under
`ElementOwner::Mode("directory-listing-mode")`, defaults taken from
today's `ext_color` / `entry_visual` values. Follow
`multibuffer-mode`'s `on_activate` shape — including the "missing
service (test harness) just skips" tolerance.

The mode also contributes its buffers' display options: `number=off`,
`wrap=off`, `signcolumn=no`, `cursorline=on`. These are **options, not
kind checks** — that is what lets the shared path render a listing
without a `match buffer_kind`.

**Tests.** Unit: the policy names exactly the two majors; the mode is a
minor claiming no keymap; registration is idempotent by name; per
language colours differ out of the box; the contributed options are the
three display ones and not `ReadOnly`.

End-to-end (`lattice-ui-tui`,
`directory_listing_mode_activates_on_oil_and_the_file_tree`): the mode
is **active on a real oil buffer and a real file-tree buffer**.
Registering a mode and having it activate are different things and the
gap is silent — a policy naming the wrong major id compiles, registers
and never fires. Verified by deleting one major from the policy and
watching that arm fail. Both kinds in one test on purpose: CV.5 was
precisely the case where oil was checked and the tree, identically
broken, was not.

**Named `directory-listing-mode`** — it owns highlighting as well as
icons.

**As landed.** Defaults are **palette keys**, not the devicons literals:
`listing.file.rust` inherits `listing.file` and sets
`fg = palette("orange")`. Mapping each hardcoded RGB onto the nearest
role in the active palette is what makes the colours follow a
colourscheme at all — a literal would be theme-rooted in name only.

Two guesses in the first draft were wrong and worth recording, because
both fail *quietly*:

- palette keys — `peach` / `mauve` / `sky` / `overlay1` do not exist in
  `default_palette` (it is `orange` / `purple` / `cyan` / `overlay`).
  An unknown key resolves to the inherited parent instead of erroring,
  so every language rendered identically and nothing complained. The
  family-vs-one-language test is what caught it and is the real guard
  on this table.
- the options assertion sniffed `format!("{:?}")`, but
  `OptionOverrideSet`'s `Debug` is `finish_non_exhaustive` and prints
  only a length — so the assertion passed on nothing. It compares
  `TypeId`s now.

`ReadOnly` is deliberately **not** contributed: the tree is read-only
and oil is not, so it stays per-major.

## DL.3 — icons as leading virtual text

**Re-sliced into DL.3a / DL.3b after investigation** (2026-08-15). The
original slice bundled a generic substrate change with a mode-specific
producer; they have different blast radii and only one of them is
testable on its own.

### What the investigation settled

**The gutter is not the vehicle.** `Mode::gutter_decorations` exists and
is genuinely mode-contributed, which made it look like the cheaper
route. It is not: `GutterDecoration` is a closed two-variant enum
(`Diff`, `Severity`) where each variant *is* a fixed physical gutter
column. An entry icon has to sit immediately left of the name, in the
text flow — and this mode contributes `SignColumn::No`, so those columns
are not even reserved. Adding a third variant would put the icon in the
wrong place by construction. The design's inlay reasoning (§6) stands.

**Inlays cannot carry a style today**, which the original slice
suspected. `InlayHintRow` is `{ line, byte, text }`, and every inlay run
in the worker takes `inlay_fg()` — a hardcoded
`Color::Named(DarkGray)` — with `run.style` discarded at
`display_line_to_cell_row`.

**And a themed element for it already exists**: `inlay.hint` is
registered in `register_builtins` and captured as
`BuiltinElementIds::inlay_hint`. The worker has been bypassing it. So
DL.3a is not "add a feature to inlays" so much as "stop ignoring the
element the theme already publishes" — it retires a hardcoded colour,
which is a defect by the standing rule, and it makes LSP inlay hints
themeable as a side effect.

## DL.3a — inlay runs carry a style ✅

**Landed 2026-08-15. Depends on:** DL.1. **Generic substrate; no
listing code.**

- `Style::InlayHint` in `lattice-cells`, with a `syntax_element_id` arm
  to the existing `ids.inlay_hint`.
- `InlayHintRow` gains `style: Style`, defaulting to `Style::InlayHint`
  so LSP hints are unchanged in appearance but now resolve through the
  theme.
- Inlay runs resolve via `resolve_style` instead of the hardcoded
  `inlay_fg`; delete `inlay_hint_fg()`.
- Both the `DisplayMatrix` build and the `display_line_to_cell_row`
  projection carry the style — they are asserted byte-identical by the
  B1 parity test, so both move or the test fails.

**Tests.** `an_inlay_can_carry_its_own_theme_element` — an inlay naming
a mode-registered element does not fall back to the plain hint colour,
which is the capability DL.3b needs.
`single_inlay_splices_into_row_and_sets_flags` previously pinned the
literal `0x7f7f7f`; it now asserts against the resolved `inlay.hint`
element, with an `assert_ne!` against the old grey so it would fail on
the pre-fix path. **Pinning the literal is what let a hardcoded colour
sit past a themed element unnoticed** — the replacement assertion moves
with the palette.

**As landed.** Both peers, one patch: GPUI's `cells_paint` carried its
own copy of the same hardcoded grey, with a comment explaining it was
tracking the worker "until the dedicated `inlay_hint_style` theme slot
lands". That slot was `inlay.hint`, and it had already landed — so the
comment was describing a wait for something that already existed.

`inlay_hint_fg()` and the `inlay_fg` parameter are deleted (~27
references), which also drops two functions below the
`too_many_arguments` threshold.

## DL.3b — the mode publishes entry icons ✅

**Landed 2026-08-15. Depends on:** DL.2, DL.3a.

The mode turns `ListingEntries` into one leading inlay per row: the
glyph from `entry_visual`, and `Style::Element(id)` for the
`listing.*` element the path resolves to.

**The publish path, as built** — the shape the precedent predicted:

- `lattice_mode::PendingInlays` (+ `InlayRow`) — the generic mode→host
  channel, peer of `PendingSyntheticHighlights`. `store_and_wake` fires
  the waker at the seam, so a producer landing off the actor thread
  cannot forget it (`feedback_async_needs_wake`).
- `ExtraInlays` buffer-local in the host, drained on the same tick as
  the highlights.
- `build_inlay_hints_for_buffer` merges it with the LSP hints into one
  list, sorted by `(line, byte)`. **The merge reads `ExtraInlays`
  first, before the LSP gates** — those gates return early on "no LSP
  hints for this buffer", which would otherwise drop every icon in a
  listing pane, where LSP is never involved.
- `listing_element_for(path, is_dir)` + `listing_inlays(entries, reg,
  nerd_fonts)` in the mode — pure functions, testable with no buffer,
  editor, or frame.

`listing_element_for` is what replaces `ext_color`'s runtime lookup:
`ext_color` answers "what RGB is a `.rs` file" and a theme cannot touch
the answer; this answers "which registered element is it", and the theme
owns what that element looks like. Buckets are language *families*, not
extensions, because the vocabulary a theme is asked to retune should be
the one a person thinks in.

Icons anchor at `byte: 0` — leading, so oil's rope stays bare filenames
(design §6): the glyph renders, the buffer never contains it, and `:w`
still diffs clean.

Do **not** shortcut this by having the host derive icons from
`ListingEntries` directly. It would work and it would put
listing-specific knowledge in the host, which is the rule this whole
plan exists to uphold.

Still invisible: the bespoke paint paths do not read inlays, so nothing
changes on screen until DL.4/DL.5.

## DL.4 — file tree onto the shared path ✅

**Landed 2026-08-15. Depends on:** DL.3b. **First visible slice.**

- `BufferData::FileTree(FileTreeBuffer)` → `FileTree(DocumentEntry)`;
  kind discriminator stays.
- Delete `FileTreeBuffer::{cursor, scroll}` (already dead).
- Stop baking glyphs into the rope — the rope becomes entry names, and
  the glyph comes from DL.3. This is the slice's main behavioural
  change and its main risk: entry-name parsing, `<CR>` follow, and
  expansion state all key off the rope today.
- Delete `draw_file_tree_pane` (TUI) and its GPUI twin, and the
  `PaneRenderProvider.render` entry. The `status` entry stays — that is
  legitimately mode-owned.

**Tests.** CV.5's invariant still passes, but its assertion had to
change shape — see below. `rope_holds_names_not_glyphs` and
`listing_entries_anchor_icons_after_indent_and_marker` replace the two
tests that asserted the glyph *was* in the rope.
`set_ui_nerd_fonts_reissues_open_file_tree_icons` guards the same
regression as before (flip the option, an open tree must follow) on the
surface that now carries it.

### The bug this uncovered

`contains_document` was a **hand-maintained list of variants** that had
drifted from `document()` — it predated `Multibuffer` joining that
method and then silently excluded `FileTree`. So `activate_document`
refused a buffer whose handle the registry would happily hand out: the
tree opened, the shared path painted *the buffer behind it*, and the
caret sat on row 0. It is derived from `document()` now, with `Help`
named as the one deliberate exception (document-backed since PU.1a, but
activated through the popup / in-pane path).

That is the "aligned-by-silence" failure the standing rule warns about,
in the registry rather than the renderer.

### CV.5's assertion had to stop pinning a mechanism

It asserted the cursor row was **reverse-videoed**. That is how the
bespoke painters marked it; the shared path tints the cursorline
instead, so the assertion would have had to be rewritten per path and
would only ever have guarded one of them.

It now asserts what the report was actually about: **the caret sits on
the row showing the cursor's own entry**, matched by text. That holds
on both paths and needs no change when oil converges in DL.5. The test
also had to start settling — the cursorline comes from the minor's
`CursorLine` contribution, and mode activation is async, so an
unsettled frame was measuring the gap rather than the invariant.

### Also landed

- `activate_file_tree` is a delegation to `activate_document` — a tree
  **is** a document now. The hand-rolled body it replaces stashed and
  restored the tree's own cursor / scroll fields, archival duplicates
  the hot path never read; they are deleted.
- `PaneRenderProvider.render` became `Option`. `None` means "paint me
  through the shared document path" and is the goal state, not a
  special case: the file tree is `None`, help and oil still supply
  painters, and oil's goes in DL.5.
- The mode contributes `CursorLine = true`. A listing's selected row
  has to be visible, and on the shared path that is an option any
  buffer can carry rather than a kind the renderer knows about.

## DL.5 — oil onto the shared path ✅

**Landed 2026-08-15. Depends on:** DL.4. **The risky one.**

Same migration, plus `OilBuffer::snapshot` → buffer-local beside
`OilDir`, and `is_dirty()` / the `:w` rename derivation reading it from
there.

The rope is already bare filenames and **must stay that way** — the
whole reason icons are virtual (design §6). The regression to fear is a
`:w` that derives wrong renames because something leaked into the rope.

Delete `draw_oil_pane` and its GPUI twin, and with them the
`let icon_width = 2;` cursor fudge in both peers.

**Tests.** `oil_write_round_trips_rename_create_and_delete` asserts
against the **filesystem**, with the listing mode settled so icons are
live. It also pins that every rope line is a bare filename — if that
ever fails, `:w` is about to treat a decoration as a filename, which is
the one way this plan could lose user data.

Written as two writes rather than one: oil's rename heuristic is
documented as *exactly* one delete plus one create, so bundling a
rename with an unrelated delete is a delete-and-create by contract. The
first draft bundled them and caught itself — the rename produced an
empty file, which is correct behaviour for the input and the wrong
test.

**As landed.**

- `OilBuffer` is gone. What survives is `OilSnapshot` — just the
  entries `:w` diffs against — in an `OilSnapshotLocal` buffer-local
  beside `OilDir`. Its rope moved to the Document; its `cursor` /
  `scroll` were archival duplicates whose own doc comment said reading
  them was unsafe.
- **`apply_edit_to_oil` deleted.** Oil is a *writable* Document, so its
  edits take the ordinary path — the bespoke rope-mutation detour and
  the `if matches!(active_buffer, Oil)` branches in front of it are
  gone. That is the clearest evidence the convergence was the right
  shape: the special case did not need replacing, it needed removing.
- `activate_oil` is a delegation to `activate_document`, like
  `activate_file_tree`.
- `write_oil_listing` is the single chokepoint that writes the text and
  stores the snapshot it was rendered from. They move together because
  `:w` diffs one against the other, and publishing them separately is
  how a stale snapshot would silently derive the wrong renames.
- `register_listing_document` generalised back to taking a
  `ListingKind`, now that both kinds are `DocumentEntry`.

## DL.6 — retire the old palette; benches + parity 🚧

**Partly landed 2026-08-15.** Depends on DL.5.

### Done

- **The parity audit passes.** No `draw_oil_pane` /
  `draw_file_tree_pane` / `build_oil_inner` / `build_file_tree_inner`
  remain in either renderer. Both listing kinds paint through the
  shared compose path, and the acid test from the standing rules holds:
  a new listing-shaped provider needs **zero** renderer additions.
- **`ext_color` has no consumers.** `listing_inlays` was still calling
  `entry_visual` and discarding the colour it returned; it calls
  `glyph_for_entry` now, so entry colour comes exclusively from the
  registered `listing.*` theme elements.
- **`lattice-ui-tui/src/icons.rs` deleted** — its only production
  callers were the two painters DL.4/DL.5 removed.
- The orphans those deletions left in GPUI (`VIEWPORT_ROWS_FALLBACK`,
  `icon_color_to_rgb`, `entry_fg`, and the two tests covering deleted
  code) are gone.

### Deliberately not done, with reasons

**`lattice_core::ui::icons` did NOT move into the listing crate**, and
the plan was wrong to say it should. That was written from an audit of
`entry_visual` / `icon_for_entry`, which missed `glyph_for_entry` —
**`lattice-multibuffer` uses it** for excerpt-header file icons. The
glyph table is genuinely shared infrastructure; moving it would make
multibuffer depend on the listing crate. It stays in `lattice-core`.

**`IconColor` / `ext_color` are dead-but-present.** They are only still
reachable because the glyph and the colour come out of the same ~200-arm
match, and splitting that is churn with no user-visible win and real
transcription risk. `entry_visual` now carries a doc note saying so, and
pointing new callers at `glyph_for_entry` plus a theme element.

**DL.0's convergence debt is still open** — the two directory readers,
the two entries→rope renderers, and the two entry models. `ListingEntry`
(DL.2) is the shape they would converge onto. Left as its own slice
rather than folded in here: it touches `:w`'s input on the oil side, and
this slice was already carrying the palette retirement.

**Benches are not written.** The four-artefact rule asks for them and
this slice owes them: first-paint and per-frame scroll cost on a
~5k-entry directory, to prove the per-row `ResolvedTheme::get` did not
regress the frame rather than assume it. Recorded as owed rather than
quietly skipped — the listings moved from an `O(viewport)` bespoke walk
to the shared cells/`DisplayMatrix` build, which is exactly the kind of
change that should not be taken on trust.

## Risks

**Oil `:w` is the one that can lose user data.** Everything else
degrades to a visual defect. DL.5 gets filesystem-level round-trip
tests, not just rope assertions.

**The file tree's rope changing meaning** (glyphs out) touches `<CR>`
follow and expansion, which parse it. DL.4 is where that bites.

**GPUI has no frame-level test harness.** Both peers' paths are deleted
in DL.4/DL.5, so the TUI tests guard the shared path both now use; the
GPUI side is verified by inspection plus
`cargo build -p lattice-ui-gpui --features window`. Note this in each
commit rather than implying symmetric coverage.

**Do not let DL.1 get entangled.** If the rest of this plan stalls,
`Style::Element` still stands on its own as the substrate gap it
documents.
