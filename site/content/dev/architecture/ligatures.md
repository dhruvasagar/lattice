+++
title = "Ligature rendering"
+++

# Ligature rendering

**Date:** 2026-06-09
**Status:** complete (LG.1 + LG.2 ✅)
**Scope:** GPUI renderer only; TUI is terminal-delegated.

---

## What are ligatures

OpenType programming ligatures (`liga` feature tag) replace adjacent
character sequences with a single presentation glyph — `->` →
single arrow, `!=` → single not-equal, `=>` → fat arrow, `//` →
double slash, etc. `calt` (contextual alternates) is the companion
feature that handles contextually-dependent glyph substitution.
Common fonts: Fira Code, JetBrains Mono, Cascadia Code, Iosevka.

---

## Rendering path

### TUI — no Lattice changes

The TUI outputs raw UTF-8 bytes + ANSI SGR codes. The terminal
emulator shapes and renders; if the user's terminal (kitty, WezTerm,
Alacritty) and their configured font support ligatures, they render
automatically. Lattice does not obstruct this path — adjacent same-
style cells emit contiguous bytes in a single SGR run.

The one case where TUI breaks a ligature is a mid-sequence
**style boundary** (e.g., `-` is one syntax colour and `>` is
another). This is semantically correct: the styles are genuinely
different tokens, and breaking the ligature makes that visible.

### GPUI — gpui → cosmic-text → OpenType shaping

The GPUI renderer builds `TextRun` slices and passes them to
`WindowTextSystem::shape_line`, which converts to
`cosmic_text::attrs::Attrs` and runs a HarfBuzz-backed shaper.

`gpui::Font` carries a `features: FontFeatures` field
(`gpui::text_system::font_features::FontFeatures`). The API:

```rust
// Default — empty feature list; shaper uses font/engine defaults
// (liga + calt enabled in all common OpenType fonts).
let features = FontFeatures::default();

// Opt-out — sets calt=0 only (calt drives programming ligatures;
// liga is left at the shaper default which is fine for most users).
let features = FontFeatures::disable_ligatures();

// Query whether calt is explicitly disabled.
features.is_calt_enabled() → Option<bool>  // None = not set (enabled by default)

// Arbitrary OpenType tags (future granular control).
features.tag_value_list() → &[(String, u32)]
```

`FontFeatures::default()` is an empty list — the shaper uses the
font's/engine's defaults, which enable `liga` and `calt` in all
common ligature fonts (Fira Code, JetBrains Mono, Cascadia Code).
`disable_ligatures()` adds `("calt", 0)` only; it does not touch
`liga`. Programming ligatures (`->`, `!=`, `=>`, …) are almost
exclusively `calt`-driven so this is the correct single-knob opt-out.

The `FontFeatures` list is passed through gpui's shaping pipeline to
cosmic-text / HarfBuzz, which respects the explicit `calt=0` override.

---

## Run-splitting at style boundaries

`cell_to_text_run` / `display_line_to_text_runs` coalesces adjacent
cells with the same `(fg, style_bits)` into a single `TextRun`. A
ligature whose characters share a style token (the common case for
operators: `->`, `!=`, `=>`, `<=`, `//`, `...`, `===`) lands in one
`TextRun` and the shaper sees the sequence → ligature fires.

**Ligatures that span style or cursor boundaries do not render.**
The cursor character always gets a distinct bg/fg override (its own
`TextRun`), so a ligature touching the cursor position breaks. This
matches VSCode, Helix, Zed, and Neovim behaviour — universally
accepted and visually correct (the cursor is ON a specific character).

No special handling of run-splitting is planned. The natural
coalescing already covers the overwhelmingly common cases.

---

## Column geometry

Programming ligature fonts (Fira Code etc.) are designed so each
ligature occupies the same total columns as the constituent
characters (`->` = 2 columns whether rendered as two glyphs or one).
`glyph_advance` (measured from `"M"` via `shape_line`) therefore
stays correct. The cell model (`CellMatrix`) does not change.

---

## Option design

`ui.ligatures` (bool, **default `true`**). Following cross-editor
convention (VSCode, Zed, Helix, Neovim: ligatures on by default when
the font supports them). `false` calls `font.features.disable_ligatures()`
before handing the `Font` to every `TextRun`.

The TUI renderer ignores this option — terminal ligatures are
controlled by the terminal emulator's font settings.

---

## Rendering path — active pane hybrid

The active-pane document body uses a hybrid paint strategy gated on
`GpuiTheme.ligatures`:

**`ligatures=true` (default)**
1. Background quads — `paint_cells_row_bg_only(seg_cells, ...)` reads
   per-cell bg colours from the cell matrix so syntax-token backgrounds
   (selection, matched-bracket tints, etc.) render correctly.
2. Glyph paint — `ShapedLine::paint(...)` renders the pre-built shaped
   line. The shaped line was built in `prepaint` from coalesced multi-char
   `TextRun`s via `display_line_to_text_runs`; adjacent same-style chars
   land in one run, HarfBuzz sees the sequence and the ligature forms.

**`ligatures=false`**
`paint_cells_row(seg_cells, ...)` handles both bg quads and glyphs
(per-char `paint_glyph` via `GlyphResolver`). No ligature sequences
form because each char is resolved independently.

Inactive panes and fall-through rows always use `ShapedLine::paint`
regardless of the flag (their cell matrix is absent).

---

## Implementation touch surface

| Location | Change |
|----------|--------|
| `lattice-host/src/ui/theme_options.rs` | Add `UiLigatures: bool = true` |
| `lattice-ui-gpui/src/lib.rs` (`GpuiTheme`) | Add `pub ligatures: bool` |
| `lattice-ui-gpui/src/lib.rs` (`rebuild_gpui_theme`) | Read `UiLigatures`, set `self.theme.ligatures` |
| `lattice-ui-gpui/src/editor_element.rs` (prepaint) | Apply `FontFeatures::disable_ligatures()` when `!self.theme.ligatures` |
| `lattice-ui-gpui/src/editor_element.rs` (paint) | Hybrid path: `paint_cells_row_bg_only` + `ShapedLine::paint` when `ligatures=true` |
| `lattice-ui-gpui/src/paint_cells.rs` | Add `paint_cells_row_bg_only` |
| `lattice-ui-gpui/src/window.rs` (advance measurement) | Apply `FontFeatures::disable_ligatures()` on reference font |
| `docs/user/display.md` | Add `ui.ligatures` row + ligature section |
