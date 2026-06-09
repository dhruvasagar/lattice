# Ligatures — slice plan

Sequencing companion to
[`docs/dev/architecture/ligatures.md`](../../architecture/ligatures.md).
The design fragment owns *what* and *why*; this file owns *when* and
*in what order*.

---

| Slice | Title | Status |
|-------|-------|--------|
| **LG.1** | Option + GPUI feature flag | ✅ |
| **LG.2** | User doc update | ✅ |

---

## LG.1 — Option + GPUI feature flag  ✅

**What lands:**

- `theme_options.rs`: `UiLigatures: bool = true` — GPUI-only,
  TUI ignores it.
- `GpuiTheme` (`lib.rs`): `pub ligatures: bool` field (default `true`).
- `rebuild_gpui_theme` (`lib.rs`): read `UiLigatures`, set
  `self.theme.ligatures`.
- `editor_element.rs` prepaint: apply `FontFeatures::disable_ligatures()`
  when `!self.theme.ligatures` (suppresses `liga`+`calt` in all `TextRun`s).
- `editor_element.rs` paint: **hybrid path** — when `ligatures=true`,
  call `paint_cells_row_bg_only` (bg quads from cell matrix) then fall
  through to `ShapedLine::paint` (glyphs from shaped multi-char runs →
  ligatures form). When `ligatures=false`, keep `paint_cells_row`
  (per-char `paint_glyph`, no sequences shaped together).
- `paint_cells.rs`: add `paint_cells_row_bg_only` — bg quads only,
  no glyph emission.
- `window.rs` advance measurement: apply `FontFeatures::disable_ligatures()`
  on the reference font when `!ligatures_enabled`.

**Why `paint_cells_row` prevented ligatures**: `GlyphResolver` shapes
single characters via `layout_line(single_char)`. Ligature glyphs
require the shaper to see the adjacent sequence. `ShapedLine::paint`
uses the pre-built shaped line (from `push_wrapped_doc_row` →
`shape_line(multi_char_run, …)`) where adjacent same-style chars land
in one `TextRun` and HarfBuzz forms the ligature.

**Tests:**
- `ui_ligatures_option_parses_and_default_is_true` — parse `:set
  ui.ligatures=off` + `:set ui.ligatures=on`; assert default is `true`.
- `rebuild_gpui_theme_propagates_ligatures` — set `UiLigatures` to
  `false` in a registry, call `rebuild_gpui_theme`, assert
  `theme.ligatures == false`.

**Files touched:**
- `crates/lattice-host/src/ui/theme_options.rs`
- `crates/lattice-ui-gpui/src/lib.rs`
- `crates/lattice-ui-gpui/src/editor_element.rs`
- `crates/lattice-ui-gpui/src/paint_cells.rs`
- `crates/lattice-ui-gpui/src/window.rs`

---

## LG.2 — User doc update  ✅

- `docs/user/display.md`: add `ui.ligatures` row to Quick reference
  table; add prose section explaining GPUI on/off semantics and the
  TUI delegation model (terminal controls it, Lattice doesn't
  interfere).
