---
summary: "terminal-insert-mode: keystrokes go to the shell, not the editor. `i` enters it in a terminal buffer, `C-\\ C-n` leaves."
related: [terminal, ex:terminal]
---

# terminal-insert-mode

The mode where your keystrokes reach the **shell** instead of the
editor. In a [terminal buffer](help:terminal-mode), press `i` to enter
it and `C-\ C-n` to leave.

`a`, `I` and `A` also enter it. `<Esc>` can too, if you set
`terminal.esc_exits` — off by default, because `<Esc>` is a key the
program inside the terminal usually wants for itself (vim inside your
terminal inside lattice being the obvious case).

## Why it's a minor mode and not Insert

This looks like Insert but isn't: lattice's modal state stays `Normal`
underneath, and this rides as a per-buffer minor mode instead.

The difference shows the moment you have two panes. Modal state is
global, so if "typing into the shell" were a modal state, switching
away to a code pane and back would need an implicit Esc handshake to
keep the two in sync — and every such handshake is a chance to strand
you in the wrong mode. As a per-buffer minor, the mode simply travels
with the buffer: leave the pane and the destination buffer's own modes
apply; come back and you're still talking to the shell.

## What changes

Only one thing: which translate layer gets your keys. In this mode they
are encoded and written to the PTY. It contributes no options of its
own — read-only and no-file already come from
[`terminal-mode`](help:terminal-mode) underneath.

## See also

- [`terminal-mode`](help:terminal-mode) — the buffer itself.
- [`terminal-normal-mode`](help:terminal-normal-mode) — the other half:
  vim motions over the scrollback.
