# Terminal Mode

Lattice embeds a real shell inside the editor as a buffer.
Open one with `:terminal`, type commands, scroll the output
with vim motions, copy results into a register — all without
leaving Lattice.

> **Status:** Design complete; implementation in progress. See
> the developer-facing
> [`docs/dev/architecture/terminal-mode.md`](../dev/architecture/terminal-mode.md)
> and
> [`docs/dev/operations/slice-plans/terminal-mode.md`](../dev/operations/slice-plans/terminal-mode.md).
> This user-facing doc describes the intended user experience
> once T1–T4 ship.

---

## What you get

A terminal in Lattice is a normal buffer. Everything that
works on a buffer works on a terminal:

- It shows up in `:ls` / `:b` / `:bnext` / `:bprev`.
- You can have many open in different splits, tabs, or panes.
- `gt` / `gT` switch tabs; a terminal can be in its own tab
  via `<C-w>T` or `:tabterminal`.
- The picker's `<C-s>` / `<C-v>` / `<C-t>` open a buffer in a
  split / vsplit / tab — including terminal buffers.

The shell runs as a real child process under a pseudo-terminal
(PTY). Your `.bashrc` / `.zshrc` / `.config/fish/config.fish`
is read on startup as usual — Lattice doesn't touch shell
config. Programs that work in your normal terminal (vim, ssh,
htop, tmux, ranger, cargo, npm, …) work in Lattice's terminal.

---

## Opening a terminal

| Command | Behavior |
|---|---|
| `:terminal` | Open a shell in the active pane |
| `:term` | Same (short form) |
| `:terminal {cmd}` | Run `{cmd}` instead of `$SHELL` (e.g. `:terminal cargo test`) |
| `:tnew` | Open a terminal (vim alias) |
| `:tabterminal` | Open a terminal in a new tab |
| `<C-w>T` | Move the current terminal to a new tab (vim convention) |

By default `:terminal` lands in the active pane. Change with
`:set terminal.display=split` (or `vsplit` / `tab`).

The shell command defaults to:

1. Your `$SHELL` environment variable.
2. `/bin/sh` (Unix) or `cmd.exe` (Windows) if `$SHELL` isn't set.

Override with `:set terminal.shell=/usr/bin/zsh`.

The working directory defaults to the **parent directory of the
active document's file path**. If the active buffer has no
file, falls back to Lattice's working directory. Configurable
via `:set terminal.cwd=cwd` (always use process cwd) or `:set
terminal.cwd=document` (default — document's path's parent).

---

## Three sub-states inside a terminal

A terminal buffer has three sub-states. The mode-line shows
which one you're in.

### Normal-in-terminal — `-- TERMINAL --`

The **default when you focus a terminal pane**. Vim's grammar
runs against the **scrollback** (the historical output), not
against the shell:

- `j` / `k` — scroll one row toward older / newer output
- `<C-d>` / `<C-u>` — half-page down / up; `<C-f>` / `<C-b>`
  page; `<C-e>` / `<C-y>` line-step (matches document
  semantics)
- `gg` / `G` — jump to top of scrollback / back to the live edge
- `/pattern<CR>` / `?pattern<CR>` — search forward / backward
  across the full grid (visible + history); viewport jumps to
  the first match
- `n` / `N` — walk to next / previous match
- `v` / `V` / `<C-v>` — Visual mode (see below)
- `<C-o>` / `<C-i>` — jump list works across terminal buffers;
  returning to a terminal restores the scrollback row you were
  on, not the live edge
- `<C-w>` family — window navigation (split / move / resize /
  close)
- All of your usual modal grammar that doesn't mutate text

The shell receives **nothing** while you're in
Normal-in-terminal. Whatever the shell is doing (prompt,
running program, etc.) continues independently.

### Terminal-Visual — `-- TERMINAL-VISUAL --`

`v` (charwise), `V` (linewise), or `<C-v>` (blockwise) enters
Visual on the terminal's cell grid. The anchor lands at the
live cursor row (or the top of the visible window when you're
scrolled into history); `h` / `j` / `k` / `l` extends the head
in the obvious direction. `gg` / `G` extends the head to the
top / bottom of scrollback.

- `y` copies the selection into the unnamed register (or
  `"ay` into named register `a`); the entry kind matches the
  Visual flavour (charwise / linewise / blockwise) so
  cross-buffer paste lands correctly.
- `<Esc>` cancels without copying.
- `gv` reselects the prior terminal Visual session.

Each kind picks the right shape:
- **Linewise (`V`)** — whole rows; trailing cell padding is
  trimmed when yanked.
- **Charwise (`v`)** — exact `(line, col)` start through
  `(line, col)` end, inclusive. Multi-line selections include
  the tail of the start row, full rows between, and the head
  of the end row (vim's shape).
- **Blockwise (`<C-v>`)** — column-aligned rectangle; cells
  inside the box are extracted with column alignment
  preserved so paste re-applies block-shape into a document.

### Terminal-Insert — `-- TERMINAL-INSERT --`

This is **active typing**. Keystrokes encode to the shell
exactly as a normal terminal would.

To enter: `i` / `a` / `I` / `A` (any of vim's Insert-entering
chords — all four are equivalent inside a terminal because the
shell owns the cursor; T2.b — landed).

To exit:
- **`<Esc>`** — default. Exits Terminal-Insert and drops you
  into Normal-in-terminal. Disable with
  `:set terminal.esc-exits=false` when you run nested programs
  (vim, htop, less) that want Esc themselves.
- **`<C-\><C-n>`** — two-key chord (vim convention). `<C-\>`
  alone arms the chord; the next `<C-n>` confirms the exit.
  Any other key after `<C-\>` sends `\x1c` to the PTY plus
  that key's normal bytes, so a stray `<C-\>` doesn't lose
  input.

In Terminal-Insert mode:

- All characters go to the shell.
- `<C-c>` sends SIGINT (cancels the running program), as in
  any other terminal.
- `<C-d>` sends EOF (logs out of the shell if at a prompt).
- `<C-z>` sends SIGTSTP (suspend, if your shell supports job
  control).
- **`<C-w>` is the shell's WERASE** — deletes the previous
  word, just like in your normal terminal. **This is NOT the
  window-management prefix in Insert mode.** To navigate panes,
  exit Insert mode first with `<C-\><C-n>` or `<Esc>`.

This separation matches vim's `:terminal` exactly. The intent
is that `<C-w>` should feel like the shell's `<C-w>` while
you're typing, and feel like Lattice's window-management prefix
while you're navigating.

---

## Scrollback

Lattice keeps a per-terminal scrollback ring sized by
`terminal.scrollback-lines` (default `10000`, set to `0` to
disable). Bytes that roll off the live screen go into the
ring; `j` / `k` / `<C-u>` / `<C-d>` / `gg` walk it.

- The modeline includes a `scroll_offset` indicator when you're
  not at the live edge so you can tell at a glance you're
  viewing history.
- Pressing `i` / `a` / `I` / `A` to enter Terminal-Insert
  snaps the view back to the live edge before activating Insert
  — matches vim's `:terminal`.

## Search

`/pattern<CR>` searches forward across the full grid (visible
+ scrollback); `?pattern<CR>` searches backward. The viewport
jumps to the matching row and the matched cells light up:

- The **current match** paints with strong reverse-video.
- **All other matches** in the visible window paint with an
  underline (`hlsearch`-style).
- `n` walks to the next match, `N` walks the other way.
- `<Esc>` from the search line, entering Insert, or running a
  new search clears the highlight.

The regex syntax is the same as the document path
(`fancy-regex`), so `\v` / `\c` / lookarounds all work.

## Resize

Resizing a terminal pane (via `<C-w>+` / `<C-w>-` / window
resize / split-ratio changes) propagates the new geometry to
both the alacritty grid and the underlying PTY, so the child
sees `SIGWINCH` and re-lays-out its UI. Fullscreen programs
(vim, htop, less) repaint automatically.

## Mouse

**Status:** Not yet implemented. Mouse passthrough — forwarding
clicks / scrolls to programs that have enabled an xterm mouse
mode (`htop`, `vim`, `tmux`, …) — is queued as its own slice
(needs the mouse-event encoder + per-renderer event capture +
the `terminal.mouse-passthrough` option). Today, mouse events
in a terminal buffer are ignored.

Once it lands, defaults will be `auto`:

- `auto` — pass mouse events to the program when it has enabled
  a mouse mode.
- `on` — always pass.
- `off` — never pass (mouse used by Lattice for selection).

Configure with `:set terminal.mouse_passthrough=auto`.

---

## When the shell exits

When the child process exits (you typed `exit`, or it crashed,
or it was killed), the buffer stays open by default so you can
review the final output. Close it with `:bd!` or use:

```
:set terminal.exit_on_process_exit=true
```

to auto-close on exit.

To force-close a terminal (kill the child and remove the
buffer), use `:bd!` — same as any other buffer.

---

## Configuration

All terminal options live in the `terminal.` group. Live-
editable with `:set`:

| Option | Type | Default | Description |
|---|---|---|---|
| `terminal.shell` | string | `$SHELL` else `/bin/sh` | Shell command to spawn |
| `terminal.cwd` | enum | `document` | Working directory: `document` (active doc's parent dir) or `cwd` (Lattice's process cwd) |
| `terminal.display` | enum | `active-pane` | Where `:terminal` lands: `active-pane`, `split`, `vsplit`, or `tab` |
| `terminal.scrollback_lines` | integer | `10000` | Max scrollback ring size |
| `terminal.refresh_hz` | integer | `60` | Render throttle ceiling — bigger = smoother for fast-output programs, but more CPU |
| `terminal.enter_insert_on_open` | bool | `true` | Auto-enter Terminal-Insert when `:terminal` runs |
| `terminal.exit_on_process_exit` | bool | `false` | Auto-close the buffer when the child exits |
| `terminal.mouse_passthrough` | enum | `auto` | Mouse events → program: `auto`, `on`, or `off` |
| `terminal.esc-exits` | bool | `true` | `<Esc>` exits Terminal-Insert (T2.b.0 — landed). Set `false` if you run nested vim / htop inside the terminal and want Esc to reach them instead. |

Options apply to **new** terminals. Existing terminals keep
their boot-time config.

Example `~/.config/lattice/config.toml`:

```toml
[terminal]
shell = "/usr/bin/zsh"
scrollback_lines = 50000
display = "split"
exit_on_process_exit = true
```

---

## Common workflows

### Run a build while editing

```
:terminal cargo watch -x test
```

Opens a terminal running `cargo watch -x test`. Continue editing
in the original pane. When the build output is interesting,
`<C-w>j` to focus the terminal and read it.

### Quick shell, then back to editing

```
:terminal
```

The terminal opens IN the active pane. Type a command, read the
output. To get back: `<C-\><C-n>` to exit Insert mode, then
`:bd!` to close the terminal (or `<C-^>` to flip back to the
alternate buffer, leaving the terminal in the background).

### Search a long log

```
:terminal cat /var/log/system.log
```

Wait for it to load. `<C-\><C-n>` to exit Insert mode. Now
`/error<CR>` searches the log. `n` for next match, `N` for
previous.

### Copy command output into a buffer

In Terminal-Insert mode:

```
$ cargo test 2>&1 | grep FAIL
```

Exit to Normal-in-terminal (`<C-\><C-n>`). Visually select the
failures with `v` / `V` / `y` — they're now in the `"` register.
Switch to your test file (`<C-w>w` or `:b some_test.rs`), and
`p` to paste them as a comment.

### Multiple terminals

Each `:terminal` opens a fresh shell as a new buffer. Use
splits, tabs, or buffer-switching to navigate between them:

```
:vsplit | terminal       " left: editor, right: shell
:tabterminal             " new tab with a shell
gt / gT / 2gt            " switch tabs
:ls                      " list all buffers; terminals
                         " labeled [shell] or [<cmd>]
:b 5                     " jump to terminal buffer #5
```

---

## What's NOT here

- **Sixel / kitty graphics protocol** — image rendering in the
  terminal is deferred. The current substrate
  (alacritty_terminal) doesn't support these; a future swap to
  libghostty would add them if there's user demand.
- **Per-terminal env var overrides on the CLI** — `:terminal
  --env=FOO=bar` isn't a thing yet; spawned shells inherit the
  editor's environment.
- **Session save/restore** — closing Lattice closes all
  terminals. Persistent terminals across sessions are tied to
  the broader session-restore feature
  ([design.md §15:27](../dev/architecture/design.md#15-open-questions))
  — deferred.
- **Live shell config reload** — Lattice doesn't read shell
  dotfiles. Your shell reads them; changes apply on the next
  `:terminal` invocation.

---

## Related

- [`modal-editing.md`](modal-editing.md) — the vim grammar that
  drives Normal-in-terminal.
- [`buffers.md`](buffers.md) — buffer concepts that terminals
  participate in.
- [`options.md`](options.md) — full `:set` reference, including
  the `terminal.*` group.
- [`docs/dev/architecture/terminal-mode.md`](../dev/architecture/terminal-mode.md)
  — developer reference (implementation details).
