---
summary: "magit-blame-mode: per-line git blame annotations — <CR> opens the commit for a line, p walks the blame back to the parent commit."
related: [magit, magit-blame, ex:magit-blame]
---

# magit-blame-mode

Per-line authorship: who last touched each line, and in which commit.
`:magit-blame` blames the current file, `:magit-blame <path>` a
specific one, and `b` in the [file dispatch
transient](help:magit-transient) (`C-c f`) blames the file you're
editing.

Each row is `<sha> <author>  <the line itself>` — SHA coloured as a
SHA, author in its own colour, the code left unstyled (this buffer has
no language context to highlight it with, and guessing one would
mislead).

The headerline reads `src/main.rs  @  a1b2c3d` and **updates as `p`
walks back**. That is the only place the walked-to revision is visible
— the annotations themselves look identical at every step, so without
the header you'd lose track of how far back you are.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Show the commit for the blamed line in [`magit-revision-mode`](help:magit-revision-mode) |
| `p` | Re-blame at the **parent** of the current revision |

`q` / `gr` / navigation come from
[`magit-core-mode`](help:magit-core-mode).

## Walking history with `p`

`p` is the reason to stay in this buffer rather than jumping straight
to a commit. It re-blames the same file at the parent of the revision
currently shown — so pressing it repeatedly peels back one commit at a
time, letting you find the change *before* the one that currently
claims a line. This is how you get past a reformat, a rename, or a
mass-update commit that owns every line but explains none of them.

At the root commit `p` has nowhere left to go and reports that rather
than appearing to do nothing.

## Behaviour worth knowing

- Blame data loads on a background thread, so a large file doesn't
  block the editor while git works.
- **Column widths are hardcoded** — an 8-character SHA and a
  12-character author column. `magit.blame.author-width` and
  `magit.blame.date-format` read like options but are **not
  registered**: `:set` on either fails with `unknown option`.
  `lattice-magit` registers no options of its own today. See
  [`magit`](help:magit#options).

## See also

- [`magit-revision-mode`](help:magit-revision-mode) — the commit detail
  `<CR>` opens.
- [`magit-log-mode`](help:magit-log-mode) — history from the other
  direction, by commit rather than by line.
