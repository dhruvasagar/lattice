---
summary: "plugins-mode: the *plugins* manager buffer — a live status table of every loaded plugin, where lowercase acts on the row under the cursor and uppercase acts on every row."
related: [plugins, plugin, ex:plugins]
---

# plugins-mode

The `*plugins*` buffer: what's loaded, what state each plugin is in,
and the chords to act on them. `:plugins`.

This is the *manager view*. For what the plugin host is and how to
write or install a plugin, see [`plugins`](help:plugins).

## The header

Two sticky rows sit above the table.

The first counts the set: how many plugins are loaded, how many are running,
how many are quarantined, and how many failed to load. That last number is
the one worth reading — a plugin that fails to load has no row in the table
at all, so the header is the only place it is visible.

The second lists the chords, grouped by the lowercase/uppercase convention
below rather than one line per key.

Both rows stay put while you scroll, and neither is part of the buffer's
text — they cannot be yanked, and they do not shift which row the cursor is
on. A third row appears above them while a build is running and disappears
when it finishes.

Counts come from the same snapshot that renders the table, so the header
cannot disagree with what is underneath it.

## Chords

**Lowercase acts on the row under the cursor; uppercase acts on every row.**

| Chord | Action |
|---|---|
| `<CR>` or `K` | Describe the plugin under the cursor |
| `r` / `R` | Reload it / reload every loaded plugin |
| `b` / `B` | Rebuild it from source / rebuild every one |
| `u` / `U` | Update it / update every one |
| `x` / `X` | Unload it / remove staged directories nothing loads any more |
| `t` | Open its boundary trace |
| `T` | Cycle its trace verbosity |
| `gr` | Refresh the list |

`u`, `U`, `R` and `X` shadow vim's `u`, `R` and `x`-adjacent meanings inside
this buffer only. Nothing is lost: the table is read-only, so there is no edit
for undo to reverse and no text for Replace to overwrite.

## Reload, rebuild, update

Three different amounts of work, cheapest first:

- **Reload** re-instantiates the `.wasm` already on disk.
- **Rebuild** compiles that artifact from the source you already have — what
  you want after editing a local plugin, and what reload cannot do.
- **Update** fetches a newer source first, then rebuilds. A plugin pinned to a
  revision declines and says so: the pin is already the answer to which commit
  you wanted.

A bulk run works through the list one plugin at a time — `cargo` already uses
the whole machine, so running six at once would contend rather than go faster.
It says where it is on the title line (`# Plugins (7 loaded) — updating 3/7
(org)…`) while the row being worked reads `building…`. One plugin failing never
stops the rest; the counts and a line per failure land in `*messages*`.

## Cleaning up

`X` removes staged plugin directories that nothing loads any more. It is the
only chord here that deletes anything, so it names exactly what would go and
waits for you to confirm.

It will not offer you a plugin that **failed** to load — that is still one you
asked for, and removing it would turn "my plugin is broken" into "my plugin is
gone", taking the error message with it. It will not offer your `init` config.
And it will not offer a directory without a `.source` marker, because
provenance is what makes the removal recoverable: with it the plugin can be
fetched and built again, without it those bytes are the only copy.

`:plugin-clean` does the same from the command line — it lists, and
`:plugin-clean!` removes.

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
