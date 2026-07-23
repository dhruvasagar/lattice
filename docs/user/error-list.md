---
summary: "The error list: a navigable list of file:line:col locations walked with :next-error/:previous-error (]qq/[qq) — populated by :compile (any CLI tool), browsed with :error-list, grouped with :problems."
related: [compilation, ex:next-error, ex:previous-error, ex:problems, project-search]
---

# The error list

The **error list** is a single, editor-wide list of source locations —
each a `file : line : column` with a message and a severity — that you
step through with `:next-error` / `:previous-error` (and the `]qq` /
`[qq` chords). It is Lattice's take on vim's **quickfix** list: a
navigation substrate that *any* tool can fill, decoupled from the tool
that produced it. Vim users: every `:c*` command still works as an
alias (`:cnext`, `:copen`, …).

The list is **core editor state, not tied to any buffer**. Once it is
populated you can walk it from anywhere — you do **not** need the
[`*compilation*`](compilation.md) buffer (or any particular buffer)
focused or even open. Closing the buffer that produced the list does
not clear it. (This matches vim; it deliberately differs from emacs,
where next-error navigation lives on the compilation buffer.)

---

## Quick reference

| Command | Vim alias | Chord | Meaning |
|---------|-----------|-------|---------|
| `:first-error` | `:cfirst` `:cr` | `[Q` | Jump to the first entry |
| `:previous-error` | `:cprev` `:cp` | `[qq` | Previous entry (wraps at the start) |
| `:previous-error-file` | `:cprevfile` `:cpf` | `[qf` | First entry in the **previous file** |
| `:next-error` | `:cnext` `:cn` | `]qq` | Next entry (wraps at the end) |
| `:next-error-file` | `:cnextfile` `:cnf` | `]qf` | First entry in the **next file** |
| `:last-error` | `:clast` | `]Q` | Jump to the last entry |
| `:error [N]` | `:cc [N]` | | Jump to entry `N` (1-based); no arg = current |
| `:error-list` | `:clist` `:cl` | | Open the **picker** (fuzzy list; `<CR>` jumps) |
| `:problems` | `:copen` | | Open the [`*problems*`](compilation.md#the-problems-view) grouped view |
| `:problems-close` | `:cclose` | | Close the `*problems*` view |

The command names lead with the readable, emacs-style vocabulary
(`next-error`); the terse vim `:c*` spellings are aliases, so muscle
memory from either editor works. The chord scheme is mnemonic: **`[`** =
backward / **`]`** = forward; doubled **`qq`** steps one entry, **`qf`**
a whole file, the capital **`Q`** jumps to the extremes. (The chords
keep vim-unimpaired's `q` — the universally-known quickfix chord.) They
are normal-mode Builtin grammar and work from **any buffer**. Each jump
records a position in the [jump list](modal-editing.md), so `<C-o>`
returns you.

> These chords are **not** `[d` / `]d`, which hop between LSP
> diagnostics **in the current file** — a separate feature (see
> [LSP](lsp.md)). Error-list navigation is cross-file and list-based.

---

## Populating the list

The error list is filled by a **producer**. Today the producer is
[compilation mode](compilation.md): running

```
:compile <any command>
```

streams the command's output into the `*compilation*` buffer **and**
parses every `file:line:col` reference in it into the error list. The
parser is **tool-agnostic** — it recognises the generic
`path:line:col: message` and `path:line: text` shapes emitted by the
vast majority of CLI tools (compilers, linters, test runners,
`grep -Hn`, `rg --vimgrep`, …), plus a dedicated `cargo`/`rustc`
multi-line matcher. Compilation mode is **not coupled to any one tool**:
point it at whatever prints locations and the list fills.

```
:compile cargo build         " rustc errors + warnings
:compile cargo test          " test panics: thread '…' panicked at file:line:col
:compile npx eslint .        " path:line:col: message
:compile grep -Hn TODO -r .  " path:line: text
:compile ctest --output-on-failure
```

Both the tool's stderr **and** its stdout are parsed, so **test
failures land in the list too**: `:compile cargo test` captures each
`thread '…' panicked at file:line:col` (printed on stdout) as an
entry, and `:next-error` walks you to the failing assertions just like
compiler errors.

A new run **replaces** the list (its first action clears the previous
entries). Entries stream in as the tool runs, so `:next-error` works
before a long build finishes.

> Other producers (e.g. project search) may feed the *same* list in
> future, leaving the navigation below unchanged. LSP diagnostics are
> **not** a producer — they have their own navigation (`[d`/`]d`,
> `:diagnostics`); the error list is for tool output.

---

## Navigating

After a run:

```
:next-error        " → first error, then next, then next…
:previous-error    " ← back one
:error 5           " jump straight to entry 5
:next-error-file   " skip the rest of this file, go to the next file's first entry
```

`:next-error` / `:previous-error` **wrap** — stepping past the last
entry returns to the first. The `-file` variants move a whole file at a
time, landing on the first entry of the next / previous file (files are
ordered as first seen in the tool's output).

### When the list is empty

`:next-error` / `:previous-error` (and `]qq` / `[qq`) touch **only** the
error list — on an empty list they echo `no error list` and do nothing
else (matching vim's `E42: No Errors`). They do **not** fall back to
diagnostics: LSP diagnostics have their own dedicated navigation, `[d` /
`]d` (current-file) and the `:diagnostics` picker (see [LSP](lsp.md)).

---

## Three views of the list

The entries can be worked three ways — pick whichever fits:

| Surface | Command | What you get |
|---------|---------|--------------|
| **Step** | `:next-error` / `]qq` … | Jump through entries one at a time |
| **Pick** | `:error-list` (`:cl`) | A fuzzy-filterable picker of the flat list; `<CR>` jumps |
| **Group** | `:problems` (`:copen`) | The `*problems*` [multibuffer](multibuffer.md): excerpts grouped per file, **editable in place** |

`:error-list` opens the **picker** — the same fuzzy surface as
`:diagnostics`, good for filtering a large list by filename or message
before jumping.

`:problems` opens `*problems*`, a multibuffer that groups the entries as
source excerpts under each file's header (the surface project-search
uses). Fixing an error in an excerpt propagates to the source file.
`:problems-close` closes it, leaving the source buffers open. Both
`:error-list` and `:problems` echo `no error list` when there is nothing
to show.

---

## See also

- [Compilation mode](compilation.md) — running commands and the
  `*compilation*` / `*problems*` buffers.
- [Project search](project-search.md) — `:search` / `g/`, the
  interactive multibuffer grep (distinct from the error list).
- [Multibuffer](multibuffer.md) — the excerpt-composition surface
  `*problems*` is built on.
