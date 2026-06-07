# Lesson 3: Visual Mode, Registers, Search, Macros

Lesson 2 taught the grammar: operators + motions + text objects + counts.

Lesson 3 covers the remaining editing power: Visual mode for selecting
arbitrary regions, registers for managing multiple clipboards, search
and substitution, and macros for recording and replaying sequences.

---

## 3.1 Visual Mode — select, then operate

Visual mode lets you select a region first and apply an operator second.
There are three kinds:

```
v       Charwise visual (character-by-character selection)
V       Linewise visual (whole lines)
<C-v>   Blockwise visual (a rectangular column block)
```

Once you have a selection, any operator applies to it:

```
d   delete the selection
y   yank the selection
c   change the selection (deletes it and enters Insert)
>   indent the selection
<   unindent the selection
=   re-indent the selection
gU  uppercase the selection
gu  lowercase the selection
~   toggle case of the selection
```

Press the same visual key again to exit back to Normal. `o` in Visual mode
moves the cursor to the other end of the selection.

**Exercise 1:** Press `V` to enter Linewise Visual, press `j` to extend
the selection to the next line, then `d` to delete both lines.

--->  Delete this line
--->  and this one too.
--->  But leave this line.

**Exercise 2:** Press `v`, move to the end of "world", then type `gU`
to uppercase the selected text.

--->  hello world

**Exercise 3:** Press `<C-v>`, move right 4 characters and down 2 lines
to select a column block. Then type `I` (capital I), type `## `, and press
`<Esc>`. The prefix is inserted on all selected lines.

--->  item one
--->  item two
--->  item three

---

## 3.2 Registers — multiple clipboards

Every yank and delete goes into a register. The unnamed register `"` holds
the most recent change. Named registers `a`–`z` let you keep multiple values
at once.

```
"a y y    yank current line into register a
"a p      paste from register a below the cursor
"A y y    APPEND current line to register a  (capital = append)
```

Special registers:

```
"0   the last yank (not overwritten by deletes)
"+   the system clipboard (works with your desktop copy-paste)
"*   the primary selection (X11/Wayland middle-click buffer)
"_   the black hole — sends text nowhere (a silent delete)
".   the last inserted text (read-only)
"%   the current file path (read-only)
":   the last ex command run (read-only)
```

Run `:registers` to show all current register contents.

**Exercise 1:** Yank the first practice line into register `a` with
`"ayy`, then move to the blank line below and paste with `"ap`.

--->  Register a holds this line.

--->  (paste here)

**Exercise 2:** Use `"+yy` to yank the line to the system clipboard
so you can paste it in another application.

--->  This line will go to the system clipboard.

**Exercise 3:** Delete the word "secret" below *without* placing it in
the unnamed register. Use `"_diw` so it vanishes cleanly.

--->  There is a secret word here that should disappear silently.

---

## 3.3 Search — /, ?, n, N, *, #

```
/ PATTERN <Enter>   search forward for PATTERN
? PATTERN <Enter>   search backward for PATTERN
n                   repeat the search in the same direction
N                   repeat the search in the opposite direction
*                   search forward for the word under the cursor
#                   search backward for the word under the cursor
```

Patterns are regular expressions. Common patterns:

```
/foo\|bar     match "foo" or "bar"
/\bword\b     whole-word match
/^begin       line starting with "begin"
/end$         line ending with "end"
```

Run `:nohlsearch` (or `:nohl`) to clear the search highlight.

**Exercise 1:** Type `/lazy<Enter>` to jump to the word "lazy" below.
Then press `n` to move to the next occurrence, `N` to go back.

--->  The lazy dog jumped over the lazy fox near the lazy river.

**Exercise 2:** Place the cursor on the word "unique" and press `*` to
search for all occurrences of that word.

--->  This unique word is unique in a unique way.

---

## 3.4 Substitution — :s

The substitute command replaces text matching a pattern:

```
:s/old/new/        replace first match on current line
:s/old/new/g       replace all matches on current line
:%s/old/new/g      replace all matches in the entire file
:%s/old/new/gc     replace all, confirm each one interactively
:5,10s/old/new/g   replace on lines 5 through 10
```

In the replacement string:

```
&    inserts the entire match
\1   inserts capture group 1
\U   uppercase the text that follows
\L   lowercase the text that follows
```

Examples:

```
:%s/colour/color/g           British to American spelling
:%s/\bfoo\b/bar/g            whole-word replacement
:%s/\(\w\+\)/[\1]/g          wrap every word in brackets
```

**Exercise 1:** Place the cursor on the line below and run `:s/cat/dog/`
to replace the first occurrence of "cat".

--->  The cat sat on the cat mat.

**Exercise 2:** Run `:s/cat/dog/g` to replace *all* occurrences on the line.

--->  The cat sat on the cat mat again.

---

## 3.5 Macros — record and replay sequences

A macro records a sequence of Normal mode commands and lets you replay
it on demand.

```
q a       start recording into register a  (any letter a–z)
... do editing ...
q         stop recording
@ a       replay the macro in register a
@ @       replay the most recently run macro
5 @ a     replay register a five times
```

In Lattice, macros record `CommandInvocation` sequences rather than raw
keystrokes. This means recorded macros survive keymap remapping and are
editable as data in a buffer-backed view.

A practical pattern — add a semicolon to the end of each line:

```
q a   start recording into a
$     jump to end of line
a ;   append a semicolon (enters Insert, types ;)
<Esc> back to Normal mode
q     stop recording
j @ a apply to the next line; continue with j @@ for more
```

**Exercise:** Record a macro that uppercases the first word of a line
(`0 g U w`), then replay it on each practice line below using `@a`
and then `j @@` to step through them.

--->  first line to fix
--->  second line to fix
--->  third line to fix

---

## 3.6 Marks — named positions

Marks save positions for quick return.

```
m a       set mark a at the current cursor position  (a–z = local)
' a       jump to the beginning of the line with mark a
` a       jump to the exact character position of mark a
m A       set global mark A  (A–Z = cross-file; opens the file)
' A       jump to global mark A, opening the file if needed
```

Special marks (read-only, set automatically):

```
' '  or ` `   the position before the last jump
' .  or ` .   the position of the last change
' ^  or ` ^   the position of the last Insert exit
```

Run `:marks` to list all current marks.

**Exercise:** Type `ma` on the first practice line to set mark `a`.
Move to a different part of this document, then type `` `a `` to
jump back to the exact character.

--->  Mark this position.

---

## Summary

| Feature | Key commands |
|---------|-------------|
| **Visual** | `v` char · `V` line · `<C-v>` block; `o` swaps ends |
| **Operators** | `d` `y` `c` `>` `<` `=` `gU` `gu` `~` apply to selection |
| **Registers** | `"x y` / `"x p` — named `a`–`z`; `"+` clipboard; `"_` black hole |
| **Search** | `/` pat · `?` pat · `n` · `N` · `*` · `#` |
| **Substitute** | `:s/old/new/[flags]` · `:%s` whole file · `c` to confirm |
| **Macros** | `qa … q` record · `@a` replay · `@@` repeat |
| **Marks** | `mx` set · `` `x `` or `'x` jump · `A`–`Z` cross-file |

Continue with Lesson 4: The mode system, emacs-style help, and customization.
