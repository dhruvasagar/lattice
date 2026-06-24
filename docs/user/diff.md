---
summary: "Diff & merge: :diffthis / :diffsplit, hunk navigation (]c / [c), do / dp to get/put hunks, sign column, two- and three-way."
related: [diff, ex:diff]
---

# Diff & merge

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
| `:diffthis`                 | Stage the active pane for a diff; run it again in another pane to pair them |
| `:diffsplit <file>`         | Open `<file>` in a vertical split and diff it against the current buffer |
| `:diffsplit <local> <remote>` | Three-way: open `<local>` and `<remote>` as splits against the base    |
| `:diffoff`                  | End the active diff session (tear down the pairing)                     |
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

For a three-way merge, `:diffsplit <local> <remote>` opens both
alongside the current buffer (the common base), in vim's
`vimdiff a b c` ordering.

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
search highlight so everything stays legible. The sign column is
reserved whenever a diff is active so the layout doesn't shift as
hunks come and go.

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

## Relationship to multibuffer views

A **project-wide** diff — every changed hunk across many files in one
scrollable, editable view — is a [multibuffer](multibuffer.md) whose
excerpts are the hunks. The per-pane diff described here and a
project-wide diff are the same diff engine surfaced two ways: side-by-
side panes for one file, composed excerpts for many. The hunk
navigation (`]c` / `[c`) and the sign column behave the same in both.
