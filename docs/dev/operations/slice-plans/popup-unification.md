# Popup unification — slice plan

Design fragment: `docs/dev/architecture/popup-unification.md` (the
*what* and *why* — chrome-vs-content split, the registry-buffer model,
the rejected ad-hoc-snapshot alternative). This file owns the *when*
and *in what order*.

Goal: every popup's **content** renders through `compose_pane_lines`
(TUI) / `EditorElement` (GPUI); only the **box** (chrome) stays
popup-specific. Outcome: folding, soft-wrap, horizontal scroll,
syntax, and decorations work in popups for free, and no popup path
contains bespoke text-layout code.

Status icons: ✅ done · 🚧 in progress · 🗒 planned. All slices below
are 🗒 (not started).

## Sequencing rationale

Help first (PU.1/PU.2): it is the most-used popup and the one with the
most divergence (`manually_wrap_lines`, `draw_help_in_pane`, the GPUI
manual cell loop), and it already owns a rope-backed `Buffer`, so it
proves the registry-buffer seam end-to-end with the least new content
modelling. Transient popups (hover/signature/docs) follow once the
seam + the ephemeral-buffer class exist.

## PU.1 — Help into the registry + TUI compose seam 🗒

The foundational slice. Make help a registry buffer and route its
popup content through the shared TUI compose path.

- Register `HelpBuffer` content as a **listed synthetic Document** in
  the `BufferRegistry` (the queued "help into the registry" move). It
  stays `HelpBuffer`-flavoured (links/anchors/dismiss-on-Esc) but
  exposes a `DocumentSnapshot` like any Document. See
  `feedback_synthetic_buffers` + the Group-1 set.
- Add the **content seam**: a helper the popup-chrome code calls with
  `(buffer_id, inner_rect, popup_scroll, popup_leftcol, popup_cursor)`
  that builds a `FrameView::for_buffer` + `PaneComposeCtx` and calls
  `compose_pane_lines`, returning the `Line`s the box paints.
- Re-point `help_pane_render` / the help-overlay interior to the seam.
- **Delete** `draw_help_in_pane`, `manually_wrap_lines`,
  `render_help_line`, `draw_inactive_help`, and the content portion of
  `draw_help_overlay` (keep the box: border, title, centering, sizing).
- **Acceptance:** `:set wrap`/`nowrap` now changes the help popup;
  folds work inside help; horizontal scroll works inside help (proves
  the HS dependency); the popup's visible content equals
  `compose_pane_lines` for the same buffer + inner rect.
- Tests: a "help content == compose_pane_lines" equivalence test; the
  wrap-toggle + fold + h-scroll behaviours inside the popup.

## PU.2 — GPUI help parity 🗒

Same seam in the GPUI peer (parity rule — same patch class as PU.1).

- Route the GPUI help popup interior through `EditorElement` with the
  popup's inner rect + scroll/leftcol/cursor.
- **Delete** the ~270-line manual cell/row + chunk-wrap loop in
  `window.rs` (~3405–3670). Keep the box chrome.
- **Acceptance:** GPUI help renders byte-equivalent content to a
  regular pane; visual pass on wrap/fold/h-scroll inside the popup
  (`cargo run --features gui -- --gui`).

## PU.3 — Ephemeral-buffer class 🗒

The mechanism transient popups need before they can join the registry.

- `BufferFlags { listed: false, hidden: true }` + an **ephemeral**
  marker; create on popup-open, garbage-collect on dismiss. Never
  appears in `:ls`, never churns the listed set.
- Lifecycle hooks: the owning mode's `on_activate` creates it, dismiss
  drops it (mirrors how transient state is owned today).
- Tests: an ephemeral buffer is invisible to `:ls` / `:bn` / `:bp` and
  is removed from the registry on dismiss.

## PU.4 — LSP hover through compose (TUI + GPUI) 🗒

- Back the hover popup with an ephemeral buffer (PU.3) carrying the
  hover markdown; route its content through the seam.
- Delete the hover-specific line builder (both renderers).
- **Acceptance:** hover content gets syntax/markdown rendering,
  wrap-toggle, and h-scroll; auto-dismiss + cursor-motion behaviour
  unchanged.

## PU.5 — Signature help + completion docs through compose 🗒

- Same ephemeral-buffer + seam treatment for signature help and the
  completion documentation popup (both renderers).
- Delete their bespoke content paths (the completion-docs plain
  `Paragraph`, the signature line builder).
- Note: the completion **candidate list** and pickers are list/
  selection widgets, not document content — out of scope for this
  initiative (their unification, if any, is a separate "list buffer"
  question). This initiative is about *content* popups.

## PU.6 — Cleanup + regression guard 🗒

- Grep-gate (CI) asserting no popup path calls a bespoke content
  renderer — mirrors the `Effect::*` / `DiffSignKind::*` GPUI-parity
  grep in the TUI/GPUI-parity rule.
- The verbatim "a popup is a regular buffer in a box" test across all
  popup kinds (K.4-style), analogous to
  `crates/lattice-host/tests/multibuffer_is_a_regular_buffer.rs`.
- Confirm the four-artefacts set landed per slice (design fragment
  here, tests per slice, perf covered by the existing compose benches
  — popups compose only their inner rect, no new hot-path cost).

## Dependencies & cross-references

- **Horizontal scroll** (`docs/dev/architecture/horizontal-scroll.md`
  §5) is the forcing function: PU.1 is where h-scroll first reaches a
  popup. Done (HS.1–HS.3) and merged on the `horizontal-scroll` branch.
- **Synthetic buffers / Group-1 set** — `feedback_synthetic_buffers`;
  help stays `HelpBuffer`-flavoured.
- **K.4 / no kind-specific rendering** — `feedback_buffers_no_special_case`;
  the rule this initiative satisfies for popups.
- **TUI/GPUI parity** — `feedback_tui_gpui_parity`; each renderer slice
  lands in the same patch class.
