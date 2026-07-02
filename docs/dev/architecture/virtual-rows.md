# Virtual Rows

Authoritative design for Lattice's displacing virtual-row
primitive: a sibling lane to [`CellMatrix`](cell-grid-renderer.md)
that lets the renderer interleave non-rope rows (diff deletion
blocks, multibuffer excerpt headers, code-lens summaries,
multi-line inlay hints, signature-help previews) into the
display stream **without** mutating the cell-builder's matrix
or breaking the vim grammar's source-line semantics.

This document is a *companion* to `design.md` (§5.6 rendering,
§5.6.8 render-snapshot coherence) and to
[`cell-grid-renderer.md`](cell-grid-renderer.md) (the matrix
contract). It specifies the §5.15 subsystem that
[`diff-system.md`](diff-system.md) and
[`multibuffer-views.md`](multibuffer-views.md) reference as
their shared "displacing virtual row" primitive.

## 1. The design goal

Two consumer flows need rows that paint inline between
document rows:

- **Diff deletion blocks** (D.3 in `diff-system.md`). When the
  user edits `foo.rs` and removes lines 12-14, the inline
  overlay shows those removed lines as a virtual block above
  the line that now follows (was line 15, now line 12). The
  removed cells aren't in the rope; they're decoration from
  the diff session's baseline.
- **Multibuffer excerpt headers and separators** (M.2 in
  `multibuffer-views.md`). A multibuffer view of N file
  excerpts paints a one-line header before each excerpt
  ("`src/app.rs : 102–118`") and a separator rule between
  them. Neither header nor separator is in any source rope;
  they're presentation chrome.

Both consumers, plus the post-v1 inlay-hint / code-lens /
signature-preview flows, share the same shape:

- A row of [`Cell`](../../../crates/lattice-cells/src/cell.rs)s
  to paint.
- An anchor to a source line.
- A position (above or below the anchor) defining the paint
  order.
- A height (typically 1, reserved >1 for multi-line blocks).

The primitive must satisfy three invariants:

> Everything is a buffer. -- `CLAUDE.md`, `design.md` §5.9.8

> Buffers must not have kind-specific logic — never branch
> render / motion / scroll on `BufferKind`. -- saved feedback
> `feedback_buffers_no_special_case.md`

> Sub-frame input latency: keystroke -> glyph within the
> one-frame ceiling (<= 8.3 ms at 120Hz). -- Paramount goal #1

The first two say virtual rows must NOT change the document /
buffer model — they ride alongside, not inside. The third says
the interleaver must be sub-µs per frame, even at large
virtual-row counts.

## 2. Why not bake them into CellMatrix

The cell-builder's `CellMatrix` is a multi-axis cache
(`text + syntax + inlay_hints + folds + theme + whitespace`)
RCU-published via `ArcSwap` (§5.6.8). Baking virtual rows in
would mean:

- Every virtual-row registration (a diff session edits, a
  multibuffer provider mutates) would bump a new axis and
  invalidate the matrix. Even the **incremental** rebuild
  path touches every chunk's cells; a fresh diff hunk
  shouldn't cost the matrix.
- Coordinating "matrix is in flight; virtual rows already
  published" between the cell-builder and the renderer would
  add a coherence point that doesn't exist today.

Per paramount goal #4 (asynchronicity, three-layer message
passing) the cleaner architecture is two **independent**
RCU-published lanes: `CellMatrix` (cells_worker) and
`VirtualRowMatrix` (virtual_rows_worker). The renderer reads
both per frame and interleaves at slice() time. Either lane
can mutate without touching the other.

## 3. The data model

Three types in `lattice-cells::virtual_rows`. The interleaver
+ `DisplayRowEntry` enum live in `lattice-cells::matrix`
alongside `CellSlice` / `CellSliceIter`.

### 3.1 `VirtualRow`

```rust
pub struct VirtualRow {
	pub anchor_line: u32,
	pub position: AnchorPosition,
	pub cells: Arc<[Cell]>,
	pub height: u16,
	pub kind: VirtualRowKind,      // D.6.i — backdrop/decoration discriminant
	pub bg: Option<u32>,           // D.6.i — per-row background override
	pub scales: Option<Arc<[u16]>>, // F.3  — per-column font scale (hundredths)
}

pub enum AnchorPosition {
	Above,
	Below,
}
```

`anchor_line` is the 0-based source line this row attaches
to. `position` selects whether the row paints above or below
the anchor. `cells` is the row content — the **same**
`Arc<[Cell]>` shape that backs `CellRow`, so renderers paint
virtual rows through the existing fast path with zero special
casing. `height` is 1 for the common case; values >1 are
reserved for future multi-row blocks (code-lens summaries,
multi-line signature previews).

`kind` and `bg` (D.6.i) let a renderer pick the right backdrop
without inspecting cell content (deletion-block red vs. blank
filler vs. an explicit `bg` override); they are a *painting*
discriminant, never a motion/scroll/cursor branch — virtual
rows still flow through the uniform Document paths.

`scales` (**F.3**, Thread F) is an optional per-display-column
font scale in **hundredths** (`100` = base), parallel to
`cells`. `None` ⇒ the whole row is base size (the common case,
zero-cost). It extends the variable-font commitment — per-token
scaling, the emacs markdown-heading model where only the title
scales, not the leading `#` markers — from document rows to
virtual rows. See §3.4.

### 3.2 `VirtualRowMatrix`

```rust
pub struct VirtualRowMatrix {
	pub rows: Arc<[VirtualRow]>,
	pub line_index: Arc<[u32]>,
	pub source_line_count: u32,
	pub version: VirtualRowVersion,
}
```

`rows` is sorted by `(anchor_line, position)` with
`Above` < `Below` at the same line. `line_index[i]` is the
index of the first row in `rows` whose `anchor_line >= i`;
length is `source_line_count + 1`. The index turns "how many
virtual rows anchor before line L" into a constant-time array
lookup, used by the interleaver to fast-forward past
scrolled-off virtual rows when the scroll position lands
mid-document.

`version` is a single `u64` counter; the virtual-rows worker
bumps it on each publish. Unlike `MatrixVersion`'s multi-axis
hash, a single counter is sufficient because virtual rows
have only one source of change (provider mutation).
Consumers compare `version` across frames to invalidate
caches.

### 3.3 `VirtualRowProvider`

```rust
pub trait VirtualRowProvider: Send + Sync {
	fn id(&self) -> ProviderId;
	fn collect(&self) -> Vec<VirtualRow>;
}

pub type ProviderId = u64;
```

Producers (the diff subsystem, the multibuffer subsystem,
future inlay-hint emitters) implement this. The **future
virtual-rows worker** (D.0a.1 or as part of D.3, whichever
ships first) calls `collect()` on all registered providers
when rebuilding the published `VirtualRowMatrix`, merges the
outputs, sorts, and publishes via `ArcSwap`. Providers must
not block; non-trivial computation belongs in the provider's
own task with the result cached for the synchronous
`collect()` call.

The worker itself is not part of D.0a — without a producer,
it has nothing to do. The trait ships in D.0a so consumer
slices can register providers immediately when they land.
D.0a tests build `VirtualRowMatrix` directly via
`VirtualRowMatrix::build(rows, source_line_count, version)`
to exercise the lane end-to-end.

### 3.4 Per-token font scaling (F.3)

`VirtualRow::scales` is the substrate for **variable-sized
fonts inside a virtual row**. It is a `u16`-hundredths scale
per display column (`100` = base), stored parallel to `cells`
so the tight 16-byte `Cell` never grows — scale rides beside
the grid, not inside it. `lattice-cells` owns the encoding and
the pure `coalesce_scales(scales, total_cols) -> [ScaleRun]`
helper (contiguous same-scale runs, mirroring how a renderer
already coalesces per-cell `fg`); `BASE_SCALE = 100`.

The model is **per token, not per row** — the emacs
markdown-heading convention where the title scales but the
leading `#`/`##` markers stay base-size. A branding row is
`[base mark blocks][base gap][scaled wordmark]`; a heading is
`[base markers][scaled title]`; both are just column runs at
different scales.

Rendering is renderer-specific and **additive**:

- **GPUI** honors it. The peer coalesces the scales, shapes
  each run at `font_size × scale` on **one shared baseline**,
  and grows the row height to the tallest run — the *same*
  N-piece `ScaledLine` path the document heading rows use
  (F.2 generalized to N pieces in F.3). The host scroll model
  is unchanged: a scaled virtual row is still **one** logical
  row, painted taller (per-row cumulative tops, O(viewport)).
- **TUI** ignores it: a terminal cell grid cannot vary font
  size, so every cell paints base-size. This is a tracked,
  intentional cross-renderer divergence (GPUI is the peer
  where rich formatting is a differentiator).

First consumer: the dashboard branding block (DB.4-gpui) —
the "Lattice" wordmark carries a scale > 1.0 while the mark
blocks and tagline stay base. See `theme-system.md` Thread F
and `dashboard.md` §5.2.

## 4. The display interleaver

`CellMatrix` gains one new method:

```rust
pub fn display_slice<'a>(
	&'a self,
	scroll: u32,
	height: u32,
	virtual_rows: &'a VirtualRowMatrix,
) -> DisplaySlice<'a>;
```

`scroll` and `height` are in **display-row** space — they
count both document rows and virtual rows. The returned
`DisplaySlice` exposes `iter() -> DisplaySliceIter<'a>` which
yields `DisplayRowEntry<'a>`:

```rust
pub enum DisplayRowEntry<'a> {
	Document(&'a CellRow),
	Virtual(&'a VirtualRow),
}
```

### 4.1 Ordering contract

For each source line `L` in matrix-row order (i.e., visible
lines in `CellMatrix`):

1. All virtual rows with `anchor_line == L && position ==
   Above`, in `rows`-array order.
2. The `CellRow` for line `L` (yielded by the underlying
   `CellSliceIter`).
3. All virtual rows with `anchor_line == L && position ==
   Below`, in `rows`-array order.

After the last visible source line, any remaining virtual
rows are yielded in their sorted order — this covers
past-EOF anchors (clamped to `source_line_count` by `build`)
and anchors on **folded-out trailing lines**.

### 4.2 Folded-line anchors

When a virtual row anchors at line `L` but `L` is folded out
(no `CellRow` in the matrix), the interleaver emits the
virtual row at the **next visible line's** Above-position.
Concretely: the predicate
`vrow.anchor_line < crow.source_line` accepts the virtual row
for emission before the next visible document row. This
gives consumers a deterministic answer ("virtual rows on
folded lines surface at the fold boundary") without requiring
provider awareness of fold state.

Consumers that want fold-aware placement can subscribe to
fold events and re-emit; the primitive does not bake fold
semantics in.

### 4.3 The empty-virtual-rows fast path

`DisplaySlice::iter` checks `virtual_rows.is_empty()` and
takes a non-interleaver path when no virtual rows are
present: it constructs the underlying `CellSliceIter`
directly with the scroll-adjusted start, identical to
`CellMatrix::slice`'s output. Renderers can call
`display_slice` unconditionally; they pay nothing for the
interleaver when no provider has registered.

### 4.4 Scrolling

`display_slice(scroll, height, virtual_rows)` skips `scroll`
display entries before yielding `height`. The current
implementation is naive (O(scroll)) — for v1 viewport sizes
and realistic scroll positions this is sub-frame. If a bench
surfaces it, the optimised path uses `line_index` to convert
a display-row scroll position to a `(cell_iter_start,
virtual_row_start)` pair in O(log V), removing the naive
walk.

### 4.5 Existing `slice()` stays

`CellMatrix::slice(scroll, height)` is **unchanged**. Existing
renderer call sites (TUI peer, GPUI peer) continue to use it
and see zero behavioural change. `display_slice` is opt-in;
production renderers adopt it when consuming virtual rows
(D.3 inline diff is the first). This is the
"land-each-slice-green" heuristic at work — the primitive
ships green without requiring a renderer big-bang
migration.

## 5. Cursor + motion semantics

> j / k motions are unchanged. -- design conclusion from D.0a
> heuristic-#4 review

`motion_line_down` and `motion_line_up` in
`lattice-grammar/builtins.rs` operate in source-line space
(`Position { line, byte }`) and have no knowledge of display
rows. They step the cursor from source line N to N+1 or
N-1 directly. **Virtual rows interpose visually but do not
participate in cursor stepping** — this preserves vim
semantics by construction.

`gj` / `gk` (vim's display-line motions) currently step
matrix rows. With virtual rows present, the question of
"does `gj` over a virtual row land on the virtual row" is
deferred to a downstream slice: either D.3 (inline diff
overlay's first concrete consumer) or M.2 (multibuffer
rendering). The principled answer per fold semantics is
"`gj` skips virtual rows" — virtual rows are visual
chrome, not navigation targets. But the call lands with a
concrete consumer to validate the UX, not in this slice.

## 6. Performance posture

- **Hot path (per-frame paint):** When `virtual_rows.is_
  empty()` (the common case for documents with no diff
  session and no multibuffer), `display_slice` is equivalent
  to `slice`. Cost: identical.
- **Interleaver per-frame cost:** For viewport height = 80
  and `virtual_rows.len() = 100`, the interleaver yields ≤
  ~180 display entries and walks `O(viewport + virtual_in_
  window)` items. Bench gate (recorded, not yet enforced)
  `display_slice_iter_p99_us` at `(scroll=2500, height=80,
  N_virtual=1000)`.
- **`VirtualRowMatrix::build` cost:** O(N log N) sort + O(N
  + source_line_count) index. Bench gate (recorded)
  `virtual_row_layout_p99_us` at 1k and 10k rows. The
  worker calls `build` whenever a registered provider
  notifies mutation; debouncing belongs to the worker, not
  the primitive.
- **Memory:** `VirtualRowMatrix` at 1k rows ≈ 30KB
  (`VirtualRow` is ~32 bytes; `line_index` is 4 bytes × N+1
  source lines). Negligible.

## 7. Open questions

- **Per-pane masking.** A diff session active on pane A but
  not pane B: should pane B see the diff's virtual rows? The
  current primitive is document-scoped (one
  `VirtualRowMatrix` per document, all panes see it).
  Per-pane masking is a layered concern — the future worker
  can support per-pane provider gating via a pane-id arg on
  `collect()`. Decide before D.3 lands the first
  multi-pane-different-view case.
- **`gj` / `gk` over virtual rows.** As above, deferred to
  D.3 or M.2 — needs a UX validation in flight.
- **Multi-row virtual rows (`height > 1`).** The interleaver
  yields one `DisplayRowEntry` per virtual row regardless of
  `height`; the renderer is responsible for honouring height
  when paint-side. Tests in D.0a use `height = 1`
  exclusively. Multi-row consumers (code-lens, multi-line
  signature) will exercise > 1 when they land.
- **Per-provider invalidation axes.** The current
  `VirtualRowVersion` is a single `u64` counter — every
  provider mutation bumps the same version. If a renderer
  caches per-provider state, it can't disambiguate "diff
  changed" from "multibuffer changed" without inspecting
  rows. A multi-axis version (one slot per registered
  provider) is the obvious extension; decide when a
  consumer cares.

## 8. Slice plan

Sequencing lives in
[`docs/d../archive/virtual-rows.md`](../archive/virtual-rows.md);
authoritative status per slice lives in
[`docs/dev/operations/implementation.md`](../operations/implementation.md).
This fragment owns *what* and *why*; the slice plan owns
*when* and *in what order*.

## 9. Testing strategy

- **Unit tests** (in `lattice-cells/src/virtual_rows.rs`'s
  `mod tests` + `lattice-cells/src/matrix.rs`'s `mod
  tests`): sort/lookup correctness, line-index binary-
  search semantics, past-EOF anchor clamping, version
  increment, interleaver ordering across {Above, Below,
  multiple at same anchor}, fold-line redirect, past-EOF
  anchor placement, scroll/height honour, empty-matrix +
  virtual-rows case, multi-chunk interleaving.
- **Bench** (`benches/virtual_rows.rs`):
  `virtual_row_layout::build` at {100, 1k, 10k} rows;
  `display_slice_iter::scroll_0_height_80` and
  `scroll_2500_height_80` at {0, 100, 1k} virtual rows
  against a 5k-line matrix. Records CI baselines; gate
  enforcement deferred.
- **Future** (D.3 / M.2): end-to-end renderer paint tests
  asserting cursor position, visual row ordering, and
  scroll behaviour with a concrete consumer.

## 10. Risks

- **Display-row scroll vs source-row scroll confusion.**
  `slice()` takes source-row scroll; `display_slice()`
  takes display-row scroll. Renderers must not mix them.
  Mitigation: distinct method names, clear docstrings,
  open question (per-pane masking) flags the next likely
  drift point.
- **Naive scroll cost at large `scroll`.** The current
  O(scroll) skip is fine for typical viewport positions but
  could surface at programmatic large scrolls. Mitigation:
  bench observation; line-index-based fast skip is a
  drop-in replacement when needed.
- **Provider mutation churn.** A noisy provider (e.g., a
  diff session emitting on every keystroke) would force
  worker rebuilds at edit cadence. Mitigation: worker
  debounces; providers cache and only notify on real
  mutation. Validated when D.0a.1 lands the worker.
- **Per-pane masking deferred.** If D.3 ships before
  per-pane masking, every pane sees all diff sessions.
  Mitigation: D.3's design must explicitly evaluate
  whether per-pane gating is required for v1.

## 11. Cross-references

- `design.md` §5.15 — synopsis paragraph linking here.
- `design.md` §5.6 — rendering; `display_slice` extends
  the layered fast paths additively.
- `design.md` §5.6.8 — render-snapshot coherence; the
  virtual-rows lane follows the same RCU publish pattern
  (`Arc<VirtualRowMatrix>` swapped atomically) as
  `CellMatrix`.
- `cell-grid-renderer.md` — the matrix contract; this doc's
  primitive sits alongside it as a parallel lane.
- `diff-system.md` §5.1 — references this fragment as the
  shared home for the displacing virtual-row primitive.
  D.3 is the first production consumer.
- `multibuffer-views.md` §5 — references this fragment for
  excerpt header / separator rows. M.2 is the multibuffer
  consumer.
- `actor-seam-discipline.md` — the actor + arc-swap publish
  pattern the virtual-rows worker will inherit (D.0a.1).
- `implementation.md` — `## diff-system` D.0 entry tracks
  D.0a (this slice) + D.0a.1 (worker) + D.0b (scroll-bind).
