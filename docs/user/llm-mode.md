---
summary: "llm-mode: ask a language model about any text, from any buffer, or have it rewrite text. <leader>aa{motion} asks about the motion's text; the answer opens beside you without taking the focus. <leader>ar{motion} rewrites it through a diff you accept or reject."
related: [llm, llm-chat-mode, ask, rewrite, operator, model, diff]
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
| `<leader>aA{motion}` | The same, opening [`:llm-menu`](help:llm) first |

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

### Choosing settings first

`<leader>aA` is the same operator, but it opens the settings menu before
the prompt. Change the model, the temperature or where the answer goes,
then press `a` to type the instruction. Choices made in the menu's
next-request scope apply to this ask only; in its session scope they stay
on the buffer you asked from. `q` dismisses the menu and sends nothing.

### Continuing a chat

An ask normally opens a new chat. To add it to one already open, name that
chat as the destination:

```
:'<,'>llm-ask destination=*llm:main.rs:1* and this part?
```

The text and the instruction become the next turn of that chat, with its
history, and it is shown beside you as a new answer would be. The `d` row
of `<leader>aA`'s menu sets the same thing. A name that is not an open chat
is refused. The `review` destination is not built yet: an ask that names
it opens a new chat and says so.

### Answers under the text

`destination=below` writes the answer into the buffer itself, on new lines
under the text you asked about, as it streams:

```
:'<,'>llm-ask destination=below write the doc comment for this
```

Keep working while it arrives. Text you type above it moves it down; text
you type where it is being written stays after it, so the answer is never
split by your typing. Your caret stays on the text it was on, and the view
follows the answer only if your caret is at its end.

- `u` stops it and takes back what was written, all at once if you typed
  nothing in between, or the latest part otherwise.
- Deleting the lines it is going under stops it; what was written stays.
- If the answer fails partway, what arrived stays and ends with an
  `[interrupted]` line; `u` removes both. If it fails before anything
  arrives, nothing is written and the echo area says why.
- One answer at a time at one place: a second ask there is refused until
  the first finishes.

A read-only buffer, or a view of several files, cannot take an answer: it
opens in a new chat instead, and the echo area says why. The full answer is
also kept in a hidden chat, so it can be followed up like any other.

### Answers in the echo area or a register

`destination=echo` shows the answer in the echo area when it is complete.
The echo area is one line, so it shows the first line and says how many
more there are.

`destination=register` puts the whole answer in a register, ready for `p`.
Name the register before the operator, as for a yank: `"a<leader>aaj`
puts the answer in `a`. Without one it goes in the unnamed register; the
command-line form always uses the unnamed register. An answer of several
lines puts as lines, so `p` opens it under the caret.

Neither changes the buffer, and a failure only shows in the echo area.

### Seeing the full answer

`:llm-last` opens the latest ask's chat beside you: the whole answer of an
echo, register, `below` or rewrite request, with its history, ready to
follow up. The last 32 unshown chats are kept.

### Requests that may use tools

A model calling tools needs somewhere to show the call and ask your
approval, which only a chat has. A request whose settings allow tools
(`llm.tools`), aimed at `below`, `echo` or a register, opens in a new chat
instead, and the echo area says so.

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

## Rewriting text

`<leader>ar` is an operator that changes text, and it never changes it
directly: the answer opens as a **diff** for you to accept or reject.

| Keys | Rewrites |
|---|---|
| `<leader>arip` | The paragraph |
| `<leader>arr` | This line |
| `<leader>ar` in Visual | The selection |

A prompt takes the instruction (`make this a match`, `fix the grammar`);
`<CR>` sends, `<Esc>` sends nothing, and an empty instruction sends
nothing. The echo area says the rewrite is running; you can go on editing.
When the answer arrives, a diff opens in a new tab: on the left the text as
it was in your buffer, saved or not; on the right the proposal, which you
can edit. `:diff-accept` replaces the text with the right side, edits
included, as **one change**: one `u` undoes it. `:diff-reject` leaves the
buffer as it was. Nothing is written to disk either way; save when you
want to.

The rewritten text follows your edits while the model works: lines added
above it move it, as a mark would move. Then:

- **You changed the text itself** while the diff was open: accepting does
  not apply it. The diff opens again, now against the text as it is, with
  what you accepted as the proposal, so you see exactly what would be
  replaced. Accept that to apply it.
- **You deleted it:** nothing is applied, and the answer opens in its chat
  buffer so it is not lost.
- **You closed the buffer:** the same, the answer opens in its chat.

Models often wrap code in a <code>```</code> fence even when asked not to;
a reply that is one fenced block is unwrapped before the diff.

Each rewrite is a conversation, kept out of sight: its chat holds what was
sent and the full answer, and opens when a rewrite cannot be applied.

Rewrites queue: a second one waits for the first diff to be accepted or
rejected before its own opens, so a macro that rewrites several places
shows them to you one at a time.

A rewrite is refused up front, with the reason, in a read-only buffer, in
a view composed from several files (a search result: rewrite in the file),
over a block selection (`<C-v>`: one replacement cannot stand for a
column of pieces), and in a file `llm.exclude` names.

## Settings

The answer's chat starts from a **copy** of the asking buffer's settings
(its `:setlocal llm.*` values and anything `:llm-menu` set for the next
request), with the command's arguments on top. Nothing stays linked: change
the source afterwards and the answer keeps its settings, and the other way
round. `@profile` in the instruction picks a profile for this request;
only what you typed is read for it, never the text you are asking about.
A profile that would turn on tools cannot be confirmed from an ask: open a
chat with `:llm <profile>` instead.

## Files that are never sent

`llm.exclude` is a list of globs naming files whose text is never sent.
Asking from one is refused before the prompt opens, with the file and the
glob it matched. The default covers files that hold secrets by
convention:

```toml
[llm]
exclude = [".env", ".env.*", "*.pem", "*.key", "id_rsa*", "id_ed25519*", ".netrc"]
```

A glob without a `/` matches the file name in any directory; one with a
`/` matches the whole path. Setting it **replaces** the list, so copy the
default if you want to add to it. `:set llm.exclude?` shows the current
list, and an empty list excludes nothing.

## Large selections

A selection over 32 KB (about 8,000 tokens) asks before it is sent, naming
its size. `y` sends it; anything else sends nothing.

## Read-only buffers and other sources

Asking reads text and changes nothing, so it works where `d` and `c` are
refused: `*compilation*`, `*messages*`, help pages. A block selection
(`<C-v>`) is sent as one span of text, not row by row.

## Repeating

`.` does not repeat an ask, and does repeat a rewrite. It repeats your last *change*, and an ask is
not one, in the same way `y` is not; it would also re-send a prompt you
typed once.

A rewrite is a change, so `.` repeats it: over the same motion, from where
the caret is now, with the prompt already holding the instruction you
typed last time. `<CR>` sends it, or edit it first; `<Esc>` sends nothing.
Each `.` opens its own diff.

A macro that asks or rewrites runs it and
moves on: the request runs in the background and the macro does not wait
for the answer; its rewrites' diffs queue.

## See also

- [`llm-chat-mode`](help:llm-chat-mode) — the answer and chat buffer.
- [`llm`](help:llm) — providers, profiles and the `llm.*` options.
- [`llm-system-mode`](help:llm-system-mode) — editing a system prompt.
