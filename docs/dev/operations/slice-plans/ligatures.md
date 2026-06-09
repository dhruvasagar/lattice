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
- `editor_element.rs` prepaint: after `let font = text_style.font()`,
  apply `font.features.disable_ligatures()` when `!self.theme.ligatures`.
- `window.rs` advance measurement: same post-modify on the `font()`
  call for `glyph_advance_px`.

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
- `crates/lattice-ui-gpui/src/window.rs`

---

## LG.2 — User doc update  ✅

- `docs/user/display.md`: add `ui.ligatures` row to Quick reference
  table; add prose section explaining GPUI on/off semantics and the
  TUI delegation model (terminal controls it, Lattice doesn't
  interfere).
