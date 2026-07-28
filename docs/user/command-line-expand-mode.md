---
summary: "command-line-expand-mode: C-x C-e grows the one-row : line into a multi-line band for editing a long command comfortably."
related: [command-line, ex-commands]
---

# command-line-expand-mode

Press `<C-x><C-e>` in the `:` line and the single row grows into a
multi-line band. This mode takes over as the buffer's major for the
duration.

It's for the commands that don't fit: a long `:g/pattern/normal ...`,
a substitution with a dense regex, anything you want to *see* while you
edit it rather than scroll horizontally through.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Submit the command line |
| `<Esc>` or `<C-c>` | Cancel |
| `<C-p>` / `<C-n>` | Previous / next history entry |
| `<Up>` / `<Down>` | Previous / next history entry |

## Its own option stack

While expanded, the band gets its own options — no line numbers, no
sign column — rather than inheriting the document's. That's why
swapping the major mode is the mechanism: the expanded band is
visually a different surface from both the one-row `:` line and the
document behind it, and giving it its own mode is what keeps those
three from leaking settings into each other.

Everything else about the `:` line is unchanged: same history, same
completion, same parsing. Expanding changes how much you can see, not
what you can do.

## See also

- [`command-line-mode`](help:command-line-mode) — the one-row `:` line.
- [`ex-commands`](help:ex-commands) — what you can type in it.
