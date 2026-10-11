---
summary: "plugin-output-mode: major mode for a plugin's output buffer — a read-only, live-tailing log a plugin fills while it works."
related: [plugins, plugin-trace-mode, core-plugins]
---

# plugin-output-mode

Major mode for a buffer a plugin writes its progress into. The install log
`*lsp-install:rust-analyzer*` and the server list `*lsp-servers*` are both
this mode.

## What it is for

Showing work that is happening in the background. A plugin that downloads
something, runs a tool or walks a tree reports into one of these buffers as
it goes, so the command that started the work can return at once and you can
keep editing — or watch.

## What you see

- **Lines**, added at the bottom as they are produced. The buffer follows
  along without your pressing anything.
- **A headerline** — the row pinned at the top — saying where the work is and
  how it ended: `⟳` while it runs, `✔` when it worked, `✗` when it did not.
  The reason for a failure is in the lines, usually on one beginning `error:`.

Closing the buffer loses nothing. The plugin keeps the lines (the most recent
10 000), so reopening it shows everything, including what arrived while it
was closed.

## It is an ordinary buffer

Read-only, and otherwise like any other: `/` searches it, every motion works,
and it can sit in a split beside the file you are working on. `:w` does
nothing — there is no file behind it.

A buffer in this mode has no keys of its own. A plugin may add some for a
particular buffer — the server list has `i`, `u`, `x` — and says which on the
buffer itself.

## Keybindings

None beyond the editor's own.

## See also

- [`plugins`](help:plugins) — loading plugins and the manager view.
- [`plugin-trace-mode`](help:plugin-trace-mode) — the other plugin log: what
  a plugin *called*, rather than what it chose to tell you.
- [`core-plugins`](help:core-plugins) — the plugins that ship with lattice,
  `lighthouse` among them.
