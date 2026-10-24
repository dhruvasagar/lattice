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
| `<C-n>` / `<C-p>` | On a choice: next / previous option |

Landing on a placeholder selects its default, so typing replaces it.
Placeholders move with the text, so `<Tab>` finds the next one after any
edit. The session ends at the final stop (`$0`, else the snippet's end);
the next `<Tab>` is an ordinary one.

## Why the scoping matters

In Insert, `<Tab>` inserts a tab or completes, so "next placeholder"
cannot own it globally. This mode binds it only while a snippet is in
flight; when the expansion ends, `<Tab>` means what it always did.

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
