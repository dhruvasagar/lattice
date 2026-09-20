# Contributing to Lattice

Lattice is 0.9 — alpha. The most useful contribution right now is a bug
report from an install that went wrong.

## Setup

```sh
git clone https://github.com/dhruvasagar/lattice
cd lattice
rustup target add wasm32-wasip2     # for the bundled plugins
cargo build
cargo xtask build-core-plugins      # NOT optional — see below
cargo run
```

Without `cargo xtask build-core-plugins`, the editor starts with no bundled
plugins and says nothing about it: an absent plugin directory is
indistinguishable from an empty one.

The GPU renderer is behind a cargo feature and is **not** in a plain build:

```sh
cargo run --features gui -- --gui
```

A plain `cargo build -p lattice-cli` does not compile `lattice-ui-gpui` at
all, so edits there appear to succeed while never being built.

## Before you push

```sh
scripts/precommit.sh <crate-you-touched>...
```

That runs three gates: `cargo fmt --check` (strict), zero new rustc warnings
in code you touched, and the targeted tests. `unwrap_used` / `panic` / `todo`
warnings are `warn` on purpose and overwhelmingly test code — don't try to
zero them. The blocking lints are `unsafe_code` and `unused_must_use`.

Whole-workspace test runs are slow (`lattice-host` alone is ~1670 tests).
Run the tests your change touches; leave the full sweep to CI.

## Commits

`<type>: <description>` — `feat`, `fix`, `refactor`, `docs`, `test`,
`chore`, `perf`, `ci`. One logical change per commit.

## Design

Read [the design spec](./docs/dev/architecture/design.md) before proposing
architecture. Four paramount goals govern, in order: performance,
extensibility, vim modal editing, asynchronicity — with user experience
above all four. A change that ships only code is incomplete: the doc, the
test, and the benchmark are part of the deliverable.

Open an issue with the design rationale before a non-trivial PR — the
design doc is load-bearing, not a suggestion, and a PR that contradicts it
without discussion is likely to be declined regardless of code quality.
There are no backwards-compatibility shims for vim or emacs configs; that
is an explicit non-goal.

## Docs

User docs live in `docs/user/` and are embedded in the binary as `:help`
topics. A new page needs `summary:` frontmatter, a row in
`docs/user/README.md`, and an entry in `site/data/nav.toml` — the site build
fails if the last two disagree with the directory.

## Good first issues

Issues labelled [`good first issue`](https://github.com/dhruvasagar/lattice/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
are a reasonable place to start if you're new to the codebase.
