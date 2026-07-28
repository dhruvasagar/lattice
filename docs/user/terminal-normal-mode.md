---
summary: "terminal-normal-mode: vim motions, search and yank over a terminal's scrollback — the mode a terminal buffer sits in when you're not typing at the shell."
related: [terminal, ex:terminal]
---

# terminal-normal-mode

The mode a [terminal buffer](help:terminal-mode) is in when your keys
belong to the **editor** rather than the shell. This is where a
terminal stops being a black box and becomes a buffer: search the
scrollback with `/`, walk it with any motion, yank a stack trace out of
it with `y`.

You're here by default. Press `i` to hand keys to the shell
([`terminal-insert-mode`](help:terminal-insert-mode)); `C-\ C-n` comes
back.

## What it does

It builds a document view over the terminal's scrollback so the
ordinary editing machinery has something to work on. That means the
whole vim grammar applies — motions, text objects, registers, marks,
search — to output that has already scrolled past, not just what's on
screen.

Copying an error message out of a build log needs no mouse and no
terminal-specific selection mode; it's `/error<CR>` then `yy`.

## Behaviour worth knowing

- **Read-only.** You're reading output, not editing it — those come
  from [`terminal-mode`](help:terminal-mode), not from this minor.
- **Degrades gracefully.** If the scrollback view can't be built the
  mode simply doesn't activate rather than erroring; the terminal keeps
  working, you just don't get motions over it.

## See also

- [`terminal-mode`](help:terminal-mode) — the buffer and how to open
  one.
- [`terminal-insert-mode`](help:terminal-insert-mode) — talking to the
  shell.
