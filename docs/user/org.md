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

Build the component and drop it in your plugins directory:

```bash
git clone https://github.com/dhruvasagar/lattice-org-plugin
cd lattice-org-plugin
cargo build --release --target wasm32-wasip2
mkdir -p ~/.config/lattice/plugins/org
cp target/wasm32-wasip2/release/lattice_org_plugin.wasm \
   ~/.config/lattice/plugins/org/org.wasm
cp plugin.toml ~/.config/lattice/plugins/org/
```

Or point the plugin manager at a local checkout and let the editor build it on
first boot — see [`plugins`](help:plugins) for the loading model, capability
grants and the `:plugins` manager view.

Confirm it loaded with `:plugins`, and reach for `:plugin-trace` if it did not.

## What you get

| | |
|---|---|
| **Outline** | headline folding, promotion and demotion, structure motions, tree-sitter highlighting |
| **TODO workflow** | your own keyword sequences with fast-select keys and `(@)` / `(!)` logging, per-keyword colours |
| **Agenda** | `:org-agenda` — every dated headline across your files in one editable view, with filters, custom commands and a clock report |
| **Capture** | `<leader>oc` — templates that file a note without leaving what you were doing |
| **Habits** | repeating tasks with a consistency graph under their agenda row |
| **Roam** | a Zettelkasten layer: id links, backlinks, dailies |
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
- **`:help`** inside lattice — the plugin contributes its own help topics at
  load, so they describe the version you have installed.

This page is deliberately thin. Duplicating that reference here would put two
copies of the same text in two repositories that release on different
schedules, and the copy in the slower one would quietly become wrong.

## How it works, if you are curious

The design documents live in this repository, under
`docs/dev/architecture/org-*.md`, because they double as the worked example for
the plugin seams — the agenda drove the multibuffer scan-view seam, capture
drove cross-file writes, habits drove per-row annotations. They are written for
someone extending lattice, not for someone using org.
