---
summary: "read-only-mode: Refuses edits to this buffer"
related: [read-only, display, options]
---

# read-only-mode

Refuses edits to this buffer.

| | |
|---|---|
| Toggle the mode | `:read-only-mode` |
| Equivalent option | — |
| Contributes | ``ReadOnly`` |

This is the *user gesture* for read-only on an arbitrary buffer. Help, file-tree and the LSP logs are already read-only because their major modes contribute the same option by buffer kind; this minor is how you make a buffer read-only that wouldn't be otherwise.

`ReadOnly` has no `:set` surface — it is mode-only — so `:read-only-mode` is the only way to type it.

## Options

| Option | Type | Default | `:set` surface |
|---|---|---|---|
| `ReadOnly` | bool | `false` | — (mode-only; no `:set` surface) |

`:customize display` lists these together with the rest of the display
group; `:describe-option ReadOnly` shows the resolved value for the
current buffer and where it came from.

## Keybindings

None. Display minors are toggled by name (`:read-only-mode`) or through their
option, not by a chord. Bind one yourself in `init.rs` if you want a
key for it.

## Mode and option are one state

A display minor and its option are two spellings of the same thing.
The mode contributes the option, and the option's value mirrors back
onto whether the mode is active, so `:set` and `:read-only-mode` cannot drift
apart. Ask either way — `:describe-mode read-only-mode` or
`:describe-option` — and you get the same answer.

Scope is per buffer, so one split can show line numbers while another
doesn't.

## See also

- [`display`](help:display) — every display option together.
- [`options`](help:options) — how `:set`, layering, and mode
  contributions resolve.
