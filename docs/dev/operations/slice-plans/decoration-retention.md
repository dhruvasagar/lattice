# Decoration retention across focus — slice plan

Sequencing companion to
[`../../architecture/decoration-retention.md`](../../architecture/decoration-retention.md).
The design fragment owns *what* and *why*; this file owns *when* and
*in what order*. Each slice lands green with the four artefacts
(doc · bench/coverage · tests · graceful failure) and keeps TUI +
GPUI in lockstep (`feedback_tui_gpui_parity`).

Confirmed direction (2026-06-05, user): decorations are buffer-
intrinsic retained state; active vs. inactive is a paint-time opacity;
focus change does zero decoration recompute. First slice = **retain +
repaint** (remove the teardown, land the UX fix), then unify the
producers, then collapse the inactive render fork. (Scope chosen over
"full unification now" and "inlays-only" — see design §Rejected.)

| Slice    | Title                                                                                  | Status |
|----------|----------------------------------------------------------------------------------------|--------|
| **DR.1** | **Retain + repaint.** Stop the teardown and render the full decoration set on inactive panes; focus-gain frame already carries it (no keystroke). | 🚧 |
| **DR.2** | **Per-buffer syntax producer.** Retained per-buffer cells/spans for all visible panes; remove cells-active-only gating + the cleared-each-frame `pane_highlights` map. | 🗒 |
| **DR.3** | **One render path.** Collapse `draw_inactive_document` / inactive-compose fork into the shared path; inactive = opacity + no interaction state only. | 🗒 |
| **DR.4** | **Four-artefact close.** Bench proving zero decoration recompute on focus change; design/doc finalize; parity audit. | 🗒 |

## DR.1 — Retain + repaint

**Goal.** Toggling focus between two panes showing different
buffers (each with syntax + LSP inlays + diagnostics) shows the full
decoration set on **both** at all times (inactive dimmed), with zero
decoration recompute on the switch and **no keystroke** needed for the
newly-focused pane to paint complete.

**Progress (2026-06-05):** item 2 (stop the teardown) landed — the
syntax-highlight retention foundation. `refresh_pane_highlights` no
longer `clear()`s and re-slices every frame; it decides changes from
immutable reads first and takes `&mut` only when a pane must be
recomputed or pruned, keyed on `(buffer_id, scroll,
syntax_snapshot.text_version())`. New `Editor.pane_highlight_keys`
provenance; the two `:redraw`/buffer-switch `pane_highlights.clear()`
sites clear it in lockstep. Regression test
`refresh_pane_highlights_no_op_does_not_bump_version` (693 host tests
green). **Remaining for DR.1:** item 1 (inlays on inactive panes),
item 3 (republish + repaint on focus-gain — the "needs a keystroke"
symptom), item 4 (diagnostics parity on inactive).

**Work items (mechanics refined during implementation):**

1. **Inlays on inactive panes.** Source inlays from the retained
   per-buffer cache (`render_state.lsp.inlay_hints.get_for(buffer_id)`)
   on the inactive render path and weave them like the active path.
   - TUI: `draw_inactive_document` currently renders `pane_highlights`
     spans with no inlay weaving — add inlay sourcing + weaving.
   - GPUI: already sources `inlay_hints` per-buffer in `paint_pane`;
     confirm inactive panes actually paint them (the legacy
     `build_line_with_inlays` fallback path runs for inactive, so
     verify end-to-end).
2. **Stop the wholesale teardown.** `refresh_pane_highlights` must not
   `clear()` the whole map and re-slice every frame. Retain each
   pane's entry; recompute one only when its buffer's syntax version
   actually advanced, guarded by `(buffer_id, version)` so a reused
   pane index can't read a stale buffer's spans.
3. **Republish + repaint on focus-gain.** Activating a pane/doc
   republishes the now-active buffer's decorations and fires a
   `paint_request` so the focus-change frame is already complete —
   removing the "lags one keystroke" symptom. (Removing the teardown
   in (2) is the architectural half; this is the trigger half, kept in
   the same slice because the fix is incomplete without both.)
4. **Diagnostics parity on inactive.** Confirm inactive panes paint
   diagnostic underlines (not just reserve the gutter column); source
   from the per-URI cache like the active path.

**Acceptance / tests.**

- Unit: `refresh_pane_highlights` retains an unchanged pane's spans
  across calls (no clear); recomputes only on version bump; never
  serves a stale buffer's spans after a pane reuses an index.
- Unit/host: focus change does not invalidate the per-buffer inlay /
  diagnostic caches.
- Manual (both renderers, runnable for GPUI here): split with two LSP
  buffers; confirm inlays/diagnostics/syntax stay on the inactive pane
  (dimmed) and the newly-focused pane is complete on the **first**
  frame after `Ctrl-W w` — no keystroke needed.

**Graceful failure.** Missing per-buffer cache entry (buffer not yet
parsed / no LSP) → render text + whatever decorations exist, never
panic, never clear another buffer's state.

## DR.2 — Per-buffer syntax producer

Retain a per-buffer cells/spans product for every visible buffer,
refreshed only when that buffer's syntax version advances. Removes the
cells-active-only gating in both renderers (`cell_matrix: if
render_active`) and the cleared-each-frame `pane_highlights` map.
Inactive panes then render through the **same** cells path as active
(syntax parity, not the lesser span fallback). Depends on DR.1's
retention guarantee.

## DR.3 — One render path

Collapse `draw_inactive_document` (TUI) and the inactive-compose
branch into the shared render path. A pane's input becomes
`(buffer decorations, interaction state | None, opacity)`; inactive
panes pass `None` interaction state + `inactive_pane_opacity`. Removes
the focus-keyed special-case (`feedback_buffers_no_special_case`).
Depends on DR.2 (both paths must already source the same per-buffer
decoration product).

## DR.4 — Four-artefact close

- **Bench:** focus-change decoration recompute == 0 (assert no reparse
  request, no span re-slice, no cells rebuild on pure focus toggle).
- **Doc:** finalize the design fragment; record any rejected-path that
  became the chosen path during DR.1–DR.3.
- **Parity audit:** `grep` the renderer match arms / sourcing sites to
  confirm no remaining focus-keyed decoration branch in either peer.

## Sequencing

- **DR.1** is independent and lands the UX fix first by *removing*
  teardown (not patching the symptom) — the relocation-first rule
  (`feedback_no_ui_thread_work`).
- **DR.2** depends on DR.1 (needs the per-buffer retention guarantee
  before widening the cells producer to all visible buffers).
- **DR.3** depends on DR.2 (the shared render path requires both
  active and inactive to source the same per-buffer product).
- **DR.4** closes once DR.1–DR.3 land.

## Cross-references

- Contracts, data model, rejected alternatives, paramount-goal
  mapping: [`../../architecture/decoration-retention.md`](../../architecture/decoration-retention.md).
- Related: `feedback_decorations_update_in_place`,
  `feedback_buffers_no_special_case`, `feedback_no_ui_thread_work`,
  `feedback_tui_gpui_parity`.
