---
summary: "magit-rebase-mode: the interactive rebase todo — an editable pick list built from real history, C-c C-c to run it, C-c C-k to abort (asks first)."
related: [magit, magit-rebase, ex:magit-rebase]
---

# magit-rebase-mode

The interactive-rebase todo list, as an editable buffer.
`:magit-rebase` uses your branch's configured upstream;
`:magit-rebase <ref>` rebases onto a specific ref.

The todo is built from your real history —
`git log --reverse --format="pick %h %s" <upstream>..HEAD` — one line
per commit, oldest first. The headerline names the upstream and the
commit count, and adds `REBASE IN PROGRESS` when one is already
running, which is exactly when you need to know before pressing
anything.

If no upstream can be resolved (none configured, no ref given), the
buffer explains why instead of showing a list, and `C-c C-c` refuses to
run.

## Chords

| Chord | Action |
|---|---|
| `C-c C-c` | Start and run the rebase |
| `C-c C-k` | Abort the rebase — **asks first**, when one is in progress |
| `<CR>` | Show the commit for the todo line at cursor |

`q` and navigation come from
[`magit-core-mode`](help:magit-core-mode).

## Editing the todo

It's a real buffer, so edit it with ordinary vim commands:

| Edit | Effect |
|---|---|
| Change `pick` → `reword` | Keep the commit, change its message |
| Change `pick` → `squash` | Fold into the previous commit, keeping both messages |
| Change `pick` → `fixup` | Fold into the previous commit, discarding this message |
| Change `pick` → `edit` | Stop at this commit so you can amend it |
| Change `pick` → `drop` | Discard the commit |
| Reorder lines | Reorder the commits |
| Delete a line | Same as `drop` |

Verbs are coloured, so a todo you've edited reads at a glance.

## Behaviour worth knowing

- **`reword` does not prompt for a message.** The commit keeps its
  original one. There's no message-editing UI wired up for this yet —
  a real limitation, not a silent failure.
- **`C-c C-c` closes the buffer as soon as the rebase is kicked off,**
  not when it finishes. A failure is logged rather than reported back
  into a buffer that's already gone.
- **`C-c C-k` is safe to press early.** It only runs
  `git rebase --abort` when a rebase is genuinely in progress (checked
  via `.git/rebase-merge` / `.git/rebase-apply`), so it can't fail
  against one that never started — and when there *is* one, it asks
  first, because aborting discards everything replayed so far.

## See also

- [`magit-branch-mode`](help:magit-branch-mode) — where the upstream
  you're rebasing onto usually comes from.
- [`magit-revision-mode`](help:magit-revision-mode) — `<CR>`'s target,
  for checking what a todo line actually contains.
