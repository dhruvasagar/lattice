---
summary: "llm-mode: ask a language model about any text, from any buffer. <leader>aa{motion} asks about the motion's text; the answer opens beside you without taking the focus. :'<,'>llm-ask does the same from the command line."
related: [llm, llm-chat-mode, ask, operator, model]
---

# llm-mode

`llm-mode` puts a language model a motion away in every buffer: files,
`*compilation*`, `*messages*`, help, a terminal, a search result. It is on
everywhere; there is nothing to enable.

## Asking about text

`<leader>aa` is an **operator**, like `d` or `y`. Follow it with a motion
or a text object, and it asks about the text that covers:

| Keys | Asks about |
|---|---|
| `<leader>aaip` | The paragraph |
| `<leader>aaj` | This line and the next |
| `<leader>aaa` | This line |
| `<leader>aa` in Visual | The selection |

`<leader>` is `<Space>` unless you set `keymap.leader`.

A prompt opens for the instruction (`explain this`, `why does it panic?`).
`<CR>` sends; `<Esc>` sends nothing. The text is captured when you press
the motion, so editing while you type the instruction does not change what
is asked about. An empty instruction sends the text alone.

The answer opens in a **new buffer** beside the one you are in, and **the
focus stays where it was**: your caret, your mode and your pane do not
move. A second ask reuses that pane rather than splitting again, and the
first answer stays a buffer you can go back to with `:b` or `:ls`. Each
answer is named after where the text came from, `*llm:main.rs:4*`, the
number counting requests.

The answer buffer is a chat: [`llm-chat-mode`](help:llm-chat-mode). Its
first turn is what you sent, under a head line naming the file and lines,
folded when it is long. `gf` on it goes back to that place. Type a
follow-up in its prompt and `<CR>` continues the conversation.

### From the command line

`:'<,'>llm-ask explain this` asks about the range's lines, with the rest of
the line as the instruction. Without a range it asks about the current
line. Leading `key=value` words set this request only:

```
:'<,'>llm-ask model=local/qwen temperature=0 explain this
```

The keys are `model`, `profile`, `temperature`, `max_tokens` and the other
`llm.*` settings named in [`llm`](help:llm); `system` is not one of them,
because a system prompt is a paragraph and has `:llm-system`. A bad value
is named and nothing is sent.

## Settings

The answer's chat starts from a **copy** of the asking buffer's settings
(its `:setlocal llm.*` values and anything `:llm-menu` set for the next
request), with the command's arguments on top. Nothing stays linked: change
the source afterwards and the answer keeps its settings, and the other way
round. `@profile` in the instruction picks a profile for this request;
only what you typed is read for it, never the text you are asking about.
A profile that would turn on tools cannot be confirmed from an ask: open a
chat with `:llm <profile>` instead.

## Read-only buffers and other sources

Asking reads text and changes nothing, so it works where `d` and `c` are
refused: `*compilation*`, `*messages*`, help pages. A block selection
(`<C-v>`) is sent as one span of text, not row by row.

## Repeating

`.` does not repeat an ask. It repeats your last *change*, and an ask is
not one, in the same way `y` is not; it would also re-send a prompt you
typed once. A macro that asks runs the ask and moves on: the request runs
in the background and the macro does not wait for the answer.

## See also

- [`llm-chat-mode`](help:llm-chat-mode) — the answer and chat buffer.
- [`llm`](help:llm) — providers, profiles and the `llm.*` options.
- [`llm-system-mode`](help:llm-system-mode) — editing a system prompt.
