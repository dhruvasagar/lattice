+++
title = "Horizontal scroll"
+++

# Horizontal scroll

When soft-wrap is off, a line longer than the body area must be
reachable: the view scrolls left/right to keep the cursor visible and
to let the user pan to off-screen content. This is the horizontal twin
of the vertical viewport model (`scroll` / `ensure_cursor_visible`).

The slice plan (sequencing, status) lives at
`docs/dev/operations/slice-plans/archive/horizontal-scroll.md` (archived —
HS.1–HS.3 all landed).

## 1. Why this is a new axis, not a tweak

The renderer historically *clipped* long lines at the right edge with
no offset (TUI `truncate_spans_to_width`; GPUI `overflow_hidden`).
There was no horizontal state at all — `scroll` is purely a line
index. So off-screen-right content was simply unreachable. This is a
table-stakes parity gap (vim, Helix, Zed all horizontal-scroll on
`nowrap`), not a refinement of existing behaviour.

## 2. Convention (the UX anchor)

Vim is the reference, because muscle memory is the dominant cost on a
user-facing surface (see `feedback_convention_first`):

- The view follows the cursor past the left/right edge.
- `sidescroll` (default `0`) is the step: `0` jump-scrolls so the
  cursor lands at the **window centre**; `N > 0` scrolls `N` columns
  at a time.
- `sidescrolloff` (default `0`) keeps that many columns of context on
  each side of the cursor — the horizontal analog of `scrolloff`.
- `zl`/`zh` scroll one column; `zL`/`zH` half a screen; `zs`/`ze` put
  the cursor at the left/right edge.
- Horizontal scroll is **mutually exclusive with wrap**: when `wrap`
  is on the body reflows, so nothing is off-screen-right and `leftcol`
  is pinned to `0`.
- The **gutter never scrolls** — line numbers, diagnostic and diff
  sign cells stay fixed; only the text body pans.

## 3. The model

`leftcol: u32` — the first visible display column — is the horizontal
peer of `scroll`. It is **per-pane** (`PaneState::leftcol`) with the
active pane's value mirrored into the hot field (`Editor::leftcol`),
exactly like `scroll`. It rides the pane↔editor swap
(`snapshot_active_pane` / `load_active_pane`) and the per-pane render
snapshot.

`ensure_cursor_horizontally_visible` is the clamp. It is called at the
tail of `ensure_cursor_visible`, so every existing caller maintains
both axes for free. The math is a pure, total function
(`horizontal_leftcol(leftcol, cursor_col, body_w, sidescroll,
sidescrolloff)`) so it unit-tests without any renderer or matrix
state.

### 3.1 Body width

The clamp needs the **body** width (pane width minus gutter), the
horizontal analog of `viewport_height`. The renderer owns gutter
geometry, so `body_text_width` mirrors the renderer's `buffer_w`
derivation host-side: `viewport_width − (digits + 5)` with line
numbers on, `− 4` with them off (gutter + diagnostic + diff-sign
cells). The clamp is robust to a few columns of slack, so a transient
gutter-width disagreement degrades gracefully (cursor stays visible,
centre is off by the delta) rather than hiding the cursor. When gutter
geometry moves to a shared crate this duplication folds away.

### 3.2 Cursor display column

`cursor_display_col` reads the active buffer's cell matrix
(`CellRow::byte_to_combined_col`, tab- and inlay-expanded — matches
what the renderer paints) when the cursor's row is built, and falls
back to a tab-expanded scan of the line text when it isn't.

### 3.3 Renderer offset

Both renderers apply `leftcol` only when wrap is off:

- **TUI** `clip_spans_horizontally(spans, leftcol, body_w)` drops the
  first `leftcol` display columns, then clips to the body width
  (byte-based, mirroring `truncate_spans_to_width`'s ASCII-width
  model). The cursor's screen column subtracts `leftcol`.
- **GPUI** offsets the body element and the cursor by the same
  `leftcol` (parity slice — see slice plan).

## 4. Paramount-goal alignment

- **#1 (latency).** The column-skip is `O(viewport-lines)` — the same
  cost class as the existing right-edge truncation; the cursor-follow
  clamp is `O(1)`. No new UI-thread work.
- **#3 (everything is a buffer).** `leftcol` lives in the one
  `compose_pane_lines` path, so every buffer kind inherits it. The
  same property is what lets **popups** inherit horizontal scroll once
  their content routes through the shared compose path — see the popup
  unification work (`feedback_buffers_no_special_case`). A popup that
  paints its own text would *not* get horizontal scroll, which is
  exactly the drift the unification removes.

## 5. Relationship to popup unification

Horizontal scroll lands in the shared compose path; it does **not**
reach popups that still paint their own text (help/hover/completion
docs/signature). Bringing it to popups is not per-popup work — it is
the payoff of routing popup *content* through `compose_pane_lines`
(chrome stays popup-specific). Horizontal scroll is therefore a
forcing function for, and a beneficiary of, the popup-content
unification initiative.
