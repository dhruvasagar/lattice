+++
title = "Popup unification — a popup is a regular buffer in a box"
+++


A popup must render its content through the **exact same path** as a
regular buffer pane. The only thing popup-specific is the *box* it
sits in (border, placement, sizing). Everything inside — wrapping,
folding, horizontal scroll, syntax highlighting, decorations, cursor
positioning — comes from the shared compose path, so every feature we
add reaches popups automatically with zero per-popup work and zero
drift.

The slice plan (sequencing, status) lives at
`docs/dev/operations/slice-plans/archive/popup-unification.md` (archived —
the initiative is complete).

## 1. The drift this removes

Today every popup builds its own text layout. The canonical
regular-buffer path is `compose_pane_lines` (TUI,
`crates/lattice-ui-tui/src/render.rs`) and `EditorElement` (GPUI,
`crates/lattice-ui-gpui/src/editor_element.rs`). Each popup re-implements
a *subset* of it and misses the rest:

| Popup | TUI bespoke path (to delete) | GPUI bespoke path (to delete) | Missing today |
|-------|------------------------------|-------------------------------|---------------|
| Help (`:help`, `:describe-*`, hover-help) | `draw_help_in_pane`, `draw_help_overlay`, `manually_wrap_lines`, `render_help_line`, `help_pane_render`, `draw_inactive_help` (render.rs ~1568–2232, ~2690) | manual cell/row loop + chunk wrap (window.rs ~3405–3670, ~3517–3598) | folding, h-scroll, option-driven wrap (hardcoded on) |
| LSP hover | own line builder | own | folding, h-scroll, wrap-toggle |
| Completion docs | plain ratatui `Paragraph` (render.rs ~752–837) | own | syntax, h-scroll, wrap |
| Signature help | own | own | folding, h-scroll |

The keystroke contract and "everything is a buffer" goal both require
these to share one path. The `leftcol` horizontal scroll added in the
HS series proves the problem concretely: it lives in
`compose_pane_lines`, so popups that paint their own text do **not**
get it. Unification is the structural fix.

## 2. Chrome vs. content

The split is the whole design:

- **Chrome (stays popup-specific):** the floating box — border,
  background, title/header row, centered-vs-cursor-anchored placement,
  outer/inner-rect sizing, dismiss-on-Esc. This is legitimately a
  popup concern (a `DocumentRenderer` placement, per the renderer-trait
  design §5.6).
- **Content (routes through the shared path):** given the popup's
  inner rect, call `compose_pane_lines(view, snap, inner_h, inner_w,
  ctx)` with the popup buffer's own `scroll` / `leftcol` / `cursor`.
  `compose_pane_lines` already serves arbitrary buffer ids (it renders
  inactive panes), so the inner content is just another pane view.

The acid test: a new editor feature (a gutter sign, a decoration, a
motion-driven highlight) lands in `compose_pane_lines` and appears in
popups with **no popup code touched**.

## 3. The model: popup content is a registry buffer

`compose_pane_lines` needs a `DocumentSnapshot` plus the buffer's
per-buffer state (folds, syntax highlights, decorations), all of which
live in the `BufferRegistry` keyed by `BufferId`. So popup content
must be a registry buffer. Two classes:

- **Listed synthetic buffers** — `:help`, `:describe-*`, `:apropos`,
  `:list-*`, etc. These are the queued "help into the registry" move.
  They are read-only Documents with a synthetic name, already carry
  help-mode features (links, anchors, dismiss-on-Esc) and stay
  `HelpBuffer`-flavoured (Group-1 set; see
  `feedback_synthetic_buffers`). They appear in `:ls` / `:b <name>`.
- **Ephemeral buffers** — LSP hover, signature help, completion docs.
  Cursor-anchored, auto-dismissed, created and destroyed rapidly. They
  register with `BufferFlags { listed: false, hidden: true }` and are
  garbage-collected on dismiss, so they never pollute `:ls` and never
  churn the listed set. The ephemeral class is what makes routing
  transient popups through the registry safe.

`HelpBuffer` already owns a rope-backed `Buffer` (`content`) in
`crates/lattice-help`; the move is registry membership + a snapshot,
not new content modelling.

## 4. Rejected alternative — compose from an ad-hoc snapshot

Give `compose_pane_lines` a path that renders an ephemeral content
snapshot directly, without registry membership.

- Smaller change, no registry churn for transient popups.
- **But** the per-buffer folds / highlights / decorations are resolved
  off the registry (`FrameView::for_buffer` reads registry state), so
  this needs a *parallel* resolution path — re-introducing the exact
  divergence the initiative removes. It satisfies "looks unified" while
  leaving a second code path that drifts again the next time a feature
  reads per-buffer state.

Rejected on heuristic #1 (genuinely-better long-term fit) and
paramount goal #3: keeping popup state off the registry is a
half-migration, not everything-is-a-buffer. The registry-buffer model
(§3) is chosen even though it is the larger change, because it is the
one where drift cannot recur.

## 5. What gets deleted

When the seam is in, these bespoke content renderers are removed (not
extended — K.4: remove the special path):

- TUI: `manually_wrap_lines`, `render_help_line`, `draw_help_overlay`
  (content portion), `draw_help_in_pane`, `draw_inactive_help`, and the
  per-popup line builders for hover / signature / completion-docs.
- GPUI: the manual cell/row + chunk-wrap loop in `window.rs`
  (~3405–3670) for help, and the equivalent hover/signature/docs paint
  code.

What stays: the box-drawing / placement / sizing helpers (chrome), now
calling the shared compose path for their interior.

## 6. Paramount-goal alignment

- **#1 (latency).** Compose is already O(viewport-lines); a popup
  composes only its inner rect. No new per-frame work — if anything,
  less (one path, shared caches) than N bespoke layouts.
- **#2 (extensibility).** One content path is the surface plugins and
  future renderers extend; popups inherit by construction.
- **#3 (everything is a buffer).** The endgame: popup content is a
  `BufferId` in the registry; `:b` / `:ls` / save-path / decorations /
  folds / wrap / h-scroll all apply uniformly. A popup is a *view*, not
  a *kind*.

## 7. Regression guard

A K.4-style verbatim test ("a popup is a regular buffer in a box"):
open each popup kind, assert its visible content equals what
`compose_pane_lines` produces for the same buffer + inner rect, and
assert no popup path calls a bespoke content renderer (grep-gate in
CI, mirroring the `Effect::*` / `DiffSignKind::*` GPUI-parity grep).
