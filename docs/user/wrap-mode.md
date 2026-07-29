---
summary: "wrap-mode: Wraps long lines to the window width instead of scrolling horizontally Same state as `:set wrap`."
related: [wrap, display, options]
---

# wrap-mode

Wraps long lines to the window width instead of scrolling horizontally.

| | |
|---|---|
| Toggle the mode | `:wrap-mode` |
| Equivalent option | `:set wrap` |
| Contributes | ``wrap`` |

Soft wrap only — it changes how a line is displayed, never the buffer's bytes. No newline is inserted, so a wrapped line is still one line to every motion, operator and `:s`.

## Options

| Option | Type | Default | `:set` surface |
|---|---|---|---|
| `wrap` | bool | `false` | `:set wrap`, `:set nowrap` |

`:customize display` lists these together with the rest of the display
group; `:describe-option wrap` shows the resolved value for the
current buffer and where it came from.

## Keybindings

None. Display minors are toggled by name (`:wrap-mode`) or through their
option, not by a chord. Bind one yourself in `init.rs` if you want a
key for it.

## Mode and option are one state

A display minor and its option are two spellings of the same thing.
The mode contributes the option, and the option's value mirrors back
onto whether the mode is active, so `:set` and `:wrap-mode` cannot drift
apart. Ask either way — `:describe-mode wrap-mode` or
`:describe-option` — and you get the same answer.

Scope is per buffer, so one split can show line numbers while another
doesn't.

## See also

- [`display`](help:display) — every display option together.
- [`options`](help:options) — how `:set`, layering, and mode
  contributions resolve.
