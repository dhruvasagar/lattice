---
summary: "magit-status: the primary magit workhorse — staged / unstaged / untracked sections, stashes, recent commits, lazy inline diffs (via = for files, <CR> for stashes), a dedicated per-file diff buffer (d), file-level staging (s/u/x — no hunk-level staging), commit (cc/ca), and context-aware visit (<CR>, opens the commit buffer for a commit entry)."
related: [magit, magit-transient, ex:magit-status]
---

# magit-status-mode

The `*magit:status*` buffer is the primary workhorse — a section-
collapsible view of your repository's current state. It shows every
changed file organised into sections, lets you stage and unstage at
file granularity (there is no hunk-level staging), commit, amend, open
diffs on demand, and navigate between sections, files, and hunks.

Open it with **`C-x g`** or **`:magit-status`** from any buffer.

> **Status:** status buffer rendering, all five sections, lazy inline
> diffs via `=` (files) and `<CR>` (stashes), a dedicated per-file diff
> buffer via `d` (opens against the file's own section baseline —
> `--cached` for Staged, working-tree-vs-index for Unstaged — useful
> for diffs too large to read comfortably inline), file-level staging
> (`s`/`u`/`x` — there is no hunk-level staging anywhere in this
> buffer), commit (`cc`/`ca`), context-aware visit (`<CR>`, opens the
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
| `s` | Stage the file at cursor |
| `u` | Unstage the file at cursor |
| `x` | Discard the file at cursor (asks for confirmation first) |
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

---

## Staging and unstaging

### File-level

When the cursor is on a **file header** (the `  modified   path` line):

| Chord | Action |
|---|---|
| `s` | Stage the entire file |
| `u` | Unstage the entire file |
| `x` | Discard all working-tree changes to the file — asks for confirmation first (`Discard changes to <path>?`, `y`/`n`) before running `git checkout --` |

### There is no hunk-level staging

`s` / `u` / `x` are always **file-level**, even with the cursor
positioned inside an expanded (`=`-toggled) diff — the status buffer
has no concept of staging or discarding an individual hunk. Pressing
`s`/`u`/`x` while the cursor is on a diff content line does nothing
(there's no file entry under the cursor to act on); move the cursor
back onto the file's header line first.

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

After committing, the status buffer auto-refreshes — the staged section
clears and the recent-commits section updates.

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
rebuild the buffer content. This is the fast path — only file statuses
are re-fetched (no diffs). Cached diffs for files whose status hasn't
changed are preserved.

### Auto-refresh

When `magit.auto-refresh` is `true` (default), the status buffer
automatically refreshes in response to `RepositoryEvent` — index
changes, HEAD changes, ref changes, and filesystem events detected by
the `RepositoryWatcher`. The auto-refresh runs the same fast path as
`gr` (status only, no diffs).

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
