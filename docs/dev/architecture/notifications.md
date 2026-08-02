# Notifications — telling the user about work with no buffer

> **Status: built.** NOTIF.1a (data layer), 1b/c (both renderers) and
> 1d (magit's remote ops as first consumer) landed 2026-08-02; see the
> slice plan. Actions, config and the `*messages*` tee remain.
> The original note is kept below because the *reasoning* is what this
> fragment is for.
>
> **Status when written: designed, not built.** The subsystem's shape is already
> specified in [`design.md`](design.md) §5.9.9 (`Notification`,
> `NotificationLevel`, `NotificationAction`, corner anchoring,
> stacking, timeouts) and its config in §12. **This fragment does not
> re-specify it.** It records *why the gate opened*, what notifications
> are for versus the two surfaces that already exist, and what the
> first consumer is — so the decision does not have to be re-derived.

## The gate, and the evidence that opened it

§5.9.9 has carried an explicit condition since it was written:

> revisit only if the echo area proves insufficient.

It has. `C-c g f` (fetch) is the case that shows it:

- Fired from **any** buffer — you may be editing a file with no
  relation to the repository view.
- `run_remote_op` spawns the git call and returns immediately with an
  optimistic echo ("magit: fetching…").
- On completion, **nothing**. Success is invisible; failure reaches
  `*messages*` via `tracing::error!` and nowhere else.

So the user cannot tell whether a fetch is running, finished, or
failed, without going to look. The echo area cannot fix this: it is a
single transient line written at *fire* time, with no completion event
and no persistence.

Push and pull are the same shape. So are future long operations
(clone, a large `git log`, LSP server restarts, plugin load failures).

## Why the existing surfaces do not cover it

Three surfaces, three different questions. The distinction is what
keeps them from competing.

| Surface | Answers | Scope |
|---|---|---|
| **Headerline** | "what is the buffer I am looking at doing?" | a buffer |
| **Notification** | "the thing I started has finished — wherever I am now" | no buffer |
| **`*messages*`** | "what happened earlier?" | durable history |

**Headerline is buffer-scoped, and that is correct.** It is the right
home for magit-status's refresh, `*compilation*`'s build, a
multibuffer's scan — each belongs to a buffer you are looking at while
it happens. `lattice-compilation` already ships the convention
(`⟳ "cargo build" …` → `✔ … ok` / `✗ … 3e 2w`).

It is the *wrong* home for a fetch, for a reason that is structural
rather than stylistic: **the operation has no buffer.** Putting its
status in whatever buffer happens to be active would attach git's
state to an unrelated file. Putting it in a `*magit:process*` buffer
would be correct but invisible — you are not looking at that buffer,
which is the whole problem.

**`*messages*` is a record, not a signal.** It should receive
everything (§B.9 already plans notification-to-`*messages*` teeing),
but reading it requires already knowing to look.

### Relationship to the "async status in the headerline" rule

`CLAUDE.md` carries:

> **Async-buffer status in headerline.** Multibuffer providers +
> future async-buffer mechanisms surface progress + completion via the
> buffer's headerline, NOT status lines or notification badges.

**This does not conflict, and the qualifier is the reason.** The rule
governs *async-**buffer*** mechanisms — a buffer that fills
asynchronously. Its cases (multibuffer scans, provider results) all
have an owning buffer, and for those the rule stands unchanged: they
go to the headerline, not to a toast.

A fetch is not an async buffer. It is a repo operation with no buffer
at all, which is the case the rule does not address. `design.md`
states both §5.9.4's headerline convention and §5.9.9's notifications
as parts of one design; they divide by *scope*, not by preference.

The rule is worth tightening when this is built, from "async-buffer
status" to "**buffer-scoped** async status", so the boundary is on the
page rather than in this fragment.

## First consumer

magit's remote operations — `run_remote_op`'s family: fetch, pull,
push, and the one-shot git invocations that ride the same shape
(stash-push, tag, merge, init). They already have the three states a
notification needs, and currently express only the first:

| State | Today | With notifications |
|---|---|---|
| started | optimistic echo | notification, Info |
| succeeded | *nothing* | notification, Info, auto-timeout |
| failed | `tracing::error!` → `*messages*` | notification, Error, longer timeout, output in `*messages*` |

That is the smallest change that closes the reported gap, and it
exercises level, timeout and teeing without needing actions or
stacking-under-load.

## Scope, stated honestly

This is a subsystem, not a slice:

- Both renderers — corner anchoring, stacking, and the first
  **timer-driven UI in the editor**. Nothing in lattice animates or
  expires on a clock today.
- Expiry needs a wake that is not a keystroke; the inbound primitive
  is the shape (`SubsystemBoot::inbound`), not a bare tick callback —
  see `CLAUDE.md`'s note on async results reaching the screen.
- Config (`notifications = { corner, max-visible, default-timeout }`),
  theme roles per level, and the `*messages*` tee.
- Paramount #1: a notification must not repaint the document. Expiry
  should redraw the notification layer only; if that is not separable
  today, that constraint decides the first slice.

## Open — resolved by NOTIF.1a

- **Do notifications need actions in v1?** ⛔ **No.** Deferred as this
  section proposed; the magit consumer needs none, and display +
  expiry is enough to close the reported gap. `NotificationAction`
  lands with a consumer that wants it.
- **What replaces the optimistic echo?** ✅ **Nothing — the echo
  stays.** It is the immediate feedback at *fire* time and costs
  nothing; the notification carries *completion*, which is the part
  that was missing. `NotificationStore::replace_or_post` is shaped for
  the variant where a caller does want one row for the whole
  operation: post "fetching…" with no timeout, replace it with the
  outcome.
- **Does a notification survive a buffer switch?** ✅ **Yes** — the
  store is window-scoped, holding no `BufferId` at all. A notification
  exists precisely because you have moved on from whatever started the
  work.

## Decided while building NOTIF.1a

- **Levels are `EchoLevel`'s three, not a new vocabulary.** Two
  scales for "how bad is this" would drift, and a consumer mapping
  between them is a bug waiting to happen.
- **Errors linger longer than info** (15s / 8s / 4s). An error you
  blink past is an error you will hit again, which is the whole reason
  the gate opened.
- **Expiry goes through `SubsystemBoot::inbound`, not a tick
  callback.** This is the textbook case of the bug `CLAUDE.md` names:
  a bare tick callback would remove the notification and then wait for
  the user to press a key, so a popup would linger past its timeout
  and vanish the instant you typed — which reads as a rendering bug
  rather than a missing wake. `InboundBus::send` bakes the wake in.
- **A `replace` re-arms the timer.** "fetching…" (no timeout) replaced
  by "fetched" (timeout) has to start counting down, or the completion
  stays up forever — the same invisibility, inverted.
- **The visible ones are the OLDEST, and a queued notification's clock
  does not start until it becomes visible.** §5.9.9 says "maximum
  visible count (default 3); excess queued" without saying which end
  queues. Showing the *newest* three — which NOTIF.1a did on its first
  pass — lets an early notification in a burst run out its timeout
  while invisible and be dismissed having never been seen. That is the
  bug this whole subsystem exists to remove, reached from the other
  end. Pinned by
  `a_queued_notification_does_not_expire_before_it_is_seen`.
- **A completion whose start already expired posts fresh.** A long
  fetch can outlive its own "started" notification, and dropping the
  completion there would restore exactly the invisible-success bug.

## Cross-references

- [`design.md`](design.md) §5.9.9 — the specification this defers to
- [`design.md`](design.md) §5.9.4 — the headerline status convention
- `lattice-compilation`'s `CompilationHeaderline` — the shipped
  in-repo precedent for buffer-scoped progress

## How it reaches the screen (NOTIF.1b/c)

Notifications are published into `RenderState` like every other
per-frame surface, **not** read back through the editor. That is
structural rather than stylistic: in production the renderer holds an
`EditorActorHandle`, so reaching the editor is a blocking RPC, and a
per-frame round-trip asking "any notifications?" would sit on the paint
path — exactly what paramount goal #1 forbids. Both peers read the same
`NotificationsRenderState`, so they cannot disagree about what is up.

Both attach the stack **last**, bottom-right, over every other overlay.
A notification a picker or transient could cover would be invisible
precisely when the user is busy, which is when it matters most.

Queued notifications are named (`+N more`) rather than dropped — a
burst that silently discarded its tail would be the invisible-work bug
again, one level up.

### The "must not repaint the document" constraint, honestly

This fragment asked that expiry redraw the notification layer only. **It
does not, and cannot today**, in either peer: the TUI is immediate-mode
(ratatui rebuilds the frame) and GPUI re-renders the element tree.

What makes that acceptable is where the cost actually lands. Ratatui
diffs its double buffer, so only the *changed cells* are written to the
terminal — a notification appearing or expiring does not rewrite the
document's cells. And the rebuild happens on notification *events*
(post, replace, expire), which are a handful per operation, not per
frame. An idle notification costs one version comparison.

Recorded rather than quietly dropped, because the constraint was
written down and a reader deserves to know it was weighed. A genuinely
separable layer would be a renderer-architecture change well beyond
this subsystem.
