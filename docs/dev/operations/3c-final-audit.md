# 3c.final.A — Renderer ↔ Editor read/write audit

**Status:** audit complete — 2026-05-20.
**Predecessor:** `docs/dev/architecture/3c-final-editor-thread.md` §5 (slicing).
**Successor:** 3c.final.B (RenderState field additions), 3c.final.C (mutation→Action lifts).

## 0. Purpose

3c.final puts `Editor` on its own dedicated thread. Renderer peers
(GPUI + TUI) become pure consumers of `Arc<ArcSwap<RenderState>>`
plus producers of `Action` values on an `mpsc::UnboundedSender`.

Before any thread-move, every renderer-thread access of `editor.*`
state must be enumerated and classified. This document is that
enumeration. Each entry carries one of four classifications:

- **A — Already on RenderState.** Read site already uses
  `render_state.X` or one of its sub-state caches. No work.
- **B — Move-able to RenderState.** Read site reads
  `app.editor.X` today; the value is publishable as part of the
  dispatch tail. Slice 3c.final.B widens the relevant sub-state
  and switches the read.
- **C — Mutation, convert to Action.** Renderer-thread write
  (direct field assignment or `app.X()` helper that mutates). Slice
  3c.final.C lifts to `Action::*` variant.
- **D — Sync-answer holdout.** Renderer needs a value the
  Editor must compute synchronously and that cannot be lifted to
  RenderState (no natural per-tick cadence, or value depends on
  the renderer's own per-frame inputs). Slice 3c.final.D
  resolves per case.

A fifth bucket — **N — Not on renderer thread** — covers the
~70% of `self.editor.*` accesses that occur inside App-side
helpers which themselves are only called from `App::apply` (the
dispatch tail). After slice F those helpers move into
`Editor::dispatch_action` and stop being renderer-thread code at
all. Listed here only when a helper IS called from a
renderer-hot-path site.

## 1. Scope and methodology

The grep that drove this audit:

```
grep -rn 'self\.app\.editor\.\|\bapp\.editor\.' crates/lattice-ui-gpui crates/lattice-ui-tui
```

Raw line count: **500**. Distribution:

| File                                          | Hits |
| --------------------------------------------- | ---- |
| `crates/lattice-ui-tui/src/render.rs`         | 183  |
| `crates/lattice-ui-tui/src/app/lsp.rs`        | 144  |
| `crates/lattice-ui-tui/src/app/picker.rs`     |  58  |
| `crates/lattice-ui-tui/src/picker_sources.rs` |  36  |
| `crates/lattice-ui-tui/src/app/lifecycle.rs`  |  22  |
| `crates/lattice-ui-tui/src/app.rs`            |  12  |
| `crates/lattice-ui-gpui/src/window.rs`        |  11  |
| `crates/lattice-ui-tui/src/runtime.rs`        |   8  |
| `crates/lattice-ui-gpui/src/lib.rs`           |   6  |
| `crates/lattice-ui-tui/src/app/test_helpers.rs` |   5 |
| `crates/lattice-ui-tui/src/app/messages.rs`   |   3  |
| `crates/lattice-ui-tui/src/app/dispatch.rs`   |   3  |
| `crates/lattice-ui-tui/src/app/popup.rs`      |   2  |
| `crates/lattice-ui-tui/src/app/boot.rs`       |   2  |
| `crates/lattice-ui-tui/src/app/search.rs`     |   1  |
| `crates/lattice-ui-tui/src/app/options.rs`    |   1  |
| `crates/lattice-ui-tui/src/app/help.rs`       |   1  |
| `crates/lattice-ui-tui/src/app/display.rs`    |   1  |
| `crates/lattice-ui-tui/benches/render.rs`     |   1  |

Of `render.rs`'s 183, **~120 live below line 4000 inside
`#[cfg(test)]` test modules**. Those are not renderer-thread paths
— they're test fixtures that construct an `App` and poke it
directly. They migrate as part of slice F's `App::editor_mut()`
escape hatch (declared in plan §7 risks); not in scope here.

Production renderer-thread sites that matter for the migration
shrink to roughly **120 distinct call sites**, which is the field
of work for slices B (~half) and C+D+E (the other half).

The audit walks the two peers separately. Within each peer:
read-track first (paint snapshot construction), then write-track
(keystroke / per-frame setup).

## 2. GPUI peer

### 2.1 Read-track — `EditorView::paint_pane` / `EditorView::render`

`window.rs` lines 200-1300, roughly. Per pane, the renderer
extracts ~15 fields off `&self.app.editor.*` into one
`EditorElement` per frame (the X3.full owned-data snapshot).

| Field read                              | Site (window.rs)                  | Class | Target sub-state                  | Notes                                                |
| --------------------------------------- | --------------------------------- | ----- | --------------------------------- | ---------------------------------------------------- |
| `editor.paint_request`                  | new() / cx.spawn  L208            | A     | already an `Arc<Notify>`          | clone in constructor, kept as a separate cell        |
| `editor.popup_buffer.is_some()`         | on_key_down L237                  | B     | `popup.is_open`                   | also: §2.2 write-track entry below                   |
| `editor.should_quit`                    | on_key_down L267                  | B     | `lifecycle.should_quit`           | new sub-state field (no domicile today)              |
| `editor.viewport_height`                | render L802                       | A     | `active_document.viewport_height` | compared against newly-computed; if differs → write  |
| `editor.refresh_pane_highlights()`      | render L831                       | C     | n/a — Action::RefreshPaneHighlights | already noop'd most ticks; lift call entirely        |
| `editor.command_line`                   | render L868 (ModalState::Command) | B     | `cmdline.text`                    | currently a `String`; lift as `Arc<String>`          |
| `editor.search_line.as_ref()`           | render L874                       | B     | `cmdline.search_pattern`          | also carries `direction` (`SearchDirection`)         |
| `editor.pane_tree.active_index()`       | render L896                       | B     | `panes.active_index`              | PanesRenderState is empty placeholder today          |
| `editor.pane_tree.root()`               | render L898                       | B     | `panes.root: Arc<PaneNode>`       | pane tree is small; `Arc::clone` per publish         |
| `editor.insert_completion.as_ref()`     | render L902                       | B     | `completion.popup: Option<...>`   | CompletionRenderState placeholder today              |
| `editor.picker.as_ref()` (overlay)      | render L995                       | B     | `picker.state: Option<...>`       | PickerRenderState placeholder today                  |
| `editor.picker.as_ref()` (minibuffer)   | render L1127                      | B     | same as above                     | one publication satisfies both reads                 |
| `editor.popup_help()`                   | render L1235                      | B     | `popup.help: Option<...>`         | returns `&PopupBuffer`; lift as `Arc<PopupBuffer>`   |
| `editor.popup_help_highlights()`        | render L1242                      | B     | `popup.help_highlights`           | `Arc<Vec<Vec<StyledSpan>>>`                          |
| `editor.popup_placement` (via L1424)    | render L1424 (status routing)     | B     | `popup.placement`                 | small Copy enum                                      |

Inside `paint_pane` (window.rs lines 332-737):

| Field                                            | Site                  | Class | Target                                 | Notes                                                |
| ------------------------------------------------ | --------------------- | ----- | -------------------------------------- | ---------------------------------------------------- |
| `editor.render_state.load()` (twice)             | L350, L515            | A     | RenderState load itself                | wait-free; the load IS the contract                  |
| `editor.pane_tree.leaves()`                      | L352                  | B     | `panes.leaves: Arc<[PaneState]>`       | PanesRenderState field                                |
| `editor.buffers.document_handle(buffer_id)`      | L361                  | D     | sync-answer (`Arc<DocumentHandle>`)    | per-pane buffer resolution — needs `buffers.lookup(id)` on RenderState |
| `editor.buffer_uris.get(&pane.buffer_id)`        | L437                  | B     | `buffers.uri_for(id)`                  | small map; `Arc<HashMap<BufferId, Uri>>`             |
| `editor.host_theme`                              | L453, L553            | B     | `theme: Arc<HostTheme>`                | new top-level RenderState field                      |
| `editor.line_inside_closed_fold(idx)`            | L462                  | B     | `active_document.folds: Arc<[Fold]>`   | predicate becomes method on slice                    |
| `editor.fold_start_at(idx)`                      | L464                  | B     | same as above                          | same source                                          |
| `editor.visual_selection_range()`                | L492                  | B     | `active_document.visual_range`         | already have `visual_anchor`; compute range on publish |
| `editor.current_match`                           | L496                  | B     | `active_document.current_match`        | small Copy                                            |
| `editor.all_matches.clone()`                     | L498                  | B     | `active_document.all_matches: Arc<[Range]>` | already a `Vec<Range>` on editor               |
| `editor.substitute_preview.as_ref()`             | L504                  | B     | `active_document.substitute_preview`   | `Option<Arc<SubstitutePreview>>`                     |

All marked **B** are publish-on-tail candidates — `Arc::clone`
in `publish_render_state`, the renderer reads through the snapshot
guard. Only the one **D** (per-buffer document handle lookup) needs
design attention; the leaning is: lift `buffers.handles:
Arc<HashMap<BufferId, Arc<DocumentHandle>>>` so the lookup becomes
a HashMap probe against the snapshot, not a sync call.

### 2.2 Write-track — GPUI

| Site                                | Action surface                                                 | Class |
| ----------------------------------- | -------------------------------------------------------------- | ----- |
| `EditorView::on_key_down` L246      | `self.app.dispatch_keystroke(...)` — the canonical entry point | C     |
| `EditorView::on_key_down` L241      | `self.app.dismiss_popup()` (sync method)                       | C     |
| `EditorView::render` L803           | `self.app.set_viewport_height(n)`                              | C     |
| `EditorView::render` L813           | `self.app.ensure_cursor_in_viewport()`                         | C     |
| `EditorView::render` L831           | `self.app.editor.refresh_pane_highlights()`                    | C     |

The five rows above are the GPUI peer's full write-track surface.
Three observations:

1. `set_viewport_height` and `ensure_cursor_in_viewport` already
   publish their own `RenderState`; they're sync today only
   because the renderer needs the published values back this
   frame. After slice E they become `Action::SetViewportHeight(n)`
   + `Action::EnsureCursorVisible`; the renderer reads the next
   frame's RenderState. This is functionally equivalent (one
   frame of latency) and the latency was already there for any
   value the dispatch tail produces.
2. `refresh_pane_highlights` is a noop most ticks (its body is
   gated on per-pane buffer change). Lift to
   `Action::RefreshPaneHighlights` and let the dispatcher noop
   internally — the renderer-side call cost is exactly zero.
3. `dismiss_popup` becomes `Action::DismissPopup`. The "popup
   open?" check at L237 reads `popup.is_open` from the snapshot.

### 2.3 GPUI test-only touches

`lib.rs` L1035-L1085 (5 hits) read `app.editor.mode_registry`,
`app.editor.modal`, `app.editor.document_buffer_id` from test
assertions. The `modal` and `document_buffer_id` reads migrate to
`render_state.active_document` directly (already on RS). The
`mode_registry.iter_meta()` walk is a test-only introspection;
keep it on the `App::editor_mut()` escape hatch behind
`cfg(test, feature = "test-internals")` per plan §7.

## 3. TUI peer

### 3.1 Read-track — `draw_frame` and chain

`render.rs` lines 71-3450 (production code). The TUI's
`FrameView::from_app` (L93) is the snapshot-construction site
analogous to `paint_pane`'s `EditorElement` build.

`FrameView` carries three frozen fields and a `&App` borrow. The
`&App` borrow is what slice F has to eliminate — every read
through `view.app.editor.X` is implicitly a renderer-thread read
even when the doc-comment says "renderer-agnostic":

| Field on FrameView                        | Source                            | Class | Target                                                    |
| ----------------------------------------- | --------------------------------- | ----- | --------------------------------------------------------- |
| `view.folds: Arc<[Fold]>`                 | `app.editor.folds.clone()`        | B     | `active_document.folds: Arc<[Fold]>`                      |
| `view.visible_highlights: Arc<[Vec<...>]>`| `rs.syntax.visible_spans.load()`  | A     | already RenderState (X2)                                  |
| `view.show_line_numbers: bool`            | `app.show_line_numbers()`         | B     | `active_document.show_line_numbers` (option_cache mirror) |
| `view.relative_line_numbers: bool`        | `app.relative_line_numbers()`     | B     | `active_document.relative_line_numbers`                   |

Inside `draw_frame` body (`render.rs` L186+) and its callees, the
direct `app.editor.*` reads break down as:

| Read                                                  | Site (render.rs)        | Class | Target                                                    |
| ----------------------------------------------------- | ----------------------- | ----- | --------------------------------------------------------- |
| `app.editor.picker.as_ref()`                          | L208, L249, L283, L823, L854, L920 | B     | `picker.state: Option<Arc<PickerState>>`         |
| `app.editor.completion_state.as_ref()`                | L216, L708, L2284       | B     | `completion.state: Option<Arc<CompletionState>>`          |
| `app.editor.popup_buffer.is_some()` / `.expect()`     | L267, L1062             | B     | `popup.buffer_id: Option<BufferId>`                       |
| `app.editor.insert_completion.as_ref()`               | L296, L330, L511        | B     | `completion.insert: Option<Arc<InsertCompletion>>`        |
| `app.editor.popup_placement`                          | L1076, L1424            | B     | `popup.placement`                                         |
| `app.editor.active_buffer`                            | L1098, L1148, L1195, L1437, L1992 | A | already `active_document.buffer_kind` (X3a)               |
| `app.editor.all_matches`                              | L1150, L1578, L2718     | B     | `active_document.all_matches: Arc<[Range]>`               |
| `app.editor.current_match`                            | L1158, L1587, L2725     | B     | `active_document.current_match`                           |
| `app.editor.pane_tree.{active,active_index,leaves,compute_rects}` | L1440, L1487, L1488, L1523, L1548, L1659, L1660, L1699, L2531 | B | `panes.{root,active_index,leaves}` |
| `app.editor.pane_highlights.get(&pane_idx)`           | L2000                   | B     | `syntax.pane_highlights: Arc<HashMap<usize, ...>>`        |
| `app.editor.option_cache.show_whitespace`             | L2033, L2631, L2913, L3445-L3449 | B | `active_document.option_cache: Arc<OptionCache>`         |
| `app.editor.buffers.with_oil(...)`                    | L2167                   | D     | sync-answer (Oil snapshot extraction)                      |
| `app.editor.buffers.document_handle(buffer_id)`       | L1965                   | D     | sync-answer (same as GPUI §2.1)                           |
| `app.editor.command_line`                             | L2248                   | B     | `cmdline.text`                                            |
| `app.editor.auto_submit_after_chord`                  | L2263                   | B     | `cmdline.auto_submit_hint`                                |
| `app.editor.last_message`                             | L2319                   | B     | `messages.last: Option<Arc<Message>>`                     |
| `app.editor.buffer_uris.get(...)`                     | L2355, L3367            | B     | `buffers.uri_for(id)`                                     |
| `app.editor.lsp.servers_for(uri)`                     | L2358                   | B     | `lsp.servers_for(uri)` — fold into LspRenderState         |
| `app.editor.lsp_progress` (HashMap iter)              | L2377                   | B     | `lsp.progress: Arc<HashMap<...>>`                         |
| `app.editor.buffers.name_of(id)`                      | L2437                   | B     | `buffers.name_of(id)` — lift HashMap                      |
| `app.editor.substitute_preview.as_ref()`              | L2855                   | B     | `active_document.substitute_preview`                      |
| `app.editor.option_cache.current_line_highlight`      | L2913                   | B     | (same option_cache lift as L2033)                         |
| `app.editor.document.selections()`                    | L2993                   | B     | `active_document.selections: Arc<[Selection]>`            |
| `app.editor.visual_selection_range()`                 | L3024                   | B     | `active_document.visual_range`                            |
| `app.editor.snapshot_cache.load_arc()`                | runtime.rs L208         | A     | `active_document.snapshot` (X3a)                          |
| `app.editor.render_state.load()` / `.load_full()`     | L100, L120, L2666, L2774, L2817, L3394 | A | self-reference; already RS                       |
| `app.editor.publish_render_state()`                   | L107 (boot)             | C     | called by App boot; slice E moves to actor                |

A handful of `editor.X` accesses are doc-comments referencing the
field name in prose, not actual reads — counted in the grep but
not in this table.

### 3.2 Write-track — TUI runtime

| Site                                  | Surface                                                | Class |
| ------------------------------------- | ------------------------------------------------------ | ----- |
| `runtime::main_loop` L171             | `app.editor.terminal_width = Some(size.width)`         | C     |
| `runtime::main_loop` L170             | `app.set_viewport_height(...)`                         | C     |
| `runtime::main_loop` L178             | `app.editor.pending_redraw = false` (after clear)      | C     |
| `runtime::main_loop` L191             | `app.refresh_pane_highlights()`                        | C     |
| `runtime::main_loop` L222-L233        | `TranslateContext { builtins, keymap, partial_chord }` | D     |
| `runtime::main_loop` (input dispatch) | `app.apply(action)` (synchronous)                      | C     |

The TranslateContext reads (L222-L233 borrow `&app.editor.builtins`,
`&app.editor.keymap`, `&app.editor.partial_chord` — all `&'a`
borrows that survive for the duration of the translate call) are the
canonical D-bucket: the input translator needs these read snapshots
to decide which action a keystroke produces, and they cannot move
to RenderState in their current shape because they hold large
indexed structures the translator needs to query randomly.

Resolution path (per plan §5 slice D): publish each as
`Arc<...>` cells (already cheap to clone) on a new
`TranslatorRenderState` sub-state. The translator reads `Arc::clone`
once per keystroke; the `keymap` resolver runs on the renderer
side against that clone. This preserves the renderer's "translate
keystroke → Action" being fully renderer-thread work, with the
only Editor-side step being the dispatch of the produced Action.

### 3.3 TUI App-side helpers (called from renderer thread today)

The 144 hits in `app/lsp.rs`, 58 in `app/picker.rs`, 36 in
`picker_sources.rs`, etc. are App-method bodies that mutate
`self.editor.*`. They are called from:

- `App::apply` (the dispatch tail) — runs on the renderer thread
  today because `App::apply` is called synchronously from
  `main_loop`. After slice E, `apply` runs on the Editor thread.
- A handful of helpers called directly from `runtime::main_loop`
  (per §3.2 table above) — those need direct `Action::*` lifts.

The helpers called only from `apply` are **N-bucket** for this
audit. They migrate as a wholesale move when `apply` moves to the
Editor thread (slice F); no per-line work is needed in slice C.

The four helpers called from `main_loop` directly are:

| Helper                          | Slice C action                                  |
| ------------------------------- | ----------------------------------------------- |
| `app.set_viewport_height(n)`    | `Action::SetViewportHeight(n)`                  |
| `app.refresh_pane_highlights()` | `Action::RefreshPaneHighlights`                 |
| `app.ensure_cursor_in_viewport()` (GPUI) | `Action::EnsureCursorVisible`          |
| `app.dismiss_popup()` (GPUI)    | `Action::DismissPopup`                          |

The TUI's `app.refresh_pane_highlights()` deliberately uses the
non-`editor.*` form (it's a method on `App` not on `editor` — see
`crates/lattice-ui-tui/src/app/highlights.rs` L122) but it
mutates editor state under the hood. Same lift.

## 4. Synchronous-return holdouts (slice D targets)

Per §1 taxonomy, **D-bucket** entries are renderer-thread reads
where the Editor must answer synchronously and the answer cannot
be lifted to RenderState in its present shape.

After the walk in §2 and §3 the surviving D entries are:

1. **Per-buffer document handle lookup** —
   `editor.buffers.document_handle(buffer_id)` from `paint_pane` and
   `draw_inactive_document`. Used to obtain a snapshot for an
   inactive pane's buffer. **Resolution:** lift
   `buffers.handles: Arc<HashMap<BufferId, Arc<DocumentHandle>>>` to
   `BuffersRenderState`. Per-handle snapshot loads are already
   wait-free behind the cache.
2. **Per-buffer Oil view extraction** —
   `editor.buffers.with_oil(buffer_id, |o| ...)` from
   `draw_inactive_document`. Closure-form so the renderer
   borrows the Oil view briefly. **Resolution:** publish
   `buffers.oil_views: Arc<HashMap<BufferId, Arc<OilView>>>` on
   the registry; renderer takes `Arc::clone` instead of borrowing
   under closure.
3. **TranslateContext borrow batch** (`builtins`, `keymap`,
   `partial_chord` from `runtime.rs`). **Resolution:** new
   `TranslatorRenderState` sub-state carrying `Arc`-clone of
   each; translator reads once per keystroke into a local owned
   `TranslateContext`. This preserves the translator's locality
   (one HashMap of bindings → one resolved Action) while removing
   the lifetime tie.

No entries require oneshot reply-channels (the plan's escape
hatch). Every sync-return case dissolves into "publish the value
as RenderState; read the snapshot."

## 5. Slice B field set (concrete enumeration)

Direct enumeration of the RenderState fields slice B needs to
add. Grouped by sub-state:

### 5.1 `ActiveDocumentRenderState` additions

```
folds: Arc<[Fold]>,
all_matches: Arc<[Range]>,
current_match: Option<Range>,
visual_range: Option<Range>,
substitute_preview: Option<Arc<SubstitutePreview>>,
selections: Arc<[Selection]>,        // from doc.selections()
show_line_numbers: bool,              // option_cache mirror
relative_line_numbers: bool,
option_cache: Arc<OptionCache>,       // entire cache once; whitespace, current_line_highlight, …
```

### 5.2 `BuffersRenderState` (today empty)

```
handles: Arc<HashMap<BufferId, Arc<DocumentHandle>>>,
oil_views: Arc<HashMap<BufferId, Arc<OilView>>>,
uris: Arc<HashMap<BufferId, Uri>>,
names: Arc<HashMap<BufferId, Arc<str>>>,
```

### 5.3 `PanesRenderState` (today empty)

```
root: Arc<PaneNode>,
leaves: Arc<[PaneState]>,
active_index: usize,
```

### 5.4 `PickerRenderState` (today empty)

```
state: Option<Arc<PickerState>>,      // Picker struct as a whole
```

### 5.5 `CompletionRenderState` (today empty)

```
insert: Option<Arc<InsertCompletion>>,
state: Option<Arc<CompletionState>>,  // cmdline-completion variant
```

### 5.6 `PopupRenderState` (today empty)

```
buffer_id: Option<BufferId>,
help: Option<Arc<PopupBuffer>>,
help_highlights: Arc<[Vec<StyledSpan>]>,
placement: PopupPlacement,
is_open: bool,                         // derived from buffer_id; convenience
```

### 5.7 `MessagesRenderState` (today empty)

```
last: Option<Arc<EchoMessage>>,
```

### 5.8 `ModelineRenderState` (today empty)

```
modal_label: &'static str,             // resolved on publish
auto_submit_hint: bool,
search_pattern: Option<Arc<str>>,
search_direction: Option<SearchDirection>,
```

### 5.9 `CmdlineRenderState` (new sub-state)

```
text: Arc<str>,
search: Option<SearchLineSnapshot>,
```

### 5.10 `LspRenderState` additions

```
progress: Arc<HashMap<(ServerId, Token), ProgressUpdate>>,
servers_by_uri: Arc<HashMap<Uri, Arc<[ServerHandle]>>>,
```

### 5.11 `TranslatorRenderState` (new sub-state, slice D-driven)

```
builtins: Arc<Builtins>,
keymap: Arc<Keymap>,
partial_chord: Arc<PartialChord>,
```

### 5.12 `LifecycleRenderState` (new sub-state)

```
should_quit: bool,
pending_redraw: bool,                  // for TUI redraw signal
terminal_width: Option<u16>,
```

### 5.13 Top-level `RenderState`

```
theme: Arc<HostTheme>,                  // host theme used by both peers
```

Per plan §7 (render-state struct churn risk): every newly-added
field's `publish_render_state` cost must stay under 1 µs.
`Arc::clone` + scalar moves are the rule; no per-publish
allocations. The existing `syntax_visible_spans_cell` nested-Arc
pattern is the template for fields large enough to benefit from
inner-cell publishes (`cmdline.text`, `picker.state.candidates`,
`completion.state.candidates`).

## 6. Slice C action enum additions

Per §2.2 and §3.2, the renderer-thread mutations that need
`Action::*` variants:

```
Action::SetViewportHeight(u32),
Action::EnsureCursorVisible,
Action::RefreshPaneHighlights,
Action::DismissPopup,
Action::SetTerminalWidth(u16),
Action::AcknowledgeRedraw,
```

All six are scalar / unit. None require reply-channels. The
dispatcher's existing match arm covers most by trivial extension.

`Action::DispatchKeystroke(...)` is **not** added here — the
GPUI peer's `app.dispatch_keystroke(...)` already produces an
internal `Action::*` value via the translator. Slice E wires the
translator's output through the channel; no new variant.

## 7. Acceptance check for slice A

- [x] Every renderer-thread `editor.*` access classified A/B/C/D/N.
- [x] Slice B field set enumerated by sub-state.
- [x] Slice C action set enumerated.
- [x] Slice D holdouts enumerated with resolutions (no
      reply-channels needed).
- [x] App-side helpers (the 70% N-bucket) accounted for via
      slice F's wholesale move-into-Editor-thread.

Open questions punted to subsequent slices:

- **Slice B ordering.** Plan §7 prefers one PR per
  `EditorElement` field group with `paint_pane` updated
  atomically. Recommended group order: (1) panes + buffers
  (foundation for inactive panes), (2) folds + selections (low
  churn), (3) picker / completion / popup (largest types but
  bounded structure), (4) lsp.progress + servers_by_uri,
  (5) translator (slice D-coupled), (6) lifecycle + theme.
- **Slice C action enum ordering.** Recommended: lift
  `SetViewportHeight` + `EnsureCursorVisible` first (both peers
  share the call shape); validate dispatch tail still publishes
  in time; lift the remaining four together.
- **Test-internals escape hatch.** The plan's risk-3 note. Slice
  F will need `App::editor_mut()` behind `cfg(any(test,
  feature = "test-internals"))` so the existing test helpers
  (`set_modal`, `set_cursor`, direct field assignments in
  `render.rs` tests) keep compiling. No action needed in slice
  A–E; flagged here so it isn't forgotten.

## 8. References

- `docs/dev/architecture/3c-final-editor-thread.md` (plan).
- `crates/lattice-host/src/render_state.rs` (current
  sub-state shape).
- `crates/lattice-ui-gpui/src/editor_element.rs` (GPUI per-pane
  snapshot type).
- `crates/lattice-ui-tui/src/render.rs` L71-L163 (TUI FrameView
  + chain).
- `docs/dev/operations/render-thread-discipline-remediation.md`
  (X-series successor pointer).
