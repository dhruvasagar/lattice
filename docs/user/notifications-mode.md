---
summary: "notifications-mode: the *notifications* buffer — every live and queued notification, where <CR> runs a notification's action and d dismisses it."
related: [notifications, messages-mode, magit]
---

# notifications-mode

Every notification, live and queued. `:notifications`.

```
*notifications*
  ✗ push failed: rejected (non-fast-forward)
  ✓ fetch finished
  ✓ pull finished (queued)
```

The corner popup is a **signal** — it tells you something happened.
This buffer is where you find the ones the corner counted as `+N more`,
dismiss the ones you are done with, and act on any that offer an
action.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Run the action for the notification at cursor |
| `d` | Dismiss the notification at cursor |
| `gr` | Refresh the list |

`<CR>` works from a notification's own row **or** from its action row —
they belong to the same notification, so you never have to aim.

A notification with no action simply does nothing on `<CR>`. Most have
none, and a key that complained in the common case would train you to
stop pressing it.

## Why actions live here and not in the corner

§5.9.9 describes notifications with buttons. There are none, and that
is deliberate: a corner popup you have to *aim at* is worse than one
you merely read, and making it focusable would mean a bespoke widget
plus a way to move focus into and out of it.

Instead the notification is a plain signal and this is a plain buffer,
so acting on one uses the chords you already know. It costs no new
global key and it doubles as the queue view.

## What posts an action today

**Nothing.** The mechanism exists; no notification currently carries an
action.

The first one tried was "show output in `*messages*`" on a failed git
operation — and it was removed, because the output is in `*messages*`
*unconditionally* already. The row bought a keystroke over `:messages`
and nothing else, and by that standard every notification would carry
"go look at the thing".

An action earns its place when it does something you could not
otherwise do from where you are. Navigating to a buffer that already
has the information is not that.

## See also

- [`notifications`](help:notifications) — the corner popups themselves,
  and their options.
- [`messages-mode`](help:messages-mode) — the durable record every
  notification tees to.
