---
summary: "help-mode: the minor mode that turns a markdown buffer into a help buffer — read-only, with <CR> following links."
related: [help, describe]
---

# help-mode

The minor that makes a markdown buffer *a help buffer*. It composes
with [`markdown-mode`](help:markdown-mode), which carries the parsing
and rendering; help-mode adds what makes it navigable rather than just
readable:

- **`ReadOnly`** — help is a record, not a scratchpad.
- **Link and anchor metadata** — the `[label](help:topic)` targets and
  the heading anchors they can point at.
- **`<CR>` follows the link under the cursor**, gated on this minor
  being active. That gate is why `<CR>` opens a topic in `:help` and
  inserts a newline everywhere else, without the dispatcher special-
  casing buffer names.

You never toggle it: `:help`, `:describe-*`, `:apropos` and the hover
popup all bring it with the buffer.

## Why it is a minor and not the major

The major is `markdown-mode`, because a help buffer *is* markdown and
should highlight, fold and move exactly like any other markdown. Help
is a role layered on top. Splitting it that way means improvements to
markdown rendering reach help for free, and help-specific behaviour
(link following, dismissal) has somewhere to live that doesn't touch
markdown files you're editing.

## Options

`ReadOnly = true`

## Keybindings

`<CR>` follows the link under the cursor. Dismissal and navigation come from the help buffer's own bindings — see [`help`](help:help).

## See also

- [`help`](help:help) — the help system: `:describe-*`, `:apropos`,
  the `<C-h>` prefix map.
- [`markdown-mode`](help:markdown-mode) — the major underneath.
