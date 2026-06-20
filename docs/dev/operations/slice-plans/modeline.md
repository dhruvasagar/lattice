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

### ML.2 — GPUI render parity  🗒
**Design:** §7, §8, §10.
**Change:** replace the single-string `pane_chrome` status with a `div`
flex row of Left/Center/Right zones, per-`Span` theme, matching ML.1
exactly (lockstep, `feedback_tui_gpui_parity`). Still no interaction.
**Reuses the shared content layer (ML.1a-render):** GPUI calls the same
`lattice_host::modeline::{resolve_builtin_content, resolve_mode_items_content}`
the TUI does — it only adds its own div-zone layout + paint (and its own
provider-label lookup). No content logic is re-implemented; the strategy
is already common (the whole point of landing the resolver host-side).
**Artefacts:** *test* — element/zone snapshot parity with TUI; *doc* —
§10; *parity grep* — `Zone`/element sites present in `lattice-ui-gpui`.
*error handling* — empty content hidden. **Deps:** ML.1.

### ML.3 — event-bus update path + migrate modes + retire trait  🗒
**Design:** §5, §6.
**Change:** add `ModelineElementUpdate` typed event + a host forwarder
that drains it into the content store and fires `async_landed` (§12
wake). Migrate LSP (badge/progress/readiness) + diff signs from
`status_line_items` to registered elements pushed via the event. Delete
the `Mode::status_line_items` trait + the temporary ML.1 adapter.
**Artefacts:** *test* — an event updates an element's content + repaints
with NO keystroke (assert via the §12 wake, like L1c's test); LSP/diff
elements render identically post-migration. *responsiveness* —
`current_thread` runtime coverage. *doc* — §5/§6. *error handling* —
closed channel exits the forwarder, never panics. **Deps:** ML.2.

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
