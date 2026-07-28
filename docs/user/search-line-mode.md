---
summary: "search-line-mode: the / and ? search line as a real one-line buffer — full editing, C-p/C-n history, and BS on an empty pattern cancels."
related: [search, ex:nohlsearch]
---

# search-line-mode

The `/` and `?` search line. Not a text field — a real one-line buffer
you edit with the ordinary grammar, which is why a long or fiddly
pattern is no harder to fix than any other text.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Submit the pattern |
| `<Esc>` or `<C-c>` | Cancel |
| `<BS>` | Delete a character — **or cancel**, when the pattern is empty |
| `<C-p>` / `<C-n>` | Previous / next search history entry |

## Backspace on an empty pattern cancels

Vim's behaviour, kept: if you've deleted the whole pattern, one more
`<BS>` backs you out of searching altogether. It matches the intent —
you're erasing your way out — and saves a reach for `<Esc>`.

## History

`<C-p>` / `<C-n>` walk your previous searches. The history is a real
list, so a complex regex you built last week is a few presses away
rather than something to retype.

## It's a buffer

The pattern lives in a synthetic `*search-line*` document that's
focus-swapped in while you type. That's what gives you full editing —
word motions, registers, paste — inside the search line, and it's the
same arrangement [`command-line-mode`](help:command-line-mode) uses for
`:`.

## See also

- [`command-line-mode`](help:command-line-mode) — the `:` line, same
  substrate.
- [`modal-editing`](help:modal-editing) — search as a motion, offsets,
  and `n` / `N`.
