---
summary: "Magit: git porcelain inside Lattice — status, commit, diff, log, blame, stash, branch, rebase, and transient dispatch menus, all backed by the VCS subsystem."
related: [magit-status, magit-buffers, magit-transient, ex:magit-status, ex:magit-commit, ex:magit-diff, ex:magit-log, ex:magit-blame]
---

# Magit

Magit is Lattice's git porcelain — a complete, modal, keyboard-driven
interface for git that lives inside the editor. It is modeled on Emacs
magit's section-collapsible status buffer and transient prefix menus,
adapted to Lattice's vim-normal-mode conventions and
everything-is-a-buffer architecture. Unlike Emacs magit, staging today
is **file-level only** — there is no hunk-at-a-time staging anywhere in
Lattice's magit yet.

Every magit view is a buffer-backed Document with a major mode. You
open, close, navigate, and search them the same way you do any other
buffer — there are no special sidebars, no separate tool windows, and
no hidden state.

> **Status:** magit-status (the primary workhorse — staged, unstaged,
> untracked, stashes, recent commits), magit-commit, magit-diff,
> magit-log, magit-blame, magit-stash, magit-branch, magit-rebase,
> and the transient dispatch menus (`C-c g` / `C-c f`) are shipped.
> Auto-gutter-diff against HEAD is on by default (`git.auto-head-diff`).
> See the [implementation ledger](help:magit-status) for per-slice detail.

---

## Quick reference

| Key / command | Meaning |
|---|---|
| `C-x g` | Open [magit-status](magit-status.md) for the current repo |
| `C-c g` | Open the [repo dispatch transient](magit-transient.md) — flat menu, one entry point per view (status/commit/log/branch/stash/rebase), plus `F` (pull) / `P` (push), both real git operations run in the background |
| `C-c f` | Open the [file dispatch transient](magit-transient.md) — `s` stages / `d` diffs the file in your *current* buffer (not an entry under the cursor elsewhere) |
| `:magit-status` | Same as `C-x g` — open the status buffer |
| `:magit-commit` | Open the commit message buffer |
| `:magit-diff` | Open a read-only `git diff HEAD` view with file-level stage/unstage |
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
| `*magit:log*` | `git log --oneline --graph --decorate -50` (count is currently hardcoded) | `<CR>` opens `*magit:commit:<sha>*`, a `git show <sha>` view, for the commit at cursor |
| `*magit:blame*` | Blame loaded on open (the view IS the blame) | `<CR>` shows the commit for the blamed line; `p` re-blames at parent |
| `*magit:commit*` | Staged diff loaded on open (the purpose of this view) | — |

This is the single most important performance decision in the magit
design — status opens in **10-50ms** regardless of repository size,
because no diffs are pre-computed.

### Shared navigation (magit-core)

Every magit buffer inherits a shared [minor mode](modes.md),
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
transient** — a grouped menu with one entry point per magit view:
status, commit, log, branch, stash, rebase all genuinely open their
buffer, from wherever you happen to be. `F` (pull) and `P` (push) are
also real: `F` runs `git fetch` + a fast-forward-only merge (it will
never create a merge commit — if your branch has diverged it fails
cleanly instead of merging), `P` runs `git push`. Both run in the
background and fail fast if git needs credentials it doesn't have;
the result shows up in the `*messages*` buffer / debug log, not as an
immediate on-screen confirmation. It's a flat list today — pressing
`s`, `l`, `b`, `z`, `r`, `F`, or `P` just fires the corresponding
buffer-open or git operation (the same thing `:magit-status` /
`:magit-log` / … does for the buffer-opening ones); there are no
nested branch/stash/push submenus with their own actions yet. See
[transient menus](magit-transient.md).

### `C-c f` — file dispatch transient

Press `C-c f` to open the **file-level dispatch transient**. `s`
stages and `d` opens a diff scoped to just that one file — both act
on the file belonging to whatever buffer was active when you pressed
`C-c f`, not an entry at the cursor in some other buffer (pressing
`C-c f` while inside `magit-status`, for instance, does not act on
the entry under the cursor there). If the active buffer has no file
(a synthetic buffer, an empty scratch buffer, …) there's no path to
resolve and the action does nothing. See [transient
menus](magit-transient.md).

All three chords follow Emacs convention and are unused in default vim
normal mode — they map cleanly over the vim grammar.

---

## Options

`git.auto-head-diff` is registered through the typed-options system
(`:set` / `:customize`), owned by `lattice-host`'s VCS subsystem:

| Option | Type | Default | Description |
|---|---|---|---|
| `git.auto-head-diff` | `bool` | `true` | Auto-register a gutter-diff against HEAD when opening files in git repos |

Everything else that earlier revisions of this page listed here
(`magit.auto-refresh`, `magit.refresh-debounce-ms`,
`magit.status.show-untracked`, `magit.status.show-stashes`,
`magit.status.recent-commits-count`, `magit.log.count`,
`magit.log.graph`, `magit.log.decorate`, `magit.blame.author-width`,
`magit.blame.date-format`, `magit.commit.show-diff`,
`magit.diff.context-lines`) is **not currently a registered option** —
`lattice-magit` doesn't register any options of its own, and none of
these names appear in the typed-options registry. `:set` on any of them
fails loudly with `unknown option` rather than silently accepting and
ignoring the value. The behavior each name implies is often real
(untracked files do show by default, the log does default to `-50`
entries, blame dates are relative, …) but today it's hardcoded, not a
live knob — treat this whole list as a roadmap for options that should
exist, not ones that do.

---

## Help and discovery

- `:help magit` — this page
- `:help magit-status` — the [status buffer](magit-status.md) deep-dive
- `:help magit-buffers` — the [commit/diff/log/blame/stash/branch/rebase buffers](magit-buffers.md)
- `:help magit-transient` — the [transient dispatch menus](magit-transient.md)
- `:magit-<Tab>` — list all magit ex-commands
- `:describe-key` then press any magit chord to see its bound action
- `:describe-mode` in a magit buffer to see the active mode's full keymap
- `g?` — (future Lattice-wide convention) opens a help buffer for the current buffer's major mode
