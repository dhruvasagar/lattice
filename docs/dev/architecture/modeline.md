# Modeline architecture (developer reference)

The **modeline** is the per-pane bottom status row
(`[MODE]  path … L:n C:n  lang`). This is the design fragment for its
redesign into a configurable, extensible **element system**: host
built-ins, modes, and plugins all contribute styled, positioned,
optionally-interactive elements, updated over the event bus.

Sequencing + status live in the slice plan
[`../operations/slice-plans/modeline.md`](../operations/slice-plans/modeline.md);
canonical design context is [design.md §5.9](design.md). The wake
mechanism reused here is [lsp-architecture.md §12](lsp-architecture.md).

> **Not the headerline.** [`headerline.md`](headerline.md) documents the
> *view-header virtual row* at the **top** of a buffer (async-buffer /
> multibuffer progress, per `async_buffer_status_in_headerline`). The
> modeline is the **bottom** per-pane status row. Different surfaces.

---

## 1. Status quo + motivation

The global modeline was removed ("Option A"); each pane draws a 1-row
footer (`lattice-ui-tui::render::draw_pane_status_line`,
`lattice-ui-gpui::window::pane_chrome`). Content is assembled as a
**single formatted string** —
`"[MODE]  path  <mode-items>   L:n C:n  lang"` — where `<mode-items>`
comes from `Mode::status_line_items(&StatusLineCtx) -> Vec<StatusLineItem
{ text, priority }>`, flat-mapped across active modes, priority-sorted,
space-joined. Theming is one coarse `pane_status_active` /
`pane_status_inactive` `Style` for the whole row.

Limitations this redesign removes:

- **No zones** — everything is one centre-ish string; no left/right split.
- **No per-element style** — the whole row shares one `Style`.
- **No interaction** — the row is painted text; nothing is clickable.
- **No plugin path** — `status_line_items` is a Rust trait; WASM plugins
  cannot implement it.
- **Producer coupling** — content is pulled from the producer at render
  time rather than pushed and decoupled.

Dynamic content already flows over the event bus today (LSP
progress/status → accumulate → `render_state` → `StatusLineCtx` service →
`status_line_items`); this redesign **generalises** that into one model.

---

## 2. Principles

1. **One uniform element model** for host built-ins, modes, and plugins.
2. **Producers register; the host renders.** No producer is invoked on
   the render path (paramount #1). Render is `O(zones · elements)`.
3. **Content flows over the event bus.** The renderer reads a wait-free
   snapshot; it never calls back into a producer.
4. **Plugins are first-class.** The model is data-driven (no Rust trait),
   WIT-shaped from the start.
5. **The owner owns its whole surface** — the element descriptor, its
   content updates, AND its interaction handlers all live with the
   registering mode/plugin, never the host (see §6).
6. **Theme-driven styling**, TUI + GPUI in lockstep.

---

## 3. Element model

The **descriptor** is static (registered once); the **content** churns
(updated often). Splitting them is what lets content change per-keystroke
without re-registering, and lets the renderer treat every element
uniformly regardless of who sources its content.

```rust
/// Static descriptor — registered once, lives in the ModelineRegistry.
pub struct ModelineElement {
    pub id: ElementId,            // stable key for update / removal
    pub zone: Zone,               // Left | Center | Right
    pub priority: i32,            // order within the zone
    pub scope: Scope,             // PaneLocal | Global
    pub interaction: Option<Interaction>,  // designed now, realized in ML.4 (§9)
}

/// Namespaced to avoid collisions: "core.mode", "lsp", "diagnostics",
/// "<plugin-id>.<name>". The namespace is also the OWNER key (§6).
pub struct ElementId(pub std::sync::Arc<str>);

pub enum Zone { Left, Center, Right }
pub enum Scope { PaneLocal, Global }

/// Dynamic content — lives in the content store (§4), updated via events.
pub struct ElementContent {
    pub spans: Vec<Span>,         // styled runs; empty ⇒ element hidden this frame
}
pub struct Span {
    pub text: String,
    pub role: ThemeRole,          // resolved via ResolvedTheme (T-series)
}
```

Ordering within a zone: **Left** sorts by `priority` ascending (left→right);
**Right** sorts descending (so the highest-priority element sits at the
far right); **Center** ascending around the midpoint. An element whose
content `spans` is empty is skipped for that frame (the cheap way a
producer hides itself without deregistering).

---

## 4. Registry + content store

Two pieces, mirroring the existing `lsp_progress` split (descriptor set
changes rarely; content changes often):

- **`ModelineRegistry`** — descriptors keyed by `ElementId`
  (`register` / `remove`). Host-owned storage; mutated only through the
  `ModelineService` by the owning mode (and by plugins via WIT in ML.6).
  Changes only on mode activate/deactivate or plugin load.
- **Content store** — `HashMap<ElementId, ElementContent>` accumulated on
  the **actor thread** (single-writer) and published into
  `RenderState` each tick as a wait-free `Arc` snapshot, exactly like
  `lsp_progress`. Built-in content is written by `build_render_state`;
  mode/plugin content is drained from `ModelineElementUpdate` events in
  `run_tick_pending`. Both writers are the actor thread → no race.

The renderer reads `(registry descriptors, content snapshot)` and lays
out. It is a pure read: no locks, no producer calls.

---

## 5. Update flow (event bus → §12 wake)

```
producer (mode / plugin)
   │  publish ModelineElementUpdate { id, content }
   ▼
event bus ──► host forwarder task ──► async_landed.notify_one()   (lsp-architecture.md §12)
                                          │
actor thread: run_tick_pending()  ◄───────┘
   │  drain ModelineElementUpdate → content store (single-writer)
   │  build_render_state(): write built-in content + publish Arc snapshot
   ▼
renderer reads RenderState.modeline (wait-free) → lays out zones
```

This **reuses the async-result render-wake** L1c built
(lsp-architecture.md §12): the forwarder fires `async_landed`, so a
content update repaints off-keystroke without the renderer polling.
A per-frame WASM callback would violate paramount #1; an event-driven
push does not — which is precisely why plugins must use this path.

---

## 6. Ownership — modes (and plugins) own their full element surface

This is the load-bearing rule for the whole redesign
(`feedback_mode_owns_its_surface`): a custom element is owned **end to
end** by whoever registers it. The owner is responsible for, and the host
NEVER carries:

- the **descriptor** — registered in the mode's `on_activate`, removed in
  `on_deactivate` (plugins: on load / unload);
- the **content** — pushed via `ModelineElementUpdate` from the owner;
- the **interaction handler bodies** — registered as closures bound to
  the owner's `ActionId`s, living in the owner's crate.

| Producer | Registers descriptor | Updates content | Click handler |
|---|---|---|---|
| Host built-ins (`core.*`) | at boot | `build_render_state` | host action (built-in only) |
| Modes (`lsp`, `diagnostics`, diff, …) | `on_activate` / remove `on_deactivate` | `ModelineElementUpdate` | mode's `ActionId` + closure in the mode crate |
| Plugins (WIT, ML.6) | on load via host API | `ModelineElementUpdate` over the boundary | plugin-exported handler via capability-gated effect |

The host exposes only generic primitives — `ModelineService`
(register/update/remove), the event bus, the action-handler registry,
and the click router. **The acid test:** a new provider crate that adds
a clickable modeline element compiles with **zero** `Editor::` method
additions and **zero** new variants in the host `Action` enum. A
half-migration (mode registers the element but the host wires its click)
does **not** satisfy the rule — the binding choice AND the handler body
both stay with the owner.

The current `Mode::status_line_items` trait becomes a thin built-in
adapter in ML.1–ML.3, then retires once LSP/diff migrate to registered
elements (ML.3).

---

## 7. Layout

- **Three zones.** Left (ascending, left→right), Right (descending, so
  top-priority is far-right), Center (custom/plugin default; centred in
  the gap between Left and Right).
- **Width-aware.** When the zones exceed the pane width, truncate
  Center first, then Right, then Left (override via config); per-element
  ellipsis. Truncation is `O(elements)`, computed at render.
- **`scope: Global`** elements render only on the **active** pane's
  modeline — no per-pane duplication, and no global chrome is
  reintroduced (Option A stays). PaneLocal is the default.
- **Separators.** A configurable string between elements within a zone
  (Helix `separator`), itself a theme role.

---

## 8. Theme

Each `Span` carries a `ThemeRole` resolved through the T-series
element+style+palette registry (`ResolvedTheme`), with **active /
inactive** pane variants (replacing the single coarse `pane_status_*`
`Style`). Per `feedback_renderer_cache_protects_ux`, the TUI keeps its
adapted-style cache keyed on `ResolvedTheme::version()` — repoint the
builder, never adapt per-frame; GPUI adapts inline. New modeline theme
roles register through the same `register_theme` catalog as every other
element key.

---

## 9. Interaction API (designed now, realized in ML.4)

The `Interaction` field ships in the ML.0 model even though behaviour
lands in ML.4, so producers can declare clickable elements from day one
and the capability lights up with no model churn.

```rust
pub struct Interaction {
    /// Dispatched through the host action registry on click. The handler
    /// closure was registered by the producing mode/plugin — the host is
    /// only a router (feedback_effect_vocabulary_is_host_boundary, §6).
    pub on_click: Option<ActionId>,
    /// GPUI tooltip on hover. TUI has no hover → ignored there (degrade).
    pub hover: Option<HoverSpec>,
}
pub struct HoverSpec { pub content: ElementContent }   // markdown body later
```

- **Routing + ownership.** `on_click` → host dispatches the `ActionId`
  through the action registry; the body lives in the registering
  mode/plugin crate (§6). The host adds no per-element handler. For
  plugins the click crosses the WASM boundary as a capability-gated
  effect (the same boundary the Effect vocabulary defines —
  `feedback_effect_vocabulary_is_host_boundary`).
- **GPUI realization (ML.4).** Each element becomes a `div` child with
  `on_mouse_down(MouseButton::Left, …)` → dispatch the `ActionId`, and
  `.tooltip(…)` from `hover`. (`window.rs` already uses `on_mouse_down`,
  so the primitive exists.)
- **TUI realization (ML.4).** The renderer records each element's painted
  x-range per zone; a ratatui `MouseEvent` of kind `Down` is hit-tested
  against those ranges → the same `ActionId`. **Hover degrades**: the
  terminal has no hover, so `hover` is ignored (optionally surfaced as an
  echo-line hint on click). This is graceful degradation, not a parity
  break — both peers honour `on_click`; only `hover` is GPUI-only.

---

## 10. Cross-renderer parity

Per `feedback_tui_gpui_parity`, every slice updates both peers in the
same patch: the element model + zone layout + theme roles land in
`lattice-ui-tui` and `lattice-ui-gpui` together; ML.4 interaction lands
in both (GPUI mouse+tooltip, TUI mouse+degraded-hover). End-of-slice
audit: `grep` the new `Zone` / element-render sites in
`crates/lattice-ui-gpui/` — an empty grep means GPUI was missed.

---

## 11. Config surface

Helix-shaped — element ids assigned to zones, in order:

```toml
[ui.modeline]
left      = ["core.mode", "core.path", "diagnostics"]
center    = []                              # custom / plugin zone
right     = ["lsp", "core.position", "core.lang"]
separator = " "
```

Ids are registered `ElementId`s; unknown ids are skipped + logged
(`debug!`, never panic). Absent config falls back to a built-in default
layout. The `[ui.modeline]` keys are typed options (§5.12) so
`:set`/`:customize` reach them; programmable assignment (predicates,
per-language layouts) is the WASM-init job, not TOML.

---

## 12. Paramount-goal alignment + rejected alternatives

- **#1 performance.** Render is `O(zones · elements)`; no producer call
  on the render path; content read from a wait-free `Arc` snapshot;
  repaint via the §12 wake (actor thread, never UI thread).
- **#2 extensibility.** The data-driven registry is WIT-shaped; plugins
  register + update + handle clicks exactly like modes (§6).
- **#4 asynchronicity.** Updates over the event bus; clean
  producer/renderer separation.

**Rejected:**
- *Enrich the `status_line_items` trait* — blocks plugins (no Rust trait
  across WASM) and keeps producer-coupling. Heuristic #1: keeping the
  inferior primitive out of risk-aversion.
- *lualine 6-section (a/b/c/x/y/z)* — more than the requirement; three
  zones cover left/right/custom and grow to N named sections later
  without breaking the element model.
- *Reintroduce a global modeline bar* — re-adds the fixed chrome Option A
  deliberately removed, without a merit win; `scope: Global` on the
  active pane covers global content instead.

---

## See also

- [`../operations/slice-plans/modeline.md`](../operations/slice-plans/modeline.md) — slice plan (ML-series sequencing + status).
- [`headerline.md`](headerline.md) — the *other* status surface (top view-header virtual row).
- [`theme-system.md`](theme-system.md) — element+style+palette registry the spans resolve through.
- [`lsp-architecture.md`](lsp-architecture.md) §12 — the async render-wake reused for content updates.
- [design.md §5.9](design.md) — UI components / everything-is-a-buffer context.
