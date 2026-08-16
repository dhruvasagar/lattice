# `indent-guides` — slice plan (IG.0–IG.6)

> Sequencing for [`docs/dev/architecture/indent-guides.md`](../../architecture/indent-guides.md).
> That fragment owns the *what* and *why*; this file owns the *when* and *in
> what order*. Opened 2026-08-16.

## Status

| Slice | Title | Status |
|---|---|---|
| IG.0 | Design fragment + this plan | ✅ |
| IG.1 | `lattice-core::indent_blocks` — the shared block walk | ✅ |
| IG.2 | `IndentGuides` substrate; per-pane publish; options; theme | ✅ |
| IG.3 | TUI paint pre-pass | 📝 |
| IG.4 | GPUI quad pass | 📝 |
| IG.5 | Migrate `compute_indent_folds` onto the shared walk | 📝 |
| IG.6 | Bench, ledger, user docs, mode opt-outs | 📝 |

## Shape of the sequence

**IG.1 lands the primitive with no consumer.** The block walk is where every
subtle case lives — blank runs, closer inclusion, multi-level jumps, tabs — and
it is pure (`&[Option<u16>]` in, `Vec<IndentBlock>` out). Testing it before any
renderer exists means the cases are argued against a unit test rather than
against a screenshot. It ships alone so a later behavioural surprise bisects to
one commit with no rendering in it.

**IG.2 is the whole architecture; IG.3 and IG.4 are the two output targets.**
Once the layer is published per pane, each renderer is a small localised pass.
Splitting them is what keeps the cross-renderer lockstep rule checkable: IG.4's
diff is the parity audit for IG.3, and a reviewer can see at a glance that both
consume the same `IndentGuides` and neither re-derives the paint predicate.

**IG.3 before IG.4 because the TUI is the harder target.** It substitutes
characters into a span vector that overlay positioning later indexes by column;
if the marks are wrong in display-column space, the TUI shows it immediately.
The GPU peer paints quads that disturb nothing, so it cannot surface a
coordinate bug the same way.

**IG.5 changes fold behaviour and is therefore last and separable.** Migrating
`compute_indent_folds` onto the shared walk is what makes the design's
shared-extent guarantee literally true — today `foldmethod=indent` counts a tab
as one column, so `zc` and the active guide disagree on tab-indented files. It
is a real fix to a documented TODO (`folds.rs`, `leading_indent`), but it is a
behaviour change to a landed feature and belongs in its own commit with its own
fold tests, revertable without touching guides.

**IG.6 closes the four-artefact requirement.** Guides work after IG.4; they are
not *finished* until the bench exists, the ledger records it, and the modes that
should not have guides have said so.

## Slices

### IG.0 — design + plan ✅

`docs/dev/architecture/indent-guides.md` and this file. No code.

### IG.1 — `lattice-core::indent_blocks` ✅

New module `crates/lattice-core/src/indent_blocks.rs`. Own file rather than an
addition to `indent.rs`: `IndentUnit` answers "how wide is one level", this
answers "what are the blocks", and the second is ~150 lines with a dense test
body.

```rust
pub struct IndentBlock { pub col: u16, pub start_line: u32, pub end_line: u32 }
pub fn indent_blocks(depths: &[Option<u16>], step: u16) -> Vec<IndentBlock>;
pub fn depths_of<'a>(lines: impl Iterator<Item = &'a str>, unit: &IndentUnit) -> Vec<Option<u16>>;
pub fn is_closer_line(line: &str) -> bool;
```

The walk, from `compute_indent_folds` and generalised: open at `i` when the next
non-blank has greater depth; extend while deeper, blanks transparent; swallow a
trailing closer line at the opener's depth. Then emit one `IndentBlock` per grid
column in `opener_depth .. body_depth` stepping by `step`.

`MAX_BLOCKS` guard, mirroring the existing `MAX_FOLDS = 5000`, so a
pathological monotonically-indenting file cannot make the walk quadratic.

Tests: flat file; single block; nesting; interior blank; blank before closer;
blank between blocks; closer inclusion and its negative (`} else {` is not a
closer); multi-level jump; tab-indented file at `tabstop=8`; `step` not dividing
the indent evenly; empty file; all-blank file; the `MAX_BLOCKS` cap.

Tests for the *predicate* live here too, as a `paints_on(block, row, depth)`
helper exercised against the design fragment's worked picture — the producer
owns the rule, so the rule's tests belong beside it.

### IG.2 — substrate + publish + surface ✅

- `lattice-host`: `GuideMark`, `IndentGuides`, `build_indent_guides`
  (`src/indent_guides.rs`) — **not** `lattice-cells`, which is deliberately
  near-dep-free and would have needed a new `lattice-core` edge to hold
  `IndentBlock`. See the design fragment's IG.2 amendment.
- `lattice-config`: `display.indent-guides`, `display.indent-guides.char`,
  `display.indent-guides.active`.
- `lattice-theme`: `indent.guide`, `indent.guide.active` element ids +
  defaults in the bundled themes.
- `lattice-host`: `CellsRenderState::pane_indent_guides` (`PaneId →
  Arc<ArcSwap<IndentGuides>>`) + `indent_guides_for_pane`, populated by
  `cells_worker` in the pass that builds the pane's `DisplayMatrix`.
- **The one new invalidation edge**: `tabstop` turned out to live on the
  `whitespace` axis, which renderers gate *painting* on — so `shiftwidth` did
  not go there. `MatrixVersion` gained an `indent` axis instead, gating the
  rebuild only. See the design fragment.
- **`resolved_option_opt`**: the publish path cannot use the panicking
  `resolved_option`. Reading a buffer-local option in `publish_render_state`
  aborts every test whose minimal config never registered it — the hazard the
  `wrap_reserved_cols` comment already names. The degrading accessor is for
  publish-path reads; command-path callers keep the loud one.

Tests: guides published with a version stamp equal to the matrix built beside
them; option off ⇒ empty layer; `shiftwidth` change ⇒ rebuild without a
keystroke (per the inbound-primitive rule: assert visibility with no further
action dispatched); covered-window indexing at a non-zero `covered_start`;
every published mark lands on a blank column (the invariant, asserted against
the built `DisplayLine`).

### IG.3 — TUI 📝

`crates/lattice-ui-tui/src/render.rs`: `apply_indent_guides(spans, marks,
active, glyph, styles)` in the `apply_whitespace_decoration` position, before
`clip_spans_horizontally`. Padding past EOL for blank rows; background
preserved.

Tests: glyph at the expected columns for a nested fixture; never over text
(assert the character replaced was blank); blank-line pass-through; active
column resolves to the active style and its siblings do not; `leftcol > 0`
clipping; guides absent when the matrix is stale (no panic, no misplacement);
`:set nolist` and `:set list` both correct at guide columns.

### IG.4 — GPUI 📝

`crates/lattice-ui-gpui/src/editor_element.rs`: quad pass after the body glyph
emission, reusing the existing `col → x` formula. Widths 1px / 2px.

Tests: the shared resolution (columns + active index) asserted at the same layer
IG.3 asserts, so the peers are provably reading one source. Parity audit:
`grep -rn "IndentGuides\|GuideMark" crates/lattice-ui-gpui/ --include="*.rs"`
must be non-empty.

### IG.5 — fold migration 📝

`compute_indent_folds` calls `indent_blocks`, with depths from
`IndentUnit::columns_of` instead of `leading_indent`. Delete `leading_indent`
and the local `is_closer_line` / `next_non_blank_line` helpers superseded by the
shared walk. Update the `folds.rs` doc comment that promises this refinement.

Tests: existing fold tests stay green for space-indented fixtures; new tests
cover a tab-indented file folding at the same boundaries as its space-indented
twin, which is the bug being fixed.

### IG.6 — bench, ledger, docs, opt-outs 📝

- `crates/lattice-host/benches/indent_guides.rs`: block walk over 1k and 10k
  covered lines; per-frame renderer resolution for a 120-row viewport.
- Results into `docs/dev/operations/benchmarks.md`.
- User documentation for the three options and the theme elements.
- `Mode::options()` opt-outs for the buffers where guides are noise. Structural
  exclusion already covers non-Document panes (no `DisplayMatrix`, no layer);
  this is for document-backed modes that do not want them.
- `docs/dev/operations/implementation.md` ledger row.
