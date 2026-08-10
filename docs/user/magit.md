---
summary: "Magit: git porcelain inside Lattice — status, commit, diff, log, blame, stash, branch, rebase, and transient dispatch menus, all backed by the VCS subsystem."
related: [magit-status, magit-transient, ex:magit-status, ex:magit-commit, ex:magit-diff, ex:magit-log, ex:magit-blame]
---

# Magit

Magit is Lattice's git porcelain — a complete, modal, keyboard-driven
interface for git that lives inside the editor. It is modeled on Emacs
magit's section-collapsible status buffer and transient prefix menus,
adapted to Lattice's vim-normal-mode conventions and
everything-is-a-buffer architecture. Staging works at **file, hunk and
line** granularity — including Emacs magit's visual-mode partial stage,
where you select lines inside a hunk and move only those.

Every magit view is a buffer-backed Document with a major mode. You
open, close, navigate, and search them the same way you do any other
buffer — there are no special sidebars, no separate tool windows, and
no hidden state.

> **Status:** magit-status (the primary workhorse — staged, unstaged,
> untracked, stashes, recent commits), magit-commit, magit-diff,
> magit-log, magit-blame, magit-stash, magit-branch, magit-remote,
> magit-submodule, magit-rebase, bisect, side-by-side diffs (`dv`),
> and the transient dispatch menus (`C-c g` / `C-c f`) are shipped.
> Auto-gutter-diff against HEAD is on by default (`git.auto-head-diff`).
> See [magit-status-mode](help:magit-status-mode) for the workhorse
> view's full chord set.

---

## Quick reference

| Key / command | Meaning |
|---|---|
| `C-x g` | Open [magit-status](help:magit-status-mode) for the current repo |
| `C-c g` | Open the [repo dispatch transient](help:magit-transient) — one entry point per view (status/commit/log/branch/remote/stash/rebase), plus `S` (stage all) / `U` (unstage all), `B` (bisect) and `f` (fetch) / `F` (pull) / `P` (push), all real git operations run in the background |
| `C-c f` | Open the [file dispatch transient](help:magit-transient) — `s` stages / `d` diffs the file in your *current* buffer (not an entry under the cursor elsewhere) |
| `:magit-status` | Same as `C-x g` — open the status buffer |
| `:magit-commit` | Open the commit message buffer |
| `:magit-diff` | Open a read-only `git diff HEAD` view with file-level stage/unstage |
| `:magit-log` | Open the commit history log |
| `:magit-blame` | Toggle [blame annotations](help:magit-blame-mode) on the current file |
| `:magit-find-file <rev> <path>` | Open a file [as it was at a revision](help:magit-file-revision-mode) |
| `:magit-checkout <branch>` | Check out a branch |
| `:magit-stash-list` | Open the stash list |
| `:magit-branch` | Open the branch list |
| `:magit-remote` | Open the [remote list](help:magit-remote-mode) — add / rename / remove / set-url / prune |
| `:magit-submodule` | Open the [submodule list](help:magit-submodule-mode) — add / update / sync / remove |
| `:magit-refs` | Open the [refs buffer](help:magit-refs-mode) — every branch, remote-tracking branch and tag |
| `:magit-clone <url> [<dest>]` | Clone a repository (does not switch you to it — see [the dispatch](help:magit-transient)) |
| `:magit-note-edit <commit>` | Edit a commit's [note](help:magit-notes-mode) |
| `:magit-note-remove <commit>` | Remove a commit's note |
| `:magit-note-merge <ref> [strategy]` | Merge a notes ref into this one |
| `:magit-cherries <upstream> [<head>]` | Which commits are [not upstream yet](help:magit-cherry-mode) |
| `:magit-am <patch>… [-3]` | Apply a mailbox of patches |
| `:magit-format-patch <range>` | Write a commit range out as .patch files |
| `:magit-subtree-add/-merge/-pull/-push/-split` | `git subtree` operations |
| `:magit-log-merged <commit>` | Show the merge commit that brought a commit into `HEAD` |
| `:magit-rebase` | Start interactive rebase |
| `:magit-rebase-continue` / `-skip` / `-abort` | Leave a [rebase that stopped](help:magit-rebase-mode) |
| `:magit-fetch` | Fetch from the default remote (`--all`, `--prune`) |
| `:magit-pull` | Pull from the upstream branch (fast-forward only) |
| `:magit-push` | Push the current branch (`--force-with-lease`, `--set-upstream`) |
| `:magit-stash` | Stash the working tree (`--include-untracked`, `-m <message>`); `:magit-stash-list` opens the list |
| `C-c g` | (in **any** buffer, magit or not) Open the [dispatch menu](help:magit-transient) — every root menu is one key from there |
| `g?` | (in any magit buffer) Open help for the current mode's keybindings |

Every magit command name is dashed + namespaced (`magit-status`,
`magit-log`, `magit-blame`, …) — type `:magit-<Tab>` to see the full
command palette.

The last four run a git operation rather than opening a buffer. They are
the same operations the `C-c g` transient offers, reaching the same
implementation — the transient is the discoverable surface, the
ex-command the scriptable one. Each returns immediately with a
`magit: pushing…`-style echo; the outcome arrives as a
[notification](help:notifications) when it lands, and the full text
through the log
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

### Every mutation reports

Any magit operation that **changes the repository** tells you how it
went — staging, unstaging, discarding, applying and reversing hunks,
branch checkout / create / delete / merge, stash apply / pop / drop /
create, remote and submodule operations, and everything the
[transient menus](help:magit-transient) run. Success and failure both,
as a [notification](help:notifications).

This matters most when it *fails*. A magit buffer refreshes after every
mutation, so a failed stage used to look exactly like a successful one:
the buffer redrew, the file stayed where it was, and nothing said why.
Now the failure is the notification, with git's first line of output;
the full text is in the log and `*messages*`.

Operations that only **read** stay quiet — a refresh (`gr`), opening a
log, a diff or a blame. The buffer appearing *is* the report, and a
notification per refresh would bury the ones that matter.

Notifications are how magit reports, but magit does not know that: it
publishes a "background task finished" event and the notification
layer subscribes. See [`notifications`](help:notifications) for where
they appear, how long they linger, and `:notifications` for the log of
them.

### Lazy by default

Magit buffers load only the data needed to paint the viewport.
Expensive operations — diffs, blame data, commit details — are
**deferred** until you explicitly invoke them.

| View | On open | On demand |
|---|---|---|
| `*magit:status*` | File paths + status labels (fast list view) | `=` loads `git diff --cached <path>` / `git diff <path>` per-file |
| `*magit:diff*` | Diff loaded on open (the view IS the diff) | — |
| `*magit:log*` | `git log --oneline --graph --decorate -50` (count is currently hardcoded) | `<CR>` opens `*magit:commit:<sha>*`, a `git show <sha>` view, for the commit at cursor |
| `*magit:blame*` | Blame loaded on open (the view IS the blame) | `<CR>` shows the commit for the blamed line; `p` blames back one commit |
| `*magit:blame-reverse:<rev>:<path>*` | Reverse blame loaded on open — for each line of `<rev>`'s version, the last commit it existed in | Same chords; `p` walks the starting revision back |
| `*magit:commit*` | Staged diff loaded on open (the purpose of this view) | — |

This is the single most important performance decision in the magit
design — status opens in **10-50ms** regardless of repository size,
because no diffs are pre-computed.

### Shared navigation (magit-core)

Every magit buffer inherits a shared [minor mode](help:modes),
[`magit-core-mode`](help:magit-core-mode), which supplies `gr`
(refresh), `q` (close), `]]` / `[[` (move between sections), `TAB` /
`S-TAB` (fold), and the repo-level
`S` / `U` / `C` / `i` / `yr`. One movement vocabulary across status,
log, diff, blame, stash, branch and rebase — see that page for the
full table and what `gr` means per view.

The finer two navigation scales — `]f` / `[f` between files and
`]c` / `[c` between hunks — belong to
[`magit-hunk-mode`](help:magit-hunk-mode), which is active only where a
diff is rendered. They used to be on the core mode, and so were bound
in a branch list and a blame, where there are no files or hunks to step
between.

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
cleanly instead of merging), `P` runs `git push`. `S` stages every
tracked modification (`git add --update`, untracked files deliberately
left out) and `U` unstages everything while leaving your working tree
alone. These all run in the background and fail fast if git needs
credentials it doesn't have; the result shows up in the `*messages*`
buffer / debug log, not as an immediate on-screen confirmation.

Four entries are submenus rather than direct actions — `c` (commit /
amend), `z` (stash push / list), and `f` / `P`, whose menus hold the
[toggleable flags](help:magit-transient) their git operation accepts.
`BS` returns from a submenu to the parent. See
[transient menus](help:magit-transient) for the full rendered menu and
for which magit entries are deliberately absent.

### `C-c f` — file dispatch transient

Press `C-c f` to open the **file-level dispatch transient**. `s`
stages and `d` opens a diff scoped to just that one file — both act
on the file belonging to whatever buffer was active when you pressed
`C-c f`, not an entry at the cursor in some other buffer (pressing
`C-c f` while inside `magit-status`, for instance, does not act on
the entry under the cursor there). If the active buffer has no file
(a synthetic buffer, an empty scratch buffer, …) there's no path to
resolve and the action does nothing.

There is **no "which file?" prompt** — the one deliberate deviation
from Emacs magit, which asks even though the default is always the file
you're visiting. For a file you are *not* visiting there is a separate
stand-alone command, `:magit-other-file-dispatch`, which offers the
same rows plus a target you set with `=f`. It is bound to no chord;
bind it if you prefer always being asked. See [transient
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
| [`magit-status-mode`](help:magit-status-mode) | `lattice  main ↑2 ↓1  3 staged  5 unstaged` — plus `BISECTING 3 left, ~2 steps` while bisecting |
| [`magit-commit-mode`](help:magit-commit-mode) | `main  3 files +120 −18` — plus `AMEND` |
| [`magit-revision-mode`](help:magit-revision-mode) | `a1b2c3d  Jane Doe  3 days ago  Fix the thing` |
| [`magit-file-revision-mode`](help:magit-file-revision-mode) | `src/main.rs  @  a1b2c3d`, or `@  index` |
| [`magit-diff-mode`](help:magit-diff-mode) | `staged  src/main.rs` |
| [`magit-log-mode`](help:magit-log-mode) | `HEAD  50 commits  src/main.rs` |
| [`magit-branch-mode`](help:magit-branch-mode) | `main  12 branches` |
| [`magit-remote-mode`](help:magit-remote-mode) | `2 remotes` |
| [`magit-submodule-mode`](help:magit-submodule-mode) | `3 submodules  1 uninitialised` |
| [`magit-refs-mode`](help:magit-refs-mode) | `12 branches  4 remotes  3 tags` |
| [`magit-notes-mode`](help:magit-notes-mode) | `editing note  a1b2c3d  Jane Doe  3 days ago  Fix the thing` |
| [`magit-cherry-mode`](help:magit-cherry-mode) | `HEAD  vs origin/main  3 ahead  1 already upstream` |
| [`magit-stash-mode`](help:magit-stash-mode) | `3 stashes` |
| [`magit-stash-show-mode`](help:magit-stash-show-mode) | `stash@{2}  WIP on main: fix the thing` |
| [`magit-rebase-mode`](help:magit-rebase-mode) | `onto  origin/main  4 commits` — plus `REBASE IN PROGRESS` |

While a refresh is running, the row appends **`refreshing`** after its
own fields — a `gr`, a stage, or anything else that re-reads git. It
disappears when the new content lands, so a slow git call in a large
repository looks busy rather than frozen.

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

## Diffs are syntax-highlighted

Every magit view that shows a diff — the status buffer's inline `=`,
`:magit-diff`, a commit's detail view, a stash, and the staged diff in
the commit-message buffer — highlights the code inside it, with the
diff colouring layered on top:

- the `+` / `-` column keeps its green / red, and the row keeps its
  add / remove background tint;
- everything to the right of that column is coloured by the file's
  language.

The language comes from the path in the diff's own header, so a
multi-file diff highlights each file with its own grammar, and a file
whose type has no grammar simply stays uncoloured.

**Hunks are fragments**, not whole files — a hunk starts mid-function,
so the parser has no enclosing context. Tokens it can resolve
(keywords, strings, comments, numbers) are coloured; tokens that need
the surrounding code are left plain. It errs toward *uncoloured*
rather than *miscoloured*: a hunk that will not parse looks exactly
like a magit diff did before this existed.

Turn it off with `:set magit.hunk.syntax-highlight=off`. It takes
effect on the next refresh (`gr`), not only on reopen.

## Conflicts, and finishing what you started

A merge, rebase, cherry-pick, revert or `git am` that hits a conflict
**stops** and leaves the repository mid-operation. Three things tell
you where you are and get you out.

### 1. The headerline says what you are in

The status buffer announces the stopped operation as an alert:

    MERGING · REBASING · CHERRY-PICKING · REVERTING · APPLYING

It is read from git's own marker files in the gitdir every refresh, so
it stays right even when you run git in a terminal alongside lattice.
It appears whether or not the tree looks dirty — a conflict resolved
into the index but not yet committed leaves the counts looking
ordinary, and that is exactly when you most need telling.

### 2. The unmerged files say how they conflict

Conflicted paths appear in **Unstaged changes** with git's own wording
rather than a flat "unmerged":

| Label | Meaning |
|---|---|
| `both modified` | Changed on both sides — the ordinary conflict |
| `both added` | Created on both sides |
| `both deleted` | Deleted on both sides |
| `added by us` | Only our side created it |
| `added by them` | Only their side created it |
| `deleted by us` | We deleted it, they changed it |
| `deleted by them` | They deleted it, we changed it |

> **"Us" and "them" invert.** In a **merge**, "us" is the branch you
> are on. In a **rebase**, **cherry-pick**, **revert** or **`am`**,
> git replays your work *onto* the other side — so "us" is the
> upstream and "them" is **your own commit**. Read the headerline
> alert first; it tells you which reading applies.

### 3. Resolve, stage, continue

Edit the file to resolve it — or use the
[`diff-conflict-mode`](help:diff-conflict-mode) chords (`d3o` to take
their side, `dB` to keep both) on a diffed conflict region. Then stage
the resolved file with `s`, exactly like any other change.

With everything staged, finish the operation from its menu. The menus
are **state-gated**: while a sequence is stopped they offer only the
ways OUT, because `--continue` / `--skip` / `--abort` error when
nothing is running.

| Operation | Menu | While stopped |
|---|---|---|
| Merge | `C-c g` → merge | continue · abort |
| Cherry-pick | `A` | continue · skip · abort |
| Revert | `_` | continue · skip · abort |
| Rebase | `C-c g` → rebase | continue · skip · abort |
| `git am` | `C-c g` → patches | continue · skip · abort |

A merge has no `skip` — that is a sequencer verb, and a merge is one
operation with nothing to skip to.

Keys are deliberately overloaded between the two states — `A` is
*pick* when idle and *continue* when stopped — which is magit's own
arrangement and is safe only because the gate never shows both sets at
once.

There are also ex-commands for the same operations, e.g.
`:magit-rebase-continue`, if you would rather type than navigate a
menu.

### What is not there yet

- **No conflict gutter.** Conflict regions carry the diff sign map's
  conflict kind, but no marker column distinguishes a conflict from an
  ordinary change at a glance.

## When magit changes files on disk

Plenty of magit actions rewrite your working tree: checking out a
branch, popping a stash, resetting, rebasing, discarding a hunk. Magit
runs `git` as a subprocess, so those are ordinary external writes —
and [`autoread`](help:buffers) picks them up. You do not need to
reload anything by hand.

Two details worth knowing:

- **Open buffers refresh on their own.** No keypress required. A file
  with no unsaved edits reloads silently, cursor and scroll preserved.
- **A file you're not currently in waits until you focus it.** During a
  magit action the *magit* buffer is the one you're in, so a file
  showing in another split keeps its old contents until you switch to
  it. This is vim's checktime-on-`BufEnter` behaviour, not a bug.

If a file has **unsaved edits** when git rewrites it, magit never
clobbers them — autoread opens the diff resolver and you reconcile hunk
by hunk. See [External file changes](help:buffers) for the full policy.

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
`magit.blame.date-format`, `magit.commit.show-diff`) is **not currently
a registered option**. `:set` on any of them fails loudly with `unknown
option` rather than silently accepting and ignoring the value. The
behaviour each name implies is often real (untracked files do show by
default, the log does default to `-50` entries, blame dates are
relative, …) but today it's hardcoded — treat that list as a roadmap
for options that should exist, not ones that do.

**Two are real**, and they are the first magit registers:

| Option | Default | What it does |
|---|---|---|
| `magit.hunk.context-lines` | `3` | Unchanged lines of context around each hunk in every patch magit generates — the status buffer's inline `=`, `:magit-diff`, and a commit's detail view. `D` overrides it for one view. |
| `magit.hunk.syntax-highlight` | `on` | Syntax-highlight the code inside a diff, with the `+` / `-` colouring layered over it. Off gives the flat per-line colouring — every added line one green, every removed line one red — and skips the parse. |
| `ui.diff.line-backgrounds` | `true` | Tint whole rows by what the diff did to them. `false` leaves foreground colouring only, for themes where a full-row wash fights the syntax colours underneath. |

`ui.diff.line-backgrounds` is **not** under `magit.*` on purpose: the
mechanism is shared by every diff-showing buffer in the editor, not
just magit's, and naming it for one consumer would understate what it
turns off. It sits beside `ui.diff.context` and `ui.diff.fold-unchanged`.

Note `magit.hunk.context-lines` and `ui.diff.context` are different
things: the first decides how much context git puts *into* a patch, the
second how much an unchanged-region fold leaves visible inside a
two-pane diff session.

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
  [`magit-remote-mode`](help:magit-remote-mode),
  [`magit-submodule-mode`](help:magit-submodule-mode),
  [`magit-refs-mode`](help:magit-refs-mode),
  [`magit-notes-mode`](help:magit-notes-mode),
  [`magit-cherry-mode`](help:magit-cherry-mode),
  [`magit-rebase-mode`](help:magit-rebase-mode)
- `:help magit-core-mode` — the [shared chords](help:magit-core-mode)
  every magit buffer inherits
- `:help magit-global-mode` — the [entry chords](help:magit-global-mode)
  (`C-x g`, `C-c g`, `C-c f`)
- `:magit-<Tab>` — list all magit ex-commands
- `:describe-key` then press any magit chord to see its bound action
- `<C-h> m` in any magit buffer — the live mode stack *and* the chords
  each mode contributes. This is the view worth reaching for in magit:
  a status buffer's chords come from `magit-status-mode` **and**
  `magit-core-mode`, and only a major+minor view shows both.
- `<C-h> K` — every chord that fires in the buffer you're in
- `:describe-mode magit-status-mode` — one named mode's metadata

> Earlier drafts of this page promised `g?` as a future Lattice-wide
> "help for this buffer's major mode" chord. That shipped as
> `<C-h> m` instead — the emacs help-prefix slot, which costs no vim
> key (vim's `g?` is the rot13 operator) — and it shows major *plus*
> minors rather than the major alone.
