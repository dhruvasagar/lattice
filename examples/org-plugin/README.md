# org — the reference language plugin

The first real consumer of lattice's `language` seam
([`plugin-languages.md`](../../docs/dev/architecture/plugin-languages.md)),
and the shape a language plugin takes.

**This is reference source, not a bundled plugin.** It is not a workspace
member and `cargo build` never compiles it. Bundled plugins live in
`plugins/`; org deliberately does not, because org's grammar is 2.2 MB of
generated C maintained outside crates.io — and design §1's whole argument is
that such a grammar is *the plugin's* build artefact, not the editor's.
Putting it under `plugins/` would mean every workspace build either carried
that weight or reached the network.

## What it does

Registers one language:

| | |
|---|---|
| name | `org` |
| extensions | `.org`, `.org_archive` |
| grammar | [`nvim-orgmode/tree-sitter-org`](https://github.com/nvim-orgmode/tree-sitter-org), compiled to wasm by `build.rs` |
| queries | `queries/highlights.scm` |

That is the whole plugin. Everything else org needs — headline promotion,
subtree motion, TODO and visibility cycling, agenda, tables — is *org-mode the
major mode*, a separate track riding seams that already exist (`modes`,
`keymap`, `grammar`). It is gated on nothing here.

## Per-level headlines, and why they were the interesting part

Org's headline marker is **one node whose text length is the level**:
`(headline (stars) (item))`, where `stars` is `*`, `**`, `***`… Markdown's
grammar instead gives each level its own node (`atx_h1_marker` …
`atx_h6_marker`), so per-level capture there is six ordinary patterns.

Here the level has to come from the *text*, which means `#eq?` predicates —
and **no query bundled with lattice used one**, so this plugin is also what
proved the pipeline evaluates them. It does: tree-sitter's `QueryMatches`
filters on text predicates as it advances, so nothing host-side was needed.
`crates/lattice-syntax/tests/org_headlines.rs` pins it, including the
negative — that a headline carries *only* its own level, which is what fails
if predicates are ever silently dropped.

Upstream's own `highlights.scm` cycles three levels with `#match?` regexes
(`^(\*{3})*\*$` matches 1, 4 and 7 stars). This one wants true per-level 1–6,
and `#eq?` says that directly.

## Variable-font headlines come free

The stars are captured as `@punctuation.special` (→ `Style::Markup`, the same
style markdown's `#` markers take) and the title as `@text.title.N`. The GPUI
peer's `heading_scale_split` looks for the first run whose resolved
`scale > 1.0` and knows nothing about which grammar produced it — so
`[stars at base size][title scaled]` renders as two pieces on one baseline,
with **zero renderer changes**. Pinned by
`cells_paint::tests::org_headlines_scale_without_any_renderer_change`.

## Building it

`build.rs` clones the grammar into `grammar-src/` and builds it with the
repo's `scripts/build-wasm-grammar.sh` — **clang and a rustup toolchain
only**, no emscripten, no docker, no tree-sitter CLI. Offline it embeds empty
bytes; the host then rejects the registration with a named reason rather than
failing the build.

```sh
cd examples/org-plugin
cargo build --release --target wasm32-wasip2
```

The component lands at
`target/wasm32-wasip2/release/org_plugin.wasm`. Point a plugin directory at it
with a `plugin.toml`:

```toml
id = "org"
provides = ["language"]
```

In normal use the plugin manager does this for you from the git source
(PM.5–PM.8) and caches the build under `~/.config/lattice/plugins/`.
