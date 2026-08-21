# Modeline redesign — slice plan

Sequencing + status for the modeline element-system redesign. **Design
contracts** live in
[`../../architecture/modeline.md`](../../architecture/modeline.md); this
file owns *when* and *in what order*.

Status legend: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

The redesign turns the per-pane status footer (today a single formatted
string from `Mode::status_line_items`) into a **registry of styled,
positioned, optionally-interactive elements** contributed by host
built-ins, modes, and (later) plugins, updated over the event bus. See
the design doc for the model, ownership rule, and rejected alternatives.

---

## Slices

ML.0 is carved into **ML.0a** (mode-facing data model + descriptor
registry — self-contained in `lattice-mode`, zero host changes) and
**ML.0b** (host content store + render snapshot + service wiring).

### ML.0a — element model + descriptor registry  ✅
**Design:** modeline.md §3, §6 (descriptor half), §9 (Interaction API).
**Landed:** `lattice-mode::modeline` — `ElementId` (namespaced = owner
key), `Zone` (Left/Center/Right), `Scope` (PaneLocal/Global),
`ModelineRole` (string theme-role key, no theme dep), `Span`,
`ElementContent` (empty ⇒ hidden; `plain()` for width/tests),
`HoverSpec`, `Interaction { on_click: Option<CommandId>, hover }`
(designed now, wired in ML.4), `ModelineElement` descriptor + builders,
and `ModelineRegistry` (register last-write-wins, remove idempotent,
`zone_ordered` — Left/Center asc, Right desc, ties by id). 7 unit tests;
`cargo test -p lattice-mode` green. Zero host/render changes.

ML.0b is carved into **ML.0b-1** (the shared service, self-contained in
`lattice-mode`) and **ML.0b-2** (host wiring — Editor field + render
snapshot + `ModeContext` accessor).

### ML.0b-1 — ModelineService + snapshot  ✅
**Design:** modeline.md §4, §5 (service half).
**Landed:** `lattice-mode::modeline::{ModelineService,
ModelineServiceHandle, ModelineSnapshot}` — registry + content each
behind `ArcSwap` (wait-free reads, lock-free updates, `Sync` so a mode's
spawned task can update from another thread); `register`/`remove`
(descriptors), `update`/`clear` (content), `snapshot()` →
`ModelineSnapshot` whose `zone()` resolves zone-ordered
`(descriptor, content)` pairs and skips empty/absent content (hidden
this frame). 3 new unit tests (10 total in the module);
`cargo test -p lattice-mode` green. Zero host changes.

### ML.0b-2 — host content store + render snapshot  ✅
**Design:** modeline.md §4, §5.
**Landed:** one `ModelineService` instance shared three ways — created
at boot, registered into the existing `services` block, and stashed on
`Editor.modeline: ModelineServiceHandle`. `build_render_state` snapshots
it into the new `RenderState.modeline_elements: ModelineSnapshot` field
(two Arc clones; distinct from the legacy `modeline` cmdline/search
sub-state). Round-trip test (`modeline_element_surfaces_through_render_state`)
proves register+update via `editor.modeline` surfaces in
`RenderState.modeline_elements` AND that the service-registry instance is
the same Arc. lattice-host (lib+tests), lattice-ui-tui, lattice-ui-gpui
all green.
**Refinement vs. original plan:** no `ModeContext` signature change. Modes
reach the handle via the existing generic `ctx.service::<ModelineServiceHandle>()`
(the handle is in the boot `services` registry) — the same pattern as
`LspSupervisorHandle` / `ThemeRegistryHandle`. Avoids threading a new
param through all 5 `ModeContext::new` callers; no Arc/TypeId footgun
(register the `…Handle` alias, look it up, deref one layer).
**Deps:** ML.0b-1. **Unblocks:** ML.1.

### ML.1 — TUI render: zones + built-ins as elements
**Design:** §3 (ordering), §4 (per-pane resolution), §7, §8.
**Locked decision (per-pane content model = option A):** descriptors are
global/uniform; content is resolved per pane — built-ins computed
host-side from `(pane, render_state)`, pushed content keyed
`(BufferId, ElementId)` for PaneLocal (lands ML.3). Amended design §4.
Also corrected `zone_ordered` to ascending in **every** zone (the Right
zone block is right-aligned by the renderer; `priority` is uniform
leftward→rightward) — design §3 + the ML.0a test updated.

Carved into **ML.1a-foundation** (✅) and **ML.1a-render** (✅).

#### ML.1a-foundation  ✅
Per-pane content model confirmed + recorded (design §4); `zone_ordered`
ascending-for-all fix + test (`zone_ordered_ascending_in_every_zone`);
modeline tests green. No renderer change.

#### ML.1a-render — built-in descriptors + TUI zone layout  ✅
**Landed:** built-in descriptors registered at boot via
`lattice_host::modeline::register_builtin_elements` (`core.mode` Left 0,
`core.path` Left 10, `core.position` Right 10, `core.lang` Right 20);
`draw_pane_status_line` rewritten to lay out Left/Center/Right from
`rs.modeline_elements.registry`, right-aligning the Right block, with
width-aware truncation (Center→Right→Left, ellipsis, saturating — never
panics; width 0 → empty). The legacy mode-items pull feeds Center
temporarily (retired ML.3). One `pane_status_*` style for the whole row;
per-role theming is ML.1b.

**Shared-content-strategy decision (load-bearing, per Dhruva 2026-06-20):**
the modeline's **content** is computed once, *host-side*, in the new
`lattice-host::modeline` module — `resolve_builtin_content(id, pane,
is_active, &RenderState, provider_label)` returns `ElementContent` (text
+ `ModelineRole`) for the `core.*` set, and `resolve_mode_items_content`
(migrated from the TUI `collect_status_line_items`) feeds Center. Both
renderers consume these; **only layout/paint differ** (TUI now; GPUI ML.2
reusing the same resolver), plus GPUI-only richness (tooltips/click,
ML.4). The single renderer-local input is the file-tree/oil/help custom
label (the M.4 provider mechanism), threaded into the resolver as a
string so the assembly stays common. `App::pane_status_label` /
`App::modal_label` now delegate to the host module (no duplicated
vocabulary). This realizes design §4 ("computed host-side") + §8/§10
(common content + roles, per-renderer paint).

**Artefacts:** *tests* — `lattice-host::modeline` (boot registers 4
descriptors; `core.mode` active-only vs `core.position` always; provider
label overrides `core.path`); `lattice-ui-tui` (`compose_modeline_row` /
`truncate_to` layout + truncation-priority + width-0/narrow no-panic;
TestBackend render asserting active = `[NORMAL]` + path + right-aligned
position, inactive omits the modal label). *bench* —
`lattice-host/benches/modeline.rs` (`modeline_build`, O(elements) across
N∈{0,8,32,128}). *error handling* — overflow truncates, never panics.
**Deps:** ML.1a-foundation.

#### ML.1b — theme roles + truncation polish  ✅
**Landed:** registered 7 `modeline.*` elements in `lattice-theme`
(`register_builtins` + `BuiltinElementIds`): `modeline.active` (bar bg
`surface1`), `modeline.inactive` (`surface0` bar + `overlay` fg, the
uniform muted inactive style), and per-role `modeline.{mode,path,
position,lang,mode_item}` (blue-bold / text / subtext / teal / subtext).
**Look (locked w/ Dhruva 2026-06-20):** colored bar + segments
(lualine/helix), active pane = raised bar with per-role fg, inactive =
receded uniform-muted bar — chosen over monochrome reverse-video.

**Palette-driven ⇒ all 20 themes covered automatically.** Elements
reference palette role-keys every builtin palette fills (pinned), so no
per-theme edits — proven by `every_builtin_theme_themes_the_modeline`
(applies each theme, asserts modeline elements resolve to themed fg/bg).

**TUI:** `Theme` cache gains the 7 pre-adapted modeline styles (built in
`build_tui_theme`, version-keyed — never per-frame, per
`feedback_renderer_cache_protects_ux`) + `Theme::modeline_style(role,
is_active)` (active = per-role fg patched over the bar bg; inactive =
uniform muted; unknown/padding = bar base). Renderer rewritten to
per-`Span`: zone resolution → role-tagged runs (`ModelineSeg`),
`compose_modeline_segments` lays them out (same Center→Right→Left
truncation on the styled path), `draw_pane_status_line` paints a ratatui
`Line` of per-role styled spans.

**Artefacts:** *tests* — `lattice-theme`
(`resolved_modeline_elements_are_palette_driven`,
`every_builtin_theme_themes_the_modeline`); `lattice-ui-tui`
(`default_options_adapt_to_tui_theme_default` extended; per-Span styling
active vs uniform-muted inactive; compose/truncate on runs;
width-0/narrow no-panic; TestBackend render parity unchanged). *error
handling* — truncation saturates, never panics. **Deps:** ML.1a-render.

### ML.2 — GPUI render parity  ✅
**Design:** §7, §8, §10.
**Landed:** `window.rs::pane_chrome` no longer takes a `status_text:
String`; it takes a pre-built `status_row` element. New `modeline_row(pane,
is_active, &RenderState)` builds the per-`Span` zone row — a flex row of
three zone children (`justify_between` → Left flush-left, Right
flush-right, Center between), each a row of styled text spans. Content
comes from the **same** `lattice_host::modeline::{resolve_builtin_content,
resolve_mode_items_content}` the TUI uses; only the paint differs. Per-role
colours adapt inline from the resolved theme (`resolved.get(ids.modeline_*)`
→ `to_rgb_u32` + bold), active = per-role fg over the `modeline.active`
bar bg, inactive = uniform muted `modeline.inactive` (GPUI keeps no style
cache; `feedback_renderer_cache_protects_ux`).

Both call sites (document + terminal) now build the row via `modeline_row`
— deleting GPUI's duplicated modal-label + mode-items assembly (which had
**drifted**: `PENDING`/`COMMAND` vs the host `O-PEND`/`CMD` — now unified)
and `build_terminal_inner`'s bespoke `R:row C:col` status (a GPUI-only
divergence the TUI never had; terminal panes now show the same `core.*`
content as every kind, `feedback_buffers_no_special_case`). A
terminal-cell-coords element, if wanted, is a terminal-mode contribution
for both peers later (ML.3+).

`provider_label: None` — GPUI has no M.4 pane-render provider registry yet
(Document → path, Terminal → registry name slot, both via the resolver);
a GPUI provider registry mirrors the TUI's M.4 later.

**Artefacts:** *test* — `modeline_elements_resolve_to_gpui_colours` (the
resolved→u32 colour path the row paints through; content parity is
by-construction via the shared resolver). *parity grep* —
`modeline_row` / `resolve_builtin_content` / `ids.modeline_*` / `Zone::`
all present in `lattice-ui-gpui/src/window.rs`. *error handling* — empty
content hidden (`content.is_empty()` skip). **Deps:** ML.1.

### ML.3 — event-bus update path + migrate modes + retire trait  ✅
**Design:** §5, §6. Carved into ML.3a (plumbing) → ML.3b (diff) → ML.3c
(LSP) → ML.3d (retire trait), all landed.

#### ML.3a — event + per-buffer store + forwarder + drain  ✅
`ModelineElementUpdate { key: ModelineKey, id, content }` typed event in
`lattice-mode` (registered via `register_event!`); content store re-keyed
`(ModelineKey, ElementId)` with `ModelineKey = Global | Buffer(BufferId)`
so a descriptor carries distinct content per pane (`ModelineSnapshot::resolve(el, buffer)`).
Boot subscribes the event into a drain channel + a `wake_on` forwarder
(fires `async_landed`, the §12 wake); `run_tick_pending` →
`drain_modeline_element_updates` applies each into the store
(single-writer). Renderers resolve pushed elements via `resolve(el, pane.buffer_id)`.
*Test:* `pushed_modeline_update_surfaces_without_keystroke` (publish →
`run_tick_pending` → snapshot, no dispatch) + per-buffer keying +
`apply` empty→clear. **Deps:** ML.2.

#### ML.3b — migrate diff  ✅
`diff` element owned by `lattice-host::diff` — descriptor registered at
boot (`register_diff_modeline_element`), `Scope::Global`, Left/20. Content
computed on the actor from the active session's sign map
(`sync_diff_modeline_element`, counting off the render thread) +
formatter `diff::mode::diff_content`. **Scope decision:** the diff session
holds ONE shared sign map (not per-side), so "each side shows its own
counts" is illusory — `Scope::Global` + the renderer's §7 active-pane gate
preserves the active-pane-only badge with zero teardown/tracking. Deleted
`DiffMode::status_line_items` + `DiffStatusData`. *Test:* `diff_content`
counts + `register_diff_element_is_global_left`.

#### ML.3c — migrate LSP (decision A: relocate accumulator)  ✅
`lsp` element owned by `lattice-lsp::modeline` — PaneLocal, Right/5. The
`$/progress` + `serverStatus` accumulation **relocated out of the host**
into `lattice_lsp::modeline::LspProgressStore` (ArcSwap-backed shared
handle): a forwarder task folds the events + pushes the badge per attached
buffer (gating it to LSP buffers); the host reads the SAME store via the
`LspProgressStoreHandle` it stashes at boot for `:lsp-progress-cancel`.
**Why a handle, not pure relocation:** the actual code showed the LSP
actor doesn't track progress (it only emits events) and `do_lsp_progress_cancel`
reads the in-flight map host-side — so the relocated store must be
host-readable. One accumulator (lattice-lsp), two readers (forwarder +
cancel), zero duplication. Deleted host `lsp_progress`/`lsp_server_status`
+ their drains + `RenderState.lsp.progress`/`server_status` + the
publish-cache slot + the prog/status wake forwarders + `LspProgressStatusData`
+ `LspMode`/`LspProgressMode::status_line_items`. *Test:*
`store_folds_progress_and_snapshots`, badge `lsp_content` cases.

#### ML.3d — retire the trait  ✅
Deleted `Mode::status_line_items` + `DynMode::status_line_items` +
`StatusLineCtx` + `StatusLineItem` (`lattice-mode`) + the host
`resolve_mode_items_content` adapter + the Center pull at all renderer
call sites (Center now resolves from the registry like Left/Right). The
acid test holds: a new provider crate adds a modeline element via the
`ModelineElementUpdate` event with zero `Editor::` methods + zero `Action`
variants.

*Error handling:* the forwarder/drain exit cleanly on a closed channel
(never panic); truncation/empty-content paths unchanged. *doc:* §5/§6.
**Deps:** ML.2.

### ML.5 — config surface  ✅
**Design:** §11. Carved into **ML.5a** (config layer) → **ML.5b** (host
resolver + both renderers) → **ML.5d** (lean modal label + showmode
echo) → **ML.5c** (docs). All landed.

**Locked decision (value representation = option B):** the loader was
**scalar-only** (TOML arrays warned "not applicable to scalar options"),
so the design's Helix-array + typed-option shape wasn't free. Chose a
real **`ModelineZone` typed list** (`Auto | Ids`) + a generic loader
array-join (gated on `ErasedOption::accepts_list`) over a stringly-typed
`String`-per-zone — protects paramount-#2 (reusable list-option
primitive; the TOML array shape the design specifies) over the
heuristic-#1 trap of routing around the loader gap. Per-zone default is
`Auto` (descriptor-driven) so a newly-registered element auto-appears —
Lattice's dynamic-registry adaptation of Helix's concrete-list defaults.

#### ML.5a — `ModelineZone` option + `ui.modeline.*` + loader arrays  ✅
**Landed:** `lattice_config::ModelineZone` (`Auto | Ids(Vec<Arc<str>>)`,
`OptionType` — `auto` keyword, comma/whitespace-lenient parse,
comma-join format, `accepts_list = true`); the four
`ui.modeline.{left,center,right,separator}` typed options under a new
`Modeline` `OptionGroup` (`:customize modeline`); loader
`apply_array`/`apply_assignment` join string-arrays for list options
(scalar options keep the "list not applicable" warning — the existing
`list_at_scalar_position` test still holds). *Tests:* `modeline_zone`
round-trips (7) + loader array-apply + empty-array-clears. 129 green,
zero host/renderer changes.

#### ML.5b — host `resolve_layout` + both renderers  ✅
**Landed:** `lattice_host::modeline::resolve_layout(registry, config)` →
`ModelineLayout { left, center, right, separator }`: `Auto` →
`zone_ordered` minus ids **claimed** by an explicit zone (no
double-render); explicit `Ids` → those registered ids in order, unknown
skipped + `debug!`. Both `lattice-ui-tui::render::modeline_spans` and
`lattice-ui-gpui::window::modeline_row` repointed off `zone_ordered` to
the shared resolver in the SAME patch (parity); configured separator
replaces the hardcoded space. *Tests:* 6 (Auto-matches-descriptor,
explicit-reorder, unknown-skip, claim-removal, empty-blank,
separator) + bench updated to the `resolve_layout` path (still
O(elements)).

#### ML.5d — lean modal label + showmode echo  ✅
**Landed:** `core.mode` now shows a lean 3-letter tag (`NOR`/`INS`/…,
terminal `TRM`/`TIN`/`TVI`), no brackets, via `modal_label_short`
(full-name `modal_label` retained for echo/describe). `Editor::enter_mode`
surfaces the full mode name in the echo area as vim showmode
(`-- INSERT --`) on a real transition via `set_ephemeral_echo` (sets
`last_message`, NOT the `*messages*` ring — no spam); entering Normal
clears it; Command/Search/O-pending leave it untouched (no clobber of a
real message). *Tests:* 4 (echo-no-spam, leave-clears, visual-variants,
command→normal-preserves) + TUI render assertions updated `[NORMAL]`→`NOR`.

#### ML.5e — modeline spacing (separator auto-pad + edge padding)  ✅
**Trigger:** Dhruva set `separator = "|"` and `" | "` and saw the same
result — the `:set`/TOML **trim** collapses both to `"|"`, and a bare
glyph touches its neighbours. **Fix:** the renderer now *owns* the
spacing. `resolve_layout` returns an **effective separator** — a
non-blank `ui.modeline.separator` is auto-padded ` | ` (blank → single
space), so the user supplies only the glyph. New
**`ui.modeline.padding`** option (i64, default 1, validated 0..=16) —
blank margin at the row start/end, applied as content-level spaces in
BOTH peers (`compose_modeline_segments` gains a `padding` arg; GPUI
`modeline_row` prepends/appends pad runs and `pane_chrome` drops its
`px_2` so the cell margin is identical across peers). `ModelineLayout`
gains `padding: usize`. *Tests:* host `resolve_layout_auto_pads_glyph_separator`
+ `resolve_layout_reads_padding_default_and_set`; TUI
`compose_row_applies_edge_padding` (+ the 5 compose call-sites updated).
Resolves the old "separator caveat".

**Artefacts (whole ML.5):** *tests* — per-slice above (config 9, host
resolver 8 + echo 4, loader 2, TUI compose 5). *bench* — `modeline.rs`
measures the `resolve_layout` path. *doc* — design §11 + §11.1; user
`modeline.md`; this plan. *error handling* — unknown ids skip+log,
malformed/empty → blank zone, padding clamped, truncation saturates;
never panic. **Deps:** ML.3.

### ML.4 — interaction (click + hover)  ✅ (2026-08-21)
**Design:** §9, §9.1 (as-built). The `Interaction` data model shipped in
ML.0, so this was additive with no model churn, as planned.

**Blocked on a primitive the plan did not name.** Terminal mouse
reporting did not exist anywhere in the tree — nothing ever called
`EnableMouseCapture`, so `Event::Mouse` fell through `apply_event`'s
catch-all. That landed first as **MO.1** (`ui.mouse`, default off; the
commit explains why off is the substantive choice). T4.2 (terminal
mouse passthrough) is the second consumer of the same primitive.

**Landed:**
- *TUI* — `ModelineSeg` converted from a `(String, Option<ModelineRole>)`
  tuple to a struct carrying its element's `on_click`, so identity
  survives layout and truncation; `record_modeline_hits` walks the
  **composed** row and records absolute-cell regions into a per-frame
  `ModelineHitMap`; `Event::Mouse` hit-tests and dispatches.
- *GPUI* — per-run `on_mouse_down` plus an `.id(..)`-stabilised
  `.tooltip(..)` from `hover`; `cx` threaded through
  `paint_pane_tree` → `paint_pane` → `modeline_row`.
- *Routing* — `Action::Invoke(CommandInvocation::of(id))` in both peers.
  Zero host `Action` variants, zero host-side handler bodies, as the
  design required.

**Artefacts:** *tests* — 8 (`ModelineHitMap`, host) + 7 (TUI recording:
column exactness, padding shift, split offset, ellipsis, filler-is-dead,
no-interaction-is-free, multi-span sharing) + 3 (TUI dispatch: hit, miss,
non-left-button) + 2 (host parity contract) = **20**. *doc* — design
§9.1, user `modeline.md`. *parity* — both peers read
`el.interaction.on_click` from the shared resolver; audit grep clean.
*error handling* — a click on a no-interaction element, on filler, or on
dead space is a no-op that does not arm the perf timer. **Deps:** ML.3,
MO.1.

### ML.6 — WIT plugin API  ⛔ deferred (plugin phase)
**Design:** §6 (plugin row). Plugins register/update/remove elements +
export click handlers over the capability-gated boundary. Lands with the
Plugin Architecture phase (mirrors LSP M.10). **Deps:** ML.4 + plugin host.

---

## Sequence

```
ML.0a ─► ML.0b-1 ─► ML.0b-2 ─► ML.1 ─► ML.2 ─► ML.3 ─► ML.5
                                                 └► MO.1 ─► ML.4 ✅ ─► ML.6 (deferred)
```

ML.0–ML.3 + ML.5 landed the configurable, themed, event-driven,
zone-based modeline; **ML.4 (interaction) landed 2026-08-21** on the
`Interaction` model ML.0 shipped, once MO.1 supplied the terminal mouse
primitive. **ML.6 (WIT plugin API) remains ⛔** — it needs the plugin
phase, so this plan stays active.

---

## Cross-references

- Design contracts: `modeline.md` §1–§12.
- Shared render-wake: `lsp-architecture.md` §12 (the `async_landed` path
  ML.3 reuses), `incremental-highlight.md` / `display-line.md`.
- Theme roles: `theme-system.md` (`register_theme`, `ResolvedTheme`).
- Mode ownership: `mode-architecture.md` (where ML.0/ML.3 register
  descriptors + handlers).
- Parity discipline: `feedback_tui_gpui_parity` (every slice both peers).
