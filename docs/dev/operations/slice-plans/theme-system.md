# Theme system — Slice Plan (T)

Sequencing + status for the theme/styling redesign: element +
style + palette, resolved-once into a flat table, with mode-owned
custom elements and buffer-local remap.

- **Design:** [`theme-system.md`](../../architecture/theme-system.md)
  (element/style/palette model, ownership boundary, performance
  contract, renderer-peer asymmetry).
- **Authoritative status:** `implementation.md` `## theme-system`.

Status icons: 🗒 planned · 🚧 in progress · ✅ landed.

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

**T.5.b 🗒 (remaining)** — migrate the other **6** `syntax_style`
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

**T.5.c 🗒** — whitespace finish (the re-slice from T.4.d): migrate the
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

### T.6 — hoist scattered literals → parity by construction 🗒

Register builtin elements for the styling currently hardcoded +
**divergent** between renderers: `search.match` / `search.current`,
`selection`, `doc_highlight.read|write|text`, `substitute.preview`,
`inlay.hint`, `completion.annotation.*`. Both renderers read them by
id. Closes the TUI(`Cyan`)/GPUI(`0x6c7086`)-style drift the audit
found — parity is now structural, not maintained by hand. Pin: a
test enumerating these elements asserts both renderers resolve the
same `Style`.

### T.6.t — dismantle `Theme` (non-style → `ui.*` + delete) 🗒

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

### T.7 — mode-registered elements + multibuffer header 🗒

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

### T.8 — buffer-local element remap 🗒

Wire theme resolution into the `FrameView::for_buffer` per-buffer
resolution stack ([[project_per_buffer_options_direction]],
`buffer-local-options.md` §3) as the highest-priority layer (design
§5). A mode declares element remaps for its own buffers; resolution
produces a **per-buffer resolved table** (overlay over the global
table) so the read stays O(1). This is the emacs `face-remap`
analogue — the seam that lets markdown/org restyle *their* buffers
without touching global state. Pin: a remap in buffer A does not
change buffer B's resolved table; read stays index-based.

---

## Thread D — surface + rich vocabulary

### T.9 — config + introspection 🗒

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

### T.10 — GPUI honors rich vocabulary 🗒

GPUI peer reads `Style.scale` / `.family` / `.weight` into
`TextRun` font shaping (variable per-run size/family — the Layer-1
capability, design §6.1). TUI degrade verified (attrs are no-ops;
bold/colour/underline still applied). First concrete demonstration:
a markdown heading element with `scale > 1.0` renders larger on
GPUI, bold on TUI. Bench: per-run font resolution stays
O(viewport-runs).

---

## Thread E — multi-theme library + picker

Depends on T.9 (the `:colorscheme` swap + named-theme registry).
Lands last, once every builtin element resolves through the palette
(T.4–T.6) so a palette swap re-colors the whole surface.

### T.11 — migrate ~5 popular cross-editor themes 🗒

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

### T.12 — theme picker 🗒

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
