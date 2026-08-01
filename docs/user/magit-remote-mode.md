---
summary: "magit-remote-mode: the configured remotes with their URLs — a adds, r renames, d removes, u sets the URL, p prunes."
related: [magit, magit-branch-mode, ex:magit-remote, magit-transient]
---

# magit-remote-mode

Your configured remotes, each with the URL it fetches from.
`:magit-remote`, or `M` in the repo [dispatch
transient](help:magit-transient).

```
Remotes (2)
  origin    git@github.com:you/project.git
  upstream  https://github.com/them/project.git  (push: git@github.com:them/project.git)
```

The name is highlighted, the URL is dimmed, and the headerline carries
the count. A **push URL is only printed when it differs** from the
fetch URL — which is the case worth noticing, and printing an identical
URL on every row would bury it.

## Chords

| Chord | Action |
|---|---|
| `a` | Add a remote — asks for the name, then the URL |
| `r` | Rename the remote at cursor |
| `d` | Remove the remote at cursor |
| `u` | Set the URL of the remote at cursor |
| `p` | Prune — delete local refs whose branch is gone from the remote |
| `gr` | Refresh (re-read the remotes) |

`q` and navigation come from
[`magit-core-mode`](help:magit-core-mode).

Every prompt cancels cleanly on `<Esc>`, and submitting an empty value
is also a cancel — `a` with no URL adds nothing, `r` with the name
unchanged renames nothing.

`r` and `u` open pre-filled: the rename prompt starts at the current
name and the URL prompt at the current URL, so fixing a typo is an edit
rather than a retype.

## What is *not* here

**Fetch, pull and push.** Those are `f`, `F` and `P` on the [dispatch
transient](help:magit-transient), where they have their own flag menus
(`--force-with-lease`, `--set-upstream`, `--all`, `--prune`). This
buffer is for *managing* which remotes exist and where they point.

`p` (prune) is the one row here that talks to the network, so it echoes
`pruning <name>…` and the list refreshes when it returns. The others
are local config edits and complete immediately.

## Why `d` does not ask

Removing a remote drops its config and its remote-tracking refs. That
is recoverable — `a` puts it back in two prompts, and the URL you need
is on the row in front of you when you press `d`. Confirmations are
reserved for operations that destroy work you cannot get back (`Oh`,
`git reset --hard`); asking here would train you to dismiss the prompt
that does matter. Emacs magit does not confirm it either.

## Setting the push URL

`u` sets the **fetch** URL (`git remote set-url`). A remote configured
with a separate push URL keeps it, and the list shows both so you can
see the split — but editing the push URL is not yet bound to a chord.
Use `:terminal` and `git remote set-url --push` for that.

## See also

- [`magit-branch-mode`](help:magit-branch-mode) — the local branch
  list, the same buffer shape one level in.
- [`magit-transient`](help:magit-transient) — `f` / `F` / `P` and their
  flags.
