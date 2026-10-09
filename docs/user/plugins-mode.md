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
how many are quarantined, how many failed to load, and how many are running
an older build because their latest one did not compile. The last two are the
ones worth reading. A plugin that fails to load has no row in the table at
all, so the header is the only place it is visible; a plugin whose **build
failed** has a row and works, which is exactly why it is easy to miss —
nothing is broken, your edit simply did not take.

The second lists the chords, grouped by the lowercase/uppercase convention
below rather than one line per key.

Both rows stay put while you scroll, and neither is part of the buffer's
text — they cannot be yanked, and they do not shift which row the cursor is
on. A third row appears above them while a build is running and disappears
when it finishes.

Counts come from the same snapshot that renders the table, so the header
cannot disagree with what is underneath it.

## Colour

The table is coloured by meaning, not by syntax — and only where it earns
attention. A plugin's name stands out as the thing you scan for.
`quarantined` and a failed build are highlighted because they are the rows
you have to act on, and the name of a plugin that **failed to load** is
shown in the error colour in the section at the bottom — those have no row
in the table at all, so that line is the only place they appear. `ok` is
left plain, because a screen of healthy plugins does not need decorating
and colouring it leaves less contrast for the one that is broken. `user-installed` is emphasised over `bundled`, on the
grounds that you are more likely to be hunting for something you installed
yourself.

The colours are the editor's own semantic ones — the same greens, reds and
accents `:help`, diagnostics and diffs use — so the view follows your theme
rather than carrying a palette of its own.

## Chords

**Lowercase acts on the row under the cursor; uppercase acts on every row.**

| Chord | Action |
|---|---|
| `<CR>` or `K` | Describe the plugin under the cursor — its documentation and the commands it contributed |
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

The description shows the plugin's **own manual** — the page it registered
through the `help` seam, the same text `:help <name>` opens — followed by every
command it contributed and a link to that page as a full help buffer. A plugin
that ships neither a manual nor a `doc` line in its manifest is the only case
that reports no documentation.

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

## When something is wrong

Two sections appear under the table, and only when there is something to
put in them.

**Build failed** lists plugins that are loaded and running on their previous
artifact, because the latest build did not compile. The row reads
`build-failed`; this section says why.

**Failed to load** lists plugins that are not running at all — including
your `init.rs` when it has never built, and a plugin directory that has a
manifest but nothing loadable in it.

Both cover every plugin the editor builds, not only your config: `init.rs`,
the plugins it `require`s, and any plugin project under
`~/.config/lattice/plugins/` whose source sits beside its manifest (what
`--scaffold-plugin` writes). Those are compiled at start when their source has
changed, and `b` on the row rebuilds one on demand.

Under each name is the whole error, not a summary of it: for a build, the
compiler's own report; for a load failure, the cause chain and any trap
backtrace. It is laid out the way a terminal would show it —

```
  init  still running its previous build
      cargo build failed (exit status: 101)
      error[E0425]: cannot find value `Nope` in this scope
        --> src/lib.rs:47:30
         |
      47 |                 minor_modes: Nope,
         |                              ^^^^
```

— with the `error[…]` label in the error colour, the `-->` location as a
link, the gutter dimmed, and the carets in the colour of the diagnostic they
belong to (a warning's are a warning's). A backtrace gets the same treatment:
dim frame numbers, addresses as numbers, symbols as functions. The text
itself is plain, so you can yank any of it.

A long report is cut at eighty lines, keeping the **start** — the first
error is usually the cause and the rest follow from it — and says how many
lines were dropped and how to see them. Cargo's `Compiling …` progress lines
are left out.

When `init.rs` fails because the toolchain is missing rather than because
the code is wrong, the first line says so and names the command that fixes
it: see [what `init.rs` needs](help:init#what-you-need).

## Live status

The table reflects what the loader actually holds, not a snapshot taken
when you opened it. The view re-renders itself when a plugin loads, unloads
or crashes, and when a build starts, fails or finishes — so a plugin that
traps while you're looking at the list flips to `quarantined` in place, and
a `:reload-config` that fails to compile puts its error on screen without
you touching a key. Fix the source, reload, and the section goes away the
same way. You don't need `gr` to find out what happened.

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
