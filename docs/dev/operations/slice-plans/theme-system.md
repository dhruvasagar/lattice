# Theme system — Slice Plan (T)

Sequencing + status for the theme/styling redesign: element +
style + palette, resolved-once into a flat table, with mode-owned
custom elements and buffer-local remap.

- **Design:** [`theme-system.md`](../../architecture/theme-system.md)
  (element/style/palette model, ownership boundary, performance
  contract, renderer-peer asymmetry).
- **Authoritative status:** `implementation.md` `## theme-system`.

Status icons: 📝 planned · 🚧 in progress · ✅ landed.

Every renderer-touching slice updates **TUI + GPUI in the same
patch** ([[feedback_tui_gpui_parity]]). Every slice ships its
doc/bench/test trio (CLAUDE.md four-artefacts rule). The
load-bearing invariant across the whole plan: **the per-glyph read
stays O(1) `resolved[id]`** — no slice may move resolution onto the
hot path (design §7).

---

## Thread A — foundation (`lattice-theme` crate + resolution)

### T.1 — extract `lattice-theme`, grow `Style` vocabulary ✅

Create `crates/lattice-theme`. Move `Color` / `Style` / `Modifiers`
/ `NamedColor` / `to_rgb_u32` / `parse_color` out of
`lattice-host/src/ui/theme.rs` into it; `host_theme` re-exports them
so every existing call site compiles unchanged. Grow `Style` with
additive `scale: Option<FontScale>` / `family: Option<FamilyId>` /
`weight: Option<Weight>` (all `None` by default — no behavior
change; TUI ignores them, GPUI reads them in T.10). Dep edges:
`lattice-theme → {}` (leaf); `{lattice-host, lattice-mode,
lattice-cells, lattice-ui-tui, lattice-ui-gpui} → lattice-theme`.

Pure relocation + additive fields. Green with zero visual change.

**Landed:** `lattice-theme` leaf crate created (zero workspace
deps); primitives moved + re-exported from `lattice_host::ui::theme`;
`Theme` / `syntax_style` / `Default` stay in host. Rich-vocab scale
is **fixed-point `FontScale(u16)` not `f32`** — `Style` must stay
`Eq + Hash` for the `MatrixVersion::theme` content-hash fold (design
§3.4 corrected). 12 theme-crate tests + host/TUI theme tests green
(incl. the `host_theme_default_adapts_to_tui_theme_default` round-trip
pin); host + TUI + GPUI all build unchanged.

### T.2 — palette + `StyleSpec` + `ColorRef` + resolution ✅

Add `Palette`, `PaletteKey`, `StyleSpec`, `ColorRef`, `ModifierSet`,
`ResolvedTheme`. Implement build-time resolution: inherit-chain walk
+ dotted-name fallback + palette lookup → concrete `Style`. Author
the default **Catppuccin Mocha palette** + the builtin element set
with palette-referencing `StyleSpec` defaults that **reproduce
today's `Theme::default()` + `syntax_style()` values exactly**.

Pin: `resolved_builtins_match_legacy_literals` — every builtin
element's resolved `Style` equals the current hardcoded value
(byte-for-byte the existing `Theme::default()` field / `syntax_style`
arm). This is the safety net for Thread B.

**Landed (all in `lattice-theme`):** `element.rs` (identity +
`StyleSpec`/`ColorRef`/`ModifierSet`, dotted `ElementName`), `palette.rs`
(Catppuccin accents + `ansi.*` chrome + tints; a bare `&str` in a spec
means a palette-key ref, not a literal — reference-not-absolute by
default), `registry.rs` (resolution + the ~52 builtin elements + the
parity pin). 24 tests green incl. the parity pin, inherit
(`syntax.line_comment` inherits `syntax.comment`), dotted fallback,
unknown-key-logs-not-panics, and palette-swap-rebuilds. **Re-carve:**
the `ThemeRegistry` trait + `InMemoryThemeRegistry` + `ElementId`
interning + `ArcSwap<ResolvedTheme>` landed here too (front-loaded from
T.3), so T.3 narrows to ServiceRegistry wiring + boot.

### T.3 — wire `ThemeRegistry` as a boot service ✅

The trait + `InMemoryThemeRegistry` + `ElementId` interning +
`ArcSwap<ResolvedTheme>` already landed in T.2. T.3 narrowed to:
register `InMemoryThemeRegistry::with_defaults()` in the host's
`ServiceRegistry` at boot as `ThemeRegistryHandle = Arc<dyn
ThemeRegistry>` ([[feedback_servicesregistry_arc_typeid]] — registered
and looked up under the SAME `Arc<dyn ThemeRegistry>` type).

**Landed:** one registration in `editor_boot.rs`'s `ServiceRegistry`
block; host builds green. No consumer reads it yet (the `Theme`
struct still drives rendering until T.4). The startup-time
id-capture helper folds into T.4 where its first consumer lands (no
speculative API ahead of a reader).

---

## Thread B — migrate builtin consumers to read-by-id

Depends on Thread A. Each slice flips one consumer set from
`theme.<field>` to `resolved.get(<element_id>)`, both renderers in
lockstep, parity-pinned against the T.2 net.

### T.4 — renderer reads the resolved table ✅ (a–d landed; 2 groups re-sliced)

**Landed:** T.4.a–d migrated diagnostics, diff signs+tints,
inactive-pane overlay, file-tree, cursor-line, and *messages* to the
resolved table across both renderers (TUI cache preserved + repointed;
GPUI inline). Two groups re-sliced out (clean sequencing, no
architectural change): `pane.status.*` / `pane.separator` → **T.9**
(carry live `:set ui.*` overrides needing the registry-override path);
whitespace → **T.5** (read in the cell builder, which gets the resolved
table there). Host `Theme` now holds only those deferred groups + the
non-style glyphs/chars/flags (T.6.t).

Snapshot `Arc<ResolvedTheme>` + a `Copy` `BuiltinElementIds` (interned
once at boot from the registry handle) into `RenderState` at publish
(parallel to `theme` while the migration runs), then migrate the
existing themed reads (pane chrome, file-tree, diagnostics,
whitespace, cursor-line, messages, diff signs/tints) in both
renderers to the resolved table. **Per the §10.1 decision, each host
`Theme` style field is deleted as its last reader migrates — no
getter shim.**

**Read shape (design §10.1, decided 2026-06-18 — keep the cache).**
The TUI keeps its native ratatui `Theme` cache (`App.theme`); the
migration **repoints its builder** from `From<&host Theme>` to
`from(resolved, ids)` and rebuilds it only when
`ResolvedTheme::version()` changes — the per-frame hot path keeps
reading pre-adapted ratatui styles (no per-read
`host_style_to_ratatui`). The GPUI peer has no such cache and reads
the same `resolved`/`ids`, adapting inline via `to_rgb_u32` as it does
today. Host `Theme` still dies; the resolved table is the single
source of truth.

Risk: highest in the plan — touches both paint paths. Sub-sliced by
consumer group, each green + parity-pinned, both renderers in
lockstep:

- **T.4.a** ✅ — plumbing + diagnostics (first consumer): added
  `BuiltinElementIds` + `capture` at boot + snapshot
  `Arc<ResolvedTheme>`/`theme_ids` into `RenderState` (the handle is
  looked up from `services` in `build_render_state` — `Arc<dyn
  ThemeRegistry>` has no `Default`, so it can't be an `Editor` field).
  TUI's `From<&Theme>` cache builder became `build_tui_theme(host,
  resolved, ids)` sourcing `diagnostic.{error,warning,info,hint}` from
  the resolved table; GPUI reads them inline. Deleted the 4 host
  `diagnostic_*_style` fields (TUI native cache keeps its fields,
  sourced from resolved). Glyphs stay (non-style → T.6.t). Parity:
  `builtin_ids_capture_resolves_diagnostics_to_legacy` (lattice-theme)
  + the retargeted `host_theme_default_adapts_to_tui_theme_default`.
  Green: theme 26, TUI theme 9 / lib 1475, host lib 730,
  multibuffer-is-a-regular-buffer 14; GPUI `--features window` builds.
- **T.4.b** ✅ — diff signs + tints (`diff.*.sign` ×4, `diff.*.line` /
  `diff.deletion_block` ×4). Grew `BuiltinElementIds` + `capture` with
  the 8 diff ids; deleted the 8 host `Theme` diff fields. TUI
  `build_tui_theme` sources signs (fg) via `resolved_style` + tints
  (bg) via a new `resolved_bg` closure (`Reset` if unresolved); GPUI
  window.rs reads the 3 sites (gutter signs, line tints, deletion
  block) inline from `resolved.get(ids.diff_*)`. Parity:
  `builtin_ids_capture_resolves_diff_to_legacy`. Green: theme 27, TUI
  lib 1475, host lib 730, multibuffer 14; GPUI `--features window`.
- **T.4.c** ✅ (partial — see re-slice) — `pane.inactive_overlay` +
  file-tree (`file_tree.{dir,hidden,file}`). Grew `BuiltinElementIds` +
  `capture` (4 ids); deleted the 4 writer-free host fields; TUI
  `build_tui_theme` sources them from resolved; GPUI unaffected (it
  reads no host chrome). Parity:
  `builtin_ids_capture_resolves_chrome_to_legacy`. Green: theme 28,
  TUI lib 1475, host lib 730.
  **Re-slice:** `pane.status.active` / `pane.status.inactive` /
  `pane.separator` carry live `:set ui.*` fg overrides
  (`sync_host_theme_from_config` writes them from
  `ui.statusline_active_fg` / `_inactive_fg` / `separator_color`).
  Migrating them needs the registry per-element override path, which
  is **T.9**'s designated work (§8) — so they stay on host `Theme` +
  their `:set` writers until T.9 wires the overrides to the registry
  and moves the reads. Avoids pulling T.9 substrate into T.4 (heuristic
  #1: no premature override API); each element stays wholly on one path.
- **T.4.d** ✅ (partial — see re-slice) — cursor-line
  (`editor.cursor_line`) + messages (`messages.*` ×6). Grew
  `BuiltinElementIds` + `capture` (7 ids); deleted the 7 host fields
  (`cursor_line_bg` + 6 `messages_*_style`); TUI `build_tui_theme`
  sources them from resolved (cursor via `resolved_bg`, messages via
  `resolved_style`); GPUI window.rs migrates the one cursorline read
  inline. Parity:
  `builtin_ids_capture_resolves_cursorline_and_messages_to_legacy`.
  **Re-slice:** whitespace (`whitespace`, `whitespace.trailing`)
  moves to **T.5** — `whitespace_trailing_style` is read host-side in
  the **cell builder** (`cells_worker.rs`), which gets the resolved
  table only when T.5 wires the cell-grid path; migrating it now would
  pull that plumbing onto the per-cell hot path early. Whitespace
  stays a unit with the cell-path slice.

Land behind:

- Both-renderer parity assertion: resolved-sourced style output ==
  pre-migration output for the default theme (rides the T.2
  `resolved_builtins_match_legacy_literals` net).
- `multibuffer_is_a_regular_buffer.rs` stays green.
- Keystroke→glyph ratchet unmoved (design §7); TUI cache rebuild
  stays at theme-change rate (O(1) version compare per frame).

### T.5 — unify syntax styling into elements ✅ (a/b/c landed)

**Done** (`fd551230`, `fa55874f`, `e849fc2a`): syntax + whitespace now
resolve through the element table in **all** consumers — the cell
builder (`cells_worker`, T.5.a), both display-line paths (TUI
`cells_render`, GPUI `cells_paint`) + the host/popup stylers + the diff
overlay (T.5.b), and the TUI native cache (T.5.c). `Theme::syntax_style`
+ the Catppuccin `match` are **deleted**; the shared host
`resolve_syntax_style(resolved, ids, s)` + `syntax_element_id` are the
single mapping. Host `Theme` whitespace fields deleted. Parity pinned
by the existing per-renderer colour tests. Green across host / TUI /
GPUI(window).

> **Milestone:** T.4 + T.5 complete the whole builtin-consumer
> migration — every builtin style both renderers read flows through
> `ResolvedTheme`. Host `Theme` is now only non-style config (glyphs,
> separator chars, nerd_fonts/dim flags) + the 3 `:set`-backed chrome
> style fields (→ T.9 + T.6.t).

**T.5.a ✅** — the per-cell **cell builder** (`cells_worker`) reads
syntax + whitespace-trailing from the resolved table: `CellTheme {
resolved, ids }` bundle replaces the threaded `theme: &Theme`;
`resolve_style` → `resolved.get(syntax_element_id(ids, s))` (O(1));
`CellsRenderState` carries `resolved_theme`+`theme_ids`.
`BuiltinElementIds` grew the 25 `syntax.*` + 2 whitespace ids. Commit
`fd551230`; green (theme 30, host lib 730, GPUI lockstep). Parity
pinned by the existing cells_worker colour tests.

**T.5.b 📝 (remaining)** — migrate the other **6** `syntax_style`
consumers to the resolved table, then **delete `Theme::syntax_style`**
+ retarget its `theme.rs` tests:
- TUI `cells_render::display_line_to_source_spans` (+ `render.rs:5703`
  styler).
- GPUI `cells_paint::display_line_to_text_runs` (+ `editor_element:88`,
  `window:100` stylers).
- host `diff/overlay.rs` `SyntaxContext` (315/428).
- Add a shared host `pub fn resolve_syntax_style(resolved, ids, s) ->
  Style` (+ pub `syntax_element_id`) so all consumers + `cells_worker`
  call one mapping; thread `resolved`+`ids` from the display-line
  callers (`render.rs:3634`, `editor_element:609`) + `SyntaxContext`.

**T.5.c 📝** — whitespace finish (the re-slice from T.4.d): migrate the
display-line trailing-ws reads + `build_tui_theme` (native cache) to
resolved; delete the 2 host `Theme` whitespace fields.

Map `lattice_cells::Style` (semantic category) → builtin `syntax.*`
elements (`syntax.keyword`, `syntax.string`, `syntax.heading.1`, …).
`Theme::syntax_style()` becomes a resolved-table read; **delete the
hardcoded Catppuccin `match`**. Syntax highlighting now themeable
like any other element (the emacs font-lock-faces / helix-scopes
unification). Pin: highlighted output unchanged for the default
theme; the `syntax_style_*` tests retarget to the resolved read.

**Also carries the T.4.d whitespace re-slice:** this slice wires the
resolved table + ids into the cell-grid path (`CellsRenderState` →
`cells_worker`), so it additionally migrates `whitespace` +
`whitespace.trailing` (the cell builder reads
`whitespace_trailing_style` host-side) and deletes the last 2 host
`Theme` whitespace fields. The host `Theme` struct is then down to the
3 `:set`-backed chrome fields (T.9) + the non-style glyphs/chars/flags
(T.6.t).

### T.6 — hoist scattered literals → parity by construction ✅

Register builtin elements for the styling currently hardcoded +
**divergent** between renderers: `search.match` / `search.current`,
`selection`, `doc_highlight.read|write|text`, `substitute.preview`,
`inlay.hint`, `completion.annotation.*`. Both renderers read them by
id. Closes the TUI(`Cyan`)/GPUI(`0x6c7086`)-style drift the audit
found — parity is now structural, not maintained by hand. Pin: a
test enumerating these elements asserts both renderers resolve the
same `Style`.

### T.6.t — dismantle `Theme` (non-style → `ui.*` + delete) ✅

Capstone of Thread B (design §10.1). Runs once T.4–T.6 have removed
every *style* reader of `Theme`. Migrate the remaining **non-style**
fields to the typed options system, then delete `Theme`:

- New `ui.*` options for the data with no option today: the 4
  `diagnostic_*_glyph` chars + `pane_separator_horizontal`
  (vertical already has `ui.separator`). Dashed-namespaced names
  ([[feedback_naming_dashed]]).
- Repoint consumers of `nerd_fonts` / `dim_inactive_panes` /
  separators / glyphs to read the resolved option (via
  `FrameView::for_buffer` / config), not `Theme`.
- Delete `Theme`, the TUI `From<&Theme>` adapter, and the
  `host_theme_default_adapts_to_tui_theme_default` round-trip pin
  (superseded by the per-element parity pin).
- Replace the `Theme` content-hash folded into
  `MatrixVersion::theme` with `ResolvedTheme::version()` (design §7).

Pin: glyphs/flags/separators render identically pre/post; cells
rebuild on a palette/option change via the version bump. TUI + GPUI
in lockstep.

---

## Thread C — extensibility (the requirement)

Depends on Thread B (registry live + builtin reads migrated).

### T.7 — mode-registered elements + multibuffer header ✅

Expose `register` to modes via `ThemeRegistryHandle`. First real
consumer: `MultibufferMode` registers
`multibuffer.excerpt_header` (+ `.path` / `.count`) with
palette-referencing defaults; the header renderer reads them by id.
**Delete the bespoke `multibuffer_header_*` host_theme fields** (the
ones `multibuffer-views.md` §3.8 / MH.A1 introduce) — they are
subsumed. Proves the §4 ownership boundary end-to-end: a mode
contributes elements + defaults, host adds **zero** `Style` fields,
renderer adds **zero** match arms.

> **Cross-plan dependency (MH.A1):** the multibuffer-header-polish
> plan's MH.A1 adds `multibuffer_header_*` to `host_theme`. To avoid
> building-then-deleting, sequence so MH's header *content* work
> (MH.A2/A3 icon+path+count cells) proceeds independently, but the
> header *backdrop/segment colours* route through T.7's registered
> elements rather than MH.A1's bespoke fields. If MH.A1 lands first,
> T.7 migrates it; preferred is MH.A1 waits for T.7's API.

### T.8 — buffer-local element remap ⏸ DEFERRED (no consumer yet)

**Deferred 2026-06-18 — lands with its first consuming mode, not
speculatively.** This is the third override scope (§5, the emacs
`face-remap` analogue) and traces to the original "modes/plugins
override builtins wherever applicable" requirement. But **nothing
consumes it today**: no mode performs a per-buffer remap, and the
canonical consumer — markdown/org scaling/​restyling *its own* buffer's
headings — needs **T.10** (rich vocab) to be visible plus a markdown
major mode that opts in. Building the remap mechanism now is
abstraction-ahead-of-a-reader (CLAUDE.md standing rule). When a mode
needs per-buffer styling, T.8 lands **with** it so the mechanism is
validated by a real use.

Plan when it lands: a mode declares element remaps for its own
buffers; resolution overlays the remap on the global resolved table
**at content-build time** (the established bake-color path — consistent
with T.5/T.7, keeps the read index-based + off the paint path), keyed
through the existing per-buffer resolution machinery (`BufferLocals` /
`recompute_options_for_buffer`). Pin: a remap in buffer A does not
change buffer B's baked colors.

---

## Thread D — surface + rich vocabulary

### T.9 — config + introspection ✅ (T.9.a–d landed)

**T.9.a ✅** (`2ba3ba2b`) — per-element **override API** (design §5.1):
`ThemeRegistry::set_override(name, spec)` overlays a spec's set fields
on an element's resolved default (`apply_overlay`); `set_theme(palette,
overrides)` for the `:colorscheme` swap (T.9.b). The 3 `:set`-backed
chrome fields migrated off host `Theme` onto overrides
(`sync_host_theme_from_config` writes `pane.separator` /
`pane.status.{active,inactive}` overrides); host `Theme` is now
**non-style config only**. Tests: set_override / set_theme.

**Remaining:**
- **T.9.b** — `:colorscheme <name>` ex-command + a named-theme registry
  (`builtin_themes()` in lattice-theme; ≥2 themes, e.g. Catppuccin
  Mocha default + Latte) calling `registry.set_theme(...)` + emitting
  `RendererSignal::ThemeChanged`. (Ex-command wiring lives in
  `lattice-grammar::ex_commands::populate` + dispatch.)
- **T.9.c** — grow `parse_color` (lattice-theme/src/lib.rs:324) to
  accept `#rrggbb` hex (now unblocked — palette is the indirection).
- **T.9.d** — `:describe-element` / `:customize` buffer-backed views
  (owner + resolved style + inherit chain). Larger help-view feature.
  - **T.9.d follow-up (landed):** `<Tab>` completion for `:describe-element`
    / `:describe-face`. `ThemeRegistry::element_names()` feeds a `gen:elements`
    host completion generator (`host_generators::ElementsGenerator`, registered
    in `editor_boot.rs`), pointed at by the ex-command's `ArgSpec.completion`.
    Closes the deferred completion gap; the `:describe-plugin-api` `gen:plugin-apis`
    generator remains the same-shaped open follow-up.

- `:colorscheme <name>` swaps active `(Palette, overrides)`; a
  named-theme registry (≥2 builtin themes to exercise the swap).
- Palette + builtin overrides via `:set ui.*` and user TOML; grow
  `parse_color` to accept hex (now unblocked — palette is the
  indirection point).
- `:describe-element` / `:describe-face` buffer-backed view (owner +
  resolved style + inherit chain); `:customize` over palette +
  overrides (design.md §5.11/§5.12). Falls out of the registry —
  elements are introspectable data.
- **Carries the T.4.c re-slice:** wire the existing `:set
  ui.statusline_active_fg` / `ui.statusline_inactive_fg` /
  `ui.separator_color` options to per-element overrides of
  `pane.status.active` / `pane.status.inactive` / `pane.separator`,
  then migrate those reads off host `Theme` (the last 3 host style
  fields besides syntax). `sync_host_theme_from_config` stops writing
  them to `Theme` and writes registry overrides instead.

### T.10 — GPUI honors rich vocabulary ✅ (weight; scale → Thread F)

**Landed (`1eba928e`):** GPUI peer reads `Style.weight` into per-run
`TextRun.font.weight` (finer than the bold bool); demo = heading
weights (h1 ExtraBold, h2/h3 Bold) render heavier on GPUI, degrade to
bold on TUI. **Constraint found:** this gpui's `TextRun` has NO
per-run font size (`shape_line` takes one per-LINE size), so per-run
`scale` is impossible and was left wired-but-inert. Per-LINE heading
sizing = variable row height = **Layer 2**, carried to **Thread F**
(the scale half of T.10's original "renders larger on GPUI" goal lands
there). `family` deferred (no renderer font table).

---

## Thread F — Layer-2 rich display (scaled text rows)

Amends design §6.2 (2026-06-19): the smallest, most tractable front of
Layer 2 — **variable row height for scaled text** — is pulled into
scope; the rest of Layer 2 (inline media, reflow, replaced/hidden
markup, real-component blocks) stays deferred behind `design.md`
§5.6.7 Path 4. GPUI-only by physics; the TUI degrades (a fixed cell
grid cannot vary font size — headings stay bold+colored+underlined).
First consumer: markdown headings — per-level **size** (`scale`) +
per-level **colour** (`fg`), titles scaled but the leading `#` markers
left base-size (the emacs `markdown-header-delimiter-face` convention).

**Ownership (decided 2026-06-19 — Reading A, "headings are core syntax
substrate"):** `Style::Heading1..6` is part of the universal syntax
vocabulary (like `Keyword`/`Comment`), so `syntax.heading.1..6` are
**core** theme elements (`ElementOwner::Core`) carrying the per-level
scale+colour defaults; the markdown-specific bit (distinguishing heading
LEVELS) lives in the **markdown grammar query**, where every language's
queries live. `MarkdownMode` (the major-mode object) registers nothing
here — correct, because headings aren't markdown-private. **This is
reused verbatim by future org/AsciiDoc**: same core `syntax.heading.*`
elements + `Style::HeadingN`, each grammar contributing only its own
`text.title.N` query mapping. (Reading B — `MarkdownMode` registering
`markdown.heading.*` — was rejected: it needs a per-language
`syntax_element_id` indirection for zero visible gain until a second
mode wants *different* heading styling. See design §4 + the
substrate-vs-mode rule.)

### F.3 — heading `scale` defaults ✅

Per-level `scale` on `syntax.heading.1..6` (h1 1.6 → h6 1.05, emacs
`:height`), mirroring how T.10 added `weight`. **Option A** (core
syntax-element default, NOT T.8 buffer-local remap, confirmed
2026-06-19): Heading tokens are markdown-exclusive, so a core default
is effectively buffer-local already; T.8 stays deferred for true
per-buffer divergence (variable-pitch prose, org with other scales).
Tests: `heading_builtins_carry_descending_scale` + the resolved-parity
pins grow the scale. (lattice-theme)

### F.2 — GPUI variable row height, title-only ✅

GPUI peer renders a heading row at variable height with **only the
title scaled** — the leading `#`/`##` markers stay base-size (emacs
convention; gpui's `shape_line` is one-size-per-line, so a scaled
line is split into base-prefix + scaled-title pieces sharing one
baseline). `cells_paint::heading_scale_split(line)` → `(prefix_cols,
title_scale)` (O(runs)). Prepaint carries `row_scale` (row height
multiplier = title scale) + a per-row `HeadingSplit` (the pre-shaped
prefix/title for the fallback path), both 1:1 with `shaped_text`
(`shaped_text` itself stays base-size). Paint builds cumulative per-row
tops + a per-column `col_x`/`col_scale` (advance is non-uniform within
a heading row), replacing uniform `line_height * i` / `advance * col`
at every site (cursorline, overlays, gutter, text body, diagnostics,
cursor):
- **active cell path**: `paint_cells_row` called twice per heading row
  (prefix at base advance/font, title at `× title_scale`) with a shared
  `ascent` so baselines align;
- **fallback path** (inactive/folded/ligatures-glyph): the pre-shaped
  prefix + title painted side-by-side, prefix `origin.y` shifted by the
  gpui baseline formula so both paths look identical → focus-stable
  ([[feedback_decorations_update_in_place]]).

The gutter stays base-size, centered in the taller row. With no split
(ordinary rows) the arithmetic is **byte-identical** to pre-F.2. Rows
past the pane bottom are clipped (no modeline bleed). The **host scroll
model is unchanged** — a heading is one logical display row, painted
taller. Tests: `heading_scale_split_reports_prefix_and_title_scale` +
`_counts_inlay_prefix_as_base`; 108 GPUI lib tests green. **Bench:** no
dedicated harness — zero per-glyph work added (one O(rows) prepaint
pass); the keystroke→glyph ratchet guards paint cost. (lattice-ui-gpui)

### F.5 — distinguish heading LEVELS in the markdown grammar ✅

The blocker behind both per-level size AND per-level colour: the
bundled `tree_sitter_md` block query captures headings level-less
(`(atx_heading (inline) @text.title)` → all map to `Heading1`), so
every heading rendered uniform 1.6×/red regardless of `#` count. Fixed
with a **custom markdown block highlights query**
(`crates/lattice-syntax/queries/markdown/highlights.scm`, wired in
`registry.rs` as `MARKDOWN_HIGHLIGHTS_QUERY`) that captures
`(atx_heading (atx_hN_marker) (inline) @text.title.N)` per level — the
atx marker nodes are distinct, so each title resolves to its
`Heading1..6` element and picks up that level's scale (F.3) + colour
(T.2 defaults: h1 red · h2 peach · h3 yellow · h4 green · h5 blue · h6
mauve). The level-less `text.title` stays (→ Heading1) for setext
headings. Verified by GPUI screenshot + pixel-sampling (distinct hue +
descending height per level). Test:
`native_markdown_headings_emit_heading_styles` now asserts
Heading1/2/3 distinct. **Org/AsciiDoc reuse this directly** — same core
elements, each grammar maps its headings to `text.title.N`.
(lattice-syntax)

### F.1 — host-side viewport-height accuracy ⏸ DEFERRED

First cut keeps host `viewport_height` a uniform-height estimate, so
tall on-screen headings let GPUI fit slightly fewer rows than the host
assumes (last row clipped, not bled). Lands if the clip is visibly
annoying: make the host height-aware, or have GPUI feed back true
capacity. Per-line scale is computed renderer-side (the `DisplayMatrix`
carries semantic `Style` enums; resolution to a concrete `scale`
happens at the renderer boundary), so no host `DisplayLine` field is
needed for the first cut.

**Deferred beyond Thread F** (design §6.2 "rest of Layer 2"): inline
media, proportional reflow, replaced/hidden markup, real-component
blocks — gated behind `design.md` §5.6.7 Path 4. Proportional
`family` for prose body needs a renderer font table (T.10 flagged it).

---

## Thread E — multi-theme library + picker

Depends on T.9 (the `:colorscheme` swap + named-theme registry).
Lands last, once every builtin element resolves through the palette
(T.4–T.6) so a palette swap re-colors the whole surface.

### T.11 — multi-theme library ✅ (21 themes; scope grew 5 → 18)

**Landed.** Shipped **21 named themes**: Catppuccin (mocha/macchiato/
latte) + **9 cross-editor families × {dark, light}** (gruvbox,
tokyonight, dracula, nord, solarized, one, everforest, rosepine,
monokai). Decisions made during execution:
- **T.11.0a — generic role-key vocabulary** (`mauve→purple`,
  `peach→orange`, `sapphire→cyan`, `overlay0→overlay`,
  `overlay2→subtext`): the deferred sub-decision below, resolved YES —
  with 18 diverse palettes the Catppuccin-specific keys fought; the
  rename is parity-pinned (resolved colours byte-identical).
- **T.11.0b — canvas palette-driven**: added a `base`/`mantle`/`crust`/
  `surface0..2` background family + `editor.background`/`.foreground`/
  `.cursor` + `ui.popup.background` core elements, wired into
  `rebuild_gpui_theme`. REQUIRED for light themes (the canvas was
  hardcoded dark; a swap never recoloured it). TUI canvas stays the
  terminal's (renderer-peer asymmetry).
- **catalog** (`register_theme`/`theme_names`/`apply_theme` on the
  registry, seeded at boot) — the seam `:colorscheme`, the T.12 picker,
  and `init.rs`/plugins all use.
- Pins: `every_builtin_theme_covers_the_full_role_key_set` (no
  fallback) + `light_themes_are_light_and_dark_themes_are_dark`.
  Verified visually (pixel-sampled) on Latte + gruvbox-light.
- Tuning welcome (completeness guaranteed, hue approximate):
  nord-light / monokai-light (synthesized — no official light),
  dracula-light (Alucard direction), rosepine green/teal/cyan→foam.

Original plan (for reference):

Each theme is a `(Palette, element-overrides)` pair (design §2/§8) —
mostly a `Palette` that fills the **same key vocabulary** the
builtins reference (`text`, `overlay0`, `green`, `mauve`, `red`,
`ansi.*`, the tints …), so swapping it re-colors everything with zero
per-theme element wiring. Ship the default **Catppuccin Mocha** (T.2)
plus five well-known cross-editor palettes, chosen for breadth and
recognizability ([[feedback_editor_design_references]] — weight
broadly; all are cross-editor staples):

- **Gruvbox Dark** (vim origin) · **Tokyo Night** (neovim/Zed) ·
  **Dracula** (cross-editor classic) · **Nord** · **Solarized Dark**.

Each lives as a `fn <name>_palette() -> Palette` + registration in the
named-theme registry. Light variants (Latte / Solarized Light) are a
follow-on, not in the five.

**Sub-decision deferred to this slice:** the palette-key vocabulary is
currently Catppuccin-flavored (`mauve`, `peach`, `sapphire`, `maroon`,
`pink`). Migrating non-Catppuccin themes may want **generic
role-named keys** (`purple`/`orange`/`cyan` or `accent.*`) so each
theme maps cleanly to its nearest color. Revisit when T.11 executes —
rename the vocabulary if the Catppuccin-specific names fight the other
palettes (this is a vocabulary refactor across the builtins +
default palette, parity-pinned).

Pin: each theme resolves every builtin element to *some* color (no
unknown-palette-key warnings); a golden per theme over a
representative element set.

### T.12 — theme picker ✅ T.12a (live preview) · ⏸ T.12b (persist)

**T.12a landed.** `:colorscheme` with no arg opens a buffer-backed
picker over `theme_names()`; arrowing live-previews each (real recolor
via `apply_theme` + `ThemeChanged`); `<Esc>` restores the theme active
at open (`set_theme` of the `active_theme()` snapshot captured on first
preview, stored in `Editor::pending_theme_preview_restore`); `<CR>`
keeps it. Built via approach (A) — the trait-driven
`PickerSourceGenerator` path: a new default-no-op `preview()` hook on
the trait, `PickerAcceptOutcome::ApplyColorscheme` + `RoutingPayload::
Colorscheme`, and a host `ThemePickerSource` (holds `ThemeRegistryHandle`)
registered into the boot `PickerRegistry`. Theme logic lives in the
source, not the host (the mode-ownership analog for pickers, which are
typed-source host overlays — there is no separate picker "mode"). 3 new
tests (opens on no-arg; preview recolors + dismiss restores; accept
keeps). Both renderers untouched (host-internal outcome; only the
existing `ThemeChanged` signal crosses).

**T.12b — persist the chosen theme across restarts ⏸ DEFERRED.** No
general user-TOML write-back exists (only small dedicated state files:
picker-MRU, tutor scores). Persistence needs a new small state-file
path (precedent exists); follow-on.

Original plan (for reference):

A buffer-backed picker (the existing `lattice-picker` surface, same
as `:b` / file picker) listing every registered theme. **Live
preview on highlight** — the cross-editor convention (VSCode / Zed /
neovim `telescope` colorschemes all preview the theme as you move the
selection; [[feedback_convention_first]]): moving the cursor applies
the palette via the T.9 swap so the user sees the real editor recolor;
`<Esc>` restores the prior theme, `<CR>` commits + persists to user
TOML. Triggered by `:colorscheme` with no argument (with-argument
stays the direct swap from T.9). Owned by a small theme-picker mode in
the theme/host surface — keymap + on-highlight handler live with the
mode ([[feedback_mode_owns_its_surface]]), not the host.

Pin: highlight-preview swaps the resolved table (palette version
bumps, cells rebuild); `<Esc>` restores byte-identically; no flicker
on unedited content ([[feedback_decorations_update_in_place]]); TUI +
GPUI in lockstep.

---

## Sequencing

```
A.1 → A.2 → A.3            foundation (crate, resolution, registry)  ✅✅✅
        ↓
B.4 → B.5 → B.6 → B.6.t    migrate consumers + dismantle Theme (parity-pinned)
  B.4 ✅ (a–d; pane.status/separator → D.9, whitespace → B.5)
        ↓
C.7 → C.8                 extensibility (mode elements, buffer-local remap)
        ↓
D.9, D.10                 surface + rich vocab (D.10 parallelisable after B)
        ↓
E.11 → E.12               multi-theme library + live-preview picker (after D.9)
```

Thread A is pure addition (no visual change) and lands first (T.1/T.2
✅; T.3 ✅ registers the boot service). Thread B is the risk — each
slice parity-pinned against T.2's
`resolved_builtins_match_legacy_literals` net, both renderers in
lockstep, ending with the Theme teardown (T.6.t). Thread C delivers
the user requirement (mode-registered + overridable elements). Thread
D is surface polish + the first Layer-1 rich-rendering capability.
Thread E (last) ships the migrated theme library + the picker —
needs T.9's swap mechanism + a fully palette-resolved surface.

**Deferred** (design §11): WIT plugin registration of elements
(plugin phase); Layer 2 display/layout — variable row height,
inline media, real-component blocks (separate renderer initiative,
gated behind soft-wrap display-row model + design.md §5.6.7 Path 4);
box/overline/underline-style attributes (additive when a renderer
reads them).
