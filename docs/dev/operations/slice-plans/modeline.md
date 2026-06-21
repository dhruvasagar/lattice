# Modeline redesign — slice plan

Sequencing + status for the modeline element-system redesign. **Design
contracts** live in
[`../../architecture/modeline.md`](../../architecture/modeline.md); this
file owns *when* and *in what order*.

Status legend: ✅ done · 🚧 in progress · 🗒 planned · ⛔ deferred.

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

Carved into **ML.1a-foundation** (✅) and **ML.1a-render** (🗒).

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

### ML.5 — config surface  🗒
**Design:** §11.
**Change:** typed options `ui.modeline.{left,center,right,separator}`
(Helix-shaped id lists) driving zone assignment + order; default layout
when absent; unknown ids skipped + logged. `:set`/`:customize` reach
them.
**Artefacts:** *test* — option parse + reorder + unknown-id skip; live
`:set` re-renders. *doc* — §11 + `user/` modeline help. *error handling*
— malformed config falls back to default layout. **Deps:** ML.3.

### ML.4 — interaction (click + hover)  ⛔ deferred (API designed in ML.0)
**Design:** §9. **Deferred per request**; the `Interaction` data model
ships in ML.0 so this is additive, no model churn.
**Change:** GPUI — per-element `on_mouse_down` → dispatch the element's
`ActionId`; `.tooltip(…)` from `hover`. TUI — record per-element x-ranges
+ hit-test ratatui `MouseEvent::Down` → same `ActionId`; hover degrades
(ignored / echo hint). Handler bodies live in the registering mode/plugin
(§6) — host only routes; zero host `Action` variants added.
**Artefacts:** *test* — click on an element fires its `ActionId` (both
peers); hover tooltip (GPUI); TUI hover-absent degrade. *doc* — §9.
*parity* — both peers honour `on_click`. *error handling* — click on a
no-interaction element is a no-op. **Deps:** ML.3 (registered elements).

### ML.6 — WIT plugin API  ⛔ deferred (plugin phase)
**Design:** §6 (plugin row). Plugins register/update/remove elements +
export click handlers over the capability-gated boundary. Lands with the
Plugin Architecture phase (mirrors LSP M.10). **Deps:** ML.4 + plugin host.

---

## Sequence

```
ML.0a ─► ML.0b-1 ─► ML.0b-2 ─► ML.1 ─► ML.2 ─► ML.3 ─► ML.5
                                                 └► ML.4 (deferred) ─► ML.6 (deferred)
```

Land ML.0–ML.3 + ML.5 for the configurable, themed, event-driven,
zone-based modeline; ML.4 (interaction) and ML.6 (plugins) follow on the
already-shipped `Interaction` model.

---

## Cross-references

- Design contracts: `modeline.md` §1–§12.
- Shared render-wake: `lsp-architecture.md` §12 (the `async_landed` path
  ML.3 reuses), `incremental-highlight.md` / `display-line.md`.
- Theme roles: `theme-system.md` (`register_theme`, `ResolvedTheme`).
- Mode ownership: `mode-architecture.md` (where ML.0/ML.3 register
  descriptors + handlers).
- Parity discipline: `feedback_tui_gpui_parity` (every slice both peers).
