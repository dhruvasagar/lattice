# Lesson 5: Splits, Buffers, Search, Diff, and LSP

Lessons 1–4 covered vim editing, the grammar, visual mode, registers,
macros, the mode system, and the emacs-style help map.

Lesson 5 covers Lattice's own feature set: splits and panes, the buffer
list, multibuffer search, diff, and LSP.

---

## 5.1 Splits and Panes

Lattice lets you view multiple buffers at once by splitting the window
into panes.

```
:vsplit      open a vertical split       (alias: :vs)
:split       open a horizontal split     (alias: :sp)
:vsplit FILE open FILE in a vertical split
:split FILE  open FILE in a horizontal split
```

Navigate between panes:

```
<C-w> h      move to the pane on the left
<C-w> j      move to the pane below
<C-w> k      move to the pane above
<C-w> l      move to the pane on the right
<C-w> w      cycle to the next pane
```

Resize panes:

```
<C-w> +      increase height
<C-w> -      decrease height
<C-w> >      increase width
<C-w> <      decrease width
<C-w> =      equalize all pane sizes
```

Close a pane:

```
:q           close the current pane  (buffer stays in the list)
:only        close all other panes, keep this one
```

**Exercise 1:** Type `:vsplit` to open a second pane. Use `<C-w>v` to split.

---> Press <C-w> v to open a vertical split.

**Exercise 2:** Navigate between panes with `<C-w>w`.

---> Press <C-w> w to cycle between panes.

---

## 5.2 The Buffer List — :ls, :b, :bn, :bp

Every open file (and every built-in buffer) lives in the buffer list.
Lattice separates "which buffer is in this pane" from "which buffers exist."

```
:ls              list all buffers
:b NAME          switch to buffer NAME (prefix match or exact)
:b NUMBER        switch to buffer by number
:bn              next buffer in the list
:bp              previous buffer in the list
:bd              close (delete) the current buffer
:b#              alternate buffer (the last one you were in)
```

Buffer flags shown in `:ls`:

```
%    the current buffer
#    the alternate buffer
+    modified (unsaved changes)
-    not modifiable (read-only)
h    hidden (not displayed in any pane)
```

In Lattice, **all** buffer kinds share the same list: regular files, the
help buffer, the terminal, the multibuffer search results — everything.
`:bn` / `:bp` and `:b N` operate uniformly across all of them.

**Exercise 1:** Run `:ls` to see the buffer list.

---> Run :ls to see the buffer list.

**Exercise 2:** Run `:b 1` to jump to the first buffer.

---> Run :b 1 to jump to the first buffer.

---

## 5.3 Project Search — :search

`:search PATTERN` opens a multibuffer: a single buffer that shows matching
excerpts from across the project, each in context.

```
:search TODO          — find all TODOs in the project
:search "fn main"     — find all main functions
:search -t rs TODO    — search only in .rs files
```

Inside the multibuffer results buffer:

```
]e   jump to the next excerpt (next match)
[e   jump to the previous excerpt
<CR> jump into the source file at that location
q    dismiss (close) the multibuffer
```

The multibuffer is a live buffer. You can edit the source files directly
from inside it — the excerpt expands to show context and edits propagate
back to the source.

**Exercise:** Run `:search fn main` to find all definitions.

---> Run :search fn main  to find all definitions.

---

## 5.4 Diff — :diffthis, ]c, [c, do, dp

Lattice has built-in diff support. To diff two buffers side by side:

1. Open two buffers in split panes (`:vsplit`)
2. In each pane, run `:diffthis` to mark it as part of the diff

Or open a file with `:diffsplit FILE` to open FILE in a new pane and
start the diff automatically.

Once a diff is active:

```
]c   jump to the next change hunk
[c   jump to the previous change hunk
do   diff obtain — pull the change from the other buffer INTO this one
dp   diff put   — push the change FROM this buffer to the other one
```

The gutter shows change markers:

```
+  green    added lines
-  red      deleted lines
~  yellow   changed lines
```

Run `:diffoff` to stop the diff on the current buffer, or `:diffoff!` on all.

**Exercise 1:** Run `:diffthis` in a modified buffer to see diff highlights.

---> Run :diffthis in a modified buffer to see diff highlights.

**Exercise 2:** Navigate diff hunks.

---> Press ]c to jump to the next changed hunk.

---

## 5.5 LSP — go-to-definition, hover, references, rename

When an LSP server is attached to a buffer, these bindings activate:

```
g d      go to definition
g D      go to declaration
g r      go to references  (opens a multibuffer of all sites)
g i      go to implementation
g t      go to type definition
K        hover documentation  (float popup)
<C-k>    signature help  (parameter hints while typing a call)
```

Refactoring:

```
:lsp-rename         rename the symbol under the cursor project-wide
:lsp-format         format the buffer using the LSP formatter
:lsp-code-actions   show available code actions at the cursor
```

Diagnostics navigation:

```
]d   jump to the next diagnostic
[d   jump to the previous diagnostic
]e   jump to the next error
[e   jump to the previous error
```

The gutter shows diagnostic severity icons next to affected lines.
The status line shows the count of errors, warnings, hints.

To see LSP status for the current buffer:

```
:describe-mode lsp-mode    — shows attached servers and capabilities
```

**Exercise 1:** Place the cursor on a symbol and press `gd`.

---> Place the cursor on a symbol and press g d.

**Exercise 2:** Press `K` to see hover documentation.

---> Place the cursor on a symbol and press K.

**Exercise 3:** Rename a symbol project-wide.

---> Place the cursor on a symbol and run :lsp-rename.

---

## 5.6 Everything Is a Buffer

A design principle: in Lattice, every surface is a buffer.

The file tree, diagnostics list, search results, help pages, terminal,
REPL output, LSP references — these are all buffers in the buffer list.
They open in panes like any other file. They respond to Normal mode
navigation (`j`/`k`/`gg`/`G`/`/`, etc.). You can split them, jump to
them by name with `:b`, include them in `:ls` output.

There are no fixed sidebars or bottom panels. The editor surface is
entirely composed of panes, each containing a buffer.

Built-in buffers you will encounter:

```
*messages*          editor messages and notifications
*lsp-log*           LSP server communication log
*scratch*           scratch pad (never saved, never killed)
[Help: TOPIC]       help topics from :help
[Search: PATTERN]   multibuffer search results
```

All the standard buffer commands work on them: `:b messages`, `:b scratch`,
`:bn`, `:bd`.

---

## Summary

| Feature | Commands |
|---------|----------|
| **Splits** | `:vsplit` / `:split` · `<C-w>hjkl` navigate · `:only` |
| **Buffers** | `:ls` · `:b NAME` · `:bn` · `:bp` · `:bd` · `:b#` |
| **Search** | `:search PATTERN` · `]e`/`[e` step · `<CR>` jump |
| **Diff** | `:diffthis` · `]c`/`[c` · `do` obtain · `dp` put |
| **LSP** | `gd` · `gr` · `K` hover · `:lsp-rename` · `:lsp-format` |
| **Diagnostics** | `]d`/`[d` · `]e`/`[e` |

Everything is a buffer: file tree, help, search results, terminal,
diagnostics — all in the buffer list, all in panes.

---

This is the end of the Lattice Tutor. You now know:

- **Lesson 1:** Modes, movement, basic editing, undo, saving
- **Lesson 2:** The grammar — operators, text objects, counts, dot
- **Lesson 3:** Visual mode, registers, search, substitution, macros
- **Lesson 4:** The mode system, emacs-style help, autocmds, init module
- **Lesson 5:** Splits, buffer list, project search, diff, LSP

Run `:help` to explore the full documentation.
Run `:apropos` to find anything you cannot name.
Press `<C-h> ?` for the help map reference.

*Good luck.*
