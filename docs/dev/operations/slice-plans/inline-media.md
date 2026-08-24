# Inline media (Path 4) — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts live in
> [`../../architecture/inline-media.md`](../../architecture/inline-media.md).

**Status:** IM.0 📝 — nothing started. Pulled back from post-1.0 on
2026-08-24; see the design fragment §2 for why the deferral no longer
holds (Thread F already built the per-row variable-height paint path).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Sequencing

```
IM.0  gate: what does `scroll` actually assume?     ← cheap, blocks all
  │
IM.1  peer-local height map + sub-row scroll (GPUI)  ← no media yet
  │
IM.2  document-level Fenwick height index            ← scrollbar, G, jumps
  │
IM.3  MediaBlock descriptor + VirtualRowKind         ← no decode yet
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
| IM.0 | Gate: audit every consumer of `Editor::scroll` / `viewport_height` | 📝 |
| IM.1 | Peer-local row→pixel map + `sub_row_px` smooth scroll (GPUI) | 📝 |
| IM.2 | Document-level Fenwick height index (scrollbar, absolute jumps) | 📝 |
| IM.3 | `MediaBlock` descriptor + `VirtualRowKind::MediaBlock` | 📝 |
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

### IM.0 — the gate 📝

**This can come back "bigger than expected", and that is the point of
running it first.**

`Editor::scroll: u32` is a display-row index read by both peers.
`ensure_cursor_visible` already juggles sticky rows, tree-sitter context
rows and popup viewports, all in row units. Before changing anything,
enumerate every consumer and classify each as:

- **row-unit, unaffected** — keeps working because the core's unit does
  not change (the design's §4 claim, which this slice tests rather than
  assumes);
- **assumes uniform height** — needs the peer height map;
- **assumes uniform height AND is shared host code** — the dangerous
  set, because it affects the TUI too.

*Exit:* a written list, and a test pinning current behaviour for each
member of the third group. If that group is large, the design's §4
split is wrong and this fragment gets revised before IM.1.

*paramount:* #1 — a scroll bug is a per-frame visible defect.

### IM.1 — peer-local height map + sub-row scroll 📝

GPUI already computes `row_scale` / `row_tops` per paint. This lifts that
into a queryable map and adds `sub_row_px` for scrolling *within* a tall
row, kept peer-local (never in `RenderState`, discarded on any core-driven
scroll change).

*Exit:* a fixture row declared 8 line-heights tall scrolls smoothly
through, `zz`/`zt`/`zb` land it correctly, and the TUI is byte-identical
before and after.
*test:* the failure mode is a cursor that scrolls behind a tall row —
assert cursor visibility with a tall row above and below.
*bench:* keystroke→glyph unchanged with all-1.0 scales.

### IM.2 — document-level height index 📝

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
