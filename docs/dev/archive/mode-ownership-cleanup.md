# Mode ownership cleanup — slice plan

> **Status: ✅ Completed.** All slices landed. Archived 2026-06-09.


Sequencing companion to
[`docs/dev/architecture/mode-architecture.md`](../../architecture/mode-architecture.md)
§13. The architecture fragment owns *what + why* (the principle:
modes own their full surface — keymaps, decorations,
subscriptions, status-line, completion sources, options); this
file owns *when + in what order + status*.

The cleanup migrates feature-specific surface that today lives
host-side into the mode that gates it. K.1.c's per-keystroke
filter then scopes the chord to mode-active buffers
automatically. Surfaced 2026-06-01 during the M.6.1
SearchProvider review (`gr` shadow check) — modes are
under-utilised and the host owns lots of mode-specific surface
that should live in the mode itself.

## Scheduling constraint

**Each phase lands AFTER the M-series multibuffer work
completes** (M.6.2 / M.7 / M.8 / A.1+ provider catalogue).
Each phase is independently revertible. Touching the same
per-area code as in-flight M-series slices would create churn
and merge conflict; the deferral has a clear unblock signal
(M-series done) so this isn't open-ended waiting.

## Phases

| Slice | Title | What lands |
|-------|-------|------------|
| **MO.1** | ✅ LSP keymap migration | Move 7 bindings (`K`, `gd`, `gD`, `gy`, `gI`, `gr`, `gx`) from `keymap_normal.rs` Builtin layer to a new `register_lsp_mode_keymap(handle, action_ids)` helper in `lattice-lsp/src/modes.rs`, alongside `LspMode`. Boot calls it under `PushLayerKind::MinorMode(LspMode::mode_id())`. Drop the 7 entries from `keymap_normal.rs`. Tests verify K.1.c filtering — bindings fire only when `lsp-mode` is in the active buffer's `ActiveModes`. Touches: `lattice-lsp/src/modes.rs`, `lattice-host/src/keymap_normal.rs`, `lattice-host/src/editor_boot.rs`. |
| **MO.2** | ✅ Oil keymap migration | Single binding (`-` → `oil_navigate_up`). Added `oil_mode_keymap_entries()` + `fn keymap()` to `OilMode` in `lattice-oil/src/modes.rs`. Dropped `handle.bind(- → oil_navigate_up)` from `keymap_normal.rs`. K.2.4 registers the entry at `KeymapLayer::MinorMode("oil-mode")`; K.1.c scopes it to oil buffers automatically. Touches: `lattice-oil/src/modes.rs`, `lattice-host/src/keymap_normal.rs`. |
| **MO.3** | ✅ Snippet keymap migration | Added `SnippetActiveMode` (minor) in `lattice-snippet/src/modes.rs` with `fn keymap()` contributing `<Tab>`/`<S-Tab>`/`<Esc>` Insert-mode bindings. `register_snippet_modes` now also registers it. Replaced `push_layer`/`pop_layer` in `sync_keymap_overlays` with `activate_minor`/`deactivate_minor`. Removed `active_snippet_layer_bindings`, `active_snippet_mode_id`, and `Editor::snippet_layer` field. K.2.4 registers the entries statically; K.1.c gates them to snippet-active buffers. TUI test helpers updated to use `translate_mode_keymaps` (K.2.4 path). Touches: `lattice-snippet/src/modes.rs`, `lattice-host/src/dispatch.rs`, `lattice-host/src/editor.rs`, `lattice-host/src/keymap_insert.rs`, `lattice-ui-tui/src/keymap_insert.rs`, `lattice-ui-tui/src/input.rs`. |
| **MO.4.a** | ✅ Gutter-sign decoration migration | Move gutter-sign data reads out of the renderers and into mode-owned `gutter_decorations()` contributions. See §MO.4.a detail below. |
| **MO.4.b** | ✅ Status-line infrastructure + per-pane modeline overhaul | Added `StatusLineItem { text, priority }` + `StatusLineCtx<'a>` to `lattice-mode/src/contributions.rs`. Added `Mode::status_line_items(&self, ctx: &StatusLineCtx<'_>) -> Vec<StatusLineItem>` to the `Mode` trait + `DynMode` blanket impl. `LspMode` contributes `"lsp"` (priority 60); `LspProgressMode` hand-written to read `LspProgressStatusData` from `ServiceRegistry` context and contribute the in-flight progress token (priority 70); `DiffMode` contributes `"+N ~M"` hunk count via `DiffStatusData` (priority 40). `ModesRenderState` gains `mode_registry: Arc<ModeRegistry>` (populated at publish; read wait-free). `pane_status_label` rewritten to collect + sort mode items without actor calls; `pane_status_label_makes_zero_actor_calls` test passes. **Option-A modeline overhaul:** global 1-row modeline removed from TUI top-level layout; every pane always renders its own 1-row status footer (active: `[MODE] path  items    line:col  lang`; inactive: `path    line:col` dimmed). `runtime.rs` `buffer_height`: `saturating_sub(2) → saturating_sub(1)`. Per-pane guard in `runtime.rs`, `motions.rs`, `draw_panes`: `multi && h >= 2 → h >= 2`. Buffer line count preserved: single-pane content rows = `T-2` before and after. Dead modeline helpers removed. Touches: `lattice-mode/src/contributions.rs`, `lattice-mode/src/mode.rs`, `lattice-lsp/src/modes.rs`, `lattice-host/src/diff/mode.rs`, `lattice-host/src/render_state.rs`, `lattice-host/src/dispatch.rs`, `lattice-ui-tui/src/app/lifecycle.rs`, `lattice-ui-tui/src/render.rs`, `lattice-ui-tui/src/runtime.rs`, `lattice-ui-tui/src/app/motions.rs`. |
| **MO.4.c** | ✅ Subscription RAII type | `Subscription` stub replaced with real RAII type in `lattice-mode/src/contributions.rs` (wraps `Arc<EventBus>` + `SubscriptionId`; Drop → unsubscribe). `Mode::subscriptions()` removed from trait + `DynMode` + blanket impl — `on_activate` + Guard IS the subscription mechanism; `ModeContext::events_handle()` already provides the bus. Three log-mode Guards collapse from hand-rolled `LogSubscriptionGuard { Option<(Arc<EventBus>, SubscriptionId)> }` to `Option<Subscription>`. Process-level `subscribe_typed` calls in `editor_boot.rs` are host infrastructure and correctly stay there. |

## MO.4.a detail — gutter-sign decoration migration

### Corrected blocking condition

The original entry said "blocked on M.4 decoration registry." M.4 landed
(2026-06-01) as the multibuffer event-subscription slice — it never
contained a decoration registry. The real dependency was the
`ServiceRegistry`/mode-walk pattern that MO.4.b proved. **MO.4.a is now
unblocked.**

### What the slice does

Two gutter columns are currently read directly from render-state by both
renderers:

| Column | Current read-site | Source |
|--------|-------------------|--------|
| Diff-sign (1 cell) | `rs.diff.sign_map.sign_at(line)` | `lattice-host/src/diff/overlay.rs` |
| Severity (1 cell) | `rs.diagnostics.layer.line_severity(uri, line)` | `lattice-lsp/src/diagnostics_layer.rs` |

After MO.4.a both reads are replaced by a mode walk that collects
`GutterDecoration`s — same pattern as MO.4.b's status-item walk. The
`sign_map` and `diagnostics.layer` fields **remain in render state** (other
consumers exist: status-line items, inline underlines, hover); only the
gutter-column read sites change.

### New types — where each lives

**`lattice-mode/src/contributions.rs`** (no new crate deps):

```rust
/// Renderer-agnostic diff-sign variants.
/// Mirrors `DiffSignKind`; defined here so `lattice-mode` stays
/// free of `lattice-host` imports.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GutterDiffKind { Add, Remove, Change, Conflict }

/// Renderer-agnostic LSP severity level.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GutterSeverityLevel { Hint, Info, Warning, Error }

/// A single gutter decoration contributed by a [`crate::Mode`].
/// Each variant maps to one physical gutter column.
#[derive(Copy, Clone, Debug)]
pub enum GutterDecoration {
    Diff     { line: u32, kind: GutterDiffKind },
    Severity { line: u32, level: GutterSeverityLevel },
}

/// Read-only context passed to [`crate::Mode::gutter_decorations`].
/// Same dep-inversion pattern as [`StatusLineCtx`]: the App populates
/// a `ServiceRegistry` with typed render-state snapshots; modes pull
/// their own data via `ctx.service::<T>()`.
pub struct DecorationCtx<'a> {
    pub buffer_id: BufferId,
    services: &'a ServiceRegistry,
}
impl<'a> DecorationCtx<'a> {
    pub fn new(buffer_id: BufferId, services: &'a ServiceRegistry) -> Self { ... }
    pub fn service<T: Any + Send + Sync>(&self) -> Option<Arc<T>> { ... }
}
```

`DecorationProvider` (the `_private:()` stub) stays in
`contributions.rs` unchanged — reserved as the future plugin-facing
contribution type once the WIT plugin path lands (M.10). It is **not**
used by the `Mode` trait method.

**`lattice-mode/src/mode.rs`** — replace the stub:

```rust
// before:
fn decorations(&self) -> Vec<DecorationProvider> { Vec::new() }

// after:
fn gutter_decorations(&self, _ctx: &DecorationCtx<'_>) -> Vec<GutterDecoration> { Vec::new() }
```

`DynMode` gains the matching blanket method.

**`lattice-host/src/diff/mode.rs`** (alongside `DiffStatusData`):

```rust
pub struct DiffDecorationData {
    pub sign_map: Arc<DiffSignMap>,
}
// DiffMode::gutter_decorations: reads DiffDecorationData from ctx,
// maps sign_map.entries() → GutterDecoration::Diff { line, kind }.
```

**`lattice-lsp/src/modes.rs`** (alongside `LspProgressStatusData`):

```rust
pub struct LspDiagnosticsData {
    pub layer: Arc<DiagnosticsLayer>,
    pub uri:   Option<Arc<str>>,
}
// LspMode::gutter_decorations: reads LspDiagnosticsData from ctx,
// calls layer.line_severity(uri, line) for each signed line →
// GutterDecoration::Severity { line, level }.
```

### Renderer changes

Both `lattice-ui-tui/src/render.rs` and
`lattice-ui-gpui/src/window.rs` (and `editor_element.rs`) get the same
treatment: before entering the per-line gutter loop, build two maps
(`HashMap<u32, GutterDiffKind>` and `HashMap<u32, GutterSeverityLevel>`)
by walking `modes_rs.mode_registry` + `active_modes` for the buffer,
calling `mode.gutter_decorations(&ctx)` on each. The per-line code then
reads those maps instead of calling `sign_map.sign_at` /
`layer.line_severity` directly. Service injection into `DecorationCtx`
mirrors the status-line path in TUI's `collect_status_line_items` and
GPUI's `paint_pane`.

GPUI and TUI renderers update in the same patch (TUI+GPUI parity rule).

### Touch surface

`lattice-mode/src/contributions.rs`,
`lattice-mode/src/mode.rs`,
`lattice-host/src/diff/mode.rs`,
`lattice-lsp/src/modes.rs`,
`lattice-ui-tui/src/render.rs`,
`lattice-ui-gpui/src/window.rs`,
`lattice-ui-gpui/src/editor_element.rs`.

### Test discipline

- Convention check: `DiffMode::gutter_decorations` and `LspMode::gutter_decorations` return the correct `GutterDecoration` variants when the relevant service data is present in ctx; return empty when service absent.
- Negative test: a buffer with no active modes returns empty decoration maps; renderers render a blank gutter column (existing blank-column path).
- No renderer regression: existing TUI `diagnostic_severity_glyph_appears_in_gutter_for_error` and GPUI `gutter_text_format_diff_sign_left_of_line_number` tests pass unchanged (behaviour preserved, only read-site changes).

## Audited cluster details (the targets)

Audited via grep against `crates/lattice-host/src/keymap_*.rs`
on 2026-06-01:

### LSP — 7 bindings at Normal Builtin (MO.1)

| Chord | Action | Source line | Notes |
|---|---|---|---|
| `K` | `lsp_hover_request` | `keymap_normal.rs:319` | Hover popup |
| `gd` | `lsp_definition_request` | `keymap_normal.rs:535` | Go-to-definition |
| `gD` | `lsp_declaration_request` | `keymap_normal.rs:542` | Go-to-declaration |
| `gy` | `lsp_type_definition_request` | `keymap_normal.rs:549` | Go-to-type |
| `gI` | `lsp_implementation_request` | `keymap_normal.rs:556` | Go-to-implementation |
| `gr` | `lsp_references_request` | `keymap_normal.rs:563` | References picker |
| `gx` | `lsp_follow_link_at_cursor` | `keymap_normal.rs:571` | documentLink follow |

LSP auto-activates path-driven
(`maybe_auto_activate_lsp_mode` short-circuits on no-path
buffers like multibuffer views), so these bindings are
*de facto* no-ops outside LSP-attached buffers today — but
they STILL fire the `Action::Lsp*` dispatch and reach the
supervisor before the no-op happens. Mode-scoped registration
short-circuits at the keymap layer instead.

### Oil — 1 binding at Normal Builtin (MO.2)

| Chord | Action | Source line | Notes |
|---|---|---|---|
| `-` | `oil_navigate_up` | `keymap_normal.rs:389` | Oil parent-directory chord |

### Snippet — 4 bindings at Insert Builtin (MO.3)

| Chord | Action | Source line | Notes |
|---|---|---|---|
| `<Tab>` | `snippet_expand` | `keymap_insert.rs:192` | Expand template at cursor |
| `<Tab>` | `snippet_next_placeholder` | `keymap_insert.rs:371` | Move to next placeholder |
| `<S-Tab>` | `snippet_prev_placeholder` | `keymap_insert.rs:383` | Move to prev placeholder |
| `<Esc>` | `snippet_leave` | `keymap_insert.rs:389` | Exit snippet session |

The runtime `is_snippet_active` boolean check is a
poor-person's keymap layer. A `MinorMode(snippet-active-mode)`
layer with K.1.c precedence does this cleanly.

## Test discipline (per phase)

Each phase ships green-on-merge with:

- **Convention check** — the moved bindings appear at
  `MinorMode(<mode>::mode_id())` and DO NOT appear at
  `KeymapLayer::Builtin`. A unit test resolving the chord in a
  buffer without the mode active confirms the binding is
  inactive there.
- **Behaviour preservation** — a test activating the mode on
  a buffer + firing the chord + asserting the same downstream
  action runs. The action handler itself doesn't change.
- **Negative test** — a test confirming the chord is *not*
  resolvable on a buffer without the mode (e.g. multibuffer
  view + `gr`). This is the load-bearing user-visible win.
- **Graceful error handling** — same handlers as today; no
  new error paths introduced. The migration is mechanical.

No new benches needed. Keymap resolution is K.1.c per-keystroke
filter with no measurable per-binding cost — moving 7 bindings
between layers doesn't move the needle.

## What unblocks each phase

- **MO.1** depends on: M-series multibuffer work complete
  (M.6.2 + M.7 + M.8 done). No internal dep — LSP keymap is
  self-contained.
- **MO.2** depends on: M-series done. No dep on MO.1.
- **MO.3** depends on: M-series done + a design call on the
  `snippet-active-mode` shape (one new minor or extend the
  existing snippet-mode's contribution set conditionally).
- **MO.4** depends on: MO.1 / MO.2 / MO.3 landed so the
  keymap shape is uniform before tackling the broader
  contribution-surface audit.

## Cross-references

- Convention itself + the principle + going-forward rule:
  [`mode-architecture.md`](../../architecture/mode-architecture.md) §13.
- Project memory backing the convention:
  `feedback_mode_owns_its_surface` (project-local memory).
- Related principles already established:
  `feedback_mode_owns_its_buffers` (modes own their buffers,
  App is a host), `feedback_buffers_no_special_case` (no
  kind-specific branching at universal layers).
- Patterns already correct (positive precedent): diff-mode
  layer (`crates/lattice-host/src/diff/mode.rs`),
  multibuffer-mode layer
  (`crates/lattice-host/src/multibuffer_keymap.rs`),
  project-search-multibuffer-mode layer (same file).
