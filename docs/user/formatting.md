# Formatting, reflow and wrapping

Lattice keeps three jobs separate, because they are three different jobs:

| you want | verb | what it touches |
|---|---|---|
| fix this code's indentation | `=` | leading whitespace only |
| re-wrap this paragraph to my margin | `gq` / `gw` | line breaks inside a paragraph |
| run rustfmt / prettier / the language server | `:format` | anything it likes |

Pressing the wrong one is safe. `=` will never move a line break, and `gq`
will never re-indent your code.

## Re-wrapping text — `gq` and `gw`

`gq` and `gw` are **the same operator**. In vim they differ only in where the
cursor ends up afterwards; here the cursor always stays put, so you can use
whichever your fingers reach for.

They take a motion or a text object, like any operator:

```
gqq        re-wrap the current line
gqap       re-wrap the paragraph around the cursor
gqaC       re-wrap the comment block around the cursor
gqj        re-wrap this line and the next
gqG        re-wrap from here to the end of the file
gq         (in Visual mode) re-wrap the selection
```

`gww`, `gwap` and so on are identical. `gq` is *linewise* — like vim, it
formats the whole of any line a motion touches.

### What it does

Words are re-packed to fill each line up to [`textwidth`](#textwidth), and:

- **Comment markers are preserved.** A `///` block stays `///`, a `//!`
  block stays `//!`, a `#` block stays `#`. The marker is read from the lines
  themselves, so doc comments never quietly turn into ordinary ones.
- **Indentation is preserved.** An indented paragraph stays at its indent.
- **Lists hang.** A wrapped bullet's continuation lines line up under the
  item's text, not under the bullet:

  ```
  - the first line of a bullet that runs past the
    configured width wraps to here
  ```

  `-`, `*`, `+`, `1.` and `1)` all count.
- **Blank lines separate paragraphs**, and so does a bare comment marker —
  a lone `///` between two prose runs keeps them apart, so `gqaC` on a
  multi-paragraph doc comment does the right thing.
- **Fenced code is left alone.** Inside ```` ``` ````, `~~~` or
  `#+begin_…`, the line breaks *are* the content, so `gq` steps over them.
- **Long words are never split.** A URL longer than your margin overflows
  rather than being broken in half.

If a paragraph is already wrapped correctly, `gq` does nothing at all — not
even an empty undo step.

## Wrapping as you type — `autowrap`

```
:set autowrap=off          never wrap while typing
:set autowrap=comments     wrap comment lines only  (the default)
:set autowrap=all          wrap any line past textwidth
```

`comments` is the default for code: a long comment wraps, a long string
literal does not. Breaking a line of code mid-expression is destructive in a
way breaking a sentence is not.

**Prose file types set `all` for you** — Markdown, plain text, and git commit
messages (which also use `textwidth=72`, the usual convention). You do not
need to configure anything for those.

To turn it off in a buffer that sets it for you:

```
:setlocal autowrap=off
```

A *global* `:set autowrap=off` will not reach a Markdown buffer, because the
file type's own setting wins. This is the same as vim's ftplugin behaviour;
`:setlocal` is the escape hatch.

### `textwidth`

```
:set textwidth=100
:setlocal textwidth=72
```

The column both `gq` and `autowrap` aim at. It is always a real column —
there is no "off" value, because `autowrap=off` is how you turn wrapping off,
and `gq` should still have a target when you ask for it explicitly.

This is unrelated to [`wrap`](display.md), which is *soft* wrap — a display
setting that changes no bytes. The two work together: a buffer can soft-wrap
at the window edge while hard-wrapping at 80.

## Re-indenting — `=`

`=` re-indents each line in its range to the depth the syntax tree implies.
It adjusts leading whitespace and nothing else.

```
==         re-indent the current line
=ap        re-indent the paragraph
=i{        re-indent the contents of the enclosing block
gg=G       re-indent the whole file
```

Lines it has no answer for — inside a syntax error, inside a heredoc — are
left alone rather than guessed at.

`shiftwidth`, `expandtab` and `indentmethod` control what one level of
indent *is*; `:describe-option shiftwidth` has the details.

## Running a formatter — `:format`

```
:format              format the whole buffer
:'<,'>format         format a line range
```

`:format` walks a **chain** of formatters and uses the first one that is
actually available:

```
:set format.reformat=lsp,lang-default
```

The rungs are:

| rung | means |
|---|---|
| `lsp` | an attached language server that advertises formatting |
| `lang-default` | the built-in table — rustfmt, prettier, black, gofmt, clang-format, stylua, shfmt, taplo — each used only if it is on your `PATH` |
| `external:<command>` | any program that reads the buffer on stdin and writes the result to stdout |
| `native` | Lattice's own reflow engine, as a last resort for prose |
| `plugin:<id>` | a formatter contributed by a plugin *(not yet wired)* |

The default is `lsp,lang-default`. Reorder it to change which one wins:

```
" prefer prettier over whatever the server offers, for markdown
:setlocal format.reformat=external:prettier --stdin-filepath %,lsp

" markdown with no prettier installed still gets re-wrapped
:setlocal format.reformat=external:prettier --stdin-filepath %,native

" never use the language server to format
:set format.reformat=lang-default
```

If nothing in the chain applies, the message names every rung it tried and
why — `no formatter for rust: tried lsp (no server with formatting support),
lang-default (rustfmt not on PATH)`.

> A command in an `external:` rung may not contain a comma, since the comma
> separates rungs. Use a one-line wrapper script if you need one.

### Format on save

```
:set formatonsave
```

Runs the same chain before `:w`, minus the `lsp` rung — language servers
already get their turn through `willSaveWaitUntil`, and running both would
apply two opinions to one save.

**A formatter that fails, exits non-zero, or hangs never blocks the write.**
The buffer is saved unformatted and the failure is reported. A save that
silently did not happen is far worse than an unformatted one.

### Should the language server format when I press `gq`?

By default, no — and this is deliberate rather than an omission.

LSP has exactly one range operation, `rangeFormatting`, and it is a
*reformat*. There is no "reflow to my margin" request in the protocol. So
sending `gqap` to the server on a Rust doc comment does **nothing**
(rustfmt does not re-wrap comments by default), and on Markdown does
**nothing** (prettier's `proseWrap` defaults to `preserve`). It would fail
silently in exactly the cases you reached for `gq`.

If you know your server does what you want, say so:

```
:setlocal format.reflow=lsp,native
```

The same applies to `=` via `format.indent`. Both default to `native`.

> These two chains are declared and default to `native`; routing them to a
> non-native rung is not wired yet. `format.reformat` above is fully live.

## Replaced settings

| old | now |
|---|---|
| `formatprg=<cmd>` | `format.reformat=external:<cmd>,…` — the old key still works for now and says so once |
| `equalprg` | `format.indent=external:<cmd>` — the old key never did anything and has been removed |
| `formatoptions` `t` / `c` | `autowrap=all` / `autowrap=comments` |
