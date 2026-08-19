---
summary: "magit-nav-mode: the magit chords that are safe in any buffer — section navigation and folding, split out so editable magit views can have them without the bare letters."
related: [magit, magit-core-mode]
---

# magit-nav-mode

The magit chords that mean something **anywhere**, separated from the
ones that assume you are not editing.

| Key | Does |
|---|---|
| `]]` / `[[` | Next / previous section |
| `<Tab>` | Toggle the fold at the cursor |
| `<S-Tab>` | Cycle section visibility |

## Why it is separate

[`magit-core-mode`](help:magit-core-mode) claims bare letters — `i`,
`C`, `D`, `S`, `U`, `q` — which is only legitimate because every buffer
it attaches to is a **read-only list**. There is nothing else `i` could
have meant there.

That stops being true the moment a magit view is editable. The
[project diff](help:magit-project-diff-mode) is real source you can type
into, and it inherited those letters: `i` opened the `.gitignore` prompt
instead of entering Insert.

Navigating sections and folding do not make that assumption — they are
meaningful whether or not the buffer is editable. So they live here.
Read-only magit buffers get them through `magit-core-mode`, which implies
this mode; editable ones declare this mode alone.

## What you get where

- **Read-only magit buffers** — this mode plus `magit-core-mode`'s
  letters, exactly as before.
- **Editable magit views** — this mode, plus `gr` from
  [`refreshable-view-mode`](help:refreshable-view-mode), and the ordinary
  vim grammar for everything else.

## Options

None.

## See also

- [`magit-core-mode`](help:magit-core-mode) — the read-only chord set.
- [`magit`](help:magit) — the rest of the family.
