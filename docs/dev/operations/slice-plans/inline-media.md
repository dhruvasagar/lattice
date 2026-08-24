# Inline media (Path 4) — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts live in
> [`../../architecture/inline-media.md`](../../architecture/inline-media.md).

**Status:** IM.1 ✅ (2026-08-25) — **the scroll arithmetic is
tall-row-aware, with no media in existence.** IM.0 ✅ the gate (which
found a real flaw and revised the design); IM.1a ✅ cursor visibility;
IM.1b ✅ paging and centring. **IM.1c was dropped as premature and its
work folded into IM.3 / IM.5** — see below. IM.3 next; IM.2 moved after
it.

Path 4 was pulled back from post-1.0 on 2026-08-24; the design fragment
§2 records why the deferral no longer holds (Thread F already built the
per-row variable-height paint path).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Sequencing

```
IM.0  gate: what does `scroll` actually assume?     ← cheap, blocks all
  │
IM.1  per-row weights + budget-based scroll math     ← no media yet ✅
  │   (IM.1c dropped: nothing to publish weights FOR until IM.3)
  │
IM.3  MediaBlock descriptor + VirtualRowKind         ← no decode yet
  │
IM.2  document-level Fenwick height index            ← scrollbar, G, jumps
  │
IM.4  off-thread decode + cache + inbound landing
  │
IM.5  GPUI paints it; TUI paints `alt`
  │
IM.6  WIT seam: a plugin contributes a media block
  │
IM.7  org: [[file:…]], org.inline-images, <leader>oI
  │
IM.8  docs, ledger, site nav
```

**Why the scroll work comes before any image.** IM.1–IM.2 are the
highest-risk slices and they are testable with **no media at all** — a
row can be told it is 8 line-heights tall by a test fixture. Landing them
first means the scroll arithmetic is proven before an image can be blamed
for a scrolling bug. It also means IM.1 is independently useful: scaled
markdown headings already produce tall rows and already scroll slightly
wrong.

| Slice | Description | Status |
|---|---|---|
| IM.0 | Gate: audit every consumer of `Editor::scroll` / `viewport_height` | ✅ |
| IM.1a | `RowWeights` + cursor-visibility walks spend a line-height budget | ✅ |
| IM.1b | Paging / centring (`<C-f>`, `H`/`M`/`L`, `zz`) spend the budget | ✅ |
| IM.3 | `MediaBlock` descriptor + `VirtualRowKind::MediaBlock` | 📝 |
| IM.2 | Document-level Fenwick height index (scrollbar, absolute jumps) | 📝 |
| IM.4 | Off-thread decode + cache, landing via the inbound primitive | 📝 |
| IM.5 | GPUI paint; TUI `alt` fallback at the same row count | 📝 |
| IM.6 | WIT seam for plugin-contributed media blocks | 📝 |
| IM.7 | Org: `[[file:…]]`, `org.inline-images`, `<leader>oI` | 📝 |
| IM.8 | Docs, ledger, site nav | 📝 |

Every slice ships four artefacts (CLAUDE.md heuristic #5): doc, bench
where a hot path is touched, tests covering the failure mode as well as
the happy path, graceful error handling. One slice, one commit, committed
as it goes green, `scripts/precommit.sh <crate>` before each.

---

### IM.0 — the gate ✅ (2026-08-24)

**It came back "the design is wrong in one specific place", which is
exactly what the gate was for.**

`Editor::scroll: u32` is a display-row index read by both peers, and
`ensure_cursor_visible` already juggles sticky rows, tree-sitter context
rows and popup viewports in row units. The gate enumerated every consumer
of both fields and classified each as row-unit-and-unaffected, uniform-
height-assuming, or uniform-height-assuming **and shared host code** —
the last being the dangerous set, because it reaches the TUI too.

**Result.** The raw numbers look alarming and are misleading: 340 `.scroll`
sites and 352 `viewport_height` sites, 181 of the latter in `lattice-host`
alone. Classified, the picture is much smaller.

- **Row-unit, unaffected: `Editor::scroll` entirely.** It is a row
  *index*, and an index stays meaningful however tall rows are. No change.
- **Assumes uniform height, shared host code — the dangerous set: seven
  functions.** `ensure_cursor_visible`, `bottom_anchored_scroll`,
  `do_page`, `do_scroll_line`, `do_jump_viewport` (`height / 2` centring),
  `do_scroll_cursor_to`, `preview_viewport_for`. All consume
  `viewport_height` as "the number of rows that fit".
- **Degrades rather than breaks:** `cells_worker`'s prefetch window uses
  `viewport_height * 2` as a heuristic.
- **Peer-local:** 86 GPUI sites, 39 TUI code sites; each peer's own
  business.

**The design was revised.** `viewport_height` is computed by GPUI as
`available_px / row_px` against a uniform `row_px = font_size * 1.3`.
Under variable heights that division is wrong, because how many rows fit
depends on which rows. The fragment's §4 claim that everything "keeps
working unchanged in row units" was half wrong, and the wrong half was
load-bearing.

Fix recorded in design §4.0: `viewport_height` keeps its type and becomes
a **budget in line-heights**, and each peer publishes a per-row weight
vector (TUI all `1.0`, GPUI its existing `row_scale`). The seven
functions spend the budget instead of counting rows. With all-1.0 weights
the arithmetic is today's exactly — which is what keeps the TUI
untouched and makes IM.1 testable with no media in existence.

*paramount:* #1 — a scroll bug is a per-frame visible defect.

### IM.1 — per-row weights + budget-based scroll math 📝

Per IM.0. Two halves:

**Shared.** A per-row weight vector (line-heights, `f32`) published
alongside the virtual-row matrix, and the seven functions in §4.0
converted from counting rows to spending a line-height budget. The TUI
publishes all `1.0`, so its behaviour must be bit-identical before and
after — that is the slice's main regression guard.

**GPUI-local.** Lift the existing per-paint `row_scale` / `row_tops` into
the published weights, and add `sub_row_px` for scrolling *within* a tall
row (never in `RenderState`, discarded on any core-driven scroll change).

*Exit:* a fixture row declared 8 line-heights tall scrolls smoothly
through, `zz`/`zt`/`zb` land it correctly, and the TUI is byte-identical
before and after.
*test:* the failure mode is a cursor that scrolls behind a tall row —
assert cursor visibility with a tall row above and below.
*bench:* keystroke→glyph unchanged with all-1.0 scales.

### IM.1c — dropped (folded into IM.3 / IM.5) ⛔

Scoped as "publish real weights from GPUI, add `sub_row_px`". Both halves
turned out to be premature, for the same reason: **nothing is tall yet.**

**The publish channel is not needed.** The plan assumed each peer would
push a per-row weight vector into the host. Two things killed that. A
peer only knows its *viewport*, and the scroll walks range outside it, so
a viewport-shaped vector answers the wrong question. And it is
unnecessary: `heading_scale_split` derives a heading's scale from the
display matrix and the resolved theme, both of which are host-side
already. Whether that scale *applies* is a single renderer policy flag,
not a vector — the TUI's rows are one cell tall by physics.

So weights are computed where the tall thing is defined. A media block
knows its own height (IM.3, from the descriptor). Heading scales are a
separate, pre-existing cosmetic inaccuracy — headings already scroll
slightly wrong today — and wiring them wants a lazy per-line query rather
than a vector; carved out rather than smuggled in here.

**`sub_row_px` has nothing to scroll within** until a row is taller than
the viewport step, which first happens at IM.5 when an image paints at
its natural size. It lands there, with the thing that makes it
observable.

What IM.1 actually delivered is the arithmetic: `RowWeights`, and seven
walks that spend a line-height budget instead of counting rows, proven
against a fixture row that simply declares itself tall. That was the
point — the scroll rework is now done and testable before any image can
be blamed for a scrolling bug.

### IM.2 — document-level height index 📝

**Resequenced after IM.3.** Its consumers — scrollbar proportion and
absolute jumps — only diverge from today once tall rows exist, so
building the index first would mean benching and tuning a structure with
nothing in it.

Fenwick cumulative height, peer-local, updated on edit and block resize.
Consumers: scrollbar proportion, absolute jumps (`G`, line N, restored
scroll position).

*Exit:* scrollbar thumb is proportional in a document with tall rows;
`G` lands correctly with tall rows in between.
*bench:* index update cost per edit on a 100k-line file; must not appear
on the keystroke path.

### IM.3 — the descriptor 📝

`VirtualRowKind::MediaBlock` + the descriptor from design §6 (`source`,
`intrinsic`, `rows`, `fit`, `alt`). **No decode, no painting** — a block
reserves its rows and paints a placeholder.

*Exit:* a block reserves the right number of rows on BOTH peers;
scrolling past it is correct; `alt` is what the TUI shows.

### IM.4 — decode off the UI thread 📝

`spawn_blocking` read + decode + scale; cache keyed
`(path, mtime, target_size)`; result lands via
`SubsystemBoot::inbound`. Header-only read first, so `intrinsic` is known
before the pixels.

*Exit:* the image appears **without a keypress** — asserted the way it
fails, i.e. without dispatching an action afterwards.
*failure modes:* missing file, corrupt image, unsupported format,
enormous image → placeholder + `alt`, logged once, never a panic and
never a stall.
*bench:* decode is off-thread — assert the UI thread's frame time is
unchanged while a large image decodes.

### IM.5 — paint 📝

`gpui::img()` into the block region; TUI paints `alt` in a box at the
same row count. Both peers in the same patch (cross-renderer rule).

### IM.6 — the seam 📝

WIT surface for a guest to contribute a media block: path + row count,
never bytes, `fs:read` gated host-side.

*Exit:* a fixture guest contributes a block that renders.

### IM.7 — org 📝

`[[file:…]]` alone on a line; `org.inline-images` (default off);
`<leader>oI` toggle. Lands in the plugin repo.

*Exit:* opening an org file with `org.inline-images=on` shows the image
in GPUI and its `alt` in the TUI, with identical scroll behaviour.

### IM.8 — docs, ledger, site nav 📝

Includes correcting `design.md`: Phase 9's retirement note and the
Post-1.0 list both still say Path 4 is post-1.0, and §5.6.7's "Why this
is not Path 4" describes it as deferred. The Post-1.0 list also still
names `org-mode`, which has been shipping since OM.0.
