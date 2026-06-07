# Lesson 1: Basic Motions

Welcome to the Lattice Tutor. Lattice is a powerful editor with a vim-style
modal grammar. This tutor teaches you the basics by having you practice on
this very file.

The tutor is editable — you can change anything you see. Don't worry about
messing it up; this file is a fresh copy, and you can re-open the original
by running `:tutor` again.

Approximate reading time: 8–10 minutes. Practice time: as long as you need.

---

## 1.1 Modes — Normal vs Insert

When you opened this file, you landed in **Normal mode**. In Normal mode,
your keys *don't* go into the buffer — they're commands that move the cursor
and edit the text.

To type text, switch to **Insert mode** by pressing `i`. Press `<Esc>` to
return to Normal mode when you're done.

**Try it:**

1. Move the cursor to the empty line below this paragraph.
2. Press `i` to enter Insert mode.
3. Type "Hello, lattice!"
4. Press `<Esc>` to return to Normal mode.

---> Practice on the empty line:




Notice the bottom of the screen: in Normal mode you see `-- NORMAL --`; in
Insert mode, `-- INSERT --`. Always check there if you're not sure where you are.

---

## 1.2 Moving the Cursor — h, j, k, l

In Normal mode, `h` `j` `k` `l` move the cursor:

```
     k             ↑
     |
 h ──+── l       ← →
     |
     j             ↓
```

Why `h`/`j`/`k`/`l`? They're under your right hand — no reaching for arrow keys.

**Practice:**

1. Make sure you're in Normal mode (press `<Esc>` if not).
2. Use `h` `j` `k` `l` to move the cursor around this line.
3. Move down to each "X" and remove it — press `x` on top of an X to delete
   the character under the cursor.

---> Move to each X and press x to delete:

XXXX hello XXXX world XXXX foo XXXX bar XXXX baz XXXX

The arrow keys also work in both Normal and Insert mode. But `h` `j` `k` `l`
are faster once you're used to them.

---

## 1.3 Word Motions — w, b, e

Moving one character at a time gets tedious. Word motions are faster:

```
w   jump to the next word's start
b   jump to the previous word's start
e   jump to the next word's end
```

**Practice:**

1. On the line below, press `w` repeatedly — watch the cursor jump word to word.
2. Press `b` to jump backward.
3. Press `e` to jump to word-ends.

---> Practice line:

the quick brown fox jumps over the lazy dog

---

## 1.4 Line Motions — 0, ^, $, gg, G

```
0    jump to column 0 (start of line)
^    jump to first non-blank of line
$    jump to end of line
gg   jump to first line of file
G    jump to last line of file
42G  jump to line 42 (replace 42 with any number)
```

**Practice:**

1. Press `$` to jump to the end of this line.
2. Press `0` to jump to column 0.
3. Press `^` to jump to first non-blank.
4. Press `gg` to jump to the top of this file.
5. Press `G` to jump to the bottom.
6. Press `6G` (six, then capital G) to jump to line 6.

---

## 1.5 Deleting — x, dd

```
x    delete the character under the cursor
dd   delete the entire line
D    delete from cursor to end of line
```

These are **operators** — they remove text and copy it to the "yank" buffer.
(We'll cover yanks next.)

**Practice:**

1. Move to a line below.
2. Press `dd` to delete the whole line.
3. Press `u` to undo the deletion.

---> Practice lines (delete with dd, undo with u):

delete me with dd
delete me too
and me as well

---

## 1.6 Copy/Paste — yy, p, P

```
yy   yank (copy) the current line
p    paste after the cursor
P    paste before the cursor
```

**Practice:**

1. Move to the line below this paragraph.
2. Press `yy` to yank it.
3. Move down a few lines.
4. Press `p` to paste below the cursor.
5. Press `u` to undo.

---> Yank this line:



---

## 1.7 Changing — c

The `c` operator deletes text and immediately enters Insert mode so you can
replace it.

```
cw   change a word (delete + Insert)
cc   change a whole line
C    change from cursor to end of line
```

**Practice:**

1. Move to the line below.
2. Press `cw` to change a word.
3. Type a replacement.
4. Press `<Esc>`.

---> Replace some words:

this is a placeholder line for practice

---

## 1.8 Undo/Redo — u, `<C-r>`

```
u       undo the last change
<C-r>   redo  (Control + r)
.       repeat the last change
```

**Practice:**

1. Make a small edit (e.g. delete a character with `x`).
2. Press `u` to undo.
3. Press `<C-r>` to redo.
4. Press `.` to repeat the last change.

---

## 1.9 Saving and Quitting

```
:w     write (save) the buffer
:q     quit
:wq    write and quit
:q!    quit, discarding changes
```

The `:` starts an ex-command (also called the "command line"). Everything
after the colon is parsed as a command.

In Lattice, **`:tutor`** runs this tutor — try it after you finish to start
a new copy.

**Practice:**

1. Press `:` to enter Command mode.
2. Type `w` and press `<Enter>` to save this file.
3. Press `:` again, type `e!` and press `<Enter>` to reload (discarding changes).

---

## 1.10 Help Is Always Available

Lattice has built-in help for everything:

```
:help                       index of every help topic
:help modal-editing         deep dive into the vim grammar
:help ex-commands           every : command
:help modes                 major + minor mode system
:help options               :set + :customize
:help lsp                   LSP integration
:describe-key gd            what does the chord `gd` do?
:describe-command write     metadata for a command
:options                    every customizable option
:list-modes                 every registered mode
```

Try one now: press `:` then type `help` and press `<Enter>`.

---

## End of Lesson 1

You've learned:

- **Modes:** Normal and Insert; `<Esc>` to return to Normal.
- **Motions:** `h` `j` `k` `l`, `w` `b` `e`, `0` `^` `$`, `gg` `G`.
- **Deleting:** `x` `dd` `D`.
- **Copy/paste:** `yy` `p` `P`.
- **Changing:** `cw` `cc` `C`.
- **Undo/redo:** `u` `<C-r>`; repeat with `.`.
- **Saving:** `:w` `:q` `:wq` `:q!`.
- **Help:** `:help [topic]`.

Next steps:

1. Read `:help modal-editing` — the grammar in full (operators + motions + text objects + counts).
2. Read `:help modes` — the major/minor mode system.
3. Try `:customize` — the interactive options form.

When you're ready, run `:tutor` again for a fresh copy, or move on to your
real work.

*Happy editing.*
