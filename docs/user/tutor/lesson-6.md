# Lesson 6: Advanced Editing — Text Objects, Narrowing, Folding, Soft-Wrap

Lessons 1–5 covered vim editing, the grammar, the mode system, and
Lattice's feature set. Lesson 6 is about working at the level of
*structure* instead of characters and lines.

Four ideas, each making large files feel small:

- **Tree-sitter text objects** — select a whole function, class, or
  argument by what it *is*, not by counting lines.
- **Narrowing** — collapse the whole file down to just the region you
  care about, edit it in isolation, then restore.
- **Folding** — hide the body of a section so you navigate by headings.
- **Soft-wrap** — wrap long lines visually and move by display line.

---

## 6.1 Tree-sitter Text Objects

Lattice's grammar understands the syntax tree of your code (via
tree-sitter). That means it offers text objects keyed to language
structure, not just to whitespace. They compose with every operator
(`d`, `c`, `y`, `v`, ...) exactly like `iw` / `ap` do.

```
af / if   a function   / inner function   (the fn/def + body / just the body)
ac / ic   a class      / inner class       (struct/enum/trait/impl/class)
aa / ia   an argument  / inner argument    (one parameter in a call or def)
al / il   a loop       / inner loop        (for / while / loop + body / body)
aC / iC   a comment    / inner comment      (a run of comment lines)
```

The `a` ("a"round) variant includes the delimiters / surrounding
syntax; the `i` ("i"nner) variant is just the contents. So:

```
daf   delete the whole function under the cursor
cif   change the function body, keeping the signature
yac   yank the entire class / struct
daa   delete one argument (and its comma) from a call or signature
vil   visually select the body of the enclosing loop
gqiC  reflow the comment block under the cursor (with an operator)
```

These resolve from the tree-sitter parse, so they work across every
language that ships a grammar — no per-language configuration.

**Exercise 1:** Place the cursor inside a function and delete it with `daf`.

---> Place the cursor inside a function and press d a f to delete it.

**Exercise 2:** Place the cursor on an argument and delete it with `daa`.

---> Place the cursor on an argument and press d a a to delete it.

**Exercise 3:** Visually select a class with `vac`.

---> Place the cursor in a class and press v a c to select it.

---

## 6.2 Narrowing — `zn`, `:narrow`, `:widen`

Narrowing collapses the editing surface to a single region: the rest
of the file disappears and you work in a focused, editable view. Edits
propagate back to the source. This is Emacs's `narrow-to-region`,
unified with Lattice's grammar so it is an *operator*.

`zn` is the narrow operator. It takes a motion or text object, just
like any operator:

```
znn   narrow to the current line
znip  narrow to the current paragraph
znaf  narrow to the function under the cursor (zn + the `af` text object)
znac  narrow to the class under the cursor
```

You can also narrow from the command line:

```
:narrow            narrow to the cursor's paragraph
:5,20narrow        narrow to a line range
:widen             restore the full source buffer
```

Combining narrow with the text objects from 6.1 is the payoff: `znaf`
gives you a buffer that *is* the current function, nothing else.

**Exercise 1:** Narrow to the function under the cursor with `znaf`.

---> Place the cursor in a function and press z n a f to narrow to it.

**Exercise 2:** Restore the full buffer with `:widen`.

---> Run :widen to restore the full buffer.

---

## 6.3 Folding — `za`, `zR`, `zM`, `zj`, `zk`

A fold collapses a contiguous range of lines into a single visual
line. The heading stays visible (with full syntax highlighting); the
body disappears from the visual flow. Motions and operators treat a
closed fold as one logical line.

Toggle and open / close folds:

```
za   toggle the fold under the cursor (open <-> closed)
zo   open the fold under the cursor
zc   close the fold under the cursor
zR   open every fold in the buffer
zM   close every fold in the buffer
```

Navigate between folds:

```
zj   jump to the next fold start
zk   jump to the previous fold start
```

Where folds come from is governed by `foldmethod`:

```
:set foldmethod=manual     only folds you create with zf (in Visual)
:set foldmethod=indent     fold wherever indentation rises
:set foldmethod=markdown   heading-based folds for *.md
:set foldmethod=syntax     tree-sitter scopes (the precise default)
```

To suppress folding entirely without losing your folds:

```
:set nofoldenable          render every line; fold data is preserved
```

**Exercise 1:** Toggle the fold under the cursor with `za`.

---> Press z a to toggle the fold under the cursor.

**Exercise 2:** Close all folds, then open them all again.

---> Press z M to close all folds, then z R to open them all.

**Exercise 3:** Jump between folds with `zj` and `zk`.

---> Press z j to jump to the next fold start.

---

## 6.4 Soft-Wrap — `:set wrap`, `gj` / `gk`, `g0` / `g$`

By default Lattice does not wrap long lines — they scroll horizontally.
Turn on soft-wrap to fold long lines into the viewport instead:

```
:set wrap        wrap long lines visually onto the next display line
:set nowrap       (default) long lines scroll horizontally
```

Soft-wrap is purely visual — it never inserts a newline into the
buffer. A wrapped line is still one buffer line, shown across several
*display lines* (segments).

That distinction matters for movement. `j` / `k` move by *buffer*
line; the `g`-prefixed motions move by *display* line:

```
gj   move down one display line (one wrap segment)
gk   move up one display line
g0   move to the first character of the current display line
g$   move to the last character of the current display line
```

So on a long wrapped line, `j` jumps over all of its segments to the
next buffer line, while `gj` steps one visual row at a time — exactly
what your eye expects.

**Exercise 1:** Turn on soft-wrap with `:set wrap`.

---> Run :set wrap to wrap long lines visually.

**Exercise 2:** Move down one display line with `gj`.

---> Press g j to move down one display line.

**Exercise 3:** Turn soft-wrap back off with `:set nowrap`.

---> Run :set nowrap to turn soft-wrap off again.

---

## Summary

| Feature | Keys / commands |
|---------|-----------------|
| **Text objects** | `af`/`if` fn · `ac`/`ic` class · `aa`/`ia` arg · `al`/`il` loop · `aC`/`iC` comment |
| **Narrow** | `zn` operator (`znaf`, `znip`) · `:narrow` · `:widen` |
| **Folding** | `za` toggle · `zR`/`zM` open/close all · `zj`/`zk` navigate · `:set foldmethod=…` |
| **Soft-wrap** | `:set wrap` / `:set nowrap` · `gj`/`gk` display-line move · `g0`/`g$` |

Text objects compose with every operator. Narrow + a text object
(`znaf`) gives you a buffer that is exactly one function. Folds and
soft-wrap make a large file navigable by structure and by sight.

Continue with Lesson 7: Customization — themes, options, and the modeline.
