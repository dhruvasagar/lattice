---
summary: "dashboard-mode: the *dashboard* launch buffer — the splash page lattice opens with, every row a followable link into help, commands, or the tutor."
related: [dashboard, ex:dashboard]
---

# dashboard-mode

The launch buffer. When lattice starts with no file to open, this is
what you land on: a splash page whose rows are all live links rather
than decoration.

Reopen it any time with `:dashboard`.

## What's on it

| Section | Contents |
|---|---|
| About | Version and build identity |
| Links | Project and documentation pointers |
| Tutor | Start the [interactive lessons](help:tutor-mode) |
| Help and bindings | Entry points into the help system |
| Describe | The `:describe-*` introspection commands |
| Commands | Frequently-used commands, each opening its own help |
| Help topics | `:help getting-started`, `modes`, `ex-commands`, `options` |

## Following a row

`<CR>` on any row follows its link — the same follow machinery help
buffers use, so the behaviour is identical to what you already know
from `:help`. Rows point at three different things and the distinction
is visible in what happens:

- **Describe rows** open the command's *documentation*.
- **Command rows** *run* the command.
- **Help-topic rows** open that topic.

`<Esc>` dismisses.

## Behaviour worth knowing

- **Read-only, no file.** `:w` won't try to save it, and typing won't
  edit it.
- **No line numbers, no sign column, no current-line highlight.** It's
  a splash page, not an editing surface, so the mode contributes an
  option set that strips the editing chrome.
- **Soft-wrap is on**, so the layout survives a narrow window.

## See also

- [`getting-started`](help:getting-started) — the same ground in prose.
- [`tutor-mode`](help:tutor-mode) — the hands-on version.
