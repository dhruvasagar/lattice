---
summary: "whitespace-show-mode: Renders tabs, trailing spaces and other whitespace as visible glyphs Same state as `:set list`."
related: [whitespace-show, display, options]
---

# whitespace-show-mode

Renders tabs, trailing spaces and other whitespace as visible glyphs.

| | |
|---|---|
| Toggle the mode | `:whitespace-show-mode` |
| Equivalent option | `:set list` |
| Contributes | ``whitespace`` |

**Not yet rendered.** The mode and the option exist and cascade correctly, but the renderer's whitespace-glyph plumbing has not landed, so toggling this changes state without changing what you see. Declared surface, deferred pipeline — see [`display`](help:display).

## Options

| Option | Type | Default | `:set` surface |
|---|---|---|---|
| `whitespace` | bool | `false` | `:set list`, `:set nolist` |

`:customize display` lists these together with the rest of the display
group; `:describe-option whitespace` shows the resolved value for the
current buffer and where it came from.

## Keybindings

None. Display minors are toggled by name (`:whitespace-show-mode`) or through their
option, not by a chord. Bind one yourself in `init.rs` if you want a
key for it.

## Mode and option are one state

A display minor and its option are two spellings of the same thing.
The mode contributes the option, and the option's value mirrors back
onto whether the mode is active, so `:set` and `:whitespace-show-mode` cannot drift
apart. Ask either way — `:describe-mode whitespace-show-mode` or
`:describe-option` — and you get the same answer.

Scope is per buffer, so one split can show line numbers while another
doesn't.

## See also

- [`display`](help:display) — every display option together.
- [`options`](help:options) — how `:set`, layering, and mode
  contributions resolve.
