---
summary: "prompt-line-mode: the generic one-line prompt — whenever something asks you to type an answer, this is the buffer you type it in."
related: [prompt, minibuffer]
---

# prompt-line-mode

The generic one-line prompt. Whenever something in the editor needs a
typed answer — a new branch name, a value, a label — this is the buffer
you type it in.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Submit |
| `<Esc>` or `<C-c>` | Cancel |

## Generic on purpose

`command-line-mode` is tied to ex-commands and `search-line-mode` to
search patterns. This one is tied to nothing: the caller supplies the
label, any initial text, and where the answer should go. That's what
lets a feature ask a question without inventing its own input UI.

The clearest example is the branch-create wizard in
[`magit-branch-mode`](help:magit-branch-mode): a picker chooses the
base branch, then this prompt appears — `New branch name (from
<base>):` — for the name. Two different input shapes chained, neither
of them bespoke.

## It's a buffer

Like the `:` and `/` lines, the prompt is a real one-line document
focus-swapped in while you answer, so you get the full editing grammar
rather than a text field's arrow keys.

## See also

- [`command-line-mode`](help:command-line-mode) — the `:` line.
- [`search-line-mode`](help:search-line-mode) — the `/` line.
- [`picker`](help:picker) — the other way features ask you to choose.
