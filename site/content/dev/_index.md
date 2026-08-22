+++
title = "Developer Documentation"
description = "Architecture, guides, operations, and references for Lattice contributors"
weight = 2
template = "section.html"
+++

Documentation for contributing to lattice and understanding its internals.

**New here? Read [Start here](./start-here/) first** — five pages in order:
contributor setup, the design spec, the architecture diagrams, the input
pipeline, and how the editor boots. Everything else assumes those.

The remaining sections are organised by the question you are asking, not by
where the file lives in the repo — so a section may mix a design fragment, a
guide and an audit when they cover the same subsystem.

- **Foundations** — modes, keymaps, buffers, the actor seam, configuration.
- **Editing & motions** — the vim grammar as implemented.
- **Rendering & display** — how a buffer becomes pixels, across both renderers.
- **Buffers, panes & views** — everything-is-a-buffer in practice.
- **Language intelligence** — LSP, completion, tree-sitter, diagnostics.
- **Git & diffs** — the diff engine and the magit port on top of it.
- **Plugins & extensibility** — the WASM host and its seams.
- **AI & agents** — the agent protocol and its UI.
- **Operations** — the implementation ledger, benchmarks, releases.
- **Reviews & notes** — point-in-time audits; historical context, not current design.

Source lives in `docs/dev/` and stays organised by *kind* (architecture /
guides / operations / audit / notes), because a design fragment and a slice
plan are different artefacts with different lifetimes. `site/data/dev-nav.toml`
maps that layout onto the sections above, and the docs sync fails if a page is
missing from it — so this navigation cannot quietly fall behind the corpus.
