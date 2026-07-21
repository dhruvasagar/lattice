+++
title = "Project search"
+++



`:search` walks the project, collects every line matching your
query, and streams the results into a [multibuffer](multibuffer)
view — one excerpt per match, grouped under each file's header. The
scan runs off the UI thread, so results appear progressively and the
editor stays responsive even on a large tree.

> **Status:** `:search <query>` (literal, case-insensitive,
> `.gitignore`-aware) + `<CR>` jump-to-source + `gr` refresh are
> shipped. Regex and case-sensitive matching exist in the scan
> engine but are not yet exposed as a user toggle — see
> [Not yet configurable](#not-yet-configurable).

---

## Quick reference

| Keystroke / command | Meaning                                                          |
|---------------------|------------------------------------------------------------------|
| `:search <query>`   | Project-wide search for the literal `<query>`; opens a results multibuffer |
| `g/{motion}`        | Project-search for the text a motion / text object spans (e.g. `g/iw`, `g/i"`) |
| `<CR>`              | (in results) Jump to the source file + line of the match under the cursor |
| `gr`               | (in results) Re-run the scan with the same query                 |
| `]e` / `[e`        | Move between matches (excerpt motions — see [multibuffer.md](multibuffer)) |
| `:multibuffer-expand [n]` | Show `n` lines of context around the match under the cursor |

---

## Running a search

```
:search TODO
```

This searches every file under the project root for the literal
substring `TODO`. Matching is **case-insensitive** and respects
`.gitignore` (ignored files and directories are skipped). The query
is treated **literally** — `foo.*bar` matches that exact text, not a
regex.

Results open in a new multibuffer named for the search. Each matched
line is a one-row excerpt under its file's header; a file with
several hits lists them all under one header. The view's
**headerline** shows progress while the scan runs
(`⟳ Searching "query" (N files) …`) and a summary when it finishes
(`◆ "query" — N hit(s) in M files`). The **search term is highlighted**
in an accent colour (the themeable `multibuffer.status.query` role) so
it stands out from the surrounding status text.

## The `g/` search operator

`g/` is a Vim-style **operator**: it takes any motion or text object and
runs a project search for the text that motion / text object spans — the
same search `:search` performs, without typing the query out. Think of it
as the **global** counterpart of buffer search `/` (the `g` prefix is
Vim's "extended / global variant" marker, as in `gg`, `gU`).

Press `g/`, then a motion or text object:

| Chord | Project-searches for… |
|---|---|
| `g/iw` / `g/iW` | the word under the cursor |
| `g/i"` `g/i'` `` g/i` `` | the quoted-string contents |
| `g/a(` `g/i{` `g/it` | the delimited / tag contents |
| `g/$` `g/e` `g/fx` | the span the motion covers |
| (Visual) select, then `g/` | the current selection |

Common uses: put the cursor on an identifier and `g/iw` to find every
use across the project; `g/i"` inside a string literal to hunt down a
message; select an expression in Visual mode and `g/` to grep it.

The extracted text is used **verbatim** — literal and case-insensitive,
exactly like `:search` — so `g/iw` on `needle` opens the same
`*search:needle*` view as typing `:search needle`. `g/` is aimed at
**single-line** text objects (words, quotes, calls); a multi-line motion
passes the literal text *with* its newlines, which the line-oriented scan
won't match.

`g/` and `/` are deliberately distinct: `/` searches the **current
buffer** (see [modal-editing.md](modal-editing)); `g/` searches the
**whole project**.

## Acting on results

- **`<CR>`** opens the source file in the active pane at the matched
  line and records the position in the jump list, so `<C-o>` brings
  you back — the same round-trip as `gd` or a tag jump. See
  [position history in modal-editing.md](modal-editing).
- **`gr`** re-runs the scan with the view's current query (after
  you've edited files and want fresh results). It clears the view,
  resets the headerline, and streams again.
- **Editing** — the results buffer is editable like any multibuffer:
  changes to an excerpt propagate to the source file, and `:w` saves
  through. Expand context first (`:multibuffer-expand`) if you want
  more than the matched line to work with.

## Semantics

- **Streaming.** Files are scanned in batches and excerpts appear as
  each batch completes — you can start reading (and jumping) before
  the whole tree is done.
- **Dedup.** A file with hits split across scan batches still gets a
  single source buffer (no duplicate file headers).
- **Off-thread.** The walk + match run on a background task; the UI
  thread only renders the streamed excerpts, so a large repository
  doesn't freeze the editor.

## Not yet configurable

The scan engine already supports **regex** matching (via
`fancy-regex`), **case-sensitive** matching, and per-hit **context
lines**, but there is no user-facing switch for them yet: `:search`
always runs a literal, case-insensitive query. A `:set search.*`
surface (or `:search` flags) to expose these is tracked, not
shipped. Until then, use `:multibuffer-expand` for context after the
fact.
