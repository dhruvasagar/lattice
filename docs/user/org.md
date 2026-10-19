---
summary: "Org-mode in lattice: what the org plugin gives you, how to install it, the `org.*` options, and where its full reference lives."
related: [org, agenda, capture, roam, habits, todo, outline, plugin]
---

# Org-mode

Org is **not built into lattice**. It is a WASM plugin developed in its own
repository, [`dhruvasagar/lattice-org-plugin`][repo], and the editor knows
nothing about it: there is no `BufferKind::Org`, no `Lang::Org`, no `Editor::`
method for any of it. Everything org does, it does through the same seams any
plugin uses — which is the point. Org is the deepest test of those seams, and
the reason several of them exist.

[repo]: https://github.com/dhruvasagar/lattice-org-plugin

## Installing it

The plugin is released as `vX.Y.Z` tags; its
[README](https://github.com/dhruvasagar/lattice-org-plugin#readme) names the
current one and its `CHANGELOG.md` says what each changed.

**Without building it.** Each release publishes org ready-built, so the
simplest install needs no Rust toolchain — in your `init.rs`:

```rust
plugin_manager::require_release(&ReleaseSpec {
    name: "org".to_string(),
    repo: "dhruvasagar/lattice-org-plugin".to_string(),
    version: None, // the newest release built for this editor
    sha256: None,
    enable_mode: None,
});
```

See [installing plugins from `init.rs`](help:plugins#installing-plugins-from-init-rs).

**Building it yourself.** Build a release and drop it in your plugins
directory:

```bash
git clone https://github.com/dhruvasagar/lattice-org-plugin
cd lattice-org-plugin
git checkout vX.Y.Z     # the release the README names; skip to build main
cargo build --release --target wasm32-wasip2
mkdir -p ~/.config/lattice/plugins/org
cp target/wasm32-wasip2/release/lattice_org_plugin.wasm \
   ~/.config/lattice/plugins/org/org.wasm
```

That one file is the whole plugin — its manifest is embedded in it. Or
`require` it from your config with `rev` set to a release tag, and let the
editor clone and build it on first boot — see [`plugins`](help:plugins) for the
loading model, capability grants and the `:plugins` manager view.

### Where org may write

Archive, refile and capture write into files other than the one you are in,
and org may do that only under two of its own options:

| Option | Default |
|---|---|
| `org.directory` | `~/org` |
| `org.roam-directory` | `~/org-roam` |

Point them at your notes and that is the permission — there is no separate
grant to edit, and org reloads itself when they change. A write anywhere else
is refused with a message naming the option. See [Capabilities and your
configuration](help:plugins#capabilities-and-your-configuration).

Confirm it loaded with `:plugins`, and reach for `:plugin-trace` if it did not.

## What you get

| | |
|---|---|
| **Outline** | headline folding, promotion and demotion, structure motions, tree-sitter highlighting |
| **Lists** | insert, indent, move and cycle list items and checkboxes — under org's own meta-arrows, over a Visual region, and while typing |
| **TODO workflow** | your own keyword sequences with fast-select keys and `(@)` / `(!)` logging, per-keyword colours |
| **Agenda** | `:org-agenda` — every dated headline across your files in one editable view, with filters, custom commands, a clock report, `l` for what you finished, and `m` / `x` to act on several entries at once |
| **Capture** | `<leader>oc` — templates that file a note without leaving what you were doing: into a headline, an outline path, today's date-tree node, or a row of a table. Each capture is a draft file, so several can be open at once, `:w` keeps one for later, and `<leader>oC` brings it back |
| **Habits** | repeating tasks with a consistency graph under their agenda row |
| **Roam** | a Zettelkasten layer: id links, backlinks, dailies, and notes that nest. A note created while you write another links into it, or back to it |
| **Clocking** | clock in and out, with the running clock in your modeline |

The agenda is a **multibuffer**: its rows are real excerpts of your files, so
editing a row edits the file and `:w` saves it. That is why it behaves like the
rest of the editor rather than like a list of strings.

## Configuration

Every option is namespaced `org.*` and settable with `:set` like any other:

```
:set org.agenda-files=~/org
:set org.todo-keywords=sequence: TODO NEXT(n) WAITING(w@/!) | DONE(d!) CANCELLED(c@/!)
:set org.habit-stats=false
```

`:options org.` lists them all with their current values and documentation,
which is more reliable than any table repeated here — the plugin registers them
at load, so `:options` is generated from the version you actually have.

## The full reference

The plugin ships its own documentation, and that is the authority: keymaps,
every command, capture template syntax, the agenda query language, roam, and
the habit rules. It versions with the code.

- **`doc/org.md`** in the plugin repository — the complete reference.
- **`doc/roam.md`** — roam has its own page: nodes, the index, backlinks,
  the journal, note templates, and nesting notes inside captures.
- **`:help`** inside lattice — the plugin contributes its own help topics at
  load (`:help org`, `:help org.roam`), so they describe the version you have
  installed.

This page is deliberately thin. Duplicating that reference here would put two
copies of the same text in two repositories that release on different
schedules, and the copy in the slower one would quietly become wrong.

## How it works, if you are curious

The design documents live in this repository, under
`docs/dev/architecture/org-*.md`, because they double as the worked example for
the plugin seams — the agenda drove the multibuffer scan-view seam, capture
drove cross-file writes, habits drove per-row annotations. They are written for
someone extending lattice, not for someone using org.
