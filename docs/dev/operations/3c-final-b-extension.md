# 3c.final.B-extension — perf-driven RS lifts (post-swap)

**Status:** queued — to be sliced after `3c.final.E.swap` (already
landed).
**Predecessor:** `docs/dev/operations/3c-final-audit.md` §5 (slice
B field enumeration); `implementation.md` slice ledger up to and
including `3c.final.E.swap`.

## 0. Why this exists

After `3c.final.E.swap`, the renderer thread cannot reach
`&mut Editor` in production builds. Every read either goes through
a wait-free RS sub-state accessor (`app.ad()`, `app.panes()`,
`app.popup()`, `app.buffers()`, `app.picker_state()`,
`app.completion()`, …) or through `App::read_editor` — and
`read_editor`'s body in `cfg(not(test))` is
`self.editor_actor.with_editor(f)`, which is a **sync mailbox
round-trip** to the actor thread.

Per-frame read-trip cost is small (~µs each), well under the 8ms
frame budget at 120Hz even in aggregate. Paramount goal #1 is not
violated. But each surviving `read_editor` call is the only
"actor-blocks-renderer" surface left in the read path, and a few of
them ride hot loops (per-pane / per-line). Lifting the fields they
read to RS drops every call from `~µs` to wait-free `Arc::clone`.

The audit's slice 5 enumeration anticipated most of these fields;
this doc collects them as concrete follow-up slices.

## 1. Per-frame `read_editor` inventory (post-swap)

Per-frame paint / per-tick runtime loops only:

| Site | Field read | Frequency | Owner slice |
|------|-----------|-----------|-------------|
| `render.rs::draw_frame` cmdline hint | `auto_submit_after_chord: bool` | per-frame | B.7 |
| `render.rs::draw_frame` search prompt | `search_line.pattern: String` | per-frame | B.7 |
| `render.rs::draw_frame` search direction | `search_line.direction: SearchDirection` | per-frame | B.7 |
| `render.rs::draw_frame` echo area | `last_message: Option<EchoMessage>` | per-frame | B.7 |
| `gpui/window.rs::render` cmdline | `command_line: String` | per-frame | B.7 |
| `gpui/window.rs::render` search prompt | `search_line.pattern: String` | per-frame | B.7 |
| `runtime.rs::main_loop` config | `config.get_typed::<PickerDisplay>()` | per-frame | B.10 |
| `render.rs::picker_display_is_minibuffer` | same | per-frame | B.10 |
| `gpui/window.rs::picker_display_is_minibuffer` | same | per-frame | B.10 |
| `render.rs::help_render_data` | `buffer_locals.get(...)` (HelpHighlights + HelpLinks) | per-help-paint | B.9 |
| `render.rs::file_tree_pane_status` | `buffer_locals.get(...)` (FileTreeRoot) | per-modeline | B.9 |
| `render.rs::oil_pane_status` (dir) | `buffer_locals.get(...)` (OilDir) | per-modeline | B.9 |
| `render.rs::draw_file_tree_pane` (entries) | `buffer_locals.get(...)` (FileTreeEntries) | per-pane | B.9 |
| `render.rs::draw_oil_pane` (dir) | `buffer_locals.get(...)` (OilDir) | per-pane | B.9 |
| `render.rs::draw_mode_line` (is_messages_buffer) | `active_modes.get(...)` | per-frame | B.11 |
| `render.rs::pane_highlights_for_line` | `pane_highlights.get(&pane_idx)` | per-pane per-frame | B.8 |
| `render.rs::lsp_diagnostics_on_line` | `lsp_diagnostics.diagnostics_on_line(...)` | per-line per-frame | B.8 |
| `gpui/window.rs::EditorView::new` paint_request | `paint_request: Arc<Notify>` | once at boot | B.7 (or inline-init) |

Aggregate per-frame cost (~µs each × 12-18 sites): ~12-18µs at 120Hz.
Budget headroom: 8000µs. Not load-bearing today; valuable for
headroom + architectural cleanliness.

## 2. Slice catalog

### 2.1 Slice B.7 — Messages + Modeline (smallest)

**RS fields to add:**

```
// MessagesRenderState (audit §5.7 — struct exists, currently empty)
last: Option<Arc<EchoMessage>>,

// ModelineRenderState (audit §5.8 — struct exists, currently empty)
auto_submit_hint: bool,
search_pattern: Option<Arc<str>>,
search_direction: Option<SearchDirection>,
cmdline_text: Arc<str>,
```

**Publication:** add to `Editor::build_render_state`. Each field is
either `Copy` or already an Arc-backed clone; `cmdline_text` wraps
the existing `String` into `Arc<str>` on publish (cheap).

**App accessors:** `App::messages() -> Arc<MessagesRenderState>` +
`App::modeline() -> Arc<ModelineRenderState>` matching the slice-B
pattern.

**Migrated reader sites:** 5 in render.rs + 2 in gpui/window.rs.

**Tests:** round-trip per peer (`messages_last_reflects_editor_state`,
`modeline_fields_reflect_editor_state`).

**Risk:** `EchoMessage` is small (level + text). Arc-wrap on every
dispatch tail; not expensive given message frequency.

**Estimated effort:** 1 commit, ~150 LOC + 4 tests.

### 2.2 Slice B.10 — Config (small)

**RS field:**

```
// New OptionsRenderState (sibling of existing sub-states)
config: Arc<lattice_config::Registry>,
```

`Registry` is already `Arc`-shared internally; publish is one Arc
bump.

**Migrated sites:** 3 — `runtime.rs::main_loop`,
`render.rs::picker_display_is_minibuffer`,
`gpui/window.rs::picker_display_is_minibuffer`.

**Estimated effort:** 1 commit, ~80 LOC + 1 test.

### 2.3 Slice B.11 — Active modes

**RS field:**

```
// New ModesRenderState
map: Arc<HashMap<BufferId, Arc<ActiveModes>>>,
```

Outer `Arc<HashMap<...>>` for cheap clone-on-publish; per-entry
`Arc<ActiveModes>` so reads don't clone the inner mode chain.

**Mutation surface:** every `activate_mode` / `deactivate_mode`
rebuilds the modified entry's Arc. Rare (buffer-switch), not
per-frame.

**Migrated sites:** 1 in render.rs (`is_messages_buffer`).

**Estimated effort:** 1 commit, ~100 LOC + 1 test.

### 2.4 Slice B.8 — pane_highlights + lsp_diagnostics

**RS field additions:**

```
// Extend SyntaxRenderState
pane_highlights: Arc<HashMap<usize, Arc<Vec<Vec<StyledSpan>>>>>,
```

For diagnostics, two options:
- **(a)** Extend the supervisor snapshot with a
  `diagnostics_layer: Arc<DiagnosticsLayer>` so per-line lookup
  hits the snapshot directly.
- **(b)** Lift `lsp_diagnostics` to its own top-level RS sub-state.

(a) fits with lower churn since the supervisor already carries the
layer; the snapshot just exposes it.

**Migrated sites:** 2 — `render.rs::pane_highlights_for_line`,
`render.rs::lsp_diagnostics_on_line`.

**Risk:** pane_highlights updates every worker publish (potentially
every frame on heavy edits). Arc-rebuild cost needs benchmark to
confirm < 1µs per publish.

**Estimated effort:** 1 commit, ~150 LOC + 2 tests + 1 bench update.

### 2.5 Slice B.9 — buffer_locals (largest)

**RS field:**

```
// New BufferLocalsRenderState
map: Arc<HashMap<BufferId, Arc<BufferLocals>>>,
```

**Mutation surface:** every `BufferLocals::insert` /
`BufferLocals::get_mut` rebuilds the modified entry's Arc (CoW),
OR the BufferLocals struct itself becomes `Arc<RwLock<...>>`-backed
for in-place mutation (`Arc::make_mut` pattern).

**Decision required:** clone-on-write vs locked-shared.

- CoW: reads wait-free; each mutation allocates.
- Locked-shared: wait-free reads via `RwLock::read` (fast on
  uncontended); reads take a guard.

The audit §5.2 fixed shape was `Arc<HashMap<BufferId,
Arc<DocumentHandle>>>` (CoW for handles); same pattern fits here.

**Migrated sites:** 5+ in render.rs (help_render_data,
file_tree_pane_status, oil_pane_status, file_tree_entries, oil_dir),
2 in app/help.rs.

**Risk:** the BufferLocals registry has the most mutation traffic
of any sub-state (every mode activation / deactivation / pulled
diagnostic update writes it). CoW costs need a bench.

**Estimated effort:** 1-2 commits, ~250 LOC + 3 tests + 1 bench.

## 3. Slice order recommendation

```
B.7  → smallest, audit-planned, exercises the pattern
B.10 → smaller still, single config field
B.11 → active_modes, well-understood shape
B.8  → pane_highlights + lsp_diagnostics, needs bench
B.9  → buffer_locals, needs CoW decision + bench
```

Each can land independently; B.9 should follow the others because
its publish-cost story benefits from validating against the simpler
slices first.

## 4. Acceptance for the B-extension as a whole

- `grep -n 'read_editor\b' crates/lattice-ui-tui/src/render.rs
  crates/lattice-ui-tui/src/runtime.rs
  crates/lattice-ui-gpui/src/window.rs` → zero matches.
- `App::read_editor` surface in production paint paths reduced to
  zero per-frame calls (background tasks + boot-time and ex-command
  paths may still use it).
- All planned RS fields populated in `Editor::build_render_state`
  with round-trip tests.
- `benches/render` and `benches/highlights_worker` stay within their
  existing per-frame budgets (sub-µs publish costs).
- `lattice-host` + `lattice-ui-tui` + `lattice-ui-gpui` tests green
  at each slice landing.

## 5. Out of scope for B-extension

- Removing `read_editor` from cold-path App helpers (LSP
  autopilots, picker accept tails, ex-command bodies) — those run
  off the paint hot-loop and tolerate the actor round-trip just
  fine. Migrating them would just add Arc-publish overhead to
  dispatch with no perf benefit.
- Restructuring the `#[cfg(test)] impl App` test-surface blocks.
  Those are intentional (audit §7 escape hatch) and orthogonal.
- The `#![allow(dead_code)]` annotation on `app.rs` is a separate
  cleanup slice (`3c.final.E.cleanup`, landed alongside the swap)
  and not coupled to this B-extension work.
