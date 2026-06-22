# Lesson 2: The Grammar

Lesson 1 covered the basics: modes, movement, delete, yank, change, undo.

Lesson 2 is about the grammar that makes vim feel like a language. Once you
understand the grammar, commands you have never seen before make sense at a
glance.

---

## 2.1 The Grammar — `[register] [count] operator [count] motion`

Every editing command in Normal mode follows one formula:

```
[register] [count] OPERATOR [count] MOTION / TEXT-OBJECT
```

The parts in `[brackets]` are optional. Here is the grammar in action:

```
d  w      delete a word forward        (operator + motion)
3  d  w   delete three words forward   (count + operator + motion)
d  3  w   same thing                   (count on the motion instead)
d  i  w   delete inner word            (operator + text object)
c  i  "   change text inside quotes    (operator + text object)
```

The insight: you do not memorise combinations. You memorise operators and
motions independently, then combine them freely.

**Exercise:** Place the cursor *anywhere* on the practice line and type
`d i w` to delete the word under the cursor. Then `u` to undo.

--->  The quick brown fox jumps over the lazy dog.

---

## 2.2 The Operators

The core operators:

```
d   Delete (cut to default register)
y   Yank (copy to default register)
c   Change (delete + enter Insert mode)
>   Indent right
<   Indent left
=   Re-indent (language-aware via tree-sitter)
g~  Toggle case
gU  Uppercase
gu  Lowercase
```

Every operator doubles as a line operator when you type it twice:

```
dd   delete current line
yy   yank current line
cc   change current line
>>   indent current line
==   re-indent current line
gUU  uppercase current line
```

**Exercise 1:** Place cursor on the line below, type `yy` to yank it,
then move down one line and `p` to paste it below.

--->  Duplicate me.

**Exercise 2:** Place cursor on the word "wrong" below and type `gUiw`
to uppercase the inner word, then `guiw` to lowercase it again.

--->  The wrong word stands out.

---

## 2.3 Text Objects

Text objects let you operate on semantic units regardless of where your
cursor sits within them.

```
i DELIMITER   inner — excludes the delimiters
a DELIMITER   around — includes the delimiters
```

Common text objects:

```
i w   inner word          a w   a word (includes trailing space)
i W   inner WORD          a W   a WORD (whitespace-separated)
i s   inner sentence      a s   a sentence
i p   inner paragraph     a p   a paragraph
i "   inside "..."        a "   "..." including the quotes
i '   inside '...'        a '   '...' including the quotes
i `   inside `...`        a `   `...` including the backticks
i (   inside (...)        a (   (...) including parens
i {   inside {...}        a {   {...} including braces
i [   inside [...]        a [   [...] including brackets
i <   inside <...>        a <   <...> including angle brackets
i t   inside <tag>...</tag>   a t   including the tags themselves
```

**Exercise 1:** Place cursor *anywhere* inside the string below, type
`c i "` to change the text inside the quotes. Type replacement text, then `<Esc>`.

--->  She said "replace this entire phrase" to nobody.

**Exercise 2:** Place cursor anywhere in the function call below, type
`d a (` to delete the parentheses and everything inside them.

--->  result = compute(some, arguments, here) + offset;

**Exercise 3:** Place cursor anywhere in the paragraph below, type
`y i p` to yank the inner paragraph. Then type `}` to jump past it
and `p` to paste a copy.

--->  This is a short paragraph.
      It has two lines.

---

## 2.4 Counts

A count before an operator or motion multiplies the action:

```
3 d w     delete 3 words
5 j       move down 5 lines
2 y y     yank 2 lines
4 >>      indent 4 lines
10 ==     re-indent 10 lines
```

Counts before the operator and counts before the motion both work;
they multiply together:

```
2 d 3 w   delete 6 words (2 × 3)
```

**Exercise:** On the practice line below, move to the first word and
type `3 d w` to delete three words at once.

--->  one two three four five six remaining.

---

## 2.5 The Dot Operator — repeat the last change

`.` (dot) repeats the last change — the full `[reg][count]op[count]motion`
unit, exactly as you typed it. This is one of vim's most powerful primitives.

After `d i w` deletes a word, pressing `.` deletes the next word under the
cursor. After `c i "` changes a string, pressing `.` changes the next string
the cursor lands on.

A typical workflow:

```
d i w   delete the first bad word
n       jump to next search match (or use any motion)
.       delete that word too
n .     and the next one
```

**Exercise:** Delete the word "REMOVE" below. Then use `/REMOVE<Enter>`
to search for the next occurrence, and press `.` to repeat the delete.

--->  Please REMOVE this word and also REMOVE the other one.

---

## 2.6 The Format Operator — =

`=` re-indents using the active language's rules (tree-sitter heuristics).
It follows the same grammar as every other operator:

```
= i {   re-indent everything inside the nearest braces
= i p   re-indent the current paragraph
= =     re-indent the current line
g g = G re-indent the entire file (gg moves to top; = G covers all)
```

**Exercise:** The block below is deliberately mis-indented. Place the
cursor anywhere inside the braces and type `= i {` to fix it.

--->  function hello() {
--->      const x = 1;
--->  const y = 2;
--->          return x + y;
--->  }

---

## Summary

```
Grammar:    [register] [count] OPERATOR [count] MOTION/TEXT-OBJECT

Operators:  d  y  c  >  <  =  g~  gU  gu
Doubles:    dd  yy  cc  >>  ==  (operator typed twice)

Text objs:  i/a  +  w W s p " ' ` ( { [ < t

Count:      prefix to operator or motion; they multiply
Dot:        . repeats the last complete change
```

The key insight: learn `d`, `y`, `c`, then learn text objects separately.
Every combination works without memorising each one individually.

Continue with Lesson 3: Visual mode, registers, search, and macros.
