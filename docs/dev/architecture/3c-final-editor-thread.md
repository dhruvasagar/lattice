# 3c.final — Editor on its own thread

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
┌─────────────────────────┐         action_tx        ┌──────────────────────────┐
│  Renderer thread        │ ───────channel─────────> │  Editor thread           │
│                         │                          │                          │
│  - input → Action       │ <── ArcSwap<RenderState> │  - dispatch(action)      │
│  - read RenderState     │     (wait-free)          │  - publish_render_state  │
│  - draw                 │                          │  - wake worker(s)        │
└─────────────────────────┘                          └──────────────────────────┘
                                                              ▲
                                              wake (Notify)   │
                                                              │
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

## 3. Why this is the right shape

- **Type-system enforced asynchronicity.** Renderer code that needs Editor state has to spell `render_state.syntax.…` etc. Anyone who tries to add a sync host call from a render path won't have a handle to call it through.
- **No new IPC primitives.** `ArcSwap<RenderState>` already exists (slice 3a). The action channel is a standard `tokio::sync::mpsc::UnboundedSender<Action>`. Coalescing on the wake side is already a `Notify`.
- **Survives shared workspace.** Worker threads (highlights today; LSP reparse and future search-index workers) wake the Editor thread the same way the renderer does — via the action channel. The Editor's dispatch tail is the single serialisation point.
- **Drops the discipline tax.** Any work that doesn't fit through `Action` doesn't compile from the renderer side. Reviewers don't have to spot it.

## 4. What currently blocks this (the migration cost)

Roughly N call sites that bypass the dispatch path:

- **TUI**: `App` exposes dozens of `do_*` / `set_*` / `refresh_*` methods callable directly. Bench code, test helpers, and a handful of TUI app-side helpers (`app/highlights.rs`, `app/folds.rs`, `app/picker.rs`) reach into `&mut self.editor.…` instead of going through actions.
- **GPUI**: `GpuiApp` exposes `dismiss_popup`, `dispatch_keystroke`, `ensure_cursor_in_viewport`, `set_viewport_height`, `refresh_pane_highlights` — all callable from `EditorView` directly.
- **Synchronous return values**: methods like `App::popup_help()` and `App::picker_open()` return `&` references into Editor state; callers expect them to remain valid for the rest of the frame. With Editor on its own thread these can't be `&` borrows — they have to be `Arc` clones from a published snapshot.

The migration is feasible because the prior X-series slices established the `RenderState` cell as the source of truth for renderer reads. What remains is finishing the conversion: every renderer read that currently reaches into `editor.foo` must be re-routed through `render_state.foo` (and `foo` must be in `RenderState`).

## 5. Proposed slicing

Each slice is independently mergeable; the channel doesn't appear until slice E.

### 3c.final.A — Audit reads

For each renderer-thread read of `editor.*`, classify:
- **Already on RenderState** — no work.
- **Move-able to RenderState** — add the field, populate in `publish_render_state`, switch the read.
- **Returns owned data** — switch caller to receive cloned `Arc<…>` from RenderState.
- **Needs a sync answer the Editor must compute** — these become the only blocking calls and need to be enumerated for slice E.

Output: a checklist in `docs/dev/operations/3c-final-audit.md`.

### 3c.final.B — Add the missing RenderState fields

Lift every "move-able" field from slice A. After this, the renderer never reads `editor.*` directly except for the few "Needs a sync answer" cases.

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

After slices A–E, the renderer's `App` / `GpuiApp` no longer needs `&mut self.editor`. Move `Editor` out of those types; let the renderer hold `action_tx: UnboundedSender<Action>` + a clone of `Arc<ArcSwap<RenderState>>`. The renderer-side `App` becomes a thin facade: input translation + an `Arc<ArcSwap<RenderState>>` reader.

This is the compile-time enforcement of paramount goal #4. Adding a sync host call from a renderer path won't typecheck.

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

## 8. Not in scope

- Plugins on their own threads — that's the existing Phase 7+ plugin host work. 3c.final is host-only.
- LSP reparse on its own thread — already done (lattice-lsp owns its own tokio runtime).
- Search-index workers / future indexers — deferred to whichever phase ships them.
- A second renderer (Web). The renderer-isolation contract here is shape-compatible with adding one later.

## 9. References

- `CLAUDE.md` paramount goals (especially #4).
- `docs/dev/architecture/design.md` §3 (three-layer architecture), §5.7 (async runtime + threading).
- `docs/dev/operations/render-thread-discipline-remediation.md` (X-series successor pointer).
- `crates/lattice-host/src/render_state.rs` (the wait-free read contract this slice extends).
- `crates/lattice-host/src/highlights_worker.rs` (existing model for off-thread work + Notify wake).
