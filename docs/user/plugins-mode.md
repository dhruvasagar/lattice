---
summary: "plugins-mode: the *plugins* manager buffer — a live status table of every loaded plugin, with r to reload, x to unload, and t to open its boundary trace."
related: [plugins, plugin, ex:plugins]
---

# plugins-mode

The `*plugins*` buffer: what's loaded, what state each plugin is in,
and the chords to act on them. `:plugins`.

This is the *manager view*. For what the plugin host is and how to
write or install a plugin, see [`plugins`](help:plugins).

## Chords

| Chord | Action |
|---|---|
| `<CR>` or `K` | Describe the plugin under the cursor |
| `r` | Reload it |
| `x` | Unload it |
| `t` | Open its boundary trace |
| `T` | Cycle its trace verbosity |
| `gr` | Refresh the list |

## Live status

The table reflects what the loader actually holds, not a snapshot taken
when you opened it. The view subscribes to plugin-crash events, so a
plugin that traps while you're looking at the list flips to
`quarantined` in place — you don't need to `gr` to find out something
died.

That matters because crash isolation is the point of the WASM host: a
trapping plugin is contained rather than taking the editor with it, and
this buffer is where that containment becomes visible.

## Tracing a plugin

`t` opens the boundary trace for one plugin — the host↔guest calls it
makes, streamed to its own buffer. `T` cycles how much detail it
records. Tracing streams off the hot path, so a traced plugin doesn't
slow the editor's input loop.

## Behaviour worth knowing

- **Read-only, no file.** You can't edit the table and `:w` won't try
  to save it. The mode writes the content itself, off the actor
  thread, so a slow status read never blocks input.

## See also

- [`plugins`](help:plugins) — the plugin host: capabilities, fuel
  limits, crash isolation, and how to install one.
- [`init`](help:init) — configuring lattice in Rust/WASM, which loads
  through the same host.
