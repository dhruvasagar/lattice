---
summary: "relative-line-numbers-mode: Numbers each line by its distance from the cursor, so `8k` / `3j` can be read straight off the gutter Same state as `:set relativenumber` / `:set rnu`."
related: [relative-line-numbers, display, options]
---

# relative-line-numbers-mode

Numbers each line by its distance from the cursor, so `8k` / `3j` can be read straight off the gutter.

| | |
|---|---|
| Toggle the mode | `:relative-line-numbers-mode` |
| Equivalent option | `:set relativenumber` / `:set rnu` |
| Contributes | ``relativenumber` + `number`` |

Contributes **both** `relativenumber` and `number`, mirroring vim's `rnu` cascade: relative numbering implies the gutter renders at all, and the current line shows its absolute number.

## Options

| Option | Type | Default | `:set` surface |
|---|---|---|---|
| `relativenumber` | bool | `false` | `:set relativenumber`, `:set rnu`, `:set nornu` |

`:customize display` lists these together with the rest of the display
group; `:describe-option relativenumber` shows the resolved value for the
current buffer and where it came from.

## Keybindings

None. Display minors are toggled by name (`:relative-line-numbers-mode`) or through their
option, not by a chord. Bind one yourself in `init.rs` if you want a
key for it.

## Mode and option are one state

A display minor and its option are two spellings of the same thing.
The mode contributes the option, and the option's value mirrors back
onto whether the mode is active, so `:set` and `:relative-line-numbers-mode` cannot drift
apart. Ask either way — `:describe-mode relative-line-numbers-mode` or
`:describe-option` — and you get the same answer.

Scope is per buffer, so one split can show line numbers while another
doesn't.

## See also

- [`display`](help:display) — every display option together.
- [`options`](help:options) — how `:set`, layering, and mode
  contributions resolve.
