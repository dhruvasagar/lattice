+++
title = "Magit"
+++



Magit is Lattice's git porcelain — a complete, modal, keyboard-driven
interface for git that lives inside the editor. It is modeled on Emacs
magit's section-collapsible status buffer, hunk-at-a-time staging, and
transient prefix menus, adapted to Lattice's vim-normal-mode
conventions and everything-is-a-buffer architecture.

Every magit view is a buffer-backed Document with a major mode. You
open, close, navigate, and search them the same way you do any other
buffer — there are no special sidebars, no separate tool windows, and
no hidden state.

> **Status:** magit-status (the primary workhorse — staged, unstaged,
> untracked, stashes, recent commits), magit-commit, magit-diff,
> magit-log, magit-blame, magit-stash, magit-branch, magit-rebase,
> and the transient dispatch menus (`C-c g` / `C-c f`) are shipped.
> Auto-gutter-diff against HEAD is on by default (`git.auto-head-diff`).
> See the [implementation ledger](../magit-status/) for per-slice detail.

---

## Quick reference

| Key / command | Meaning |
|---|---|
| `C-x g` | Open [magit-status](../magit-status/) for the current repo |
| `C-c g` | Open the [repo dispatch transient](../magit-transient/) (branch, merge, rebase, fetch, push, …) |
| `C-c f` | Open the [file dispatch transient](../magit-transient/) (stage, unstage, diff, log, blame for the current file) |
| `:magit-status` | Same as `C-x g` — open the status buffer |
| `:magit-commit` | Open the commit message buffer |
| `:magit-diff` | Open a side-by-side diff view |
| `:magit-log` | Open the commit history log |
| `:magit-blame` | Open blame annotations for the current file |
| `:magit-stash-list` | Open the stash list |
| `:magit-branch` | Open the branch list |
| `:magit-rebase` | Start interactive rebase |
| `g?` | (in any magit buffer) Open help for the current mode's keybindings |

Every magit command name is dashed + namespaced (`magit-status`,
`magit-log`, `magit-blame`, …) — type `:magit-<Tab>` to see the full
command palette.

---

## Concepts

### Everything is a buffer

Magit views are ordinary Documents. They appear in panes, respond to
`:ls` / `:bn` / `:bp` / `:bd`, and inherit all the standard vim grammar
(yanks, searches, folds). The renderer treats them identically to code
buffers — there is zero special-cased rendering for magit.

### Modes own their surface

Every chord (`s`, `u`, `x`, `q`, `gr`, `TAB`, `]]`/`[[`) and every
action-handler body lives inside the `lattice-magit` crate. The editor
host has no `do_magit_stage_hunk` method — magit is fully inverted out
of the host, installing through the same `SubsystemBoot` seam as
every other core feature (terminal, compilation, LSP, oil, dashboard).

### Three-layer architecture

```
lattice-magit          FEATURE — modes, keymaps, action handlers, synthetic buffers
lattice-host::vcs      CORE    — RepositoryWatcher, auto-gutter-diff, RepositoryEvent
lattice-vcs            DATA    — Repository, WorkingTree, Index, Commit, Branch, Stash
```

`lattice-vcs` is a pure data crate (zero `lattice-*` dependencies)
wrapping `gix` and the `git` CLI. `lattice-host::vcs` auto-registers
a `DiffSession` against HEAD whenever you open a file in a git repository,
producing immediate gutter signs. `lattice-magit` consumes both layers
to provide the full porcelain.

### Lazy by default

Magit buffers load only the data needed to paint the viewport.
Expensive operations — diffs, blame data, commit details — are
**deferred** until you explicitly invoke them.

| View | On open | On demand |
|---|---|---|
| `*magit:status*` | File paths + status labels (fast list view) | `=` loads `git diff --cached <path>` / `git diff <path>` per-file |
| `*magit:diff*` | Diff loaded on open (the view IS the diff) | — |
| `*magit:log*` | `git log --oneline --graph --decorate -N` | `<CR>` loads `git show <sha>` for the commit at cursor |
| `*magit:blame*` | Blame loaded on open (the view IS the blame) | `<CR>` shows the commit for the blamed line; `p` re-blames at parent |
| `*magit:commit*` | Staged diff loaded on open (the purpose of this view) | — |

This is the single most important performance decision in the magit
design — status opens in **10-50ms** regardless of repository size,
because no diffs are pre-computed.

### Shared navigation (magit-core)

Every magit buffer inherits a shared [minor mode](../modes/),
`magit-core-mode`, which provides the following chords:

| Chord | Action |
|---|---|
| `gr` | Refresh the current magit buffer |
| `q` | Close the buffer (bury — return to previous) |
| `]]` / `[[` | Next / previous top-level section |
| `]f` / `[f` | Next / previous file or entry within the current section |
| `]c` / `[c` | Next / previous hunk |
| `TAB` | Toggle section or hunk fold at cursor |
| `S-TAB` | Cycle section visibility (all → changed only → collapsed → all) |

These work in every magit buffer: status, log, diff, stash, branch —
the shared minor mode ensures a consistent navigation model.

---

## Global entry points

### `C-x g` — open status

Press `C-x g` from any buffer to open `*magit:status*` for the current
repository. This is the primary entry point — the same way `C-x g` opens
magit-status in Emacs.

The command discovers the git repository by walking up from the current
buffer's directory. If you're not in a git repository, the buffer shows
"Not a git repository."

### `C-c g` — dispatch transient

Press `C-c g` from any buffer to open the **repo-level dispatch
transient** — a grouped action menu that gives you single-key access to
every magit operation: stage/unstage, commit, log, branch, merge,
rebase, stash, fetch, push. See [transient menus](../magit-transient/).

### `C-c f` — file dispatch transient

Press `C-c f` to open the **file-level dispatch transient** for the
current buffer's file. Shows per-file operations: stage, unstage,
discard, diff, log, blame, rename, delete. See [transient
menus](../magit-transient/).

All three chords follow Emacs convention and are unused in default vim
normal mode — they map cleanly over the vim grammar.

---

## Options

Registered through the typed-options system (`:set` / `:customize`),
owned by `lattice-magit`:

| Option | Type | Default | Description |
|---|---|---|---|
| `git.auto-head-diff` | `bool` | `true` | Auto-register a gutter-diff against HEAD when opening files in git repos |
| `magit.auto-refresh` | `bool` | `true` | Auto-refresh magit buffers on repository changes |
| `magit.refresh-debounce-ms` | `u32` | `100` | Debounce window for auto-refresh (milliseconds) |
| `magit.status.show-untracked` | `bool` | `true` | Show untracked files section |
| `magit.status.show-stashes` | `bool` | `true` | Show stash list section |
| `magit.status.recent-commits-count` | `u32` | `20` | Number of recent commits to show |
| `magit.log.count` | `u32` | `50` | Default log entry count |
| `magit.log.graph` | `bool` | `true` | Show commit graph in log |
| `magit.log.decorate` | `bool` | `true` | Show branch/tag decorations in log |
| `magit.blame.author-width` | `u8` | `12` | Max author name width in blame gutter |
| `magit.blame.date-format` | `string` | `"relative"` | `relative`, `short`, or `iso` |
| `magit.commit.show-diff` | `bool` | `true` | Show staged diff in commit buffer |
| `magit.diff.context-lines` | `u32` | `3` | Context lines in inline and side-by-side diffs |

Use `:set magit.status.show-untracked=false` to hide the untracked
files section. Options are live — changes take effect on the next
buffer refresh.

---

## Help and discovery

- `:help magit` — this page
- `:help magit-status` — the [status buffer](../magit-status/) deep-dive
- `:help magit-buffers` — the [commit/diff/log/blame/stash/branch/rebase buffers](../magit-buffers/)
- `:help magit-transient` — the [transient dispatch menus](../magit-transient/)
- `:magit-<Tab>` — list all magit ex-commands
- `:describe-key` then press any magit chord to see its bound action
- `:describe-mode` in a magit buffer to see the active mode's full keymap
- `g?` — (future Lattice-wide convention) opens a help buffer for the current buffer's major mode
