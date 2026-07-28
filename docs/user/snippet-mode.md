---
summary: "snippet-mode: the per-buffer snippet gate — C-x C-s expands directly, and it brings the snippet completion source with it."
related: [snippet, snippets, ex:customize]
---

# snippet-mode

The gate that decides whether snippets are available in a buffer.

It does two things:

- Binds `<C-x><C-s>` in Insert mode — **expand the snippet whose
  trigger is before the cursor**, directly, without going through the
  completion popup.
- Brings [`snippet-completion-mode`](help:snippet-completion-mode) with
  it, so snippet candidates appear in the completion popup alongside
  buffer words, tree-sitter symbols, and LSP results. You never
  activate that one separately.

Once a snippet is expanding, a third mode takes over the placeholder
navigation — see
[`active-snippet-mode`](help:active-snippet-mode).

## Which buffers get it

`snippet.activation` decides. The default is `global` — every writable
buffer. Set it to restrict snippets to the languages in
`snippet.languages`:

```
:set snippet.activation=languages
:customize snippet
```

Both options are real and registered, so `:customize snippet` lists
them with their current values.

## The three-mode split

It's worth knowing why this is three modes rather than one, because it
explains what `:describe-mode` will show you:

| Mode | Owns |
|---|---|
| `snippet-mode` | the gate + `<C-x><C-s>` |
| [`snippet-completion-mode`](help:snippet-completion-mode) | the completion source only |
| [`active-snippet-mode`](help:active-snippet-mode) | placeholder navigation, only while a snippet is in flight |

The last one is only active *during* an expansion, which is what lets
`<Tab>` mean "next placeholder" there without meaning that everywhere
else.

## See also

- [`completion`](help:completion) — the popup snippets appear in.
