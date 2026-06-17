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

### T.3 — wire `ThemeRegistry` as a boot service 🗒

The trait + `InMemoryThemeRegistry` + `ElementId` interning +
`ArcSwap<ResolvedTheme>` already landed in T.2. T.3 narrows to:
register `InMemoryThemeRegistry::with_defaults()` in the host's
`ServiceRegistry` at boot as `ThemeRegistryHandle = Arc<dyn
ThemeRegistry>` ([[feedback_servicesregistry_arc_typeid]] — register
and look up the SAME `Arc<dyn ThemeRegistry>` type), and expose a
startup-time id-capture helper so consumers intern their `ElementId`s
once. No consumer reads it yet (the `Theme` struct still drives
rendering until T.4).

---

## Thread B — migrate builtin consumers to read-by-id

Depends on Thread A. Each slice flips one consumer set from
`theme.<field>` to `resolved.get(<element_id>)`, both renderers in
lockstep, parity-pinned against the T.2 net.

### T.4 — renderer reads the resolved table 🗒

Snapshot `Arc<ResolvedTheme>` into `RenderState` at publish (parallel
to `theme` while the migration runs), then migrate the existing
themed reads (pane chrome, file-tree, diagnostics, whitespace,
cursor-line, messages, diff signs/tints) in both renderers from
`App.theme.<field>` / `host_theme.<field>` to `resolved.get(id)`.
**Per the §10.1 decision, each `Theme` style field is deleted as its
last reader migrates — no getter shim.** Risk: highest in the plan —
touches both paint paths; sub-slice by consumer group if needed. Land
behind:

- Both-renderer parity assertion: resolved-read style output ==
  pre-migration output for the default theme.
- `multibuffer_is_a_regular_buffer.rs` stays green.
- Keystroke→glyph ratchet unmoved (design §7).

### T.5 — unify syntax styling into elements 🗒

Map `lattice_cells::Style` (semantic category) → builtin `syntax.*`
elements (`syntax.keyword`, `syntax.string`, `syntax.heading.1`, …).
`Theme::syntax_style()` becomes a resolved-table read; **delete the
hardcoded Catppuccin `match`**. Syntax highlighting now themeable
like any other element (the emacs font-lock-faces / helix-scopes
unification). Pin: highlighted output unchanged for the default
theme; the `syntax_style_*` tests retarget to the resolved read.

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

### T.10 — GPUI honors rich vocabulary 🗒

GPUI peer reads `Style.scale` / `.family` / `.weight` into
`TextRun` font shaping (variable per-run size/family — the Layer-1
capability, design §6.1). TUI degrade verified (attrs are no-ops;
bold/colour/underline still applied). First concrete demonstration:
a markdown heading element with `scale > 1.0` renders larger on
GPUI, bold on TUI. Bench: per-run font resolution stays
O(viewport-runs).

---

## Sequencing

```
A.1 → A.2 → A.3        foundation (crate, resolution, registry)
        ↓
B.4 → B.5 → B.6        migrate builtin consumers (parity-pinned)
        ↓
C.7 → C.8             extensibility (mode elements, buffer-local remap)
        ↓
D.9, D.10            surface + rich vocab (D.10 parallelisable after B)
```

Thread A is pure addition (no visual change) and lands first. Thread
B is the risk — each slice parity-pinned against T.2's
`resolved_builtins_match_legacy_literals` net, both renderers in
lockstep. Thread C delivers the user requirement (mode-registered +
overridable elements). Thread D is surface polish + the first
Layer-1 rich-rendering capability.

**Deferred** (design §11): WIT plugin registration of elements
(plugin phase); Layer 2 display/layout — variable row height,
inline media, real-component blocks (separate renderer initiative,
gated behind soft-wrap display-row model + design.md §5.6.7 Path 4);
box/overline/underline-style attributes (additive when a renderer
reads them).
