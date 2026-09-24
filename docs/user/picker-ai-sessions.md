---
summary: "picker-ai-sessions: choose which AI agent session's log to open, when more than one is running."
related: [ai-log]
---

# The AI session picker

`:ai-log [provider]` opens an agent session's log. When more than one
session matches, this picker asks which; one match opens straight away,
and none says so (with a hint to start one, e.g. `:opencode`).

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by provider or session number |
| `<CR>` | Open that session's log |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<Esc>` / `<C-c>` | Close |
| `<C-h>` | This page |

`<C-q>` has no location to send. `<C-l>` and `<C-d>` do nothing, and
there is no preview.

---

## What is listed

One row per session, as `provider:index` — `opencode:0`,
`opencode:1`. The provider you passed (`:ai-log opencode`) narrows the
list before it opens.

---

## See also

- [`ai-log-mode`](help:ai-log-mode) — the log buffer this opens.
- [`opencode-mode`](help:opencode-mode) — an agent session.
