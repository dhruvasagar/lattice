---
summary: "Diff & merge: :diffthis / :diffsplit, hunk navigation (]c / [c), do / dp to get/put hunks, sign column, two- and three-way."
related: [diff, ex:diff]
---

# diff-mode

Lattice diffs two (or three) buffers side by side, marks the changed
lines in the sign column, and lets you move and transfer hunks with
vim's familiar `vimdiff` vocabulary (`]c` / `[c`, `do` / `dp`).

> **Status:** two-way diff (`:diffthis` pairing, `:diffsplit
> <file>`, hunk navigation, `do` / `dp`, the sign column) is the
> production path. Three-way merge (`:diffsplit <local> <remote>`,
> conflict rendering, `:diff-accept` / `:diff-reject`) is wired
> (D.6) and newer — treat it as usable-but-fresh.

---

## Quick reference

| Command / keystroke         | Meaning                                                                 |
|-----------------------------|-------------------------------------------------------------------------|
| `:diff`                     | Diff the active document against its **on-disk** content (inline session) |
| `:diffthis`                 | Stage the active pane for a diff; run it again in another pane to pair them |
| `:diffsplit <base>`         | Open `<base>` in a vertical split and diff it against the current buffer |
| `:diffsplit <base> <remote>` | Three-way: open `<base>` (common ancestor) and `<remote>` as splits; the current pane is *local* |
| `:diffoff` / `:diffoff!`    | End the active diff session (tear down the pairing); `!` forces         |
| `]c` / `[c`                 | Jump to the next / previous changed **hunk** (wraps)                    |
| `do`                        | **Diff get** — pull the hunk under the cursor from the other side       |
| `dp`                        | **Diff put** — push the hunk under the cursor to the other side         |
| `:diffget [bufnr]`          | Get the hunk from a named side (explicit form of `do`)                  |
| `:diffput [bufnr]`          | Put the hunk to a named side (explicit form of `dp`)                    |
| `:diff-accept` / `:diff-reject` | Resolve a (three-way) session with an accept / reject outcome       |

`do` and `dp` only fire when a diff session is active on the buffer;
outside a diff they fall through to the normal `d` operator.

---

## Starting a diff

Two ways in:

1. **Pair two open panes.** Put the cursor in one pane, `:diffthis`;
   move to another pane, `:diffthis` again. The two are now diffed
   against each other. A third `:diffthis` in the same pair, or
   `:diffoff`, tears it down. Closing either pane also ends the
   session.

2. **Diff against a file.** `:diffsplit path/to/other.rs` opens that
   file in a vertical split and starts a two-way diff immediately.

3. **Diff against disk.** `:diff` (no arguments) starts an inline diff
   of the active document against its own on-disk content — the fast
   way to see your unsaved edits as hunks without opening a second
   pane.

For a three-way merge, `:diffsplit <base> <remote>` opens the common
base and the remote alongside the current buffer (which becomes the
editable *local* side), in vim's `vimdiff a b c` ordering.

## The sign column

Changed lines are marked in the gutter's **diff sign column**, which
sits between the diagnostic column and the line numbers (the
Vim/Helix/Zed/VSCode convention):

| Sign | Meaning           |
|------|-------------------|
| `+`  | Added line        |
| `~`  | Changed line      |
| `-`  | Deleted line (a marker where lines were removed) |

The line itself also gets a subtle background **tint** matching the
hunk kind, layered behind syntax highlighting and any selection /
search highlight so everything stays legible. Both sides of a
side-by-side diff are tinted — additions/changes on the right, the
matching removals on the left. The sign column is reserved whenever a
diff is active so the layout doesn't shift as hunks come and go.

## Folding the unchanged code

By default a diff **folds away the unchanged code** so only the changes
(plus a few lines of context around each) stay on screen — vimdiff's
`foldmethod=diff`, VS Code's "Collapse Unchanged Regions". On a
side-by-side diff **both panes fold in lockstep**, so the two stay
vertically aligned as you scroll. When a diff opens, the cursor also
**jumps to the first change** so you start at something that matters
instead of the top of the file.

These are just folds — the normal [fold](help:folding) vocabulary drives
them:

- `zR` opens every fold (show the whole file); `zM` re-collapses to the
  changes.
- `zo` / `za` on a folded region expands that one gap to read the
  surrounding code; `zc` closes it again.
- Hunk folds and unchanged folds coexist: `za` on a *change* collapses
  that hunk; the closed folds between changes are the unchanged gaps.

Two options tune it (set with `:set`, inspect with `:describe-option`):

| Option | Default | Meaning |
|--------|---------|---------|
| `ui.diff.fold-unchanged` | `on` | Fold the unchanged regions when a diff opens. Set `=false` to open diffs fully expanded. |
| `ui.diff.context` | `6` | How many unchanged lines to keep visible around each change (vimdiff's `diffopt context:N`). `0` collapses right up to the change. |

```vim
:set ui.diff.fold-unchanged=false   " show the whole file, no folding
:set ui.diff.context=3              " tighter — 3 lines of context per change
```

A change is never hidden, and very small gaps between nearby changes
stay visible rather than collapsing to a fold marker that hides almost
nothing. A clean diff (no changes) folds nothing and doesn't scroll.

## Navigating and transferring hunks

`]c` / `[c` jump between hunk starts and wrap around the ends —
the same keys as vim. With the cursor on a hunk, `do` pulls that
hunk's content **from** the other side into this buffer, and `dp`
pushes this buffer's version **to** the other side. In a two-way
diff the "other side" is unambiguous; in a three-way session name
the side with `:diffget <bufnr>` / `:diffput <bufnr>`.

## Three-way merge

`:diffsplit <base> <remote>` turns the current buffer into the editable
*local* (= "ours") side and opens *base* and *remote* in vertical
splits, forming a three-way session (`[base, local, remote]`). Regions
where both local and remote changed the same base lines render as
**conflict** hunks (distinctly tinted, with a `?` sign).

Resolve each conflict from the **local** buffer with the vim-fugitive
3-way chords:

| Chord | Action |
| --- | --- |
| `d3o` | **keep-theirs** — replace the conflict region with the remote side |
| `dB`  | **keep-both** — splice ours then theirs into local |
| `d3p` | **put-theirs** — push the local side into remote |
| `d2o` / `d2p` | **keep-ours / put-ours** — local already holds ours, so these echo a note rather than editing |

`]c` / `[c` walk between hunks, conflicts included. `:diffget <bufnr>` /
`:diffput <bufnr>` are the ex-command equivalents with an explicit target
side. When the result looks right, `:diff-accept` resolves the session
with the merged content (or `:diff-reject` abandons it).

> **Why `d2o` / `d2p` only echo:** Lattice's three-way model is
> marker-free — the local buffer already contains *your* version, so
> "keep ours" has nothing to apply (there are no conflict markers to
> strip). `d3o` (take theirs) and `dB` (keep both) are the chords that
> actually edit the buffer.

---

## Autoread conflicts

When [`autoread`](help:buffers#external-file-changes-autoread) finds that a
file changed on disk **while you had unsaved edits**, it won't silently
reload (that would lose your work) and it won't overwrite the disk. It
opens a **two-way diff resolver** so you reconcile the two versions hunk
by hunk. This is the same diff machinery as everything above — only the
trigger differs.

### What you see

Three panes open, equal width:

| Pane | Content | Editable? |
| --- | --- | --- |
| left (origin) | your buffer, untouched | — (bystander) |
| middle (baseline) | the **on-disk / external** version | read-only |
| right (proposed) | a copy of **your** version | **editable, active** |

The cursor starts in the right (proposed) pane on the first hunk. The
diff is between the **middle** (disk) and **right** (yours) panes.

### Reconcile per hunk, then finalize

Per-hunk transfer is the main tool; `:diff-accept` / `:diff-reject` are
only the finish line for the whole session:

1. `]c` / `[c` — walk between the hunks.
2. On a hunk you want to take **from disk**, press `do` (or
   `:diffget`) — it pulls that hunk from the baseline into the proposed
   pane. Hunks you leave untouched keep **your** version. You can also
   edit the proposed pane freely.
3. When the proposed pane holds the result you want, `:diff-accept` —
   this **writes the proposed content to the file** and closes the two
   diff panes. (`:diff-reject` abandons it and leaves the file with its
   external content.)
4. Back in your original buffer, run `:e!` to load the resolved file.
   This reloads from disk and re-enables autoread for the buffer.

> **Why the extra `:e!`?** The resolver edits a *copy* of your buffer,
> so after `:diff-accept` writes the file your original buffer still
> shows your old unsaved edits. `:e!` brings it in sync. Until you
> reload (or close) the buffer, autoread stays hands-off for it — so a
> resolve-and-save can't bounce you back into another diff. If instead
> you want to **keep your version and overwrite the disk change**, just
> `:w` your buffer (skip the diff entirely, or `:diff-reject` first).

> Autoread conflicts are 2-way today. A future release will diff the
> resolver's editable side against your live buffer (dropping the `:e!`
> step) and add 3-way auto-merge for non-overlapping changes.

---

## Relationship to multibuffer views

A **project-wide** diff — every changed hunk across many files in one
scrollable, editable view — is a [multibuffer](help:multibuffer-mode) whose
excerpts are the hunks. The per-pane diff described here and a
project-wide diff are the same diff engine surfaced two ways: side-by-
side panes for one file, composed excerpts for many. The hunk
navigation (`]c` / `[c`) and the sign column behave the same in both.
