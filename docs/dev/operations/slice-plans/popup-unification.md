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

## Locked decision (2026-06-27): full conversion (α) + 2A highlights

The PU.1 internal shape is **α — full conversion**: help becomes an
actor-backed synthetic Document outright (`BufferData::Help` carries a
`DocumentEntry`, exactly like `BufferData::Messages`). `HelpBuffer`
storage is retired; content lives once in the Document; the popup's
view state (scroll/cursor) routes through the same `self.cursor` /
`self.scroll` it already uses when focused; motions come from the
normal grammar path; and the `active_text` / `active_cursor` /
`active_buffer_id` `BufferKind::Help` branches are replaced by
**focus-state routing** (a focused popup → `popup_buffer`) that names
no kind. Chosen over the dual-backed transitional (β, duplicates
content — a fresh drift seam) and the single-rope-keep-motions
intermediate (γ); α is the endgame with no half-migration residue
(paramount #3, heuristic #1, `feedback_mode_owns_its_surface`).

Highlights take path **2A**: help is a real markdown Document, so its
`DisplayMatrix` is built by the cells worker from a live markdown
`SyntaxHandle` like any document — the `with_markdown_syntax`
precompute + `popup_help_highlights` read path are deleted, not
special-cased in compose (which 2B would do, re-introducing a K.4
kind-branch + leaving the highlight source as drift). Pixel-identical
today because `with_markdown_syntax` already runs the same grammar.

Sub-slices: **PU.1a** (storage/cursor/motion/kind-branch conversion,
green, no visual change — bespoke renderers keep painting via a
reconstructed `HelpBuffer` *view* built from the Document) → **PU.1b**
(compose seam + markdown handle + delete bespoke renderers) →
**PU.1c** (mop up any residual focus-routing edge cases the render
switch exposes). PU.2 (GPUI parity) unchanged.

## PU.1a — Help → actor-backed Document ✅

Landed 2026-06-28. Help content is now `BufferData::Help(DocumentEntry)`
— an actor-backed synthetic Document seeded by
`Editor::register_help_document`, exactly like `*messages*`. Title →
registry `name`; links/anchors/highlights → `buffer_locals`. The popup
view state (scroll/cursor when help is NOT focused, plus the focus
stash) moved off the retired `HelpBuffer.{scroll,cursor}` registry
fields onto `Editor::{popup_scroll,popup_cursor}` (a faithful
relocation of the prior registry-cursor behaviour — `snapshot_active_pane`
syncs them identically). `popup_help()` survives as a view
reconstructor (`BufferRegistry::help_content_view` + the popup stash) so
the bespoke renderers paint unchanged this slice; PU.1b deletes both.
`active_text` / `active_cursor` / `active_buffer_id` route through
`popup_buffer`/`popup_help()` (Document-backed), and HelpBuffer's motion
methods + their tests are gone (motions come from the grammar path).
No visual change; full suite green (lattice-help 39, lattice-host 590,
lattice-ui-tui 1499, 0 failed).

Two pre-existing branch breakages were fixed while landing this slice
(both unrelated to popup unification): the GPUI `EditorElement::paint`
`self.wrap_width` → `prepaint.wrap_width` field error from HS.1b (broke
`--features window`), and a stale `lattice-help` markdown test asserting
`##` → `Heading1` (the T-series per-level heading query makes it
`Heading2`).

- Add `SyntheticDocVariant::Help` → `BufferData::Help(DocumentEntry)`;
  spawn/seed help content as a Document (mirror
  `ensure_named_synthetic_doc_with_variant`). Title → registry `name`;
  metadata (links/anchors/highlights) → `buffer_locals` (already the
  `HelpLinks`/`HelpAnchors`/`HelpHighlights` slots).
- Rewrite the registry accessors (`help`/`help_mut`/`with_help`/
  `with_help_mut`/`help_with_title`/`contains_help`/`help_ids_sorted`)
  onto the `DocumentEntry`. `popup_help()` survives PU.1a as a
  **view reconstructor** (builds a transient `HelpBuffer` value from
  the Document snapshot + popup scroll/cursor) so the bespoke
  renderers need zero change this slice — PU.1b deletes both.
- Rewrite the popup state machine off `HelpBuffer` storage:
  `open_popup` / `open_floating_popup` / `open_help_in_pane` /
  `swap_popup_content` / `snapshot_current_popup` (back-stack) /
  `dismiss_popup` / `focus_help_popup`.
- Replace the `BufferKind::Help` branches in `active_text` /
  `active_cursor` / `active_buffer_id` with focus-state routing.
- Delete `HelpBuffer`'s motion methods + their lattice-help tests
  (motions now come from the grammar path).
- **Acceptance:** no visual change; `:help` / `:describe-*` / hover /
  back-stack (`<C-o>`) / dismiss-on-Esc all behave as before; full
  test suite green.

## PU.1b — Compose seam + markdown handle + delete bespoke 🗒

- Add the **content seam**: a helper the popup-chrome code calls with
  `(buffer_id, inner_rect, popup_scroll, popup_leftcol, popup_cursor)`
  that builds a `FrameView::for_buffer` + `PaneComposeCtx` and calls
  `compose_pane_lines`, returning the `Line`s the box paints.
- Wire the markdown `SyntaxHandle` onto the help pane so the cells
  worker builds its `DisplayMatrix` (2A); delete `with_markdown_syntax`
  + `popup_help_highlights`.
- Re-point `help_pane_render` / the help-overlay interior to the seam.
- **Delete** `draw_help_in_pane`, `manually_wrap_lines`,
  `render_help_line`, `draw_inactive_help`, the content portion of
  `draw_help_overlay`, and `popup_help` (keep the box: border, title,
  centering, sizing).
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
