# 3c.final — Editor on its own thread

> **Status: ✅ Completed in slice 3c.final.E.swap (commit `6d89915`).**
> This was the design proposal for moving Editor to its own thread.
> The literal struct swap landed; the actor handle now backs every
> production `App.editor_actor` field. Follow-up perf work catalogued
> in [`../operations/3c-final-b-extension.md`](../operations/3c-final-b-extension.md).
> Archived 2026-05-21.

**Status:** design proposal — 2026-05-20.
**Predecessor:** Phase 5.8.AF.5 / Slices X1, X1b, X2 (worker), X2.6, X2.9, X3.full, X5. All landed.
**Goal:** Final enforcement of paramount goal #4 (asynchronicity) — the Editor runs on a dedicated thread; renderer peers communicate via typed message passing only.

## 1. Why this exists

The X-series moved the **expensive batch work** off the renderer's per-frame body: syntax-highlight parsing (X2 worker), tick drain (X1), idle wakes (X1b), and the per-cell Div tree fan-out (X3.full). What remains: every `App::apply(action)` / `GpuiApp::dispatch_keystroke(...)` still runs **synchronously** on the renderer thread. The actions execute, the dispatch tail publishes a fresh `RenderState`, and only then does the renderer return from its keystroke handler.

For most actions this is sub-millisecond and invisible. But:

- Any action that touches LSP / search / fold-recompute / mode-cascade adds variable cost.
- Held-key bursts (`j`/`k`, `<C-d>`/`<C-u>`) saturate the renderer's input loop because every dispatched action serialises against paint.
- The paramount-goal #4 contract ("nothing blocks the UI — enforced architecturally, not by discipline") still relies on discipline today. A new contributor adding a sync filesystem call inside a dispatch arm would silently regress the renderer's responsiveness with no compile-time signal.

3c.final closes this by making the renderer's keystroke handler **non-blocking by construction**: send the action through a channel, return immediately. The Editor thread consumes actions and publishes `RenderState` updates that the renderer reads on its next frame.

## 2. Target shape

```
┌──────────────────────────────────┐    action_tx   ┌──────────────────────────┐
│  Renderer thread                 │ ────channel──> │  Editor thread           │
│                                  │                │                          │
│  input → Action                  │ <── ArcSwap<RenderState> ──               │
│  RenderState → EditorElement     │     (wait-free)│  dispatch(action)        │
│                  / FrameView     │                │  publish_render_state    │
│  GPUI paints / ratatui draws     │                │  wake worker(s)          │
└──────────────────────────────────┘                └──────────────────────────┘
                                                              ▲
                                              wake (Notify)   │
                                                     ┌────────┴────────┐
                                                     │  Worker threads  │
                                                     │  (highlights,    │
                                                     │   future: LSP    │
                                                     │   reparse, …)    │
                                                     └─────────────────┘
```

Three invariants:

1. **Renderer never holds `&mut Editor`.** All mutation goes through `action_tx.send(Action)`.
2. **Renderer never blocks on the Editor.** Send is unbounded + non-blocking; reads are `ArcSwap::load_full()` (already wait-free).
3. **One Action receiver, one thread.** No worker reads the action channel; the Editor thread owns the entire dispatch tail.

### What X3.full already gave us

The GPUI peer's `EditorElement` (slice X3.full.1–4, commit `0239cd7`) is a per-frame snapshot type — `pub(crate) struct EditorElement` holds only owned data (`Arc<String>` text, `Arc<VisibleSpans>` worker output, owned `Vec<…>` for gutter/inlays/diagnostic-underlines/matches, `Option<CursorState>`, scalars). The element's `prepaint` + `paint` methods never reach back into `GpuiApp` or `Editor` — they operate exclusively on those owned fields. The TUI peer's `FrameView` (X2.5) has the analogous shape for ratatui.

This means the renderer's **read path is already decoupled from Editor's mutable state** at the per-frame snapshot boundary. The remaining read-side work is plumbing what `window::paint_pane` and `FrameView::from_app` currently pull from `&self.app.editor.*` through `Arc<ArcSwap<RenderState>>` instead. The field set for slice B is enumerable directly from `EditorElement`'s struct definition.

What's NOT decoupled yet: the input-path mutations. `EditorView::on_key_down` calls `self.app.dispatch_keystroke(...)` synchronously on the GPUI render thread; the TUI's main loop calls `app.apply(action)` directly. These are the write-path migration targets — where the channel actually goes in.

## 3. Why this is the right shape

- **Type-system enforced asynchronicity.** Renderer code that needs Editor state has to spell `render_state.syntax.…` etc. Anyone who tries to add a sync host call from a render path won't have a handle to call it through.
- **No new IPC primitives.** `ArcSwap<RenderState>` already exists (slice 3a). The action channel is a standard `tokio::sync::mpsc::UnboundedSender<Action>`. Coalescing on the wake side is already a `Notify`.
- **Survives shared workspace.** Worker threads (highlights today; LSP reparse and future search-index workers) wake the Editor thread the same way the renderer does — via the action channel. The Editor's dispatch tail is the single serialisation point.
- **Drops the discipline tax.** Any work that doesn't fit through `Action` doesn't compile from the renderer side. Reviewers don't have to spot it.

## 4. What currently blocks this (the migration cost)

Split by read vs write track.

### Read-track holdouts (smaller than originally thought)

Now that `EditorElement` + `FrameView` are in place, the renderer's *paint-time* reads cluster at a few well-defined extraction sites:

- **GPUI**: `crates/lattice-ui-gpui/src/window.rs::paint_pane_*` reads ~10 fields off `self.app.editor.*` to populate one `EditorElement` per pane:
  - `editor.viewport_height` (line 802)
  - `editor.pane_tree` (lines 896, 898) — pane geometry
  - `editor.popup_buffer` / `editor.popup_help()` / `editor.popup_help_highlights()` (line 237, 1235)
  - `editor.should_quit` (line 267 — input-path, see below)
  - `editor.command_line` (line 868)
  - `editor.picker` (lines 995, 1127)
  - `editor.refresh_pane_highlights()` call (line 831 — moved to action in slice C)
  Plus per-pane reads inside `paint_pane`: cursor / pane.cursor / pane.scroll / pane.buffer_id / fold list / search state / visual range / inlay-hint cache / lsp_diagnostics layer. All of these populate `EditorElement` fields and are 1:1 lift candidates for `RenderState`.

- **TUI**: `crates/lattice-ui-tui/src/render.rs::FrameView::from_app` already reads through `render_state` for syntax spans (X2.5). Remaining direct reads inside the render chain (folds, show_line_numbers, app.theme, picker, completion_state, popup_buffer, command_line) follow the same pattern as GPUI — lift into RenderState.

Both sets are bounded and enumerable. The slice-B work is finite and concrete.

### Write-track holdouts (the real migration)

Every renderer-thread mutation against Editor. Three sub-clusters:

- **GPUI `EditorView::on_key_down`** (the keystroke arrival): calls `self.app.dispatch_keystroke(...)` synchronously. This is the canonical conversion target.
- **GPUI per-frame setup**: `self.app.set_viewport_height(n)` (line 803), `self.app.ensure_cursor_in_viewport()` (line 832), `self.app.editor.refresh_pane_highlights()` (line 831), `self.app.dismiss_popup()`. Each becomes an `Action::*` variant or moves into the Editor's per-tick body.
- **TUI runtime + app helpers**: `app.apply(action)` synchronous call inside `main_loop`. Plus the App-side helpers that currently mutate `&mut self.editor.…` (`App::refresh_pane_highlights`, `App::set_viewport_height`, picker setup, file-tree ops).

### Synchronous-return holdouts

Methods like `App::popup_help()`, `App::picker_open()`, `App::completion_state()` return `&` references into Editor state. Callers expect them to remain valid for the rest of the frame. With Editor on its own thread these can't be `&` borrows.

The fix is uniform: those reads come from RenderState, so the renderer holds an `Arc<…>` clone for the frame's lifetime instead of an `&` borrow. Slice B lifts the field; the caller's `app.popup_help()` becomes `render_state.popup.help.as_ref()`.

## 5. Proposed slicing

Each slice is independently mergeable; the channel doesn't appear until slice E.

### 3c.final.A — Audit reads (narrowed by EditorElement)

Walk every renderer-thread `editor.*` read and classify:
- **Already on RenderState** — no work. (After X2 + X3.full, most paint-time reads.)
- **Move-able to RenderState** — add the field, populate in `publish_render_state`, switch the read.
- **Returns owned data** — switch caller to receive cloned `Arc<…>` from RenderState.
- **Needs a sync answer the Editor must compute** — these become the only blocking calls and need to be enumerated for slice E.

Output: a checklist in `docs/dev/archive/3c-final-audit.md`. The starting set is small enough to walk in one session — `grep -rn 'self\.app\.editor\.\|app\.editor\.' crates/lattice-ui-*` is the exhaustive list.

### 3c.final.B — Add the missing RenderState fields

Lift every "move-able" field from slice A. The concrete target field set is the union of `EditorElement`'s struct (GPUI) and `FrameView`'s struct (TUI) plus the chrome-overlay state both peers read (popup_help, picker, completion_state, command_line, modal label, lsp_progress headline). Each field already has a definite owning subsystem in `RenderState`'s nested children (`syntax.*`, `diagnostics.*`, `popup.*`, `picker.*`, `completion.*`, `cmdline.*`, ...) so the addition is mostly "expand the child struct, populate in `publish_render_state`, switch the read site".

After this, the renderer never reads `editor.*` directly except for the few "Needs a sync answer" cases. Both peers' snapshot-construction sites (`EditorElement` build, `FrameView::from_app`) read exclusively from RenderState.

### 3c.final.C — Convert non-dispatch mutations to actions

Calls like `app.set_viewport_height(n)` that currently mutate directly become `app.send_action(Action::SetViewportHeight(n))`. The action handler dispatches `Editor::set_viewport_height(n)` host-side. Slice introduces new Action variants but the action channel stays as it is today (synchronous, in-thread); the dispatcher just gets a wider switch.

### 3c.final.D — Handle blocking-call holdouts

For the few "Needs a sync answer" cases from slice A, decide per case:
- Refactor away (most should disappear once we read from RenderState instead).
- Reply-channel: `Action::QueryFoo { reply: oneshot::Sender<Foo> }`.
- Out of scope: move to plugin / async background work.

### 3c.final.E — Spawn the Editor thread

A new `std::thread::spawn` (NOT a tokio task — the Editor needs its own dedicated thread, isolated from the workers) loops on `action_rx.blocking_recv()`. Each pulled action runs through `Editor::dispatch_action(action)`; the existing publish + wake-worker tail stays unchanged.

Renderer's `app.send_action(Action::X)` becomes a `action_tx.send(Action::X)?`. Errors map to "Editor thread dead" — a fatal condition.

### 3c.final.F — Drop `&mut Editor` from renderer entirely

After slices A–E, the renderer's `App` / `GpuiApp` no longer needs `&mut self.editor`. Move `Editor` out of those types; let the renderer hold `action_tx: UnboundedSender<Action>` + a clone of `Arc<ArcSwap<RenderState>>`. The renderer-side `App` becomes a thin facade:
- Input translation (`KeyDownEvent` → `Action`).
- A `RenderState` reader (loads once per frame, fans into `EditorElement` / `FrameView`).

For the GPUI peer the practical shape is "`EditorView` holds `EditorViewHandles { action_tx, render_state: Arc<ArcSwap<RenderState>> }` and that's it". The `EditorElement` construction site (`paint_pane`) reads from the snapshot. The `cx.spawn` paint-request bridge from X1b stays unchanged — it already takes a `Notify` clone, no Editor borrow.

This is the compile-time enforcement of paramount goal #4. Adding a sync host call from a renderer path won't typecheck — there's no `Editor` accessible to call into.

### 3c.final.G — Cleanup + docs

- Update `CLAUDE.md` paramount-goal-#4 wording to match the new architecture.
- Update `design.md §3` (architectural overview) + §5.7 (async runtime) with the realised shape.
- Add an architectural diagram (the ASCII in section 2 above, fleshed out).
- Retire the App-side helpers that became no-op shims in slice F.

## 6. Acceptance criteria

- `cargo bench -p lattice-host --bench highlights_worker` and
  `cargo bench -p lattice-ui-gpui --features window,bench-internals --bench editor_element_frame`
  both stay green (no regression from the thread move).
- Held-j stress test (existing X-series regression target):
  - GPUI peer: cursor visible throughout; no "stuck screen" pause.
  - TUI peer: scroll smooth at terminal refresh rate.
- New test class: a renderer-side test that submits N actions through `send_action` and asserts the renderer's input-handler latency is sub-millisecond regardless of action cost (proves the renderer doesn't block on Editor work).
- `cargo check --workspace --all-features` clean. No `unsafe` introduced. No new direct `editor.` reads from renderer crates (enforced by `grep` in CI).

## 7. Risks

- **Sync-return holdouts** (slice D). If any can't be eliminated and need a reply-channel, latency under load could regress vs the in-thread call. Mitigation: choose the action shape carefully — most expectation-of-sync-return cases are actually "read this field after the action settles", which the RenderState read handles.
- **Action enum bloat.** Each currently-direct mutation becomes an Action variant. Mitigation: group by domain (e.g. `Action::Editor(EditorAction)` if the enum grows past ~200 variants).
- **Test infrastructure.** Many existing tests construct `App` and call `app.editor.x = y` directly. They'll need refactoring to either use the action channel or to be re-pointed at `Editor`-only constructors. Mitigation: keep a test-only `App::editor_mut()` escape hatch behind `#[cfg(any(test, feature = "test-internals"))]`.
- **Render-state struct churn** (post-EditorElement). Slice B will grow `RenderState`'s nested children significantly. Mitigation: each new field's `publish_render_state` cost must stay under 1 µs total — `Arc::clone` + plain-old-data move, no per-publish allocations. The existing `syntax_visible_spans_cell` nested-Arc pattern is the template; reuse it for any field large enough to benefit from inner-cell publishes (cmdline buffer, picker candidates, completion list).
- **EditorElement field drift.** If slice B is sliced as multiple sub-slices ("lift the cursor/scroll fields", then "lift the gutter fields", then …), `EditorElement` would carry MIXED reads — some from snapshot, some from `&self.app.editor.…` — for the duration of the migration. That's awkward but compiles. Better: land slice B as one PR per EditorElement field group, with `paint_pane` updated atomically each time. The X3.full slicing established that as a tractable pattern.

## 8. Not in scope

- Plugins on their own threads — that's the existing Phase 7+ plugin host work. 3c.final is host-only.
- LSP reparse on its own thread — already done (lattice-lsp owns its own tokio runtime).
- Search-index workers / future indexers — deferred to whichever phase ships them.
- A second renderer (Web). The renderer-isolation contract here is shape-compatible with adding one later.

## 9. References

- `CLAUDE.md` paramount goals (especially #4).
- `docs/dev/architecture/design.md` §3 (three-layer architecture), §5.7 (async runtime + threading).
- `docs/dev/archive/render-thread-discipline-remediation.md` (X-series successor pointer).
- `crates/lattice-host/src/render_state.rs` (the wait-free read contract this slice extends).
- `crates/lattice-host/src/highlights_worker.rs` (existing model for off-thread work + Notify wake).
