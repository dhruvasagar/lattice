# Decoration retention across focus — slice plan

Sequencing companion to
[`../../../../architecture/decoration-retention.md`](../../../../architecture/decoration-retention.md).
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
| **DR.1** | **Retain + repaint.** Stop the teardown and render the full decoration set on inactive panes; focus-gain frame already carries it (no keystroke). | ✅ |
| **DR.2** | **One producer (retire `pane_highlights`).** Both renderers read the per-pane retained `DisplayMatrix` for inactive panes (the producer already existed per-pane — see Premise below); the redundant `pane_highlights` span producer + its per-frame refresh are deleted. `:redraw` stays the forceful clean-slate escape hatch. | ✅ |
| **DR.3** | **One render path.** Collapse `draw_inactive_document` / inactive-compose fork into the shared path; inactive = opacity + no interaction state only. | ✅ |
| **DR.4** | **Four-artefact close.** Bench proving zero decoration recompute on focus change; design/doc finalize; parity audit. | ✅ |

## DR.1 — Retain + repaint

**Goal.** Toggling focus between two panes showing different
buffers (each with syntax + LSP inlays + diagnostics) shows the full
decoration set on **both** at all times (inactive dimmed), with zero
decoration recompute on the switch and **no keystroke** needed for the
newly-focused pane to paint complete.

**Progress (2026-06-05):**

- *Item 2 (stop the teardown)* landed — the syntax-highlight retention
  foundation. `refresh_pane_highlights` no longer `clear()`s and
  re-slices every frame; it decides changes from immutable reads first
  and takes `&mut` only when a pane must be recomputed or pruned, keyed
  on `(buffer_id, scroll, syntax_snapshot.text_version())`. New
  `Editor.pane_highlight_keys` provenance; the two `:redraw`/
  buffer-switch `pane_highlights.clear()` sites clear it in lockstep.
  Regression test `refresh_pane_highlights_no_op_does_not_bump_version`
  (693 host tests green).
- *Item 1 (inlays on inactive panes)* landed (TUI). `draw_inactive_document`
  now sources the pane's OWN buffer inlays from `rs.lsp.inlay_hints.
  get_for(buffer_id)`, gated by `lsp_inlay_hint_mode_enabled_for`
  (parity with the active publish), and splices them into the inactive
  body before the dim overlay — so hints no longer vanish when a pane
  loses focus. GPUI already sourced inactive inlays per-buffer in
  `paint_pane` + splices via the legacy path, so this brings TUI up to
  GPUI (converging parity); 1470 TUI tests green. Manual TUI check
  pending.

**Items 3 + 4 resolved by downstream slices (not separate code):**

- *Item 3 (republish + repaint on focus-gain):* DR.2 removed the
  active-only cells-publish gate. The cells worker now builds the
  `DisplayMatrix` for ALL visible panes, so a newly-focused pane's
  matrix is already current on the focus-gain frame — no republish
  step needed. The "lags one keystroke" symptom is gone.
- *Item 4 (diagnostics parity on inactive):* DR.3's unified
  `compose_pane_lines` renders diagnostic underlines + severity cells
  unconditionally (not `is_active`-gated), so inactive panes get the
  full diagnostic decoration set.

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

## DR.2 — One producer (retire `pane_highlights`)

**Premise correction (2026-06-05).** DR.2 was originally framed as
*"build a per-buffer syntax producer."* Tracing the code showed that
producer **already exists, per-pane, for both renderers**: the cells
worker (`recompute` → `recompute_pane`) builds the canonical
`DisplayMatrix` (+ projected `CellMatrix`) for **every** visible
Document pane (it iterates `cells.panes`), keyed by `PaneId`. The
active path of both renderers already consumes it (GPUI via the
top-level `display_matrix`, which shares Arc identity with the active
pane's `pane_matrices` entry; TUI via `cells_rs.display_matrix` →
`row_at_source_line` → `display_line_to_source_spans`). Only the
**inactive** path still used the legacy `pane_highlights` span map —
a redundant second producer left over from before per-pane matrices
existed. So DR.2 is *redirect inactive reads to the existing matrix +
retire the redundant producer*, not *build a producer*.

**Heuristic mapping.** UX: net win, no regression (inactive panes get
full syntax via the same matrix as active; the only plain-text moment
is the one transient frame after a split, identical to the active
pane's first frame). Paramount #1: retiring `refresh_pane_highlights`
deletes a per-frame dispatch that re-sliced `highlight_lines` per
inactive pane and could fire focus-keyed async reparses — one
producer, zero recompute on a pure focus toggle. Paramount #3
(`feedback_buffers_no_special_case`): removes the focus-keyed
`if render_active { … } else { pane_highlights }` branch in both
renderers. Heuristic #1: `pane_highlights` is the inferior primitive
kept past its purpose; deleting it is the merit win (one producer,
less host state — `Editor::pane_highlights` + `pane_highlight_keys` +
`refresh_pane_highlights` + `pane_highlights.rs` +
`Action::RefreshPaneHighlights` + `SyntaxRenderState::pane_highlights`
+ `PublishCache::pane_highlights_map` all deleted).

**Landed surface.**

- GPUI `window.rs`: `cell_matrix` / `display_matrix` read
  `matrix_for_pane(pane.id)` / `display_matrix_for_pane(pane.id)` for
  *all* panes (active entry shares Arc identity → behaviour-preserving),
  with the per-pane snapshot stale guard; the inactive `visible_spans`
  branch (read `pane_highlights`) is gone; the per-frame
  `RefreshPaneHighlights` dispatch + its `EnsureGateCache.pane_refresh_key`
  gate are removed.
- TUI `render.rs`: `draw_inactive_document` sources its body from
  `display_matrix_for_pane(pane.id)` → `display_line_to_source_spans`
  (mirrors the active compose path, incl. the `body_from_cells`
  whitespace gate + W.4.t.1 inlay byte-mapping), not `pane_highlights`
  spans.
- Host: the whole `pane_highlights` producer retired (see deleted-symbol
  list above), with its render-state publish + memoisation + the two
  B.4 Arc-identity / round-trip tests.

**`:redraw` is the exception.** A routine focus change recomputes *no*
decorations (the whole point). But `:redraw` / `<C-l>` is the user's
forceful, user-initiated escape hatch for a corrupted display, so it
re-derives **every** visible pane from a clean slate — matching the
pre-DR.2 `pane_highlights.clear()` semantics. `do_redraw_screen` now
resets each visible Document pane's per-buffer cell + display matrix to
empty (so the cells worker rebuilds it) in addition to bumping
`last_parsed_text_version`. A one-frame plain-text flash during the
rebuild is acceptable there (the user asked for a redraw;
`pending_redraw` clears the terminal). (User direction, 2026-06-05.)

Depends on DR.1's retention guarantee.

## DR.3 — One render path

**✅ Landed (`278740e5`).**

`draw_inactive_document` (TUI) is now a 30-line thin entry that builds
`PaneComposeCtx { is_active: false, … }` and delegates to
`compose_pane_lines` — the same function `compose_visible_lines` calls
for the active pane. The old parallel inactive compose body (~300 lines
sourcing from the retired `pane_highlights`) is gone. Interaction-state
features (visual selection, hlsearch, substitute preview, ghost text,
cursor-line highlight) are gated on `ctx.is_active`; buffer-intrinsic
decorations (diagnostics, diff signs, syntax, inlays) are not.

**Documented seams lifted to future work:**
- *Folds on inactive panes:* `closed_fold_at_start` is gated on
  `is_active` because fold state lives in the active document; the ` ┄
  N lines` annotation is omitted on inactive panes. Lifts when
  per-buffer fold state lands.
- *Soft-wrap on inactive panes:* `view.wrap_lines` (via
  `FrameView::for_buffer`) is the resolver. Currently global
  (same value for all panes); when buffer-local options land,
  `for_buffer` resolves the buffer's local value with the global
  default — the emacs buffer-local pattern. No `is_active` gate.
- *Inlay two-source seam:* active path reads `rs.syntax.inlay_hints`
  (pre-built active-buffer list); inactive path reads
  `rs.lsp.inlay_hints.get_for(buffer_id)` (per-buffer LSP cache).
  Functionally identical; converges to one source when both paths
  consume the per-buffer cache.

**GPUI parity (no change needed):** GPUI never had a separate
`draw_inactive_document`; its paint loop already iterates panes
uniformly with per-pane `display_matrix_for_pane` (from DR.2). GPUI
was already in the DR.3 target shape before this slice.
`feedback_tui_gpui_parity` is satisfied.

Depends on DR.2 (both paths must source the same per-buffer decoration
product).

## DR.4 — Four-artefact close ✅

- **Bench ✅** — `focus_toggle_does_not_recompute_pane_cells`
  (`dispatch.rs` tests). A two-pane vertical split, prime each pane's
  matrix, then a PURE focus toggle (`pane_tree.set_active`, no edit / no
  resize) → assert `recompute_pane` returns `WorkerDecision::CacheHit` for
  every visible Document pane. No new instrumentation: the cells worker's
  existing `WorkerDecision` (`CacheHit` = "version matches the published
  matrix; worker does nothing", leaving it bit-identical) IS the
  zero-recompute signal — adding hot-path counters just to prove zero
  overhead would itself violate paramount #1. The focus-path sibling of
  `recompute_with_matching_version_is_cache_hit` and the inverse of
  `version_bump_rebuilds_matrix`. Note `set_active` bumps the *pane-tree*
  version (layout), which is deliberately NOT a cells version axis —
  `build_cells_panes` stamps `MatrixVersion` purely from buffer-intrinsic
  axes (text / syntax-snapshot / inlay / folds / theme / whitespace), none
  focus-keyed — so the matrix survives a focus change untouched.
- **Parity audit ✅** — `pane_highlights` and `refresh_pane_highlights`
  survive only in explanatory comments (no live producer or call; the
  runtime call is commented out); `draw_inactive_document` is the ~30-line
  thin wrapper delegating to `compose_pane_lines`; the remaining
  `is_active` gates are interaction-state only (visual selection,
  hlsearch, substitute preview, ghost text, cursor-line), NOT decoration
  recompute. No focus-keyed decoration branch remains in either peer; GPUI
  was already in the target shape (DR.2).
- **Doc ✅** — markers above corrected (DR.3 was committed but still marked
  🚧); the DR.2 premise-correction (the per-pane producer already existed
  — the bug was a renderer-read gate, not a missing producer) is recorded
  in the DR.2 section as the chosen-over-original path.

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
  mapping: [`../../../../architecture/decoration-retention.md`](../../../../architecture/decoration-retention.md).
- Related: `feedback_decorations_update_in_place`,
  `feedback_buffers_no_special_case`, `feedback_no_ui_thread_work`,
  `feedback_tui_gpui_parity`.
