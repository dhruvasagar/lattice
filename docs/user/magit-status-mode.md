---
summary: "magit-status: the primary magit workhorse — staged / unstaged / untracked sections, stashes, recent commits, lazy inline diffs (via = for files, <CR> for stashes), a dedicated per-file diff buffer (d), line-, hunk- and file-level staging (s/u/x), commit (cc/ca), and context-aware visit (<CR>, opens the commit buffer for a commit entry)."
related: [magit, magit-transient, ex:magit-status]
---

# magit-status-mode

The `*magit:status*` buffer is the primary workhorse — a section-
collapsible view of your repository's current state. It shows every
changed file organised into sections, lets you stage and unstage a
whole file, a single hunk, or just the lines you select, commit, amend,
open diffs on demand, and navigate between sections, files, and hunks.

Open it with **`C-x g`** or **`:magit-status`** from any buffer.

> **Status:** status buffer rendering, all five sections, lazy inline
> diffs via `=` (files) and `<CR>` (stashes), a dedicated per-file diff
> buffer via `d` (opens against the file's own section baseline —
> `--cached` for Staged, working-tree-vs-index for Unstaged — useful
> for diffs too large to read comfortably inline), staging
> (`s`/`u`/`x` — on the lines you select, else the hunk under the
> cursor, else the whole file), commit (`cc`/`ca`), context-aware
> visit (`<CR>`, opens the
> dedicated [commit buffer](help:magit-commit-mode) for a
> commit entry), manual refresh (`gr`), and close (`q`) are shipped.
> `TAB` genuinely folds:
> closing a file's fold hides its inline diff, and each `@@` hunk
> inside an expanded diff is independently foldable and nested inside
> the file's fold.
>
> **Headerline (MG.14).** A sticky row above the first line shows the
> repository name, the checked-out branch with upstream tracking, and
> the dirty counts — ` lattice  main ↑2 ↓1  3 staged  5 unstaged `, or
> ` lattice  main  clean ` when there is nothing to commit. It refreshes
> with the buffer (`gr` and every staging action), and every other magit
> buffer carries the equivalent row for its own view. (An earlier
> revision of this page claimed this headerline was already active when
> it was not; MG.14 made the claim true.)

---

## Quick reference

| Chord | Action |
|---|---|
| `s` | Stage the hunk or file at cursor |
| `u` | Unstage the hunk or file at cursor |
| `x` | Discard the hunk or file at cursor (asks for confirmation first) |
| `=` | Toggle inline diff for the file at cursor |
| `d` | Open the file at cursor's diff in a dedicated buffer (against the section's baseline) |
| `cc` | Open the [commit buffer](help:magit-commit-mode) |
| `ca` | Amend the previous commit |
| `p` | Disabled — shows an error (see [Staging and unstaging](#staging-and-unstaging)) |
| `<CR>` | Context-aware open/visit at cursor (open file, toggle a stash's inline patch, open the commit buffer for a commit entry) |
| `gr` | Manual refresh (re-runs git status) |
| `q` | Close the buffer (bury, return to previous) |
| `]]` / `[[` | Next / previous top-level section |
| `]f` / `[f` | Next / previous file or entry within the current section |
| `]c` / `[c` | Next / previous hunk (within expanded diffs) |
| `s` / `u` / `x` in Visual | Act on the selected lines only |
| `A` / `_` | Cherry-pick / revert the commit at cursor |
| `Os` / `Om` / `Oh` | Reset `--soft` / `--mixed` / `--hard` to the commit at cursor |
| `TAB` | Toggle section or hunk fold at cursor |
| `S-TAB` | Cycle section visibility |

---

## Sections

The status buffer is organised into five collapsible sections (top to
bottom):

### Staged changes

Files with changes in the index — what will go into the next commit.
Listed with their status labels (`modified`, `new file`, `deleted`).
Diffs are **not** pre-computed — press **`=`** on a file entry to load
its staged diff inline, or **`d`** to open it in a dedicated buffer
(`git diff --cached`) instead — better for a diff too large to read
comfortably inline, since it doesn't inflate the status buffer's line
count or its inline-highlight bookkeeping.

```
Staged changes (3)
  modified    src/main.rs
  new file    src/auth.rs
  deleted     src/old.rs
```

After pressing `=` on `src/main.rs`:

```
Staged changes (3)
  modified    src/main.rs
  ─────────────────────────
  + fn authenticate() {
  +     // new auth module
  + }
  ─────────────────────────
  new file    src/auth.rs
  deleted     src/old.rs
```

The diff is inserted as a **local edit** to the buffer — other sections
and files are untouched. The diff content is styled text with virtual
rows for deletion blocks; hunk boundaries are foldable. Press `=` again
to collapse the diff.

### Unstaged changes

Files with working-tree modifications not yet staged. Same format and
behaviour as staged — file list with status labels, diffs loaded on
demand via `=` or `d`. `d` here opens `git diff` (working tree vs
index) — the Unstaged section's own baseline, distinct from Staged's
`--cached`.

### Untracked files

Files not tracked by git. Shown by default; hide this section with
`:set magit.status.show-untracked=false`.

### Stashes

The stash list (most recent first). Shown by default; hide with
`:set magit.status.show-stashes=false`. `<CR>` toggles the stash's
patch inline, at the cursor — the same mechanism `=` uses for files.

### Recent commits

Last N commits (default 20) with abbreviated SHAs and subjects.
`<CR>` opens the dedicated [commit buffer](help:magit-commit-mode)
for the commit at cursor — the same target `:magit-log`'s own `<CR>`
opens, and every other magit view that shows a per-row SHA (log,
blame, rebase). This used to toggle the commit's patch inline (the
same mechanism `=` uses for files) — changed for consistency with
those other views, which all treat `<CR>` on a SHA as "go to the
commit", not "preview inline".

The commit operations from
[`magit-core-mode`](help:magit-core-mode) act on the row under the
cursor here: `A` cherry-pick, `_` revert, `Os` / `Om` / `Oh` reset
`--soft` / `--mixed` / `--hard`. On a row that isn't a commit — a file
entry, a stash — they **ask which commit**, opening a picker of recent
commits rather than acting on a neighbour.

---

## Staging and unstaging

### File-level

When the cursor is on a **file header** (the `  modified   path` line):

| Chord | Action |
|---|---|
| `s` | Stage the entire file |
| `u` | Unstage the entire file |
| `x` | Discard all working-tree changes to the file — asks for confirmation first (`Discard changes to <path>?`, `y`/`n`) before running `git checkout --` |

### Hunk-level

Expand a file's diff with `=`, put the cursor anywhere inside one hunk
— a `+`, `-`, or context line, or the `@@` header itself — and the same
three chords act on that hunk alone:

| Chord | Action |
|---|---|
| `s` | Stage this hunk, leaving the file's other hunks unstaged |
| `u` | Unstage this hunk (cursor in a **Staged** entry's diff) |
| `x` | Discard this hunk from the working tree — asks first (`Discard hunk at <path>:<line>?`) |

`]c` / `[c` jump between hunks, and staging uses the same hunk
boundaries they do, so `]c` then `s` always stages the hunk you just
landed on.

Each chord acts on the side it belongs to. `s` and `x` want an
**Unstaged** hunk, `u` wants a **Staged** one; press the wrong one and
magit says so ("that hunk isn't staged") rather than running a git
command that would fail — or, for `x` on a staged hunk, one that would
*succeed* and quietly remove the change from your file while leaving it
staged for the next commit. Unstage it with `u` first, then discard.

Hunks inside a commit's or stash's expanded patch can't be staged —
they belong to neither the index nor the working tree. `s` there
reports that hunk staging isn't available in this view; move to the
file header if you meant the whole file.

If the working tree has moved under the buffer since it was drawn, git
refuses the patch outright rather than applying it somewhere
plausible-looking. The failure is reported in `*messages*` and the view
refreshes; press `gr` and try again.

**The view keeps your place.** After staging a hunk the file's diff is
still open and the cursor is on the hunk that took the staged one's
place — so staging four of a file's six hunks is four keypresses, not
four keypresses and four searches. Staging the last one leaves you on
the new last hunk; staging a file's only remaining hunk moves it to
another section, and the cursor stays where the refresh put it rather
than jumping somewhere arbitrary.

### A selection of lines

Select the lines inside a hunk — `V` for linewise, extended with
`j` / `k` — then press the same chord:

| Chord | Action |
|---|---|
| `s` | Stage only the selected lines |
| `u` | Unstage only the selected lines |
| `x` | Discard only the selected lines — asks first, naming the count |

This is the finest granularity magit offers, and the usual way to split
one edit across two commits. The echo names what moved (`magit: staged
3 lines of src/main.rs:42`), and Visual mode ends, as it does after any
operator on a selection.

Two things worth knowing:

- **One hunk at a time.** The selection is intersected with the hunk
  your cursor is in; lines outside it are ignored. The echo's count is
  what actually moved, so a selection drawn across two hunks reads as
  the smaller number.
- **A selection with no `+`/`-` line in it does nothing**, and says so —
  selecting only context lines is not a change to move.

Because of how git formats a diff, a modified line appears as a removal
*and* an addition, usually with all the removals grouped above all the
additions. Selecting one line's removal without its addition stages the
deletion alone, which is valid and occasionally what you want; to move a
whole modification, select both rows.

`p` (interactively stage via `git add -p`) is disabled outright: it
shows `magit: interactive git add -p isn't supported yet` rather than
attempting anything. `git add -p` is fundamentally interactive — it
reads its own prompts from stdin — and there's no terminal-suspend
mechanism yet to hand it a real TTY. Stage the whole file with `s`, or
expand the diff with `=` to review before staging.

### Staged + unstaged simultaneously

A file with changes in both the index AND the working tree shows as
`modified` in both sections, as two independent rows. `s`/`u`/`x` on
the staged row target the index; on the unstaged row they target the
working tree. This matches git's two-staging-area model, at the
file level.

---

## Navigation model — three levels

The status buffer has a three-level hierarchy, each with its own chord
family:

| Level | What you navigate | Chords |
|---|---|---|
| **Sections** | Top-level headers (Staged, Unstaged, Untracked, Stashes, Recent commits) | `]]` / `[[` |
| **Files / entries** | File headers within the current section, stash entries, commit lines | `]f` / `[f` |
| **Hunks** | Individual diff hunks within an expanded file diff | `]c` / `[c` |

Hunk navigation only works for files whose diff is currently expanded
(via `=`). If the cursor is on a file header (diff not expanded), `]c`
/ `[c` falls back to file-level navigation (same as `]f` / `[f`).

---

## Committing

### New commit (`cc`)

Press `cc` to open the [commit buffer](help:magit-commit-mode)
(`*magit:commit*`). The commit buffer shows the staged diff as a read-
only preview and provides an editable message region. `C-c C-c` creates
the commit; `C-c C-k` aborts.

After committing, the buffer refreshes as part of the commit action —
the staged section clears and the recent-commits section updates.

### Amend (`ca`)

Press `ca` to amend the previous commit. The commit buffer opens with
the previous message pre-populated. `C-c C-c` amends; `C-c C-k` aborts.

---

## Context-aware visit (`<CR>`)

`<CR>` is a general "visit / drill-into" action. Its behaviour depends
on what's under the cursor:

| Cursor on | `<CR>` action |
|---|---|
| File entry (staged / unstaged) | Open the file for editing (working-tree version) |
| Untracked file | Open the file for editing |
| Commit line | Open the dedicated [commit buffer](help:magit-commit-mode) for that commit |
| Stash entry | Toggle the stash's patch inline (same mechanism as `=`) |
| A diff content line (inside an expanded entry) | Nothing — `<CR>` only acts on a classified file/stash/commit line, not on the diff text itself |

There are no branch entries in the status buffer, so there's no
"check out a branch" case here — that's `magit-branch`'s `<CR>`.

---

## Section visibility cycling (`S-TAB`)

`S-TAB` cycles through four visibility states:

1. **All sections expanded** (default on open)
2. **Only changed sections visible** (Staged, Unstaged) — untracked / stashes / commits collapsed
3. **Only section headers visible** — file-list content hidden
4. **All collapsed** (only headers, no body content)

`TAB` toggles the fold at the cursor (innermost first: hunk → file → section).

---

## Refreshing

### Manual refresh (`gr`)

Press `gr` to re-run `git status`, `git stash list`, and `git log` and
rebuild the buffer content.

**Expanded diffs stay expanded.** A refresh rebuilds the buffer, and it
rebuilds the diffs you had open along with it — re-running `git diff`
for those entries only, so a refresh with nothing open costs nothing
extra. An entry that has since left its section simply doesn't come
back. Every staging action refreshes too, which is why staging one hunk
leaves you looking at the rest of the file's diff rather than at a
collapsed entry.

### Auto-refresh — not built

Earlier revisions of this page described an automatic refresh driven by
a repository watcher, controlled by `magit.auto-refresh`. **None of it
exists** — there is no such option, no repository watcher, and no
filesystem-event path into the status buffer. `:set magit.auto-refresh`
fails with `unknown option`, as the Options section below says of every
`magit.*` name.

What does refresh the buffer: `gr`, and every staging action, which
refreshes as part of its own completion.

---

## Edge cases

- **Not a git repository:** the buffer shows `Not a git repository.`
- **Detached HEAD:** the headerline shows `HEAD` as the branch (what
  `git rev-parse --abbrev-ref HEAD` reports), with no ahead/behind.
- **Bare repository:** write operations (stage, commit, branch) are
  rejected with a user-visible message.
- **Empty repository (no commits yet):** the staged / unstaged sections
  function normally; the recent-commits section is empty.
- **Binary files:** shown in the file list but diffs are not expandable
  (no text diff to display).
- **Merge conflicts:** files in the `Unmerged` state appear in both
  staged and unstaged sections with a `conflicted` label.

## See also

- [`magit-diff-mode`](help:magit-diff-mode) — how the inline `=` diffs
  are coloured, and the theme elements that control it.
- [`magit-core-mode`](help:magit-core-mode) — the shared navigation,
  refresh and fold chords.
