---
summary: "active-snippet-mode: live only while a snippet is expanding — Tab and S-Tab walk its placeholders, Esc leaves the session."
related: [snippet, snippets]
---

# active-snippet-mode

Active **only while a snippet is expanding**. It owns the placeholder
navigation, and it goes away when the session ends.

## Chords

| Chord | Action |
|---|---|
| `<Tab>` | Next placeholder |
| `<S-Tab>` | Previous placeholder |
| `<Esc>` | Leave the snippet session, then exit Insert |

## Why the scoping matters

`<Tab>` is not a key you can casually rebind — in Insert it inserts a
tab or triggers completion, depending on context. Binding "next
placeholder" to it globally would be wrong; binding it only for the
duration of an expansion is exactly right.

That is what this mode buys: the chords exist while a snippet is in
flight and not one keystroke longer. Finish or abandon the expansion
and `<Tab>` goes back to meaning what it always meant.

## `<Esc>` falls through

`<Esc>` here does two things in order: ends the snippet session, then
continues to the *native* `<Esc>` (exit Insert). It doesn't shadow the
key — it augments it, so muscle memory keeps working and you don't need
two presses.

## See also

- [`snippet-mode`](help:snippet-mode) — the gate, and `<C-x><C-s>` to
  start an expansion.
- [`snippet-completion-mode`](help:snippet-completion-mode) — snippets
  as completion candidates.
