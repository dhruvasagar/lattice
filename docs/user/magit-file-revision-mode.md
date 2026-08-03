---
summary: "magit-file-revision-mode: one file's content at one fixed reference — a commit SHA, or the index (`staged`). Read-only, never opened directly."
related: [magit, magit-file-revision]
---

# magit-file-revision-mode

## Getting here

| How | What |
|---|---|
| `C-c f` then `v` | The file you are visiting, at a revision you type |
| `:magit-find-file <rev> <path>` | Any file, at any revision |
| `<CR>` on a file in a diff / revision view | That file at that view's revision |
| `gj` / `gk` once here | Walk to the next / previous revision of it |
| `C-c f` then `V` | Back out to the **live** file, at the same line |

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

## Walking the file's history

| Key | Does |
|---|---|
| `gk` | this file at the **previous** revision (older) |
| `gj` | this file at the **next** revision (newer) |

Both step through the commits that touched *this file*, newest first —
commits that changed something else are skipped, so `gk` always lands
somewhere the content actually differs.

At either end you get a message rather than a jump: the history has two
ends, and wrapping silently from the first commit round to `HEAD` would
read as a glitch. You also get the message from a `staged` blob, because
the index is not a commit and has no place in the walk — open the file
at a real revision first.

Renames are not followed. The buffer name carries one path, and a step
that silently changed which file you were reading would be worse than
stopping.

magit binds these to `n` / `p`; lattice follows evil-collection-magit's
`gj` / `gk` remap, for the reason the remap exists — `n` is
search-repeat, and a read-only view of a file is exactly where you want
to search.

## Behaviour worth knowing

- **Everything else** — `q` / `gr` / section navigation — comes from
  [`magit-core-mode`](help:magit-core-mode); apart from the two history
  steps this is a plain read-only view, not an interactive one.
- **`gr` is a deliberate no-op** — a fixed ref's blob never changes.
- **Syntax highlighting works**, with the same grammar the file would
  get in your working tree — the language comes from the path in the
  buffer's name. A file type with no grammar shows as plain text,
  exactly as it would anywhere else. (Earlier versions of this page
  listed the absence of highlighting as a known limitation; it was
  fixed in MG.26c.)

## See also

- [`magit-revision-mode`](help:magit-revision-mode) — the commit this
  file's version belongs to.
