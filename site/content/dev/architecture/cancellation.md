+++
title = "Foreground cancellation (`<C-g>`)"
+++


Design for user-initiated cancellation of long-running or stuck
foreground operations — search scans, LSP commands, picker fills,
WASM plugin calls — via a single universal key binding.

Companion documents: `design.md` §5.7 (async runtime), `mode-architecture.md`
(mode reset path), `lsp-architecture.md` (LSP pending-token pattern).
Slice sequencing lives in
[`docs/dev/operations/slice-plans/cancellation.md`](../operations/slice-plans/cancellation).

## 1. The problem

Any user-initiated operation that spawns async work — search,
LSP rename, picker fill, WASM plugin call — can stall. The user
has no escape short of restarting the process. Vim's `<C-c>`,
emacs's `<C-g>`, and every terminal tool's `^C` all exist because
this failure mode is universal.

The substrate already has `CancellationToken` (`lattice_protocol::
CancellationToken`) threaded through search loops, LSP requests,
and the grammar `execute()` call. What is missing is:

1. A way for the user to signal cancellation at a single, memorable
   key regardless of which operation is in flight.
2. A seam in the Editor that lets every async entry-point enroll in
   that signal.
3. A mode-reset that returns the editor to a stable Normal state so
   the user can keep working.

## 2. Paramount-goal alignment

**Goal #1 (performance):** The cancel signal is a single atomic flag
flip (`CancellationToken::cancel()`). The UI thread pays zero polling
cost; async tasks wake on drop/cancellation via tokio's select macro.
No renderer work on the hot path.

**Goal #3 (vim semantics):** `<C-c>` is vim's universal cancel. We
expose the action at `<C-g>` (emacs keyboard-quit) and also `<C-c>`
in Normal/Op-pending/Visual modes. Insert-mode `<C-c>` already exits
to Normal without abbreviation trigger — that semantics is preserved;
Insert cancellation lands via `<Esc>` or a second `<C-g>`.

**Goal #4 (asynchronicity):** Cancellation is non-blocking by
construction. The token is a cheap clone-able handle; the async task
observes it at its next `yield_now` / select-sleep / fuel-check point
and exits cleanly. The UI thread never waits for confirmation.

## 3. Scope: foreground vs. background

`<C-g>` cancels **foreground operations** — work the user explicitly
triggered that may hold up the next interaction:

- Project-wide search / picker fill
- LSP commands: hover, rename, code-actions, format
- WASM plugin calls driven by a user action

It does NOT cancel:

- Background indexing (owns its own long-lived token managed by the
  indexer subsystem lifecycle)
- Auto-save / file watchers (same)
- LSP server boot handshake (already has its own token; interrupting
  it would leave the session in an undefined state)

The distinction is simple: if the user triggered it interactively, it
is foreground.

## 4. Data model

### 4.1 `Editor.active_cancel`

```rust
// in lattice-host/src/lib.rs  (Editor struct)
pub active_cancel: Option<lattice_protocol::CancellationToken>,
```

Holds the token for the most recently armed foreground operation.
`None` when the editor is idle.

### 4.2 `Editor::arm_cancel()`

```rust
pub fn arm_cancel(&mut self) -> lattice_protocol::CancellationToken {
    if let Some(old) = self.active_cancel.take() {
        old.cancel();    // kill any prior op immediately
    }
    let token = lattice_protocol::CancellationToken::new();
    self.active_cancel = Some(token.clone());
    token
}
```

Called by every user-initiated async entry-point before spawning.
Returns a child-token the spawned task holds. Cancelling the parent
token (stored in `active_cancel`) propagates to every child.

Crucially, `arm_cancel()` cancels the predecessor first. This means
a second `:search` before the first completes safely abandons the
first scan — no zombie tasks.

### 4.3 `Editor::cancel_foreground()`

```rust
pub fn cancel_foreground(&mut self) {
    if let Some(token) = self.active_cancel.take() {
        token.cancel();
    }
    // Also cancel the LSP hover token (currently separate)
    if let Some(token) = self.pending_hover_token.take() {
        token.cancel();
    }
    // Snap to Normal and clear any partial chord
    self.reset_to_normal();
}
```

Called by the `Action::Cancel` dispatch arm. Idempotent — safe to
call when idle (token is `None`).

### 4.4 `Action::Cancel`

New variant in the `Action` enum. Fires from the `<C-g>` binding
(and `<C-c>` in non-Insert modes). The dispatch arm calls
`editor.cancel_foreground()` and emits `RendererSignal::Redraw`.

### 4.5 Binding

`<C-g>` registers at `KeymapLayer::Builtin` — fires in every mode
and every buffer, matching emacs keyboard-quit universality. This
is deliberate: a user who is stuck should not need to know what mode
or buffer they are in.

`<C-c>` is also registered at Builtin for Normal / Op-pending /
Visual modes, matching vim convention. Insert-mode `<C-c>` keeps its
existing semantics (exit Insert without abbreviation trigger) and
does not route to `Action::Cancel`.

### 4.6 Existing `pending_hover_token`

`Editor.pending_hover_token` already exists for LSP hover
cancellation. In CG.3 this gets folded into the unified
`active_cancel` pattern so `<C-g>` subsumes it. Until then,
`cancel_foreground()` explicitly cancels both.

## 5. Async entry-point contract

Every user-initiated long-running spawn follows this pattern:

```rust
// In the host dispatch / mode handler that kicks off the work:
let token = editor.arm_cancel();
tokio::spawn(async move {
    some_service.run(params, token).await;
});
```

The spawned task checks `token.is_cancelled()` at its natural
yield points:

- **Search:** inside the per-chunk loop in `lattice-core::search`
  (already has the check; just needs the live token instead of
  `CancellationToken::never()`).
- **LSP commands:** the `Pending<T>` future in
  `lattice-lsp::features` already selects on the token; it just
  needs the user-facing token plumbed in.
- **WASM:** the fuel-exhaustion trampoline in the plugin host checks
  the token before re-fuelling. A cancelled token causes the plugin
  call to return `Err(CommandError::Cancelled)` at the next fuel
  boundary.

## 6. Mode reset on cancel

`reset_to_normal()` (already exists, used by `<Esc>` path):

- Clears `partial_chord`
- Exits Visual / Op-pending / Command-line / Search mode → Normal
- Does NOT discard unsaved buffer edits (cancel is not `:q!`)
- Does NOT clear the register or yank ring

If the user is in Insert mode and presses `<C-g>`, the mode snaps to
Normal first, then the cancel fires. Two keystrokes is intentional:
Insert mode already has a clear exit path (`<Esc>`) and an accidental
`<C-g>` should not surprise the user.

## 7. Status indication (deferred to CG.5)

v1 ships no in-editor progress spinner. The user knows an operation
is running because the picker / search pane has not populated yet.

CG.5 (stack-based multi-op) is the right time to add a status-line
`[search…]` indicator, because it introduces the concept of named
concurrent ops. Shipping a spinner prematurely for the single-token
model would be misleading (it cannot distinguish "searching" from
"hovering").

## 8. Forward-looking: stack-based multi-op (CG.5)

The single-token model has one limitation: a second user-initiated
op (`<C-g>` twice, or a picker fill while search is running)
cancels everything. For v1 this is acceptable — the user
intentionally pressed cancel.

When Lattice gains concurrent visible operations (search + hover
simultaneously surfaced in different panes), a token stack replaces
`active_cancel`:

```rust
cancel_stack: Vec<(CancellationToken, &'static str)>,
                                        // ^ human label for status line
```

`<C-g>` pops and cancels the top entry. A second `<C-g>` cancels
the next. An empty stack `<C-g>` is a no-op (or shows file info,
matching vim's `<C-g>` convention). The status line can display
`[search… ×]` with a count badge.

This is intentionally not sliced yet. The single-token v1 is the
right foundation; the stack is a drop-in replacement once the
consumer callsites are established.

## 9. Cross-references

- `lattice-protocol/src/cancel.rs` — `CancellationToken` definition.
- `lattice-core/src/search.rs` — per-chunk cancel check in the
  search loop.
- `lattice-lsp/src/features.rs` — `Pending<T>` futures; token plumbing.
- `lattice-lsp/src/actor.rs` — LSP actor token parameter.
- `crates/lattice-host/src/dispatch.rs` — `Editor::cancel_foreground`,
  `Action::Cancel` arm, `pending_hover_token` fold-in (CG.3).
- `crates/lattice-host/src/lib.rs` — `active_cancel` field,
  `arm_cancel()`, `cancel_foreground()`.
- `crates/lattice-multibuffer/src/lib.rs` — search provider spawn
  path (CG.2 hook-in point).
