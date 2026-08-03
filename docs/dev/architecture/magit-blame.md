# magit-blame — annotations on the file, not a buffer full of text

## Why the current shape is wrong

`*magit:blame:<path>*` is a synthetic buffer whose *text* is
`<sha8> <author>  <code>`, one row per source line, styled by column
position (`highlight::blame_styled_spans`). The code column gets
`Vec::new()` — no spans at all — so **the code is unhighlighted by
construction**, and no amount of work on that function changes it.

Losing syntax highlighting is the symptom. The shape is the cause: we
replaced the file with a text rendering of the file. Everything else
follows from that — the buffer is read-only so ordinary motions over
your own code stop working, the code is shifted right by a 22-column
prefix, and a long author name truncates rather than wrapping because
the styler colours fixed columns.

## What every other editor does

| Editor | Shape | Annotates the real file? |
|---|---|---|
| Emacs magit | chunk headings above each group (`heading-format` `"%-20a %C %s\n"`), or the `margin` / `highlight` / `lines` styles — a **minor mode on the file buffer** | yes |
| vim-fugitive | annotations in a **scroll-bound vertical split**; the file window is untouched | yes (sidecar) |
| Zed | `git::Blame` column + `editor::ToggleGitBlameInline` for the current line | yes |
| VS Code / GitLens | current-line inline + gutter heatmap + hover | yes |
| JetBrains | gutter annotation column | yes |
| Helix | none — no typable command | — |
| **lattice today** | **synthetic buffer, blame baked into the text** | **no** |

Unanimous among those that have blame. Nobody "adds syntax
highlighting to a blame buffer" because nobody builds a blame buffer —
highlighting survives because the file was never replaced.

This is the UX-follows-convention case from `CLAUDE.md`: blame is a
common feature with settled cross-editor behaviour, and muscle memory
carries. It is *also* what the paramount goals point at, for
independent reasons given below — the two agree here.

## The shape

**`magit-blame-mode`, a minor mode**, activated on a buffer that is
already showing the content being blamed. Not a major, and not a
buffer of its own.

It owns, per the mode-ownership rule: the chords, the handler bodies,
the blame fetch, and a **virtual-row provider** supplying one heading
row above each chunk of lines sharing a commit — magit's `headings`
style, on `VirtualRow { anchor_line, position: Above, .. }`, the same
primitive `DiffOverlayVirtualRowProvider` already uses.

**Direction is mode state, not a buffer name.** Forward and reverse
blame are the same annotations computed by a different `git blame`
invocation over the same buffer. MG.23f2 encoded the direction in a
buffer name because a buffer was the only carrier available; a minor
mode has state.

### Why headings rather than a per-line column

A per-line column (Zed, JetBrains) costs constant horizontal space and
shifts every line of code right. `gutter_width()` is sized for line
numbers plus a one-char marker, so this needs the gutter widened in
**both** renderers, and shifting content the user did not edit brushes
the pixel-stability contract.

Headings cost vertical space instead — one row per chunk, so a file
where every line has a different commit nearly doubles in height. The
trade is real and it is the reason magit's style is an acquired taste.
What it buys: the commit is read **once per chunk** with the full
summary legible, rather than a truncated sha repeated on every line,
and code stays exactly where the eye expects it horizontally.

### The buffer goes read-only while blaming

A minor on an *editable* file buffer cannot take `p`, `<CR>`, `x` —
those are grammar. Magit resolves this by making the buffer read-only
for the duration of `magit-blame-mode`, which re-frees the edit chords
for blame use. We do the same: blame is a *reading* mode, and the
alternative is either a second keymap vocabulary nobody knows or
chords that shadow editing in a buffer the user can still type in.

Read-only is per-buffer state any kind can carry, so this needs no
kind-specific branch.

## Reverse blame is the blob buffer plus this mode

`magit-file-revision-mode` already shows a file at a revision
(`*magit:file:<rev>:<path>*`). Reverse blame is that buffer with
`magit-blame-mode` active in the reverse direction — "for each line of
what I am looking at, when did it go away".

This is strictly better than MG.23f2's dedicated buffer: it composes
two things that already exist instead of adding a third, the content
shown is the blob buffer's (which is the same content reverse blame
was reproducing), and the `gj` / `gk` walk keeps working while blaming.

### What the heading names (MG.33)

`git blame --reverse` reports, per line, **the last commit in which the
line still existed**. Rendering that with the forward-blame heading
shape — sha, author, date, subject — makes it *read* as "this commit
removed the line", when the commit that removed it is that commit's
child. The feature is advertised as "when did this line go away?" and
was showing the answer's parent.

So a reverse chunk carries a `Removal`, resolved in the same
`spawn_blocking` as the blame itself:

| Variant | Heading | How it is decided |
|---|---|---|
| `By(commit)` | the **removing** commit, `· removed` | the oldest commit in `git rev-list --ancestry-path --reverse <sha>..HEAD -- <path>` |
| `StillPresent` | the blamed commit, `· still present` | that walk is empty |
| `Ambiguous` | the blamed commit, `· last contained here` | two or more candidates and the second does not descend from the first |

**The three cases stay distinct rather than collapsing into "best
guess".** A wrong attribution in a blame heading is worse than an
honest incomplete one: it is indistinguishable in shape from a correct
one, so the reader has no signal to discount it. That is the UX rule's
"within reason" clause doing real work — naming the removing commit is
the UX win, and guessing it across a merge would have been a UX
regression wearing the same clothes.

**Cost is per distinct commit, not per line.** `resolve_removals`
dedupes by sha before walking, so a file with many chunks from few
commits pays little; the `merge-base --is-ancestor` check runs only
when the candidate list has more than one entry, so the linear case is
one invocation. Uncommitted chunks are skipped — `0000000..HEAD` is not
a range.

**Resolved before publishing, not after.** Publishing headings and then
filling the answer in would relabel rows the user did not touch, which
the keystroke UX contract forbids; the whole answer lands at once.

**Retires:** `*magit:blame:<path>*`, `*magit:blame-reverse:<rev>:<path>*`,
`MagitBlameMode` as a *major*, `blame_styled_spans`, and the
`format_blame_porcelain` row formatter — the porcelain *parser* stays,
since chunk headings need exactly the (sha, author, date, summary,
line-range) it already extracts.

## The parser question, and why this is not MG.22's

Blame on a working-tree file needs nothing new: the file buffer has its
own major and therefore its own parser.

Blame on a **blob** buffer does need one, because
`*magit:file:<rev>:<path>*` has no path and today gets no highlighting
at all — it calls `PendingSyntheticHighlights::wake()` with no spans.

That is solved territory, not an open question. `lattice-multibuffer`
already gives a pathless synthetic buffer a parser by deriving `Lang`
from a path it knows *about*:

```rust
let lang = Lang::detect_from_path(path.as_deref());
if lang != Lang::Plain {
    let mut syntax = Syntax::for_language_with_registry(lang, lr.clone())?;
    syntax.parse(&text);
    state.source_syntax.insert(id, Arc::new(SyntaxHandle::seeded(syntax)));
}
```

The blob buffer carries `<path>` in its name, which is all
`detect_from_path` needs. `grep_highlight.rs` does the same thing.

**MG.22's wrinkle is a different problem** and stays open: there, a
*minor* wants to supply a *diff* parser for content whose language is
not the buffer's identity. "This blob is Rust because its name ends
`.rs`" is not that.

## Paramount-goal alignment

- **#1 performance.** Virtual rows are an existing per-frame primitive
  with O(viewport) fan-out; the diff overlay already drives them
  through a provider. Blame data is fetched on `spawn_blocking` and
  lands through the inbound primitive, never on the actor thread.
  Chunk headings are computed once per blame run, not per frame.
- **#3 modal editing.** The file stays a normal buffer, so the whole
  vim grammar keeps working over your own code while blame is shown —
  which the current shape destroys.
- **Mode ownership.** One minor owns chords, handlers, provider and
  fetch; the host gains nothing.

## Rejected alternatives

**Syntax-highlight the existing synthetic buffer.** The literal
request, and the worst option. Tree-sitter would be parsing
`a1b2c3d8     Jane Doe  fn main() {`, which is not the language, so
the unprefixed text would have to be parsed separately and every span
shifted by the prefix width — permanently, and re-derived on every
refresh. It preserves the shape that caused the problem.

**Per-line blame column.** See above: both renderers' gutters, plus a
horizontal shift of unedited content.

**Current-line inline only** (GitLens / Zed's inline). Cheap and
useful, and it would unblock MG.23f's deferred `b e` (blame echo) row,
which was filed as "no surface to map onto — needs inline virtual-text
blame first". Rejected **as the final shape**, because with headings
landed it answers a strictly smaller question for a second mechanism's
worth of code. Reconsider only if `b e` is wanted on its own terms.

**fugitive-style scroll-bound split.** D.4's pane groups make it
available, and it preserves highlighting. It costs a pane and keeps
two cursors in sync for a read-only overlay — headings put the same
information in the same window with no pane budget.

## Grammar surface

Chords are magit's, via evil-collection where it remaps (see
`magit-keys-follow-evil-magit`). Carried from the retired major:
`<CR>` show the commit for the chunk at cursor, and the parent walk.
The blame transient's own keys (`b` / `r` / `f` / `q`) are the natural
home for direction switching once the mode exists.

Chunk headings are foldable in principle — a chunk is a range with a
header, which is the shape `magit-core-mode`'s section folds already
take. Not in the first slice.

## Open

- Which key toggles the mode on a file buffer. Magit reaches blame
  through its file dispatch (`C-c f` `b` here), which already exists
  and would now activate the minor rather than open a buffer.
- Whether the heading shows relative or absolute dates, and whether
  that is an option (`magit.blame.*` — magit has `heading-format`).
- Whether a chunk heading should carry the commit's *colour* the way
  fugitive tints hashes, for scanning chunk boundaries without reading.

## Cross-references

- [`magit.md`](magit.md) §4.4 — the view being replaced
- [`magit-hunk-mode.md`](magit-hunk-mode.md) — the other minor that
  owns content across majors; its parser wrinkle is *not* this one
- [`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md)
  — MG.23f2 (reverse blame, being folded in), MG.7 (the original view)
