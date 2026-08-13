# `directory-listing-mode` — slice plan (CV.6)

> Sequencing for `docs/dev/architecture/directory-listing-mode.md`.
> That fragment owns the *what* and *why*; this file owns the *when* and
> *in what order*. Opened 2026-08-13 out of CV.5
> (`cursor-visibility.md`).

## Status

| Slice | Title | Status |
|---|---|---|
| DL.1 | `Style::Element(ElementId)` — spans can name a registered element | ✅ |
| DL.2 | `directory-listing-mode` skeleton + `listing.*` theme vocabulary | 📝 |
| DL.3 | Entry icons as leading virtual text | 📝 |
| DL.4 | File tree → `DocumentEntry`, both bespoke renderers deleted | 📝 |
| DL.5 | Oil → `DocumentEntry`, both bespoke renderers deleted | 📝 |
| DL.6 | Retire `ext_color`'s runtime lookup; benches + parity audit | 📝 |

## Shape of the sequence

DL.1–DL.3 are **invisible**: they add substrate and a mode that nothing
consumes yet. Nothing about the painted frame changes, which is what
makes them safe to land ahead of the risky part.

DL.4 and DL.5 are each **atomic per kind**. A buffer cannot be
half-rendered, so one kind's storage migration, its switch to the shared
compose path, and the wiring of its icons/spans all land together —
otherwise there is a commit where that pane paints without icons, which
is a visible regression and a bad bisect point. They are separated from
each other because the tree is read-only and oil is writable: oil's `:w`
rename derivation is the real risk in this plan and deserves its own
commit and its own bisect slot.

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

## DL.2 — mode skeleton + `listing.*` vocabulary 📝

**Depends on:** DL.1.

**New crate `lattice-directory-listing`** owning `DirectoryListingMode`
(a **minor**), with an `ActivationPolicy` naming `oil-mode` and
`file-tree-mode` as id strings — so it depends on neither major and
neither major depends on it. Deps: `lattice-mode`, `lattice-core`,
`lattice-theme`, `lattice-cells`. Registered via
`register_directory_listing_modes(&mut registry)` from
`lattice-host/src/modes.rs` + `editor_boot.rs`, beside
`register_oil_modes` / `register_file_tree_modes`.

See the design fragment's "Where it lives" for why not either major's
crate (they are siblings) and why not `lattice-mode` (substrate must not
carry domain vocabulary).

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

**Tests.** Activation on both majors and on neither other; every
element registered and idempotent across re-activation; a theme
override of `listing.file` reaching a language that does not override,
and `listing.file.rust` winning where it does.

**Naming is open** — `directory-listing-mode` vs `file-icons-mode`.
Decide at implementation; the mode owns highlighting as well as icons,
which argues for the former.

## DL.3 — icons as leading virtual text 📝

**Depends on:** DL.2.

The mode publishes a per-row leading inlay carrying the glyph from
`entry_visual` and a `Style::Element` for its colour. Uses the existing
inlay mechanism (`InlayOffset` + `splice_virtual_text_into_spans`), the
same one LSP hints ride.

Confirm at implementation whether an inlay can carry a per-inlay style
today or resolves through a single `inlay_hint_fg()`. If the latter,
extending it to carry a style is part of this slice — and is the same
generalisation as DL.1, one axis over.

Still invisible: the bespoke paths do not read inlays, so nothing
changes on screen until DL.4/DL.5.

## DL.4 — file tree onto the shared path 📝

**Depends on:** DL.3. **First visible slice.**

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

**Tests.** CV.5's invariant must still pass. A tree-shaped sibling of
`multibuffer_is_a_regular_buffer.rs`. `<CR>` follow and directory
expansion against a rope that no longer contains glyphs.

## DL.5 — oil onto the shared path 📝

**Depends on:** DL.4. **The risky one.**

Same migration, plus `OilBuffer::snapshot` → buffer-local beside
`OilDir`, and `is_dirty()` / the `:w` rename derivation reading it from
there.

The rope is already bare filenames and **must stay that way** — the
whole reason icons are virtual (design §6). The regression to fear is a
`:w` that derives wrong renames because something leaked into the rope.

Delete `draw_oil_pane` and its GPUI twin, and with them the
`let icon_width = 2;` cursor fudge in both peers.

**Tests.** `:w` round-trip: rename, delete, create, and a no-op write,
each asserted against the filesystem, with icons active. Plus the oil
sibling of the regular-buffer audit, and CV.5's invariant.

## DL.6 — retire the old palette; benches + parity 📝

**Depends on:** DL.5.

- `ext_color` **moves into the mode crate outright**, along with the
  rest of `lattice_core::ui::icons` and `lattice-ui-tui/src/icons.rs`.
  Confirmed rather than assumed: those have exactly two production
  callers per renderer — the oil pane and the file-tree pane — and none
  elsewhere, so DL.4/DL.5 leave them with no consumer. `lattice-core`
  sheds a domain table; the TUI's icon module goes away entirely.
- Benches per the four-artefact rule: first-paint and per-frame scroll
  cost on a ~5k-entry directory, into
  `docs/dev/operations/benchmarks.md`. The point is to prove the
  per-row `ResolvedTheme::get` did not regress the frame, not to assume
  it.
- Parity audit:
  `grep -rn "draw_oil_pane\|draw_file_tree_pane" crates/` must come back
  empty in both renderers, and the acid test from the standing rules —
  a new listing-shaped provider needing **zero** renderer additions —
  should now hold.

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
