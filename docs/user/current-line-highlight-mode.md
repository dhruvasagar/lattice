---
summary: "current-line-highlight-mode: Tints the line the cursor is on Same state as `:set cursorline`."
related: [current-line-highlight, display, options]
---

# current-line-highlight-mode

Tints the line the cursor is on.

| | |
|---|---|
| Toggle the mode | `:current-line-highlight-mode` |
| Equivalent option | `:set cursorline` |
| Contributes | ``cursorline`` |

**Not yet rendered.** Same deferral as [`whitespace-show-mode`](help:whitespace-show-mode): option and mode are declared and cascade, the renderer pipeline lands later.

## Options

| Option | Type | Default | `:set` surface |
|---|---|---|---|
| `cursorline` | bool | `false` | `:set cursorline`, `:set nocursorline` |

`:customize display` lists these together with the rest of the display
group; `:describe-option cursorline` shows the resolved value for the
current buffer and where it came from.

## Keybindings

None. Display minors are toggled by name (`:current-line-highlight-mode`) or through their
option, not by a chord. Bind one yourself in `init.rs` if you want a
key for it.

## Mode and option are one state

A display minor and its option are two spellings of the same thing.
The mode contributes the option, and the option's value mirrors back
onto whether the mode is active, so `:set` and `:current-line-highlight-mode` cannot drift
apart. Ask either way — `:describe-mode current-line-highlight-mode` or
`:describe-option` — and you get the same answer.

Scope is per buffer, so one split can show line numbers while another
doesn't.

## See also

- [`display`](help:display) — every display option together.
- [`options`](help:options) — how `:set`, layering, and mode
  contributions resolve.
