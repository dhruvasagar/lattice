---
summary: "Magit: git porcelain inside Lattice — status, commit, diff, log, blame, stash, branch, rebase, and transient dispatch menus, all backed by the VCS subsystem."
related: [magit-status, magit-transient, ex:magit-status, ex:magit-commit, ex:magit-diff, ex:magit-log, ex:magit-blame]
---

# Magit

Magit is Lattice's git porcelain — a complete, modal, keyboard-driven
interface for git that lives inside the editor. It is modeled on Emacs
magit's section-collapsible status buffer and transient prefix menus,
adapted to Lattice's vim-normal-mode conventions and
everything-is-a-buffer architecture. Staging works at both **file and
hunk** granularity; staging a *selection of lines* within a hunk (Emacs
magit's visual-mode partial stage) is not built yet.

Every magit view is a buffer-backed Document with a major mode. You
open, close, navigate, and search them the same way you do any other
buffer — there are no special sidebars, no separate tool windows, and
no hidden state.

> **Status:** magit-status (the primary workhorse — staged, unstaged,
> untracked, stashes, recent commits), magit-commit, magit-diff,
> magit-log, magit-blame, magit-stash, magit-branch, magit-rebase,
> and the transient dispatch menus (`C-c g` / `C-c f`) are shipped.
> Auto-gutter-diff against HEAD is on by default (`git.auto-head-diff`).
> See [magit-status-mode](help:magit-status-mode) for the workhorse
> view's full chord set.

---

## Quick reference

| Key / command | Meaning |
|---|---|
| `C-x g` | Open [magit-status](help:magit-status-mode) for the current repo |
| `C-c g` | Open the [repo dispatch transient](help:magit-transient) — flat menu, one entry point per view (status/commit/log/branch/stash/rebase), plus `F` (pull) / `P` (push), both real git operations run in the background |
| `C-c f` | Open the [file dispatch transient](help:magit-transient) — `s` stages / `d` diffs the file in your *current* buffer (not an entry under the cursor elsewhere) |
| `:magit-status` | Same as `C-x g` — open the status buffer |
| `:magit-commit` | Open the commit message buffer |
| `:magit-diff` | Open a read-only `git diff HEAD` view with file-level stage/unstage |
| `:magit-log` | Open the commit history log |
| `:magit-blame` | Open blame annotations for the current file |
| `:magit-stash-list` | Open the stash list |
| `:magit-branch` | Open the branch list |
| `:magit-rebase` | Start interactive rebase |
| `:magit-fetch` | Fetch from the default remote (`--all`, `--prune`) |
| `:magit-pull` | Pull from the upstream branch (fast-forward only) |
| `:magit-push` | Push the current branch (`--force-with-lease`, `--set-upstream`) |
| `:magit-stash` | Stash the working tree (`--include-untracked`, `-m <message>`); `:magit-stash-list` opens the list |
| `g?` | (in any magit buffer) Open help for the current mode's keybindings |

Every magit command name is dashed + namespaced (`magit-status`,
`magit-log`, `magit-blame`, …) — type `:magit-<Tab>` to see the full
command palette.

The last four run a git operation rather than opening a buffer. They are
the same operations the `C-c g` transient offers, reaching the same
implementation — the transient is the discoverable surface, the
ex-command the scriptable one. Each returns immediately with a
`magit: pushing…`-style echo; the outcome is reported through the log
(and `*messages*`), because the operation outlives the keystroke that
started it. A missing or expired credential fails fast rather than
hanging, since git is run with `GIT_TERMINAL_PROMPT=0`.

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

Every magit buffer inherits a shared [minor mode](help:modes),
[`magit-core-mode`](help:magit-core-mode), which supplies `gr`
(refresh), `q` (close), `]]` / `[[` / `]f` / `[f` / `]c` / `[c`
(navigate at three scales), and `TAB` / `S-TAB` (fold). One movement
vocabulary across status, log, diff, blame, stash, branch and rebase —
see that page for the full table and what `gr` means per view.

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
[transient menus](help:magit-transient).

### `C-c f` — file dispatch transient

Press `C-c f` to open the **file-level dispatch transient**. `s`
stages and `d` opens a diff scoped to just that one file — both act
on the file belonging to whatever buffer was active when you pressed
`C-c f`, not an entry at the cursor in some other buffer (pressing
`C-c f` while inside `magit-status`, for instance, does not act on
the entry under the cursor there). If the active buffer has no file
(a synthetic buffer, an empty scratch buffer, …) there's no path to
resolve and the action does nothing. See [transient
menus](help:magit-transient).

All three chords follow Emacs convention and are unused in default vim
normal mode — they map cleanly over the vim grammar.

---

## Headerline

Every magit buffer carries a sticky row above its first line saying
what you are looking at — the thing the buffer's own text usually
cannot tell you. A diff does not say which scope it diffed; a blame
does not say how far back `p` has walked; a file-at-revision looks
exactly like the live file. The row answers that:

| Buffer | Headerline |
|---|---|
| [`magit-status-mode`](help:magit-status-mode) | `lattice  main ↑2 ↓1  3 staged  5 unstaged` |
| [`magit-commit-mode`](help:magit-commit-mode) | `main  3 files +120 −18` — plus `AMEND` |
| [`magit-revision-mode`](help:magit-revision-mode) | `a1b2c3d  Jane Doe  3 days ago  Fix the thing` |
| [`magit-file-revision-mode`](help:magit-file-revision-mode) | `src/main.rs  @  a1b2c3d`, or `@  index` |
| [`magit-diff-mode`](help:magit-diff-mode) | `staged  src/main.rs` |
| [`magit-log-mode`](help:magit-log-mode) | `HEAD  50 commits  src/main.rs` |
| [`magit-blame-mode`](help:magit-blame-mode) | `src/main.rs  @  a1b2c3d` — updates as `p` walks back |
| [`magit-branch-mode`](help:magit-branch-mode) | `main  12 branches` |
| [`magit-stash-mode`](help:magit-stash-mode) | `3 stashes` |
| [`magit-stash-show-mode`](help:magit-stash-show-mode) | `stash@{2}  WIP on main: fix the thing` |
| [`magit-rebase-mode`](help:magit-rebase-mode) | `onto  origin/main  4 commits` — plus `REBASE IN PROGRESS` |

Fields are coloured by what they are — SHAs, branches, refs, and
authors each take their own theme colour — rather than labelled, so the
row stays short on a narrow split. Two theme elements are magit's own:
`magit.headerline.label` (counts, paths, dates) and
`magit.headerline.alert` (`AMEND`, `REBASE IN PROGRESS`); the rest
reuse the `magit.*` colours the buffer bodies already use.
`:colorscheme` repaints the row live.

The row refreshes with its buffer — `gr`, and any action that rebuilds
the view. It stays hidden until the buffer's first content lands, so
nothing shifts down and back up while git answers.

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
- `:help magit-status-mode` — the [status buffer](help:magit-status-mode) deep-dive
- `:help magit-transient` — the [transient dispatch menus](help:magit-transient)
- `:help <mode>` for any view — every magit buffer has its own page,
  named after its mode:
  [`magit-commit-mode`](help:magit-commit-mode),
  [`magit-revision-mode`](help:magit-revision-mode),
  [`magit-file-revision-mode`](help:magit-file-revision-mode),
  [`magit-diff-mode`](help:magit-diff-mode),
  [`magit-log-mode`](help:magit-log-mode),
  [`magit-blame-mode`](help:magit-blame-mode),
  [`magit-stash-mode`](help:magit-stash-mode),
  [`magit-stash-show-mode`](help:magit-stash-show-mode),
  [`magit-branch-mode`](help:magit-branch-mode),
  [`magit-rebase-mode`](help:magit-rebase-mode)
- `:help magit-core-mode` — the [shared chords](help:magit-core-mode)
  every magit buffer inherits
- `:help magit-global-mode` — the [entry chords](help:magit-global-mode)
  (`C-x g`, `C-c g`, `C-c f`)
- `:magit-<Tab>` — list all magit ex-commands
- `:describe-key` then press any magit chord to see its bound action
- `:describe-mode` in a magit buffer to see the active mode's full keymap
- `g?` — (future Lattice-wide convention) opens a help buffer for the current buffer's major mode
