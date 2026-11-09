---
summary: "llm-context-mode: *llm-context*, what requests from a buffer carry and where each item stands now. dd on a row removes it. Read-only; updates itself."
related: [llm-guide, llm-mode, llm-inspect-mode, llm-requests-mode, context]
---

# llm-context-mode

`:llm-context` (`<leader>aC`) opens `*llm-context*`, the context attached to the buffer it
ran in: each attached buffer, span of lines and file, with where it stands now (a span's
current lines, a buffer that has since closed). Context is attached with
`<leader>ac{motion}` or `:llm-context-add`; see [`llm-guide`](help:llm-guide).

| Keys | Effect |
|---|---|
| `dd` | Remove the item on this line. A header or a stale row removes nothing. |

The list is read-only and keeps itself current: attaching from another window, or an item
going stale, shows without a keystroke. An item that cannot be sent as listed (its buffer
closed, its lines deleted, its file unreadable or now matching `llm.exclude`) refuses the
request with the reason; it is never sent without the item.
