# Pane zoom — slice plan

Design fragment: [`architecture/pane-zoom.md`](../../architecture/pane-zoom.md).

tmux-style non-destructive zoom of the active pane within a tab.
`<C-w>z` / `<C-w><C-z>` / `:zoom-pane`.

| Slice | Status | Scope |
|---|---|---|
| ZP.1 | 📝 | `PaneTree::zoomed` + geometry + invariant enforcement (`lattice-core`) |
| ZP.2 | 📝 | Host wiring: action, effect, chord, ex-command, catalog entry |
| ZP.3 | 📝 | GPUI parity: both `PaneNode` walks |
| ZP.4 | 📝 | Indicators + `pane.zoom-indicator` option |
| ZP.5 | 📝 | Bench, user page, ledger |

---

## ZP.1 — core state + geometry

`crates/lattice-core/src/ui/pane.rs`.

- `PaneTree.zoomed: Option<PaneId>`; `zoomed()`, `is_zoomed()`,
  `zoomed_index()`, `toggle_zoom()`, `clear_zoom()`.
- `compute_rects` returns `vec![(idx, area)]` when zoomed.
- `compute_rects_layout` — the always-unzoomed peer; `navigate`
  switches to it (design §4).
- Invariant enforcement inside the type: `set_active`,
  `split_active`, `close_active`, `collapse_to_active` clear zoom;
  `equalize_ratios` and `resize_active_split` no-op while zoomed.

Tests (unit, in-module): toggle round-trip restores the exact rects;
single-leaf toggle is a no-op; zoomed rect is the full area; one
clear-on-X test per enforcement row in the design's table; navigation
while zoomed finds the real neighbour *and* leaves the tree unzoomed;
zoom survives a tab swap (`mem::swap` of two trees).

**Depends on:** nothing. **Lands green alone** — no host or renderer
change needed for the TUI to honour zoom, since it routes through
`compute_rects`.

## ZP.2 — host wiring

- `AppEffect::ToggleZoomPane` (`lattice-grammar/src/app_effect.rs`)
  + its two `boundary_app_effect.rs` mappings.
- `Action::ToggleZoomPane` + dispatch arm →
  `editor.do_toggle_zoom_pane()`.
- `actions.toggle_zoom_pane` registration.
- Chord: `z` in the bare table and `('z', ...)` in the ctrl table
  (`keymap_normal.rs`).
- `:zoom-pane` ex-command.
- `keymap_entry!` catalog row.

Tests: chord dispatches through `press()` (not the handler — see
`modal-states-need-a-dispatch-arm`); `:zoom-pane` toggles; the
catalog test that pins every bound chord has an entry.

**Depends on:** ZP.1.

## ZP.3 — GPUI parity

`collect_pane_geometries` + the paint walk in
`lattice-ui-gpui/src/window.rs` take the zoom branch. Audit:
`rg -n "zoomed" crates/lattice-ui-gpui/` non-empty.

Extend the existing TUI/GPUI geometry-parity test
(`lattice-ui-tui/src/render.rs`, `compute_rects`-vs-`draw_frame`
agreement) with the zoomed case.

**Depends on:** ZP.1. Independent of ZP.2.

## ZP.4 — indicators

- `pane.zoom-indicator` (`labeled_enum!` + `options!`, `Pane` group):
  `both` / `modeline` / `tabline` / `none`, default `both`.
- `core.zoom` modeline element: descriptor in
  `register_builtin_elements`, content in `resolve_builtin_content`.
- `TabRenderItem.zoomed`; publisher computes it; both tabline
  renderers paint the `Z`.

Tests: element content across all four option values; tabline marker
present/absent per option; both renderers agree on the marker text.

**Depends on:** ZP.1 (needs `is_zoomed`), ZP.3 (GPUI tabline).

## ZP.5 — bench + docs

- Criterion case in `lattice-core/benches`: `compute_rects` zoomed
  vs. unzoomed, 4-pane tree. Record in
  `docs/dev/operations/benchmarks.md`.
- User page under `docs/user/`, plus `nav.toml` + site sync + search
  (see `docs-land-on-the-zola-site-too`).
- `implementation.md` ledger row; flip this plan's statuses.

**Depends on:** ZP.1–ZP.4.
