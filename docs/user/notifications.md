---
summary: "Notifications: corner popups telling you that work with no buffer finished — a fetch, a push — with options for how many show and how long they stay."
related: [notifications-mode, messages-mode, magit, ex:notifications]
---

# Notifications

Work you start from anywhere and then walk away from — a fetch, a push,
a pull — tells you when it finishes, in the bottom-right corner,
wherever you happen to be by then.

```
┌──────────────────────────┐
│ fetch finished           │
│ push failed: rejected    │
│ +2 more                  │
└──────────────────────────┘
```

They stack, the newest below, and disappear on their own. Errors stay
up four times as long as successes, because an error you blink past is
an error you will hit again.

## Why this exists next to the echo area

Three surfaces, three different questions:

| Surface | Answers |
|---|---|
| The **headerline** | "what is the buffer I'm looking at doing?" |
| A **notification** | "the thing I started has finished — wherever I am now" |
| [`*messages*`](help:messages-mode) | "what happened earlier?" |

The echo area could not do the middle one. It writes a single line at
the moment you *fire* something — so a fetch said "fetching…" and then
never said anything again. Success was invisible and failure only
reached `*messages*`, which you had to already know to check.

## Acting on one

Press nothing at the popup — it isn't focusable, on purpose. Open
[`:notifications`](help:notifications-mode) and act there, with ordinary
chords. That buffer also lists the ones the corner counted as `+N more`.

## Nothing is ever lost

Every notification is also written to
[`*messages*`](help:messages-mode) at its own level. One you missed —
or one that never showed because you set `max-visible = 0` — is still
there afterwards.

## Options

| Option | Default | What it does |
|---|---|---|
| `notifications.max-visible` | `3` | How many show at once. The rest queue and the stack shows `+N more`. |
| `notifications.timeout` | `4` | Seconds an **info** notification stays. Warnings last twice that, errors four times. |
| `notifications.corner` | `bottom-right` | Which corner they anchor to. |

```
:set notifications.timeout=8
:set notifications.max-visible=0
```

`max-visible = 0` silences the corner **without losing anything** — the
record in `*messages*` continues and `:notifications` still lists
everything. That is the setting to use if you find them distracting;
setting the timeout to `0` is refused, because a notification that
expires before it can be read is the same as not having one.

The stack never covers the modeline.

## See also

- [`notifications-mode`](help:notifications-mode) — the
  `*notifications*` buffer, and acting on a notification.
- [`magit`](help:magit) — the first thing that posts them.
