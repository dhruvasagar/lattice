---
summary: "line-numbers-mode: Shows absolute line numbers in the gutter Same state as `:set number` / `:set nu`."
related: [line-numbers, display, options]
---

# line-numbers-mode

Shows absolute line numbers in the gutter.

| | |
|---|---|
| Toggle the mode | `:line-numbers-mode` |
| Equivalent option | `:set number` / `:set nu` |
| Contributes | ``number`` |

Toggling the mode and setting the option are the same act — the mode mirrors `number`, so `:set nu` activates it and `:line-numbers-mode` sets the option. Two spellings, one state; they cannot disagree.

## Options

| Option | Type | Default | `:set` surface |
|---|---|---|---|
| `number` | bool | `false` | `:set number`, `:set nu`, `:set nonu` |

`:customize display` lists these together with the rest of the display
group; `:describe-option number` shows the resolved value for the
current buffer and where it came from.

## Keybindings

None. Display minors are toggled by name (`:line-numbers-mode`) or through their
option, not by a chord. Bind one yourself in `init.rs` if you want a
key for it.

## Mode and option are one state

A display minor and its option are two spellings of the same thing.
The mode contributes the option, and the option's value mirrors back
onto whether the mode is active, so `:set` and `:line-numbers-mode` cannot drift
apart. Ask either way — `:describe-mode line-numbers-mode` or
`:describe-option` — and you get the same answer.

Scope is per buffer, so one split can show line numbers while another
doesn't.

## See also

- [`display`](help:display) — every display option together.
- [`options`](help:options) — how `:set`, layering, and mode
  contributions resolve.
