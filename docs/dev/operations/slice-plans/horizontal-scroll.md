# Horizontal scroll — slice plan

Design fragment: `docs/dev/architecture/horizontal-scroll.md` (the
*what* and *why*). This file owns the *when* and *in what order*.

Status icons: ✅ done · 🚧 in progress · 🗒 planned.

## HS.1 — Core: `leftcol` state + cursor-follow + renderer offset

The single user-visible deliverable: with `wrap` off, moving the
cursor past the right/left edge scrolls the body so the cursor stays
visible, and the panned-away content is reachable. Default
`sidescroll = 0` ⇒ jump-to-centre (vim default, confirmed with the
user).

- **HS.1a — host state + math.** ✅
  - `PaneState::leftcol` + `Editor::leftcol`; rides the pane↔editor
    swap (`snapshot_active_pane` / `load_active_pane`) and the
    explicit `PaneState`/tab literals.
  - `sidescroll` / `sidescrolloff` options (`core_options.rs`,
    re-exported from `lattice_config`, read into `OptionCache` by
    `rebuild_option_cache`).
  - `ensure_cursor_horizontally_visible` (called at the tail of
    `ensure_cursor_visible`) + `body_text_width` + `cursor_display_col`
    + the pure `horizontal_leftcol` clamp.
  - Tests: 5 `horizontal_leftcol` unit tests (visible no-op, jump
    centre on over/under-flow, `sidescroll` stepping, `sidescrolloff`
    margin) + 2 integration tests (follows-right, no-op-under-wrap).
    588 host lib tests green.
- **HS.1b — renderer offset.**
  - **TUI** ✅ — `PaneComposeCtx::leftcol` (active = `app.ad().leftcol`,
    inactive = `leaf.leftcol`); `ActiveDocumentRenderState::leftcol`;
    `clip_spans_horizontally` at the nowrap body site; cursor
    screen-column subtracts `leftcol`. 3 clip unit tests + existing
    compose/cursor suites green.
  - **GPUI** ✅ (common path) — `EditorElement::leftcol` (active =
    `ad.leftcol`, inactive = stashed `pane.leftcol`); `col_x` subtracts
    `leftcol` for ordinary rows (cursor + selection/decoration quads);
    the body-glyph paint slices the first `leftcol` cells off each
    ordinary row so column `leftcol` lands at `text_origin_x` (no
    content mask needed). Inert when `leftcol == 0` (saturating-sub +
    `leftcol > 0` slice guard ⇒ byte-identical to pre-HS), so the
    non-scrolled case cannot regress. **Heading-split rows are not
    offset yet** (markdown headings rarely h-scroll) — follow-up.
    Needs a visual verification pass (`cargo run --features gui --
    --gui`) on the scrolled-right case; not viewable in CI/headless.

## HS.2 — Manual h-scroll grammar ✅

`zl`/`zh` (count columns), `zL`/`zH` (half body width), `zs`/`ze`
(cursor column to left/right edge), bound at the `Builtin` keymap
layer (universal vim grammar). One `AppEffect::HorizontalScroll(
HScroll)` + `Action::HorizontalScroll` carries all six; the host
`do_horizontal_scroll` mutates `leftcol` (no-op under `wrap`) and then
`clamp_cursor_into_horizontal_window` keeps the cursor on screen
(mirroring `do_scroll_line`'s "move the cursor, not the view" rule via
the new `byte_at_display_col` inverse). Wiring: `app_effect.rs` +
`action.rs` enums, `dispatch.rs` two bridge arms + handler, 6 action
IDs (`actions.rs`), 6 binds (`keymap_normal.rs`), 6 which-key entries
(`keymap_entry.rs`), TUI dispatch-classification arm. 2 handler tests
+ 590 host / 199 grammar / 93 keymap green; both renderers build.

## HS.3 — Consolidation ✅

- **User doc** — `docs/user/display.md` gains a "Horizontal scroll"
  section + quick-reference rows (`sidescroll`/`sidescrolloff` and the
  `z*` chords).
- **Bench** — no new bench: the cursor-follow clamp runs inside
  `ensure_cursor_visible` (covered by `benches/dispatch_publish.rs`)
  and the column-skip runs inside `compose_pane_lines` (covered by
  `lattice-ui-tui/benches/render.rs`); both execute at `leftcol == 0`
  on every keystroke, so the existing hot-path benches already track
  the cost. A dedicated micro-bench for an O(viewport)/O(1) path would
  be redundant.

## Dependency: popup-content unification

Horizontal scroll lands in `compose_pane_lines`; popups that paint
their own text (help/hover/completion docs/signature) do **not**
inherit it. That is intended — the fix is not per-popup h-scroll but
routing popup *content* through the shared compose path (chrome stays
popup-specific). Tracked separately; HS is a forcing function for it.
See the design fragment §5 and `feedback_buffers_no_special_case`.

## Out of scope (v1)

- Wide-char / non-ASCII display-width accuracy in the column skip
  (mirrors the existing ASCII-width assumption in
  `truncate_spans_to_width`).
- A horizontal scrollbar (vim has none by default).
- Per-buffer `wrap` divergence across panes (a separate refinement;
  `leftcol` is already per-pane and ready for it).
