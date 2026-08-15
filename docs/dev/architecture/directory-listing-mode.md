# `directory-listing-mode` — one minor mode owning entry icons and highlighting

> **Status: design, not yet implemented.** Sequencing lives in the slice
> plan (`docs/dev/operations/slice-plans/archive/cursor-visibility.md`, CV.6).
>
> Opened 2026-08-13 out of CV.5, where the same off-by-`scroll` bug was
> found in four hand-written paint paths — oil and the file tree, in each
> renderer.

## 1. What this replaces

Oil and the file tree are the last two content kinds that are **not**
`Document`-backed:

```rust
enum BufferData {
    Document(DocumentEntry),
    Help(DocumentEntry),        // ← converged at PU.1a
    Messages(DocumentEntry),
    Multibuffer(DocumentEntry),
    Dashboard(DocumentEntry),
    FileTree(FileTreeBuffer),   // ← own struct, own `content: Buffer`
    Oil(OilBuffer),             // ← own struct, own `content: Buffer`
    Terminal(TerminalBuffer),   // out of scope, see §7
}
```

`document_handle()` returns `None` for the last two, so the generic pane
path *cannot* render them. Four bespoke paint functions exist to fill
that gap: `draw_oil_pane` / `draw_file_tree_pane` in `lattice-ui-tui`,
and their twins in `lattice-ui-gpui`.

They are the cause, not a symptom. Because each re-implements the
scroll window, the cursor row and the per-row styling, one arithmetic
slip lived in four places (CV.5), and the panes silently lack everything
the shared path provides: cursorline, wrap, folds, virtual rows, the
gutter, hlsearch, visual selection, diff signs.

**This is not a renderer problem and must not be fixed in the
renderers.** The fix is to converge the storage — the same migration
`Help` already took at PU.1a — and then delete all four paint paths.

## 2. Why a minor mode

Oil and the file tree need identical *presentation*: a per-row icon
glyph and a per-row colour keyed on what the row points at. They differ
in *behaviour* (oil is editable and diffs its rope on `:w`; the tree is
read-only and expands directories), so they stay separate majors.

Shared presentation across two majors is a **minor mode** — never the
same contribution declared twice. `magit-core-mode` is the in-repo
precedent, and CV.5 is the cost of not having done it here: the report
named only oil, and the file tree carried the identical defect with
nothing to announce it. See `prefer-minor-modes-over-duplication`.

### Where it lives — one crate for both views

`lattice-oil` and `lattice-file-tree` **merge into a single crate**, and
the minor lives there with them.

The two are the same domain seen twice: a flat editable listing of one
directory, and a hierarchical read-only tree. That is not an analogy —
it is visible in the code:

- both carry near-identical directory readers (`read_dir_entries` /
  `initial_entries`) and entries→rope renderers (`render_to_buffer`),
  which DL.4/DL.5 rewrite in both;
- their entry models are one concept in two shapes —
  `OilEntry { name, is_dir }` and
  `FileTreeEntry { path, depth, kind }` — which is precisely the
  `ListingEntries` the minor needs, already written twice;
- **no consumer takes one without the other.** The only external
  dependents are `lattice-host`, `lattice-ui-tui` and
  `lattice-ui-gpui`, and all three depend on both.

So the split was buying no modularity anyone used, while forcing the
duplication this whole design exists to remove. Merged, they are ~1,240
lines across the two existing module trees.

**Why the alternatives lose.** Two placements were considered and both
are worse:

- *`lattice-oil` owns the minor, `lattice-file-tree` depends on it* —
  the read-only tree would pull in oil's `:w` rename derivation and
  disk-mutation machinery to obtain an icon vocabulary, and any future
  listing-shaped provider would inherit that edge.
- *A third crate depended on by both* — symmetric, but it leaves the
  duplicated readers and renderers in place on either side of a crate
  boundary, which is the actual problem.

Note the coupling is real either way and was missed at first: activation
alone needs no dependency (the policy names `oil-mode` /
`file-tree-mode` as id strings), but the **entry data** does — the mode
must read path + is-dir per row, so the majors and the minor must share
one type. Merging is what makes that type have an obvious home.

**The icon table comes too.** `entry_visual` / `icon_for_entry` have
exactly two production callers in each renderer — the oil pane and the
file-tree pane — and none anywhere else (no picker, no dashboard). Once
DL.4/DL.5 delete those four paint paths the table has no consumer, so
`lattice_core::ui::icons` and `lattice-ui-tui/src/icons.rs` move into
the merged crate. They were never shared infrastructure; they only ever
served these two listings, and `lattice-core` sheds a domain table it
should not have been carrying.

`directory-listing-mode` is a minor activated on both majors. It owns:

- the theme-element vocabulary for entry presentation (§4),
- the per-row spans and icons published for its buffers (§5),
- the display options those buffers need (`number=off`, `wrap=off`,
  `signcolumn=no`, `cursorline=on`),
- its own tests.

It owns no keymap. `<CR>` means "open" in the tree and "nothing" in
oil; entry *navigation* is major-owned. Per "modes own their full
surface", a mode owning no chords is fine — what it must not do is own
half of something.

## 3. Storage: apply PU.1a to the last two kinds

`BufferData::Oil` and `BufferData::FileTree` become `DocumentEntry`,
exactly as `Help` did:

- The listing text becomes an actor-backed synthetic Document.
- The kind discriminator **stays** — `:ls`, mode lookup, and
  `BufferKind::is_read_only` still need to tell a listing from a file,
  and oil is writable while the tree is not.
- `OilBuffer::{cursor, scroll}` and `FileTreeBuffer::{cursor, scroll}`
  are deleted. They are already dead: *"carries its own `cursor` field
  as a vestige but reading it is unsafe (it's not synced to the App's
  hot-path cursor)"*.
- Oil's `snapshot: Vec<OilEntry>` moves to a buffer-local, joining
  `OilDir`. The file tree already keeps its entries in the
  `FileTreeEntries` local, so this makes the two symmetric.

`is_dirty()` (rope vs. snapshot render, the basis of `:w`) keeps working
unchanged — it just reads the snapshot from the local.

## 4. Theme rooting: elements, not enum variants

Entry colour today is `lattice_core::ui::icons::ext_color` — about
fifty hardcoded values (`"rs" => Rgb(0xDEA584)`, `"py" => Rgb(0xFFBC03)`,
…). A theme cannot touch any of it.

That vocabulary is open-ended and language-shaped, so it cannot become
`Style` enum variants the way `Style::MagitSha` and `Style::HelpKey`
did — those are small closed sets. Fifty variants in a core enum to
express "the icon colour for Python" is the wrong shape, and it is
unreachable for a future WASM plugin that wants to add its own.

Instead the mode **registers theme elements**, which the theme system
already supports:

```rust
// lattice-theme: ElementOwner
Core,
Mode(Cow<'static, str>),    // ← "Core elements ship with the editor;
Plugin(Cow<'static, str>),  //    modes/plugins register their own."
```

`multibuffer-mode`, `compilation-mode` and `dashboard-mode` all already
register elements this way, in `on_activate`, via the
`ThemeRegistryHandle` service. Registration is idempotent by name, so
re-activation is free.

`directory-listing-mode` registers, with today's devicons colours as the
**defaults**:

| element | default | applies to |
|---|---|---|
| `listing.dir` | blue | directory rows |
| `listing.file` | default fg | rows with no more specific match |
| `listing.hidden` | dim | dotfiles |
| `listing.file.rust` | `#DEA584` | `.rs` |
| `listing.file.python` | `#FFBC03` | `.py`, `.pyw`, `.pyi` |
| … | … | one per language/family already in `ext_color` |

Element names are dotted, and `ElementName::parent()` already walks the
hierarchy — so a theme can retune `listing.file` and have every language
that does not override inherit it, or pin one language. That is the
whole point of rooting this in the theme system: **the palette becomes
data a theme owns, not a table in `lattice-core`.**

`ext_color`'s table survives as the mode's default-registration source.
It stops being the runtime lookup.

## 5. The span → element gap (the one thing that does not exist yet)

A `StyledSpan` carries `Style`, a closed enum, and
`syntax_element_id(&BuiltinElementIds, Style)` bridges it to a
**builtin** element. There is no way for a span to reference a
*mode-registered* element — which is precisely what §4 needs.

This is a real gap in the substrate, not a detail of this feature. Any
mode or plugin that wants its own themed span vocabulary hits it.

Proposed: let a span name an element directly.

```rust
pub enum Style {
    Default, Comment, /* … existing closed set … */
    /// A span styled by a registered theme element — the escape hatch
    /// for mode- and plugin-owned vocabularies that cannot be enum
    /// variants. `syntax_element_id` returns it unchanged.
    Element(ElementId),
}
```

`ElementId` is a `u32` newtype, so `Style` stays `Copy` and its size is
unchanged in practice. `syntax_element_id` gains one arm. Every
existing producer and consumer is untouched.

**Paramount goal #2 (extensibility) is the argument.** Plugins ship as
WASM components in any language; such a plugin can register a theme
element by name, and can never add a variant to a Rust enum. If span
styling is reachable only through a closed enum, themed plugin
highlighting is impossible by construction. This slice is where that
becomes concrete, but the gap is general.

Rejected: a parallel "explicit colour span" axis. It would let a
producer bypass the theme entirely — the opposite of what was asked —
and `span-layering.md`'s two-axis contract (fg by style, bg by refine)
exists so producers do not each invent an axis.

## 6. Icons: leading virtual text, not rope content

The file tree bakes its glyph into its rope today; **oil cannot**. Oil's
rope is bare filenames because `:w` diffs it against the entry snapshot
to derive renames — a glyph in the rope corrupts the filename.

So the icon is *virtual*: per-row leading text that renders but is not
in the buffer. That mechanism already exists and is already generic —
LSP inlay hints, spliced via `splice_virtual_text_into_spans` with
`InlayOffset` carrying the byte↔column remap.

Both kinds move to it, which makes them symmetric and retires two
hacks:

- the file tree stops baking glyphs into its rope, so its text is the
  entry names — searchable, yankable, and not a rendering artefact;
- oil's hand-rolled cursor offset (`let icon_width = 2;` in both
  renderers) disappears, because inlay remap is exactly what computes
  the cursor's display column.

Icon glyph selection stays `lattice_core::ui::icons::entry_visual`,
which both renderers already share. Only the *colour* moves to the
theme (§4).

## 7. Scope

**In:** oil, file tree, the four bespoke paint paths, the storage
migration, the theme vocabulary, the span→element gap.

**Out — `BufferKind::Terminal`.** It is also a non-`DocumentEntry`
variant with its own paint path, but for a different reason: it is a
PTY-backed cell grid, not rope-backed text, and its renderer paints
alacritty's grid. It is not "everything is a buffer" debt of the same
kind and does not belong in this slice. Recorded so its exclusion is
deliberate rather than forgotten.

## 8. What this buys

Once the four paint paths are gone, oil and the file tree get — with no
per-kind code — cursorline, soft wrap, folds, virtual rows, the gutter
and line numbers (off by option, not by kind), hlsearch, visual
selection, diff signs, and the shared scroll model that CV.1 fixed.

The acid test from the standing rules: a new listing-shaped provider
should need **zero** renderer additions. After this, it needs none.

## 9. Test contract

- `crates/lattice-host/tests/multibuffer_is_a_regular_buffer.rs` is the
  K.4 model. A sibling asserting the same for oil and the file tree —
  every chord, verbatim, or a documented divergence per chord.
- The CV.5 invariant (`pane_listing_cursor_highlight_and_caret_share_a_row_when_scrolled`)
  must keep passing across the migration. It is renderer-level and
  kind-agnostic, so it should survive the deletion of the paths it was
  written against — that is the point of having asserted on the painted
  frame rather than on `editor.scroll`.
- Theme override: registering a theme that retunes `listing.file.rust`
  changes the painted colour, and retuning the `listing.file` parent
  changes every language that does not override.
- Oil round-trip: `:w` still derives the right renames after the icon
  becomes virtual text — the guard that the rope stayed bare.

## 10. Benchmarks

Per the four-artefact rule. The listing paths move from a bespoke
`O(viewport)` walk to the shared cells/`DisplayMatrix` build, so the
per-frame cost changes shape and must be visible:

- open a large directory (~5k entries) and record first-paint;
- scroll a full viewport and record per-frame cost.

Both go in `docs/dev/operations/benchmarks.md`. Element resolution is a
`ResolvedTheme::get` (indexed read) per row, so the per-row cost should
not regress; the bench exists to prove it rather than assume it.
