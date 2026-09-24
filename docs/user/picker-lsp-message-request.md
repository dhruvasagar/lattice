---
summary: "picker-lsp-message-request: a language server is asking you a question — pick an answer, or <Esc> to answer none."
related: [lsp]
---

# A language server's question

Sometimes a language server needs a decision — "Reload the workspace?",
"Which toolchain?" — and sends a message with buttons
(`window/showMessageRequest`). The buttons open here, titled
`[<server>] <the message>`.

---

## Keys in this picker

| Key | Here |
|---|---|
| `<CR>` | Answer with the selected choice |
| `<Esc>` / `<C-c>` | Answer **none** — the server is told you dismissed it |
| *(type)* | Filter the choices |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<C-h>` | This page |

Either way the server gets an answer, so it is never left waiting.
Nothing else does anything here.

---

## What is listed

The server's choices, numbered in the order it sent them: `1. Reload`,
`2. Later`. A message with no choices does not open a picker; it is
shown as a message and logged.

**One question at a time.** If a picker is already open when a server
asks, the question waits and opens when you are done; several waiting
questions open one after another.

---

## See also

- [`lsp`](help:lsp) — the language-server integration.
- [`lsp-log-mode`](help:lsp-log-mode) — where every server message is
  recorded.
