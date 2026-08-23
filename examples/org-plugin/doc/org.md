# org

Org files (`.org`, `.org_archive`) get syntax highlighting and folding from
this plugin. It contributes the language through lattice's `language` seam —
the grammar and its queries ship inside the plugin, so unloading it takes org
support with it and leaves nothing behind.

> **Not yet wired as a `:help` topic.** This text is written and shipped, but
> a component implements exactly one WIT world, and a plugin providing both
> `language` and `help` needs a combined world — which external plugins cannot
> yet declare. See the LG.6 note in `implementation.md`. Until then, read it
> here.

## Headlines

Each level gets its own size and colour, `*` through `******`:

```org
* Top level
** Second
*** Third
```

The stars stay at body size while the title scales, sharing one baseline —
the same two-piece rendering markdown's `#` headings get. Beyond six stars a
headline keeps heading styling at level six; org has no depth limit, but the
theme's size ramp does.

This is the one place org's grammar is genuinely harder than markdown's.
Markdown gives each heading level its own node, so a query can name them.
Org's marker is a **single node whose text length is the level**, so the query
compares that text — `#eq?` predicates, which no built-in lattice query had
needed before org.

## Folding

`za` toggles, `zR` opens everything, `zM` closes everything — the ordinary
fold commands, because org folds through the ordinary fold pipeline.

What folds:

| | |
|---|---|
| headlines | the whole subtree beneath, including nested headlines |
| `#+BEGIN_…` / `#+END_…` | source, example, quote and dynamic blocks |
| `:PROPERTIES:` / `:END:` | property drawers and hand-rolled drawers |
| lists, tables, LaTeX environments | when they span more than one line |

Nested headlines fold independently, so closing a top-level headline hides
everything under it while its children keep their own fold state.

A headline with nothing under it is not foldable — there would be nothing to
hide, and a fold marker on every bare headline is noise.

## Source blocks

A `#+BEGIN_SRC` block is highlighted as a block and folds as one. Highlighting
the code *inside* it in its own language is injection, and is not wired yet.

## What this plugin is not

Editing. Headline promotion and demotion, subtree motion, TODO cycling,
visibility cycling, tables, agenda — none of that is here. Those belong to
org-mode the *major mode*, a separate piece of work that rides seams which
already exist (`modes`, `keymap`, `grammar`). This plugin is the language:
what an org file *is*, not what you do to it.
