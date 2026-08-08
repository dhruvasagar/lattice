# Foreground cancellation (`<C-g>`)

Design for user-initiated cancellation of long-running or stuck
foreground operations — search scans, LSP commands, picker fills,
WASM plugin calls — via a single deliberate key.

Companion documents: `design.md` §5.7 (async runtime), `mode-architecture.md`
(mode reset path), `lsp-architecture.md` (LSP pending-token pattern).
Slice sequencing lives in
[`docs/dev/operations/slice-plans/cancellation.md`](../operations/slice-plans/cancellation.md).

## 1. The problem

Any user-initiated operation that spawns async work — search,
LSP rename, picker fill, WASM plugin call — can stall. The user
has no escape short of restarting the process. Vim's `<C-c>`,
emacs's `<C-g>`, and every terminal tool's `^C` all exist because
this failure mode is universal.

The substrate already has `CancellationToken` (`lattice_protocol::
CancellationToken`) threaded through search loops, LSP requests,
and the grammar `execute()` call. What is missing is:

1. A way for the user to signal cancellation at a memorable key,
   regardless of which operation is in flight.
2. A seam in the Editor that lets every async entry-point enroll in
   that signal.
3. A mode-reset that returns the editor to a stable Normal state so
   the user can keep working.

## 2. Paramount-goal alignment

**Goal #1 (performance):** The cancel signal is a single atomic flag
flip (`CancellationToken::cancel()`). The UI thread pays zero polling
cost; async tasks wake on drop/cancellation via tokio's select macro.
No renderer work on the hot path.

**Goal #3 (vim semantics):** the binding is `<C-g>` — emacs
`keyboard-quit` — because the two vim candidates are both unavailable.
`<C-c>` is a mode *prefix* here (§4.5.1, structural, not a preference),
and `<Esc>` is pressed reflexively by vim users, which makes it the
wrong home for a destructive-ish action (§4.5.2). `<C-g>` is free in
every mode cancel needs and taken only by SN.3d's Visual↔Select toggle,
which §4.5 leaves alone.

**Goal #4 (asynchronicity):** Cancellation is non-blocking by
construction. The token is a cheap clone-able handle; the async task
observes it at its next `yield_now` / select-sleep / fuel-check point
and exits cleanly. The UI thread never waits for confirmation.

## 3. Scope: foreground vs. background

Cancellation covers **foreground operations** — work the user explicitly
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
// in lattice-host/src/editor.rs  (Editor struct)
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
    // Hover still owns a separate token until CG.3 folds it in.
    if let Some(token) = self.pending_hover_token.take() {
        token.cancel();
    }
    self.reset_to_normal();
}
```

Flip **and** reset — emacs `keyboard-quit` is defined as doing both.
Idempotent and safe when idle (token is `None`), which is what makes
the binding harmless to press speculatively.

### 4.4 `Action::Cancel`

One variant in the `Action` enum → `editor.cancel_foreground()`.
Reached via `action:cancel`, bound to `<C-g>` at `Builtin` (§4.5), and
carrying an `AppEffect::Cancel` peer so it is a real registered command
rather than an input-layer special case.

Deliberately NOT in `action_is_document_mutation`: cancel is an escape
hatch, so it must keep working on a read-only buffer. The redraw comes
from the teardown publishing render state.

### 4.5 Binding

**`<C-g>` at `KeymapLayer::Builtin`**, registered in one loop in
`lattice-host/src/keymap_cancel.rs`. Builtin rather than
`emacs-keys-mode` so it does not depend on `:set emacs-keys` — a user
who turns the tribute off must not lose the only way to stop a scan.

Mode set: `Normal`, `Insert`, `Replace` — and therefore
`ModalState::Command` / `Search(_)` / `Prompt`, which dispatch through
`keymap_insert::dispatch_insert` and so look up `BindingMode::Insert`.

**Not `Visual` or `Select`.** SN.3d owns `<C-g>` there as the
Visual↔Select toggle: vim-canonical, and the only path between the two
modes that preserves the selection (`select-mode.md` §4). It matters
more than the chord count suggests, because snippet placeholders land
the user in Select, so `<C-g>` is how a placeholder selection gets
promoted to Visual for operators.

The cost is that Visual and Select have no cancel chord — from there it
is `<Esc>` then `<C-g>`. Accepted deliberately: Visual is a transient
state a user is rarely parked in while waiting on a scan, and the
alternative was relocating a vim-canonical chord to buy a case that
barely arises.

### 4.5.1 Why not `<C-c>`

`<C-c>` is vim's interrupt and was the obvious candidate. It cannot be
used, and the reason is structural rather than a matter of taste.

**`<C-c>` is a mode prefix in this codebase.** magit binds `<C-c>g`
(dispatch) and `<C-c>f` (file-dispatch) globally in Normal, and
`<C-c><C-c>` / `<C-c><C-k>` (confirm / abort) in the commit, rebase and
notes modes, in Normal *and* Insert. That is the emacs convention
`<C-c>` carries, and it is shipped behaviour.

`KeymapTrie::lookup` returns `Bound` at a terminal node **regardless of
its children** (`trie.rs:245`). So a depth-1 `<C-c>` binding resolves
immediately and every `<C-c>…` chord underneath it becomes unreachable.
Registering cancel there broke magit's transients outright.

Layer priority does not save it: the collision is not two bindings at
the same path, it is a terminal node preempting its own subtree, which
happens before priority is consulted.

This is what CM.3d meant by "`<C-c>` belongs to modes" when it removed
the hardcoded `<C-c>` → quit hatch — a point worth restating, because
the surface reading ("quit was too destructive") is only half of it.

A mode owning `<C-c>` terminally is still fine, and several do
(`compilation-mode` → kill the build, the minibuffer modes → cancel that
line). Those layers are scoped by K.1.c to buffers where the mode is
active, so they shadow nothing elsewhere. `Builtin` has no such scope.
The regression net is
`lattice_ui_tui::app::cancel::builtin_never_binds_ctrl_c_terminally`.

Making `<C-c>` work would mean teaching the trie vim's `timeoutlen`
prefix-vs-terminal disambiguation. That is a genuinely better keymap
engine and would unblock every future collision of this shape, but it
touches the hot keystroke path, needs its own slice and bench, and would
still cost a second keystroke wherever a `<C-c>` prefix is active.

### 4.5.2 Why not `<Esc>`

An earlier revision of this slice chained the cancel onto every bare
`<Esc>` in `input::translate`. Esc is universal, is never a prefix
(zero multi-key `<Esc>…` chords in tree), and `cancel.rs`'s own
"Sources of cancellation" note already named it. It was still wrong.

**Vim users press `<Esc>` reflexively and constantly** — to confirm
they are in Normal, between edits, out of habit. Tying cancellation to
it means a thirty-second project search dies to a double-tap that
carried no intent to cancel, and the user cannot tell the difference
between "it finished" and "I killed it." Cancellation is not
destructive to the *document*, but it is destructive to work in
progress, and a key pressed that often is the wrong place for it.

The general rule this leaves behind: **a key the user presses without
thinking must not do anything they would regret.** Pinned by
`lattice_ui_tui::app::cancel::esc_does_not_cancel`.

The free-chord census that led to `<C-g>`: of the CTRL chords unbound
in both Normal and Insert (`a c g j k m x z`), `c` and `x` are prefixes
(magit, the emacs-keys leader), `j` / `m` are the literal LF / CR that
terminals send for Enter, `z` is the suspend convention, `a` is vim's
increment and `k` is Insert's kill-to-end-of-line. `<C-g>` is what
remains — and it is already emacs's cancel key, so it arrives with
existing muscle memory rather than needing new.

### 4.6 Mode reset: `Editor::reset_to_normal()`

Authored by CG.1 — an earlier revision of this document assumed it
already existed (it did not; only `set_modal` and the per-mode exit
handlers did).

Each modal state exits through **its own** teardown rather than a bare
`set_modal(Normal)`, because the minibuffer states own real buffers:
dropping `ModalState::Command` without `do_command_line_dismiss()`
leaves `*command-line*` focused with no way to reach it. Insert and
Replace route through `enter_mode(Normal)` so the insert undo-group
closes and the cursor pulls back one byte (vim's insert-exit contract)
— which is also why an already-Normal editor must *not* call it, or a
bare `<C-g>` in Normal would walk the cursor left on every press.

It then clears `partial_chord`, `pending_count`, `op_count` and
`pending_register`. Per §6 it does NOT discard unsaved edits and does
NOT clear the register or the yank ring.

### 4.7 Known gap: cancel mid-chord

`<C-g>` after an operator (`d` then `<C-g>`) resolves as an *unbound
continuation*: the trie aborts the pending operator and stops, without
also reaching `Action::Cancel`. That is vim's rule for an invalid
continuation, and it leaves the user in Normal where a second press does
cancel.

Closing it would require `input::translate` to know **which**
`CommandId` is cancel — the trie resolves a binding to
`Action::Invoke(inv)`, and the `Action::Cancel` variant only
materialises after the grammar runs the `ActionSpec` — which means
threading that id through every `TranslateContext` construction site.
Not worth it for a two-press papercut. Pinned by
`lattice_ui_tui::app::cancel::ctrl_g_in_operator_pending_aborts_the_operator_first`;
revisit if CG.2 / CG.3 show it biting in practice.

### 4.8 Existing `pending_hover_token`

`Editor.pending_hover_token` already exists for LSP hover
cancellation. In CG.3 this gets folded into the unified
`active_cancel` pattern so cancel subsumes it. Until then,
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

Contract (implementation shape in §4.6):

- Clears `partial_chord`, `pending_count`, `op_count`, `pending_register`
- Exits Visual / Select / Insert / Replace / Command / Search / Prompt
  → Normal, each through its own teardown
- Does NOT discard unsaved buffer edits (cancel is not `:q!`)
- Does NOT clear the register or yank ring

One press does both halves — the token flip and the mode reset —
including from Insert. An earlier revision made Insert a deliberate
two-keystroke case. One press does both now: `<C-g>` in Insert cancels
and lands you in Normal, which is what emacs users already expect from
`keyboard-quit` and costs vim users nothing, since `<C-g>` had no
Insert-mode meaning to displace.

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
the next. An empty stack press stays a plain mode reset. The status
line can display
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
- `crates/lattice-host/src/editor.rs` — the `active_cancel` field.
- `crates/lattice-host/src/dispatch.rs` — `arm_cancel()`,
  `cancel_foreground()`, `reset_to_normal()`,
  `do_command_line_dismiss()`, the `Action::Cancel` dispatch arm, and
  the `pending_hover_token` fold-in (CG.3).
- `crates/lattice-host/src/keymap_cancel.rs` — the `<C-g>` Builtin
  registration and its mode set.
- `crates/lattice-plugin-host/src/boundary_app_effect.rs` — the typed
  "no WIT surface" error for `AppEffect::Cancel`.
- `crates/lattice-ui-tui/src/app/cancel.rs` — key-driven coverage
  (press → translate → dispatch → handler).
- `crates/lattice-multibuffer/src/lib.rs` — search provider spawn
  path (CG.2 hook-in point).
