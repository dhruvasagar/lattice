---
summary: "wit-mode: the major mode for WIT files (`.wit`) — tree-sitter highlighting and folds for the WebAssembly Interface Type language."
related: [languages, plugins]
---

# wit-mode

The major mode for **WIT** — WebAssembly Interface Types, the language plugin
APIs are declared in. Activates automatically on `.wit`.

It is here for a reason more direct than most bundled languages: WIT *is*
lattice's plugin API. Every seam a plugin implements is declared in a `.wit`
file, so anyone writing a plugin reads and edits these, and until this mode
existed they opened as plain text.

## What it gives you

- **Syntax highlighting** — declarations (`world`, `interface`, `record`,
  `enum`, `variant`, `flags`, `resource`, `type`) and their names, function
  names, parameter names, types, `@`-attributes and package versions.
  `///` doc comments style exactly as they do in Rust — lattice's syntax
  vocabulary has one comment style, so a doc comment is a comment.
- **Folds** — every braced item. A `wit/` file is often one long interface, so
  folding is how you see its shape at all. `za` toggles, `zR` / `zM` open and
  close everything.
- **Comment operators** — `gcc` and `gc{motion}` use `//`, `gC` uses `/* */`.

## What it does not give you

- **No formatter.** `:format` does nothing here. There is no consensus
  stdin-oriented WIT formatter — `wasm-tools component wit` rewrites a file in
  place, which is a different shape from the pipe every other language uses.
- **No attribute highlighting.** `@since(...)` and friends style through their
  constituent tokens rather than as a unit: in this grammar an attribute node
  *spans the doc comment attached to its item*, so capturing it whole paints the
  `///` line as an attribute.
- **No symbol list or text objects yet.** `:symbols` will not find WIT items.
  Both want query files this mode does not ship; they are additive when a real
  need appears.

## Notes

The highlighting query is lattice's own rather than the grammar crate's. The
upstream query names its captures in the TextMate vocabulary
(`entity.name.type.interface`), which lattice's theme resolver does not read —
against it, a `.wit` file renders entirely unstyled.
