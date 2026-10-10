---
summary: ":!cmd runs a one-off shell command and streams its output into *shell-command*; <C-c> stops it, gr runs it again. Separate from :compile — it never touches the build's output, command or error list."
related: [ex-commands, compilation-mode, ex:shell-command, ex:shell-command-kill]
---

# shell-command-mode

`:!command` runs a shell command and shows what it prints, in a read-only
`*shell-command*` buffer. It is for the one-off — `:!git status`, `:!ls
-la`, `:!make clean` — where you want to see the output and get on.

```
:!git log --oneline -5
:!cargo tree | head -20
:shell-command du -sh *      " the same thing, spelled out
```

The command line goes to your shell (`sh -c`; `cmd /C` on Windows), so
pipes, quoting and globs work as they do in a terminal. It runs in the
project of the buffer you ran it from.

The output streams in as the command produces it, and the editor stays
responsive while it runs — you can switch away and come back.

| Command                  | Does                                         |
|--------------------------|----------------------------------------------|
| `:!command`              | Run `command` and show its output            |
| `:shell-command command` | The same, by name                            |
| `:shell-command-kill`    | Stop the running command (what `<C-c>` runs) |

Inside the buffer:

| Key     | Does             |
|---------|------------------|
| `<C-c>` | Stop the command |
| `gr`    | Run it again     |

The headerline shows the command and how it ended: running, `ok`,
`failed` (a non-zero exit) or `killed`.

## It is not `:compile`

[`:compile`](help:compilation-mode) is for builds and has a specific job:
it reads the output for errors, fills the error list that `:cnext` walks,
and remembers its command for `:recompile`. `:!cmd` does none of that, on
purpose:

- **Nothing is parsed.** The output is shown as the command printed it —
  no error list entries, no gutter marks, no jump-to-location. Colours the
  command itself emits are kept.
- **It is highlighted as shell.** The `$ command` line and the output are
  coloured with the shell grammar: strings, variables, keywords. That is
  colouring only. Output that is not shell is coloured as though it were,
  which is usually fine and occasionally odd.
- **It has its own buffer.** A `:!git status` between a build and its
  `:recompile` leaves `*compilation*`, the error list and the command
  `:recompile` re-runs exactly as they were.

If you want a command's errors to be jumpable, that is what `:compile` is
for.

## Filtering text

With a range in front, `!` does something different: it pipes those lines
through the command and replaces them with the output. See
[shell commands](help:ex-commands) — `:%!sort`, `:.!date`.

## Not supported

`%` and `#` in the command standing for file names, `:!!` to repeat the
last command, and `:r !cmd` to read a command's output into the buffer.
