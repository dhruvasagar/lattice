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

### ML.0b-2 — host content store + render snapshot + ModeContext accessor  🗒
**Design:** modeline.md §4, §5.
**Change:** `Editor.modeline: ModelineServiceHandle` (created at boot); a
`RenderState` modeline element-snapshot field populated each
`build_render_state` via `modeline.snapshot()`; expose the handle to
modes through `ModeContext` (the register/update surface modes use in
ML.3). No renderer change yet.
**Artefacts:** *test* — `Editor.modeline` → `build_render_state` →
`RenderState` round-trip; `ModeContext` hands back the same handle.
*doc* — §4/§5. *error handling* — covered by ML.0b-1 (unknown-id no-op).
**Deps:** ML.0b-1. **Unblocks:** ML.1.

### ML.1 — TUI render: zones + built-ins as elements  🗒
**Design:** §7, §8.
**Change:** rewrite `draw_pane_status_line` to lay out Left/Center/Right
from the registry + snapshot, with per-`Span` theme roles + width-aware
truncation (Center→Right→Left). Convert host built-ins (`core.mode`,
`core.path`, `core.position`, `core.lang`) to registered elements whose
content `build_render_state` writes. `Mode::status_line_items` stays as a
temporary adapter feeding Center (retired in ML.3).
**Artefacts:** *test* — zone layout + truncation + active/inactive theme;
built-in content matches the old string. *doc* — §7/§8. *bench* —
modeline build stays O(elements) (ratchet against the old cost).
*error handling* — overflow truncates, never panics. **Deps:** ML.0.

### ML.2 — GPUI render parity  🗒
**Design:** §7, §8, §10.
**Change:** replace the single-string `pane_chrome` status with a `div`
flex row of Left/Center/Right zones, per-`Span` theme, matching ML.1
exactly (lockstep, `feedback_tui_gpui_parity`). Still no interaction.
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
