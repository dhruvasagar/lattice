# AGENTS.md

Orientation for AI coding agents working in this repository. It points at the
documents that hold the rules and the facts; it does not restate them.

## What this is

Lattice is a modal (vim-grammar), GPU-accelerated, plugin-first text editor in
Rust: a Cargo workspace of ~40 crates under `crates/`, a WIT plugin API under
`crates/lattice-wit/wit/`, bundled WASM plugins under `plugins/`, and docs
under `docs/`.

## Read before changing anything

| Document | What it is |
|---|---|
| [CLAUDE.md](CLAUDE.md) | The project's working rules: the four paramount goals and their priority, the design heuristics, the architecture rules ("everything is a buffer", modes own their surface, no UI-thread work), and the commit / test / gate discipline. They apply to every agent, not only Claude. |
| [docs/dev/architecture/design.md](docs/dev/architecture/design.md) | The design spec. |
| [docs/dev/operations/implementation.md](docs/dev/operations/implementation.md) | The ledger of what is built. Do not assume something in the design spec exists; check here, then check the source. |
| [docs/dev/guides/developing-lattice.md](docs/dev/guides/developing-lattice.md) | Contributor setup. |

## APIs

- **Plugin API** (the WIT package — the contract plugins build against):
  [reference](docs/dev/reference/plugin-api.md), generated from `wit/`, with a
  page per seam and the [worlds](docs/dev/reference/plugin-api/worlds.md);
  the same as JSON in [plugin-api.json](docs/dev/reference/plugin-api.json);
  the [authoring guide](docs/dev/guides/plugin-authoring.md) and the
  [patterns guide](docs/dev/guides/plugin-patterns.md). The four bundled
  plugins in [`plugins/`](plugins/) are complete, CI-built templates.
- **Rust API** (the crates): each crate's root module (`src/lib.rs`) carries
  its overview; `cargo doc -p <crate> --no-deps --open` renders it.

## Building and verifying

```sh
cargo check -p <crate> --tests                 # the default loop
cargo test  -p <crate> --test <file>           # targeted tests
scripts/precommit.sh <crate>...                # fmt + warnings + tests, scoped — before every commit
cargo run -p lattice-cli                       # the TUI editor
cargo run -p lattice-cli --features gui -- --gui   # the GPUI editor (a plain build is TUI-only)
```

Whole-crate and workspace test suites are large and slow; run the tests a
change touches, and leave full suites to CI unless asked. `CLAUDE.md` has
the reasons and the exact gates.

## Generated documentation

The plugin-API reference, its JSON, the example regions it quotes and the
code blocks in the plugin guides are generated and checked by tests in
`crates/lattice-plugin-api`. When one of those tests reports the docs are
stale — you changed the WIT, a guest's `// @example` region, or a quoted
file — regenerate rather than hand-edit:

```sh
UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api
```

The website is built from `docs/` by `site/scripts/sync-docs.sh` (then
`zola build` in `site/`); the sync fails on a dead relative link and on a
dev doc missing from `site/data/dev-nav.toml`.
[docs/dev/architecture/api-docs.md](docs/dev/architecture/api-docs.md)
explains what is generated from what, and every guard.
