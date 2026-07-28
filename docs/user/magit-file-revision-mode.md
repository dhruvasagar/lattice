---
summary: "magit-file-revision-mode: one file's content at one fixed reference — a commit SHA, or the index (`staged`). Read-only, never opened directly."
related: [magit, magit-file-revision]
---

# magit-file-revision-mode

One file, as it was at one fixed point: `*magit:file:<ref>:<path>*`,
read-only.

It exists to answer "what did this file actually look like *there*"
without you checking anything out. `<ref>` is either a real commit-ish
— the SHA shown in the buffer name — or the literal token `staged`,
meaning the index's blob for that path (`git show :<path>`) rather than
any commit.

The headerline reads `src/main.rs  @  a1b2c3d`, or `@  index` for a
staged blob. That row is load-bearing here: this buffer's *content*
looks exactly like the live file, so without the header there is
nothing on screen to tell you you're not editing the real thing.

## How you get here

Never directly — it is always the landing target of a `<CR>`:

| From | `<CR>` on | Lands at |
|---|---|---|
| [`magit-revision-mode`](help:magit-revision-mode) | a file in the commit | that file at that SHA |
| [`magit-commit-mode`](help:magit-commit-mode) | a file in the staged diff | that file at `index` |
| [`magit-diff-mode`](help:magit-diff-mode) (staged scope) | a file in the diff | that file at `index` |
| [`magit-status-mode`](help:magit-status-mode) | a file in Staged | that file at `index` |

The rule behind the table: when the buffer you came from describes a
*fixed* revision or the index, `<CR>` opens the file at that point.
When it describes current state, `<CR>` opens the live working-tree
file instead.

## Behaviour worth knowing

- **No mode-specific chords.** `q` / `gr` / navigation come from
  [`magit-core-mode`](help:magit-core-mode); this is a plain read-only
  view, not an interactive one.
- **`gr` is a deliberate no-op** — a fixed ref's blob never changes.
- **No syntax highlighting for the file's own language yet.** Synthetic
  buffers have no filename-based language detection wired up. A known
  limitation, not a silent failure.

## See also

- [`magit-revision-mode`](help:magit-revision-mode) — the commit this
  file's version belongs to.
