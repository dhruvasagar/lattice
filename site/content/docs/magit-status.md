+++
title = "magit-status"
+++



The `*magit:status*` buffer is the primary workhorse — a section-
collapsible view of your repository's current state. It shows every
changed file organised into sections, lets you stage and unstage at
hunk or file granularity, commit, amend, open diffs on demand, and
navigate between sections, files, and hunks.

Open it with **`C-x g`** or **`:magit-status`** from any buffer.

> **Status:** status buffer rendering, all five sections, lazy inline
> diffs via `=`, hunk and file staging (`s`/`u`/`x`), commit (`cc`/`ca`),
> context-aware visit (`<CR>`), manual refresh (`gr`), and close (`q`)
> are shipped. Section folding is handled by the standard fold engine
> (`TAB` / `za` / `zM` / `zR`). Headerline showing branch name +
> ahead/behind is active.

---

## Quick reference

| Chord | Action |
|---|---|
| `s` | Stage hunk (if diff is expanded) or entire file at cursor |
| `u` | Unstage hunk or file at cursor |
| `x` | Discard hunk or file at cursor |
| `=` | Toggle inline diff for the file at cursor |
| `cc` | Open the [commit buffer](../magit-buffers/#commit-buffer) |
| `ca` | Amend the previous commit |
| `p` | Stage hunk interactively (`git add -p`) |
| `<CR>` | Context-aware open/visit at cursor (open file, show commit, checkout branch, …) |
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
its staged diff inline.

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
demand via `=`.

### Untracked files

Files not tracked by git. Shown by default; hide this section with
`:set magit.status.show-untracked=false`.

### Stashes

The stash list (most recent first). Shown by default; hide with
`:set magit.status.show-stashes=false`. `<CR>` on a stash entry shows
its diff.

### Recent commits

Last N commits (default 20) with abbreviated SHAs and subjects.
`<CR>` on a commit opens `*magit:commit:<sha>*`.

---

## Staging and unstaging

### File-level

When the cursor is on a **file header** (the `  modified   path` line):

| Chord | Action |
|---|---|
| `s` | Stage the entire file |
| `u` | Unstage the entire file |
| `x` | Discard all working-tree changes to the file |

### Hunk-level (after expanding a diff)

Press `=` on a file to expand its inline diff. When the cursor is inside
the expanded diff:

| Chord | Action |
|---|---|
| `s` | Stage only the hunk under the cursor |
| `u` | Unstage only the hunk under the cursor |
| `x` | Discard only the hunk under the cursor |

Hunk-level operations use `git add -p` / `git reset -p` semantics.
Before applying, the status buffer re-reads the file's current hunks
and checks that the cursor's hunk boundaries still match — if the file
has been edited since the diff was loaded, the operation is rejected
with "file changed — refresh and retry."

### Staged + unstaged simultaneously

A file with changes in both the index AND the working tree shows as
`modified` in both sections. Hunk operations in the staged section
target the index; operations in the unstaged section target the working
tree. This matches git's two-staging-area model exactly.

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

Press `cc` to open the [commit buffer](../magit-buffers/#commit-buffer)
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
| Commit line | Open `*magit:commit:<sha>*` showing the full diff |
| Hunk (inside expanded diff) | Open the file with cursor at the hunk location |
| Stash entry | Open stash detail diff |
| Branch name | Check out the branch |

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
- **Detached HEAD:** the headerline shows `(HEAD detached at <sha>)`.
- **Bare repository:** write operations (stage, commit, branch) are
  rejected with a user-visible message.
- **Empty repository (no commits yet):** the staged / unstaged sections
  function normally; the recent-commits section is empty.
- **Binary files:** shown in the file list but diffs are not expandable
  (no text diff to display).
- **Merge conflicts:** files in the `Unmerged` state appear in both
  staged and unstaged sections with a `conflicted` label.
