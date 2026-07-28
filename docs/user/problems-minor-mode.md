---
summary: "problems-minor-mode: marks a multibuffer as the *problems* view — :copen groups error locations as editable excerpts, one per entry, grouped by file."
related: [problems, copen, cclose, ex:copen]
---

# problems-minor-mode

The mode that marks a [multibuffer](help:multibuffer-mode) as the
**problems view**: `:copen` takes the current
[error list](help:error-list) and composes it into one buffer, each
entry appearing as a small anchored excerpt of its source file with a
couple of lines of context, grouped by file.

`:cclose` closes it.

## Why it's a view and not a list

The plain [error list](help:error-list) is a list of locations you walk
with `:next-error`. The problems view is the same data as *source
text*, which means it is **editable in place** — you fix the error
where you're reading it, and the edit propagates back to the real file.
No jumping to the file, fixing, and jumping back.

That is the multibuffer substrate doing the work; this mode only marks
which multibuffer is the problems one. The host's `:cclose` guard reads
that marker to know what to close.

## Behaviour worth knowing

- **Editable.** The mode contributes no read-only override, precisely
  so edits reach the source.
- **Excerpts carry two lines of context** either side of each error
  location — enough to see what the error is about without pulling in
  the whole file.
- **It's a pure marker.** No chords of its own yet; a `q`-to-close
  binding is a tracked follow-up. Everything you can do in the buffer
  comes from [`multibuffer-mode`](help:multibuffer-mode) and the
  ordinary vim grammar.

## See also

- [`error-list`](help:error-list) — the underlying list and
  `:next-error` / `:previous-error`.
- [`multibuffer-mode`](help:multibuffer-mode) — excerpt composition and
  how edits propagate.
- [`compilation-mode`](help:compilation-mode) — where the entries
  usually come from.
