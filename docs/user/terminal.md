# Terminal Mode

Lattice embeds a real shell inside the editor as a buffer.
Open one with `:terminal`, type commands, scroll the output
with vim motions, copy results into a register — all without
leaving Lattice.

> **Status:** Design complete; implementation in progress. See
> the developer-facing
> [`docs/dev/architecture/terminal-mode.md`](../dev/architecture/terminal-mode.md)
> and
> [`docs/dev/operations/terminal-mode-plan.md`](../dev/operations/terminal-mode-plan.md).
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

## Two modes inside a terminal

A terminal buffer has two sub-states — same idea as vim's
`:terminal`. The mode-line shows which one you're in.

### Normal-in-terminal — `-- TERMINAL --`

This is the **default when you focus a terminal pane**. Vim's
grammar applies to the **scrollback** (the historical output),
NOT to the shell:

- `j` / `k` / `gg` / `G` / `0` / `$` — move the cursor through
  history
- `/pattern` / `?pattern` / `n` / `N` — search the output
- `v` / `V` / `<C-v>` — Visual mode for selection
- `y` — copy the selection into a register
- `<C-o>` / `<C-i>` — jump list works across terminal buffers
- `<C-w>` family — window navigation (split / move / resize /
  close)
- All your usual modal grammar

The shell receives **nothing**. Whatever the shell is doing
(prompt, running program, etc.) continues independently.

### Terminal-Insert — `-- TERMINAL-INSERT --`

This is **active typing**. Keystrokes encode to the shell
exactly as a normal terminal would.

To enter: `i` / `a` / `I` / `A` (any of vim's Insert-entering
chords).

To exit:
- **`<C-\><C-n>`** — vim convention, the always-on escape
  hatch.
- **`<Esc>`** — also exits (unless your shell binds Esc and
  you've set `:set terminal.esc_exits=false`).

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

## Mouse

If the program running in the terminal (`htop`, `vim`, `tmux`,
etc.) has enabled mouse support, mouse clicks and scrolls pass
through to the program. Defaults to `auto`:

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
| `terminal.esc_exits` | bool | `true` | `<Esc>` exits Terminal-Insert |

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
