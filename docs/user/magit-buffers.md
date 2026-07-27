---
summary: "magit-buffers: commit, diff, log, blame, stash, branch, and rebase buffers — each is a dedicated major-mode Document with its own keymap, synthetic-buffer provisioning, and action handlers."
related: [magit, magit-status, magit-transient, ex:magit-commit, ex:magit-diff, ex:magit-log, ex:magit-blame, ex:magit-stash-list, ex:magit-branch, ex:magit-rebase]
---

# Magit buffers

Every magit view beyond the status buffer is its own dedicated major-
mode Document. Each buffer has its own keymap, its own ex-command entry
point, and its own action handlers — all living inside the
`lattice-magit` crate. Every buffer inherits the [shared magit-core
navigation chords](magit.md#shared-navigation-magit-core).

---

## Commit buffer (`*magit:commit*`)

Open with `cc` from the status buffer, or `:magit-commit` from
anywhere.

```
┌─────────────────────────────────────────────┐
│ [read-only] diff of staged changes          │
│ (scrollable, background-tinted)             │
│ ─────────── message area ────────────────   │
│ │ Add user authentication endpoint          │  ← subject
│ │                                           │
│ │ Implements OAuth2 flow with...            │  ← body
│ ──────────────────────────────────────────  │
│ C-c C-c commit   C-c C-k abort             │
└─────────────────────────────────────────────┘
```

### Chords

| Chord | Action |
|---|---|
| `C-c C-c` | Create the commit with the entered message |
| `C-c C-k` | Abort the commit (close buffer without committing) |
| `<CR>` | Visit the file at cursor (diff region only) — opens it AS STAGED (`*magit:file:staged:<path>*`, read-only), not the live working-tree copy |

### Behaviour

- **Diff region** (top, read-only): populated by `git diff --cached` at
  buffer-open time. This is the one exception to the lazy-loading rule
  — you opened this buffer specifically to review what you're about to
  commit. The diff is rendered as styled text with syntax highlighting.
  `<CR>` on a file line there opens the file's STAGED content (the
  index blob, via `git show :<path>`), not the working-tree file — the
  diff you're reviewing describes what's staged, which may already
  differ from a since-edited working copy.
- **Message region** (bottom, editable): standard text content. The
  first line is the subject; a blank line separates it from the body.
  This is a real editable buffer with full vim grammar — you can use
  normal-mode editing, visual selections, and registers.
- **Amend:** when opened via `ca` from the status buffer, the previous
  commit message is pre-populated. `C-c C-c` amends; the commit count
  stays unchanged.
- **Empty subject validation:** if the subject line is empty when you
  press `C-c C-c`, the commit is rejected with an error message instead
  of silently doing nothing.
- **Post-commit:** the buffer closes as soon as the commit is kicked
  off (not after `git` confirms it landed) and the status buffer
  auto-refreshes. A failure is logged rather than reported back into
  the (already closed) buffer.

---

## Commit detail buffer (`*magit:commit:<sha>*`)

Not the compose buffer above — this is a HISTORICAL commit's full
detail, opened by `<CR>` on a SHA anywhere one appears (log, blame,
rebase, magit-status's Recent commits section). Mode: `magit-revision-
mode`. Content is `git show --stat -p <sha>`: commit metadata, message,
a file-change summary, then the full diff — read-only.

### Chords

| Chord | Action |
|---|---|
| `<CR>` | Visit the file at cursor (`--stat` summary row or a `diff --git` header) AS OF THIS COMMIT — opens `*magit:file:<sha>:<path>*`, read-only |

### Behaviour

- `gr` is a harmless no-op here — a fixed sha's content never changes,
  so there's nothing to refresh.
- `<CR>`'s file target is always historical, never the working tree:
  the commit you're looking at is fixed, so showing anything other
  than "the file exactly as it was in this commit" would be
  misleading, even if the working tree happens to currently match it.

---

## File-at-revision buffer (`*magit:file:<ref>:<path>*`)

Read-only content of one file at one fixed reference point. Never
opened directly — it's the landing target of `<CR>` from the Commit
detail buffer above, from the Staged region of the compose Commit
buffer, from a Staged-scoped Diff buffer, and from magit-status's
Staged section. Mode: `magit-file-revision-mode`.

`<ref>` is either a real commit-ish (the sha shown in the buffer name)
or the literal token `staged`, meaning "the index's blob for this
path" (`git show :<path>`) rather than any commit.

### Behaviour

- No mode-specific chords beyond the shared
  [magit-core](magit.md#shared-navigation-magit-core) navigation —
  this is a plain read-only view, not an interactive one.
- No syntax highlighting for the file's own language yet — synthetic
  buffers have no filename-based language detection wired up. A known
  limitation, not a silent failure.
- `gr` is a no-op — a fixed ref's blob never changes.

---

## Diff buffer (`*magit:diff*`)

Open with `:magit-diff` to get a read-only view of `git diff HEAD` —
staged and unstaged changes combined, against HEAD. This used to open
a permanently-empty buffer; it now shows real content, but it is a
single-pane text view, not the side-by-side layout the design
eventually calls for.

### Chords

| Chord | Action |
|---|---|
| `s` | Stage the file at cursor |
| `u` | Unstage the file at cursor |
| `<CR>` | Visit the file at cursor — the index blob (read-only) if this buffer is Staged-scoped, otherwise the live working-tree file |
| `gr` | Refresh (re-run the underlying `git diff`) |

`]]`/`[[`/`]f`/`[f`/`]c`/`[c`/`TAB`/`q` come from the shared
[magit-core](magit.md#shared-navigation-magit-core) minor mode, same as
every other magit buffer.

### Behaviour

- Populated once, on open, by `git diff HEAD`. `gr` re-runs it.
- `s`/`u` are **file-level only** — same caveat as magit-status: there
  is no hunk-level staging here either. They resolve the file from the
  nearest `diff --git a/<path> b/<path>` header above the cursor, so
  they work from anywhere inside that file's diff, not just its header
  line.
- **Not yet implemented**: no side-by-side pane layout, no
  `diff-mode`-style `do`/`dp` hunk transfer, no visual-mode
  partial-hunk staging. If you're looking for hunk-level control, it
  doesn't exist anywhere in magit today — file-level is as granular as
  it gets.
- **File-scoped variants**: pressing `d` in the [file dispatch
  transient](magit-transient.md) (`C-c f`) opens `*magit:diff:<path>*`
  — the same mode, populated with a diff scoped to just that one file
  (against HEAD) instead of the whole repo. Pressing `d` on a file in
  magit-status's Staged/Unstaged sections opens a further-scoped
  variant with a baseline matching that section:
  `*magit:diff:staged:<path>*` (`git diff --cached`) or
  `*magit:diff:unstaged:<path>*` (`git diff`) — see
  [magit-status.md](magit-status.md). The buffer's `<CR>` behaviour
  (index blob vs. working file) depends on which of these three
  scopes it was opened with.

---

## Log buffer (`*magit:log*`)

Open with `:magit-log` to browse the commit history. Content is
generated by `git log --oneline --graph --decorate -50` (default count
configurable via `magit.log.count`).

### Chords

| Chord | Action |
|---|---|
| `<CR>` | Show commit detail for the commit at cursor (opens `*magit:commit:<sha>*`) |
| `gr` | Refresh (re-run `git log`) |

Every other magit view that shows a per-row commit SHA (log, magit-
status's Recent commits section, blame, rebase) treats `<CR>` the same
way: it navigates to this buffer, never toggles anything inline.

### Behaviour

- Renders the git log graph with branch/tag decorations.
- Commit SHAs are abbreviated; refs are color-coded; subjects are plain
  text.
- Log arguments (count, `--all`, `--graph`, path filter) aren't
  interactively configurable yet — there's no Log submenu inside the
  `C-c g` dispatch transient (it's a flat menu; `l` just opens this
  buffer). The `magit.log.count`/`magit.log.graph`/`magit.log.decorate`
  options are the only current lever; see [magit.md](magit.md#options).

---

## Blame buffer (`*magit:blame:<path>*`)

Open with `:magit-blame` (or `:magit-blame <path>`) to see per-line git
blame annotations alongside a file.

### Chords

| Chord | Action |
|---|---|
| `<CR>` | Show the commit for the blamed line |
| `p` | Re-blame at the parent commit (walk history backwards) |

### Behaviour

- Data loaded from `git blame --line-porcelain` on `spawn_blocking`.
- Annotations per line: abbreviated SHA (8 chars, colored by author),
  author name (truncated to N chars, configurable via
  `magit.blame.author-width`), relative date (format configurable via
  `magit.blame.date-format`).
- Blame data is cached per-file and invalidated when the file changes.
- Re-blame (`p`) re-runs blame against the parent of the commit at the
  cursor line — you can walk history backwards commit by commit.

---

## Stash buffer (`*magit:stash*`)

Open with `:magit-stash-list`. Lists all stash entries.

### Chords

| Chord | Action |
|---|---|
| `a` | Apply the stash at cursor (keep it in the list) |
| `p` | Pop the stash at cursor (apply + drop) |
| `d` | Drop the stash at cursor without applying |
| `z` | Create a new stash from the current working tree (`git stash push`, no untracked files) |
| `gr` | Refresh (re-run `git stash list`) |

There is no `<CR>` binding in this buffer today — it doesn't show a
stash's diff. (If you want to preview a stash's patch before deciding,
open `*magit:status*` instead: `<CR>` on a stash entry there toggles
its patch inline.)

### Behaviour

- `z` always runs plain `git stash push` — there's no flag to include
  untracked files or attach a message yet.
- Stash apply/restore uses `git stash apply` / `git stash pop`. `d`
  drops without confirmation.

---

## Branch buffer (`*magit:branch*`)

Open with `:magit-branch`. Lists all local branches.

### Chords

| Chord | Action |
|---|---|
| `<CR>` | Check out the branch at cursor |
| `c` | Create a branch — opens a two-step wizard (see below) |
| `d` | Delete the branch at cursor |
| `m` | Merge the branch at cursor into the current branch |
| `gr` | Refresh (re-list branches) |

### Behaviour

- `d` uses force-delete (`-D`). Unmerged branches are deleted without
  confirmation — the `d` chord fires immediately, no confirmation
  dialog, no destructive-action warning glyph. Be sure before pressing it.
- `c` opens a real, Emacs-magit-style two-step wizard: first a picker
  listing your existing local branches — pick one as the **base**;
  submitting opens a prompt (`New branch name (from <base>):`) — type
  the new branch's name and press Enter, and the branch is created
  from that base and checked out (`git checkout -b <name> <base>`).
  `<Esc>` at either step cancels. This is the first place in the
  editor to use a genuinely new interaction shape — an Emacs
  `read-string`-style follow-up prompt chained after a picker accept
  — rather than the ex-command-line arg-prompt mechanism described in
  [ex-commands.md](ex-commands.md#arg-prompts). The direct
  `:magit-branch-create <name>` ex-command (creates from HEAD, no base
  choice, no prompt) still works exactly as before for scripting or a
  quick one-shot create — the wizard is the new interactive path, not
  a replacement for it.

---

## Rebase buffer (`*magit:rebase*`)

Open with `:magit-rebase` (optionally `:magit-rebase <upstream-ref>`)
to start an interactive rebase. The buffer is an editable todo list
built from your real commit history against the resolved upstream (the
branch's configured upstream, or the ref you gave). This used to be
entirely fake — a hardcoded sample todo that always failed silently on
confirm, because it tried to continue a rebase that had never actually
started. It's now real.

If no upstream can be resolved (none configured and no ref given), the
buffer explains why instead of showing a todo list, and `C-c C-c`
refuses to run.

### Chords

| Chord | Action |
|---|---|
| `C-c C-c` | Start and run the rebase |
| `C-c C-k` | Abort the rebase |
| `<CR>` | Show commit detail for the todo line at cursor (opens `*magit:commit:<sha>*`) |

### Behaviour

- The buffer is an editable `pick <sha> <subject>` list, one line per
  commit, oldest first. Edit it using normal vim editing commands —
  change `pick` to `reword`/`squash`/`fixup`/`drop`, reorder lines by
  cutting and pasting, delete a line to drop that commit.
- **Known limitation:** picking `reword` does not let you actually type
  a new message — the commit keeps its original message unchanged.
  There's no message-editing UI wired up for this yet; it's a real
  limitation, not a silent failure.
- `C-c C-c` writes your (possibly edited) todo back to git and starts
  the rebase. The buffer closes as soon as the rebase is kicked off, not
  after it finishes — a failure is logged rather than reported back
  into the buffer, since the buffer is already gone by then.
- `C-c C-k` is safe to press even before you've confirmed anything: it
  only runs `git rebase --abort` if a rebase is actually in progress
  (checked via `.git/rebase-merge` / `.git/rebase-apply`), so it can't
  fail against a rebase that was never started.
