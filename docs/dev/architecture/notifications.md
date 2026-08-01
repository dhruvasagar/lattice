# Notifications — telling the user about work with no buffer

> **Status: designed, not built.** The subsystem's shape is already
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

## Open

- **Do notifications need actions in v1?** §5.9.9 specifies
  `NotificationAction` buttons. The magit consumer needs none. Deferring
  them keeps the first build to display + expiry.
- **What replaces the optimistic echo?** If a fetch posts a "started"
  notification, the echo is redundant — or the echo stays for
  immediate feedback and only completion notifies. The latter is
  cheaper and probably right.
- **Does a notification survive a buffer switch?** It must (that is
  the point), which means the layer is window-scoped, not
  buffer-scoped.

## Cross-references

- [`design.md`](design.md) §5.9.9 — the specification this defers to
- [`design.md`](design.md) §5.9.4 — the headerline status convention
- `lattice-compilation`'s `CompilationHeaderline` — the shipped
  in-repo precedent for buffer-scoped progress
