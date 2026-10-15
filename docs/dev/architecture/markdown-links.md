# Markdown links

Status: design fragment (2026-10-15). Slice plan:
[`../operations/slice-plans/archive/markdown-links.md`](../operations/slice-plans/archive/markdown-links.md).

Anchors: [`conceal.md`](conceal.md) (the display-time elision this configures),
[`org-mode.md`](org-mode.md) §7 (the same feature for org, the precedent),
`design.md` §5.1.1 (position history), paramount goal #3.

A markdown link is markup to be hidden and a thing to be activated — the
sentence org-mode §7 wrote for org links, and true of markdown for the same
reasons. Until now a markdown buffer did neither: `[the guide](guide.md)` read
as twenty-three characters of punctuation and prose, and `<CR>` on it did
nothing. Help buffers did both, by a route an editable file cannot take
(stripping the URL out of the text when the buffer is built — `conceal.md`
explains why that is not available for a file the user edits).

## What a link is

| Written | Kind | `<CR>` |
|---|---|---|
| `[t](#install)` | in-document | jump to the heading whose slug is `install` |
| `[t](guide.md)`, `[t](../a/b.rs)` | document | open the file |
| `[t](guide.md#setup)` | document at heading | open the file at that heading |
| `[t](https://…)`, `mailto:…`, any `scheme://` | external | the OS handler |
| `<https://…>` | external (autolink) | the OS handler |
| anything else | — | declined |

The cursor is on a link anywhere from its `[` to its `)`. Concealed, only the
label shows; in Insert mode the raw markup shows; both must give one answer.
A `"title"` and `<…>` around the destination are stripped, `%20` is decoded,
and `file://` opens in the editor rather than the OS handler.

**Relative paths are relative to the document**, not to the editor's cwd.
That is how every renderer of markdown reads them — GitHub, a static-site
generator, a preview pane — and a link that works when the file is rendered
and fails when it is opened in the editor is a link the editor broke. A buffer
with no path has no directory, so its links resolve against the cwd.

**Slugs are GitHub's**: lowercase, punctuation dropped, whitespace runs to one
`-`. Help buffers already generated anchors this way, so `[t](#x)` means the
same heading in `:help` as in the file it was written in. Lookup ignores case,
so a hand-typed `#Install` still finds `# Install`. Headings inside fenced code
blocks are not anchors.

**A missing file is an error, not a new buffer.** Vim's `gf` makes the same
choice. A blank buffer would make a mistyped link look like it worked, and a
later `:w` would create the file the typo named. A missing *heading* in a file
that exists still opens the file, at the top, and says so.

**Every follow records a jump** (`Effect::RecordJump` first in the batch), so
`<C-o>` returns to the link. An in-document jump is as much a "big motion" as
`G` is; position history is one ring for both (§5.1.1).

**Out of scope, deliberately:** reference links (`[t][ref]` with a
`[ref]: url` definition elsewhere) and wiki links (`[[note]]`). Reference links
need a document-wide definition table — a different shape from "read the line
under the cursor" — and wiki links are not CommonMark. Neither is ruled out.

## Who owns what

```
lattice-syntax::markdown        the syntax: scan [label](url), classify the
                                destination, slugify headings, find a heading
lattice-syntax::markdown_mode   markdown-mode: the <CR> chord AND its handler
lattice-help                    help's own URL schemes (command:, key:, …),
                                built on the scanner above
host                            the effect vocabulary; nothing markdown-specific
```

**The scanner lives in `lattice-syntax`**, beside the markdown grammar, because
both of its readers need it and `lattice-help` already depends on
`lattice-syntax` — the reverse edge would be a cycle. The alternative was a
second copy in the mode, and copies of a parser diverge silently (heuristic #1).

**The chord and the handler are both markdown-mode's**, so the mode-ownership
acid test holds: no `Editor::` method, no host `Action` variant. The handler
reads the buffer through `BufferStoreHandle` and answers with
`Effect::Many([RecordJump, CursorMove | OpenBufferAt])`, `OpenExternalUri`, an
`Echo`, or `Declined`.

**A major, not a minor.** Shared behaviour goes on a minor mode; this is not
shared. Help buffers have their own `<CR>` from `help-mode` (a minor layered
over markdown-mode, so it shadows the major's binding), and org has its own
plugin-side follow. One major wants this, so the major carries it.

### Why `<CR>`, and why declining is safe

`<CR>` is unbound in Normal mode for a Document (org-mode §7.2 records the
same finding), and it is the chord that follows a link in help buffers and in
org. Off a link the handler returns `Effect::Declined`; `<CR>` is a single key,
so there is no trailing key for a decline to replay — the hazard org §7.2
describes for `<leader>oo` does not arise.

### Why not LSP

`marksman` resolves links over `textDocument/definition`, and `gx` already
follows LSP document links. Both need a running server. Following a link is a
function of the line's text and the filesystem; making it depend on a language
server would leave every user without one with dead links.

## Rendering

Markdown declares [conceal](conceal.md) rules; the host's evaluator does the
rest, on both renderers, with no markdown-specific code. An inline link renders
as its label, an autolink as its URL.

**Native languages declare rules the way plugin languages do.** Conceal rules
reached the registry only through a plugin's `language` spec; a compiled-in
language had no way to contribute one. A per-language `conceal_source(name)`
— the shape `indents_source(name)` already has — closes that, and the rules go
through the same `compile_rules` with the same caps and refusal logging.

**Patterns, not the parse tree**, for org's second reason: the tree is absent
during a reparse, and tree-driven conceal would flicker between concealed and
raw as the user types — a pixel change to content they did not edit.
`tree-sitter-md`'s inline grammar does have `inline_link` nodes, so org's first
reason (no link rule in the grammar) does not apply here; the flicker reason
alone is sufficient.

The costs of patterns, stated rather than smuggled:

- **Code spans and fences are not exempt.** A line regex cannot know it is
  inside backticks. A guard catches the common case — a link written as the
  first thing in a code span, the way this repository's own docs quote link
  syntax — but `` `see [a](b)` `` still conceals, and so does a link-shaped
  line inside a fenced block.
- **Images are left alone.** The same guard keeps `![alt](src)` raw rather
  than rendering it as `!alt`.
- **The guard consumes a character**, so of two links written back to back
  with nothing between them only the first conceals.

**Help buffers inherit the rules**, because the host looks rules up by the
buffer's language and a help buffer's is markdown. Their links are already
stripped to labels at build time, so in practice this only touches a raw
link a help page quotes in prose — which now renders as its label, the same
as it would in the `.md` file it came from.

None of these changes what `<CR>` does — the follow path scans the raw line —
and Insert mode reveals the whole buffer, as it does for org.

## Paramount-goal alignment

- **#1 Performance.** `<CR>` is user-initiated and off the render path; it
  reads one line, or the buffer's text for an in-document jump. Conceal is a
  bounded regex pass during an off-thread rebuild that was already happening,
  benched in `lattice-syntax/benches/conceal.rs`; languages with no rules skip
  it entirely.
- **#2 Extensibility.** The host learns nothing about markdown. Conceal rules
  go through the evaluator plugins already use.
- **#3 Modal editing.** The mode owns its chord and its body; position history
  records every follow.
- **#4 Asynchronicity.** Nothing here touches the UI thread. The one disk read
  — finding a heading in another file — runs on the actor, beside the
  `OpenBufferAt` that reads the same file a moment later.
