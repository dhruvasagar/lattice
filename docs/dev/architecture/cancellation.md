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

### 4.1 `ForegroundCancel` — the shared slot

```rust
// lattice-mode/src/foreground_cancel.rs
pub struct ForegroundCancel { armed: Mutex<Option<CancellationToken>> }
pub type ForegroundCancelHandle = Arc<ForegroundCancel>;

impl ForegroundCancel {
    pub fn arm(&self) -> CancellationToken;  // cancels the predecessor
    pub fn cancel(&self);
    pub fn is_armed(&self) -> bool;
}
```

One token armed at a time. The `Editor` holds the handle
(`Editor::foreground_cancel`) and boot registers **the same `Arc`** in
the `ServiceRegistry` under `ForegroundCancelHandle`.

That identity is load-bearing and its failure is silent: register a
different `ForegroundCancel` than the `Editor` holds and everything
still compiles, providers still arm, `<C-g>` still runs, and nothing is
ever cancelled. Pinned by
`lattice_ui_tui::app::cancel::a_token_armed_through_the_service_is_cancelled_by_the_key`,
which arms the way a provider does and cancels the way the user does.

### 4.2 Arming: `&self`, not `&mut Editor`

CG.1 put `arm_cancel()` on `Editor`, which needs `&mut`. CG.2 moved the
real surface onto the service because **most spawn sites never see
`&mut Editor`**: project search's `gr` refresh is an
`ActionHandlerRegistration` closure holding only `&self` services, and
LSP requests (CG.3) and plugin calls (CG.4) sit behind the same wall.

Enrolling only where `&mut` happens to be available is the
half-migration this project keeps re-discovering — `<C-g>` would cancel
a *fresh* search and silently do nothing to a refreshed one, with
nothing in the code to say so.

`Editor::arm_cancel()` survives as a convenience for the paths that do
hold `&mut`; it delegates to the same slot.

`arm()` cancels the predecessor first, which means **supersede is the
same mechanism as cancel**. A second `:search` abandons the first, and
`gr` refreshing a view no longer needs a private flag — the one project
search used to carry (`Arc<AtomicBool>`) is gone, and with it the
possibility of a scan that supersede could stop but `<C-g>` could not.

### 4.2.1 Two verbs: `arm` vs `enrol` (CG.3)

`arm()` **supersedes** — the slot holds one token, and taking it
cancels the predecessor. Right for project search, where starting a
second scan means abandoning the first.

`enrol()` **joins** — the token is added to a set that `cancel()`
walks, and nothing else is disturbed. Right for work that already has
its own cancellation discipline and only needs `<C-g>` to reach it.

Every user-triggered LSP command enrols. Each already holds a
per-feature `pending_*_token` so a second hover supersedes the first,
which is the correct granularity — a hover has no business cancelling
a rename. Arming would be actively wrong: `K` is a reflexive inspect
key, so it would mean glancing at a symbol silently kills the project
search you are waiting on, leaving a half-populated buffer and nothing
to explain it. Pinned by
`ForegroundCancel::enrolling_does_not_supersede_the_armed_operation`.

**What does NOT enrol, and why it matters.** LSP owns 21
`pending_*_token` fields, and most are automatic:
`lsp_completion_request`, `lsp_signature_help_request`,
`do_lsp_insert_completion_request`, `do_completion_resolve_focused`,
`do_lsp_on_type_formatting_request`, and the entire `maybe_request_*`
family (document-highlight, inlay-hint, semantic-tokens, code-lens,
document-link, document-color, folding-range, pull-diagnostics). These
fire on keystrokes, cursor moves and ticks. By §3 they are not
user-triggered and therefore not foreground; enrolling them would let
`<C-g>` cancel work the editor is doing on its own behalf, and — had
they been *armed* instead — would have made **every keystroke's
completion request cancel a running search**.

The rule of thumb the classification follows: anything named
`maybe_request_*` is automatic. The rest were classified individually;
note `do_lsp_on_type_formatting_request` is automatic despite sharing
`pending_format_token` with the user-triggered `do_lsp_format_request`.

The twelve enrolled sites are hover, the `gd`/`gD`/`gy`/`gI` nav
family, references, document symbols, workspace symbols, rename,
format, code-actions (request + resolve-apply), call hierarchy, type
hierarchy and selection-range.

### 4.3 `Editor::cancel_foreground()`

```rust
pub fn cancel_foreground(&mut self) {
    self.foreground_cancel.cancel();  // armed slot + enrolled set
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

Mode set: `Normal`, `Insert`, `Replace`, and the three readline
surfaces `Command` / `Search` / `Prompt`. The minibuffers each need
their own entry: they resolve keys in their own contexts rather than
borrowing Insert's table (`keymap-architecture.md` §5.7).

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

### 4.8 The `pending_*_token` fields (CG.3 outcome)

The slice plan said `pending_hover_token` would be folded into the
unified slot and removed. It is **kept**, along with its twenty
siblings, and that is the right answer rather than a deferral.

Those fields are per-feature *supersede* tokens: a second hover
cancels the first, a second rename cancels the first. That granularity
is correct and the foreground slot cannot express it — a single slot
would make a hover cancel an in-flight rename. What was actually
missing is only that `<C-g>` had no route to any of them, and
`enrol` (§4.2.1 above) supplies exactly that
without disturbing the per-feature behaviour.

The CG.1 special case (`cancel_foreground` reaching into
`pending_hover_token` directly) is gone: it covered hover alone and
left the other eleven user-triggered commands unreachable.

## 5. Async entry-point contract

Every user-initiated long-running spawn follows this pattern:

```rust
// In whatever spawns the work — an action handler, an event
// subscription, a mode's trigger. `&self` services are enough.
let token = services
    .get::<lattice_mode::ForegroundCancelHandle>()
    .map(|fc| fc.arm())
    .unwrap_or_else(CancellationToken::never);
tokio::spawn(async move {
    some_service.run(params, token).await;
});
```

A missing service degrades to `never()` — an uncancellable operation —
rather than refusing to run. That is the test-harness case (a mode
exercised without boot wiring), and refusing to search because
cancellation is unavailable would be the worse failure.

The spawned task checks `token.is_cancelled()` at its natural
yield points:

- **Search (CG.2, done):** `run_scan`'s per-file loop in
  `lattice-multibuffer::providers::search` checks `is_cancelled()` at
  each `ignore::Walk` iteration. The check predates CG.2; what CG.2
  changed is *which* token it polls — the scan's private
  `Arc<AtomicBool>` became the armed foreground `CancellationToken`,
  stored on `ProjectSearchState` and required by
  `ProjectSearchState::scanning`, so a spawn site cannot register a
  scan `<C-g>` has no way to reach.
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

## 9. Keyed supersede: the same task, triggered again (CG.6+)

**The rule.** Triggering a task that is already running is decided by
what the re-trigger asks for:

- **the same inputs** — the run in flight carries on, and the re-trigger
  says "*push is already running in lattice*". Restarting would only throw
  its progress away; a long build pressed twice should not start over.
- **new inputs** — the run in flight is computing an answer nobody wants
  (a search for the old query, a format of text that has since changed, a
  push re-run with `-f`), so it is cancelled and the new one runs.

*Only* that task, either way: a push never touches a fetch, a search or a
push to another remote. Decided with Dhruva, 2026-10-20, after two
simultaneous pushes both ran and the first was rejected by the remote —
first as "always restart", then narrowed to this when restarting a long
task on a double press turned out to waste exactly the time it was meant
to save. Convention agrees on the identical case: VS Code answers "Task is
already active" and makes restart a choice; emacs `compile` asks before
killing a running compilation.

**Why neither existing verb is it.** `arm()` (§4.2) supersedes *across*
tasks — the slot holds one token, so a search in one view cancels the
search in another. `enrol()` (§4.2.1) supersedes nothing. LSP's
per-feature `pending_*_token`s are the right granularity, written out by
hand 21 times. And most background work cannot be stopped at all: git,
cargo, grep and the external formatter run through `Command::output()`,
which hands back no child to kill.

### 9.1 `TaskKey` and `TaskTicket`

```rust
// lattice-mode/src/foreground_cancel.rs
pub struct TaskKey { kind: &'static str, scope: String, inputs: String, policy: RePolicy }
pub enum RePolicy { Supersede, RunToCompletion }

impl ForegroundCancel {
    pub fn begin(self: &Arc<Self>, key: TaskKey) -> Result<TaskTicket, AlreadyRunning>;
}
impl TaskTicket {
    pub fn token(&self) -> &CancellationToken;
    pub fn is_current(&self) -> bool;   // still this key's newest run
}
```

Two triggers are *the same task* when `kind` (the operation,
`"magit:push"`) and `scope` (what it acts on: the repository and the
destination) are equal. `inputs` fingerprints what this trigger asks for —
the push's arguments, the query, the buffer version — and is what the
rule above compares. The producer that spawns the work owns the key, so
identity and inputs are decided where the work is understood, never by a
host guessing from a label.

With a run of the same task in flight, `begin` returns `AlreadyRunning`
when the inputs are equal, or when the task is `RunToCompletion`; the
producer reports it in words instead of starting a second copy. With new
inputs and a `Supersede` task it cancels the run in flight and returns a
fresh ticket; the old run sees its token flip and stops.

**Restarting identical work** is `<C-g>`, then trigger again. A cancelled
run counts as gone while it unwinds, so the re-trigger is not refused by
the run just stopped. That is also the answer for work whose real inputs
live outside its arguments — `:compile` after editing a file has the same
command line — and it keeps a restart a deliberate act, as the convention
has it.

The ticket carries a **generation**. Delivery checks `is_current()` —
the result of a run that has since been superseded is dropped, not
applied. That one check is what closes the stale-overwrite class: a
magit refresh that started first and finished last, a format result
landing in a buffer that moved on.

A ticket removes its slot entry when dropped, if it is still the
current one, so finished tasks leave nothing behind — `enrol()`'s set
can rely on `<C-g>` to empty it, a map of every task ever run cannot.

### 9.2 Which tasks may be killed

Not every task survives being killed partway. Killing a push, a fetch, a
cargo build, a scan, a search or a formatter is harmless: their effects
are atomic or redone by the run that replaces them. Killing `git
commit`, `rebase`, `stash`, `reset`, `cherry-pick` or the merge half of
`pull` can leave `.git/index.lock` behind, or a half-applied change.

So the policy is part of the key, declared by the producer, and the
unsafe kinds are `RunToCompletion`: a second trigger is **refused** even
with new inputs — not queued (a double-pressed stash would stash twice)
and not superseded. Decided with Dhruva, 2026-10-20, over
supersede-everything and queueing.

The same reason keeps a `RunToCompletion` task out of `<C-g>`'s reach
once it has started: if killing it is unsafe on a re-trigger, it is
unsafe on a key press too. A `Supersede` ticket's token is enrolled, so
`<C-g>` stops it like any other foreground work.

### 9.3 Stopping a process

```rust
// lattice-core/src/process.rs
pub fn run_cancellable(command: Command, input: Option<Vec<u8>>,
    group: ProcessGroup, token: &CancellationToken)
    -> io::Result<RunOutcome>;          // Finished(Output) | Cancelled
```

The child starts as the leader of its own process group and is waited
on with a short poll of the token; output is drained on two reader
threads so a full pipe cannot wedge it. On cancel the **whole group** is
killed — `kill -KILL -- -<pgid>` through `kill(1)` on Unix (the
workspace denies `unsafe`, so not `libc::kill`), `taskkill /T /F` on
Windows — because a shell's grandchildren otherwise outlive it and keep
its pipes open. The plugin host's process job and compilation each
carry a private version of this; they move onto the shared one.

**Unless the child may need the terminal.** `ssh` under `git push` asks
for a key's passphrase on the terminal lattice was started from; from a
process group of its own that read is a background read, which the
terminal answers by suspending it (`SIGTTIN`) — a push that hangs
silently instead of asking. So the group is the caller's choice
(`ProcessGroup::Own` / `Shared`): git's remote operations share the
editor's group and a cancel kills git alone, leaving an `ssh` mid-prompt
to end on its own. Either way a cancel does not wait for the output
readers, so a grandchild still holding a pipe cannot delay it.

A superseded run ends quietly: one line in `*messages*`, no error
notification. The user did not fail at anything — they asked again.

### 9.4 Rejected

- **Always restart** on a re-trigger (the first version of this rule) — a
  long task pressed twice loses all its progress for nothing.
- **Never restart** — a re-trigger with new inputs would wait for an
  answer to the old question, then have to run anyway.
- **Ask on an identical re-trigger** ("restart it? y/n", as VS Code does)
  — a prompt on every double press; `<C-g>` and re-trigger is already a
  deliberate restart.
- **Supersede everything**, repository mutations included — a killed
  stash or rebase can corrupt the very state the user is acting on.
- **Queue** the second trigger — a double press would run a mutation
  twice, and a stuck first run would stall the queue.
- **Keep `arm()` and add keys beside it** — two supersede rules, one of
  which contradicts the other. `arm()`'s users move onto keys (CG.12),
  then it goes.

## 10. Cross-references

- `lattice-protocol/src/cancel.rs` — `CancellationToken` definition.
- `lattice-mode/src/foreground_cancel.rs` — `ForegroundCancel`, and (CG.6) `TaskKey` / `TaskTicket`.
- `lattice-core/src/process.rs` — (CG.6) `run_cancellable`.
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
