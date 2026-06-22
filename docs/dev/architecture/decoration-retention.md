# Decoration retention across focus

Design fragment — the *what* and *why*. Sequencing lives in the slice
plan: [`../operations/slice-plans/archive/decoration-retention.md`](../operations/slice-plans/archive/decoration-retention.md).

## Intent

A buffer's decorations — syntax highlighting, inlay hints, diagnostic
underlines, document-highlights, diff signs — are **buffer-intrinsic
retained state**. They are computed when a buffer first becomes
visible and invalidated **only** when *that* buffer is edited or
receives an LSP update. Shifting focus between panes/tabs must never
recompute, drop, or fail to paint them. Active vs. inactive is a
**paint-time opacity**, nothing more.

The only state that is genuinely active-only is *interaction* state:
the cursor, the visual selection, the current search match, the
cursor-line background. Everything else a pane shows is a property of
its buffer and survives focus changes untouched.

## Problem (resolved 2026-06-05, DR.1–DR.3)

The original state had active and inactive panes running **different
decoration machinery**, with the inactive machinery producing a *lesser*
set and throwing work away. All five issues below are closed:

- **Two syntax producers.** The active pane reads the live
  `visible_highlights` cell (continuously maintained) plus the
  `cell_matrix`/`display_matrix` — but the cells worker publishes
  **only for the active document** (`window.rs`: `cell_matrix: if
  render_active {…} else { None }`). Inactive panes get nothing from
  cells and fall back to `editor.pane_highlights[idx]`.
- **Teardown on every refresh.** `Editor::refresh_pane_highlights`
  (`pane_highlights.rs`) does `self.pane_highlights.clear()` and
  rebuilds, re-slicing `highlight_lines(scroll, end)` for every
  inactive Document pane (and re-requesting a reparse when the
  buffer's text version moved). Already-valid highlight work is
  discarded wholesale.
- **Inlays vanish when inactive.** Active inlays are published
  active-buffer-only into `SyntaxRenderState.inlay_hints`
  (`build_active_inlay_hints`) and woven into the active compose
  path. The inactive render path (`draw_inactive_document`, TUI)
  renders `pane_highlights` spans **with no inlay weaving** — so a
  buffer that showed inlays loses them the instant it goes inactive.
- **No republish on focus-gain.** When a buffer becomes active again,
  its active-only decoration publish (cells, woven inlays) lags one
  dispatch, so the focus-change frame paints the lesser inactive set
  and the decorations only reappear on the **next keystroke**
  (user-observed in the TUI).
- **A render-path fork.** `draw_inactive_document` is a parallel
  render function to `compose_visible_lines` that produces a
  different, smaller decoration set — an everything-is-a-buffer
  special-case keyed on focus.

Net: an inactive buffer's content has not changed, so its syntax,
inlays, and diagnostics are still correct — yet they are torn down and
lazily recomputed, with a window of missing decorations on every focus
change. That is wasted work and a permanent "what's missing on
re-focus" bug source.

## Contract

1. **Per-buffer retained decorations.** Syntax (spans/cells), inlay
   hints, diagnostic underlines, document-highlights, and diff signs
   live in per-buffer caches keyed by `BufferId` (the
   `PerBufferCache` pattern inlays/diagnostics already use). A visible
   buffer's decorations are computed once and held.
2. **Invalidate on the owning buffer's change only.** A buffer's
   decoration cache is invalidated when that buffer is edited or its
   LSP data updates — never on focus change, never on *another*
   buffer's edit.
3. **One render path.** Every pane renders through the same path with
   the full decoration set. Inactive panes differ only by an opacity
   (`ui.inactive_pane_opacity`, gated by `ui.dim_inactive`) and by the
   absence of interaction state (cursor/selection/match/cursor-line).
4. **Focus change is free.** Activating a pane flips opacity and
   schedules a repaint; it performs **zero** decoration recompute and
   the focus-gain frame already carries the full decoration set.
5. **Renderer parity.** TUI and GPUI satisfy this identically, in
   lockstep (`feedback_tui_gpui_parity`).

## Data model

Decoration state is addressed by `BufferId`, not by "active" / pane
index:

- **Syntax:** the parsed tree already lives per-buffer in
  `editor.syntax` / `document_syntax_for(id)`. The viewport-window
  slice (spans, or the cell/display matrix) becomes a per-buffer
  retained product, refreshed only when that buffer's syntax version
  advances — replacing the cleared-each-frame `pane_highlights` map
  and the active-only cells publish.
- **Inlay hints:** `render_state.lsp.inlay_hints`
  (`PerBufferCache<LspInlayHintCache>`) is already per-buffer; both
  render paths source from it via `get_for(buffer_id)`. The
  active-only `SyntaxRenderState.inlay_hints` republish becomes a
  convenience, not the source of truth.
- **Diagnostics / document-highlights / diff signs:** already
  per-buffer / per-URI; the inactive render path must read and paint
  them, not just reserve their gutter columns.

## Render contract

A pane's element/compose input is `(buffer decorations, interaction
state | None, opacity)`. Interaction state is `Some` only for the
focused pane. Opacity is `1.0` for the focused pane and
`ui.inactive_pane_opacity` otherwise. There is no second "inactive"
producer and no focus-keyed branch in decoration sourcing.

## Paramount-goal alignment

> **UX (higher court):** removes the missing-decoration frame and the
> keystroke-to-recover on every focus change — directly upholds
> `feedback_decorations_update_in_place` ("decorations update in
> place, never flicker; unchanged lines never lose cues").
> **Paramount goals:** protects #1 (no recompute of already-valid
> decorations; inactive buffers don't change) and #3 (everything-is-a-
> buffer: decoration data is buffer-intrinsic, so the producer must
> not branch on focus). Sacrifices nothing real — retaining an
> unchanging buffer's decorations costs bounded memory, not CPU.
> **Heuristic #1 (long-term fit, on merit):** the two-producer split
> and `draw_inactive_document` fork are kept because they work, not
> because they are better. One retained per-buffer store read by one
> render path is the genuinely-better design.
> **Heuristic #2 (paramount, not other editors):** anchored on #1/#3,
> not on "editor X dims this way."
> **Standing rule (`feedback_no_ui_thread_work`):** the fix *removes*
> the teardown (architectural relocation) as the first slice, rather
> than adding a repaint trigger on top of it (symptom patch).
> **Standing rule (`feedback_buffers_no_special_case`):** collapsing
> `draw_inactive_document` into the shared path removes a focus-keyed
> special case.

## Rejected alternatives

- **(A) Symptom repaint-trigger.** Keep the two producers + the
  clear-and-rebuild, but schedule a repaint + pane-highlight refresh
  on focus-gain so the keystroke is no longer needed. Removes the
  visible symptom while preserving the teardown and the duplication —
  exactly the patch heuristic #1 / `feedback_no_ui_thread_work`
  forbid as a first slice. Rejected.
- **(B) Retain but keep two producers.** Stop clearing
  `pane_highlights` and source inlays on the inactive path, but leave
  active-on-cells / inactive-on-spans as separate machinery. Kills
  the bug but preserves the duplication (the deeper bug surface).
  Adopted *partially* as the first slice (DR.1) because it removes the
  teardown and lands the UX fix green fast; the producer unification
  (DR.2) and render-path collapse (DR.3) follow.

## Slice plan

See [`../operations/slice-plans/archive/decoration-retention.md`](../operations/slice-plans/archive/decoration-retention.md)
for sequencing (DR.1 retain + repaint → DR.2 per-buffer producer →
DR.3 one render path → DR.4 four-artefact close).
