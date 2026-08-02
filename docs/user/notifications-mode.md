---
summary: "notifications-mode: the *notifications* buffer — every live and queued notification, where <CR> runs a notification's action and d dismisses it."
related: [notifications, messages-mode, magit]
---

# notifications-mode

Every notification, live and queued. `:notifications`.

```
*notifications*
  ✗ push failed: rejected (non-fast-forward)
      <CR>  show output in *messages*
  ✓ fetch finished
  ✓ pull finished (queued)
```

The corner popup is a **signal** — it tells you something happened. This
buffer is where you **act** on it, and where you find the ones the
corner said were queued.

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

A failed git remote operation. The notification is one line and git's
stderr is not, so it says *what* broke and the action goes to where the
rest is — [`*messages*`](help:messages-mode), which receives every
notification anyway.

## See also

- [`notifications`](help:notifications) — the corner popups themselves,
  and their options.
- [`messages-mode`](help:messages-mode) — the durable record every
  notification tees to.
