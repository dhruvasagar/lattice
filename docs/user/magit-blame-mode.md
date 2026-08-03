---
summary: "magit-blame-mode: a minor mode that annotates the file you are reading with a heading above each run of lines sharing a commit — your code keeps its syntax highlighting."
related: [magit, magit-core-mode, magit-file-revision-mode, ex:magit-blame]
---

# magit-blame-mode

Who last touched each part of the file, shown **on the file itself**.
`:magit-blame`, or `b` in the [file dispatch
transient](help:magit-transient) (`C-c f`). Both are toggles — press
again to stop blaming.

```
  a1b2c3d4  Jane Doe  3 days ago  extract the parser
fn parse(input: &str) -> Result<Ast> {
    let tokens = lex(input)?;

  9f8e7d6c  Sam Roe  2 months ago  handle empty input
    if tokens.is_empty() {
        return Ok(Ast::default());
    }
```

One heading above each **chunk** — a run of consecutive lines sharing a
commit — carrying the short SHA, the author, a relative date and the
commit summary. The SHA is coloured apart from the rest so chunk
boundaries are scannable without reading.

**Your code keeps its syntax highlighting**, because the buffer is
still your file. That is the whole point of the shape: blame is an
annotation *over* the file, not a rendering *of* it.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Show the commit for the chunk at cursor in [`magit-revision-mode`](help:magit-revision-mode) |
| `p` | Re-blame at the **parent** of the current revision |
| `gq` | Stop blaming — the buffer becomes editable again |

The buffer is **read-only while blaming**, which is what frees `<CR>`
and `p` — they are ordinary editing keys otherwise. Stop blaming and
the file is editable again immediately.

`gq` stops blaming, and `C-c f b` / `:magit-blame` toggle it off too.
Bare `q` is deliberately **not** it: blame can be active on a [file at
a revision](help:magit-file-revision-mode), where `q` already closes
the buffer, and one key meaning two different things depending on where
you are is worse than a `g`-prefixed one. `gq` sits beside `gr` in the
same namespace and shadows nothing (vim's `gq` reformats, which a
read-only buffer cannot do).

## Walking history with `p`

`p` re-blames the same file at the parent of the revision currently
blamed, **in place** — press it repeatedly to peel back one commit at
a time and find the change *before* the one that currently claims a
line. This is how you get past a reformat, a rename, or a mass-update
commit that owns every line and explains none of them.

At the root commit `p` has nowhere left to go and says so rather than
appearing to do nothing.

## Reverse blame — "when did this line go away?"

`:magit-blame-reverse <rev> <path>`, or `r` in the file dispatch when
you are already looking at a [file at a
revision](help:magit-file-revision-mode).

It answers the opposite question: for each line as it was at `<rev>`,
**what removed it**.

Each heading is one of three things, and it says which:

| Heading ends with | Means |
|---|---|
| `· removed` | This commit — the one named at the start of the heading — took the lines out. |
| `· still present` | The lines are in the file at HEAD. Nothing removed them. |
| `· last contained here` | The lines are gone, but more than one commit could have removed them, so none is named. See below. |

For a `· removed` heading the sha, author, date and subject are all the
**removing** commit's, because that is the answer to the question you
asked. (Earlier versions showed the last commit in which the line still
existed, formatted identically to a forward blame — so it read as "this
commit removed the line" while naming its parent.)

**Why some headings decline to name a commit.** If history forked after
the commit being blamed and more than one branch touched the file,
several commits qualify and git cannot say which one is *the* removal.
Picking one would be a guess presented as a fact, in a heading that
looks exactly like the confident ones — so those rows say only what is
certain: the lines were last seen here.

It annotates the *blob* buffer — the file as it was at that revision —
because that is the content the question is about. Your working-tree
copy is untouched, and `gj` / `gk` keep walking the file's history
while the annotations are up.

Both arguments are required. A default revision is exactly what
reverse blame cannot have: `HEAD` would make the range empty and
report every line as still present, which is a plausible-looking
answer that says nothing.

## Uncommitted lines

Lines you have not committed yet get a heading reading `Uncommitted
changes` rather than git's internal all-zero SHA.

## Behaviour worth knowing

- Blame runs on a background thread and the headings appear when it
  lands — a large file never blocks the editor, and you do not have to
  press anything to see the result.
- A **reverse** blame does extra history walking to work out what
  removed each run of lines, so it takes longer to land than a forward
  one. The work is per distinct commit rather than per line, so a file
  with many lines from few commits costs little. The headings still
  appear all at once when the whole answer is ready, rather than
  appearing and then relabelling themselves.
- Headings cost **vertical** space, one row per chunk. A file where
  every line has a different commit nearly doubles in height. The trade
  is deliberate: a per-line column would shift all your code sideways
  and repeat a truncated SHA on every line, where a heading states the
  commit once, legibly.
- `magit.blame.author-width` and `magit.blame.date-format` read like
  options but are **not registered**: `:set` on either fails with
  `unknown option`. See [`magit`](help:magit#options) for the ones that
  are.

## See also

- [`magit-revision-mode`](help:magit-revision-mode) — the commit detail
  `<CR>` opens.
- [`magit-log-mode`](help:magit-log-mode) — history from the other
  direction, by commit rather than by line.
