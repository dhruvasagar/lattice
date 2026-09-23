Org-mode in lattice is a plugin, developed in its own repository:
[`dhruvasagar/lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin).
The editor knows nothing about it. There is no `BufferKind::Org`, no
`Lang::Org`, no `Editor::` method for any of it — everything org does, it does
through the same seams any plugin uses.

## What you get

| | |
|---|---|
| **Outline** | headline folding, promotion and demotion, structure motions, tree-sitter highlighting |
| **Lists** | insert, indent, move and cycle list items and checkboxes — under org's own meta-arrows, over a Visual region, and while typing |
| **TODO workflow** | your own keyword sequences with fast-select keys and `(@)` / `(!)` logging, per-keyword colours |
| **Agenda** | every dated headline across your files in one **editable** view — filters, custom commands, a clock report, and bulk actions |
| **Capture** | templates that file a note without leaving what you were doing; each capture is a draft file, so several can be open at once |
| **Habits** | repeating tasks with a consistency graph under their agenda row |
| **Roam** | a Zettelkasten layer — id links, backlinks, dailies, and notes that nest |
| **Clocking** | clock in and out, with the running clock in your modeline |

The agenda is a **multibuffer**: its rows are real excerpts of your files, so
editing a row edits the file and `:w` saves it. That is why it behaves like the
rest of the editor rather than like a list of strings.

## Why it is not bundled

Org's tree-sitter grammar is 2.2 MB of generated C, maintained outside
crates.io. A grammar that size is *the plugin's* build artefact, not the
editor's — vendoring it would mean every lattice build either carried the
weight or reached the network. So org is installed, deliberately, and always
will be.

## Getting it

Point the plugin manager at the repository and let the editor build it on
first boot, or build the component yourself and drop it in:

```sh
git clone https://github.com/dhruvasagar/lattice-org-plugin
cd lattice-org-plugin
cargo build --release --target wasm32-wasip2
mkdir -p ~/.config/lattice/plugins/org
cp target/wasm32-wasip2/release/lattice_org_plugin.wasm \
   ~/.config/lattice/plugins/org/org.wasm
cp plugin.toml ~/.config/lattice/plugins/org/
```

That build needs Rust and nothing else — no checkout of lattice. Org depends
on the plugin API the way any crate depends on anything, by version:
`lattice-wit` carries the WIT package and `lattice-plugin-sdk` the typed
config helpers, both from crates.io.

Confirm it loaded with `:plugins`, and reach for `:plugin-trace` if it did
not.

## The reference implementation

If you are deciding whether lattice's plugin API is deep enough to build on,
org is the honest answer. It contributes across more than a dozen seams:

- a whole **language** — `.org` and `.org_archive`, its own tree-sitter
  grammar, highlight and fold queries
- four **modes** — `org-mode` plus the minors for TODO, tables, the agenda
  and global bindings
- a complete editing **grammar** — promote and demote, subtree moves,
  `]]` / `[[`, headline and subtree text objects, TODO and priority cycling,
  checkboxes, timestamps, links, table editing, the clock, archive, refile
  and capture
- **config** options, all namespaced `org.*` and settable with `:set`
- **theme** elements, one per TODO keyword, so `:colorscheme` recolours your
  own states
- the agenda's **scanned-excerpt source** and **multibuffer view source**
- **picker**, **completion** and **transient** sources
- **signs** and **decorations** for agenda bulk marks
- **events** for the clock's session and its modeline segment
- its own **help** pages, compiled into the component — `:help org`,
  `:help org.roam`, describing the version you actually installed

All of that from a separate repository, with **not one line in lattice's own
tree**. Nothing in lattice knows what a headline is: the host changes org
needed were generic, and none of them names org.

## Documentation

- [Org-mode](@/docs/config/org.md) — installing it, the `org.*` options, and
  where the full reference lives
- `doc/org.md` and `doc/roam.md` in the plugin repository — the complete
  reference, versioned with the code
- `:help org` and `:help org.roam` inside the editor
- [Plugins](@/docs/config/plugins.md) — the loading model, capability grants
  and the seams, if you are writing one of your own
