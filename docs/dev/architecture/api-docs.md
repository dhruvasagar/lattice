# API documentation: generated, example-backed, guarded

**Status:** designed (AD series; see the
[slice plan](../operations/slice-plans/api-docs.md) for sequencing). Extends
the PI series in [`plugin-host.md`](plugin-host.md) (the build-time plugin-API
catalog) and the site pipeline (`site/scripts/sync-docs.sh`).

Lattice has two APIs and two kinds of reader.

- **The plugin API** is the `wit/` package. It is the contract a third party
  builds against, in any Component-Model language. It is versioned, it breaks
  plugins when it moves, and it is the thing an outside developer or an AI
  agent will write code against without reading anything else in the tree.
- **The Rust API** is the crates. Its readers are contributors, human or
  agent, extending the editor from inside the workspace.

The same three rules hold for both, and they are what this design exists to
make true:

1. **Every page is derived from the code or proven against it.** A reference
   someone maintains by hand is confidently wrong the week after it is written.
   The plugin reference already demonstrated the cost: it still names
   `plugins/fuzzy-finder`, which was deleted months ago.
2. **Every example compiles in CI.** An agent copies an example verbatim; a
   stale example is worse than none because it is used.
3. **Drift fails a test, not a review.** Each generated artefact is
   checked in and compared against a fresh render; each hand-written page that
   quotes code is compared against the code it quotes.

## 1. What exists, and where it stops

`lattice-plugin-api` parses `wit/` at build time (`build.rs` → `wit-parser`)
into a `PluginApiCatalog`, merges a hand-written capability table, and renders
it to `docs/dev/reference/plugin-api.md`. A test fails when the page is stale.
The same catalog backs `:describe-plugin-api`, `:list-plugin-apis` and
`:export-plugin-api` in the editor.

It stops at function *names* and the first line of their docs. It carries no
type definitions (`types.wit`, 2.2k lines, is where every payload a guest
constructs lives), no signatures, no field or case docs, and no examples. The
test file records this as the PI.7 gap.

The Rust side has ~110k lines of doc comments but ~90 doc examples across 540k
lines, no published rustdoc, and nothing that says how 41 crates fit together.
Of the seven crates a contributor touches first (`lattice-mode`,
`lattice-grammar`, `lattice-core`, `lattice-keymap`, `lattice-config`,
`lattice-protocol`, `lattice-plugin-sdk`), ~1,200 public items carry no doc
at all (`-W missing_docs`, measured 2026-09-30).

## 2. The plugin-API reference

### 2.1 The catalog carries the whole surface

`build.rs` is extended, not replaced — it already owns the one parse of `wit/`
and the one place a wrong parse fails the build. It emits, per interface:

- **types** — every record (fields + their docs), variant (cases, payload
  types, docs), enum, flags, resource (with its constructor, methods and
  statics), and alias, with the WIT type rendered as WIT source
  (`list<option<string>>`, `result<_, string>`, `borrow<document>`);
- **uses** — the types an interface pulls in with `use`, linked to their
  definition rather than repeated;
- **function signatures** — parameters with names and types, the result, and
  whether the function is freestanding, a method, a constructor or a static.

Types are rendered, not re-modelled: the catalog stores the WIT spelling of a
type as a string plus the name of the interface that defines it, which is all a
reader or a renderer needs. Modelling the full type algebra in the catalog
would add a second type system with no consumer.

The catalog stays wasmtime-free and runtime-dependency-free (`lattice-host`
depends on it); the generated data is plain `String`s and `Vec`s.

### 2.2 Examples come from guests CI already builds

An example is a region of a real guest's source, marked in place:

```rust
// @example host-services.walk: List the project's Rust files
let files = host_services::walk(&WalkOptions { .. })?;
// @end-example
```

The target is `<interface>` or `<interface>.<item>` where the item is a
function (a method spelled `<resource>.<method>`, as the reference names it)
or a type. `lattice-plugin-api::examples::scan` reads `plugins/*/src/` and
`crates/lattice-plugin-host/tests/fixtures/*/src/`, dedents each region, and
hands it to the renderer, which places it under the item it targets — on the
seam page and in the JSON.

**Scanned at test time, not in `build.rs`.** Embedding examples in the
compiled catalog would make every guest source a build input of
`lattice-plugin-api`, which `lattice-host` links — so any edit to a plugin or
fixture would relink the whole host. The accepted cost: the editor's in-app
`:export-plugin-api` renders without examples; the published pages, the JSON
and the agent bundle carry them.

Why those two directories and not a new `examples/` tree: both are built by
`lattice-plugin-host`'s `build.rs` against the current `wit/`, and the
fixtures are exercised by real guest↔host tests. An example taken from them
compiles *and* runs. A separate examples tree would compile but nothing would
prove it does what its caption says.

That build already fails when a guest does not compile and the
`wasm32-wasip2` target is installed (as it is in CI) — so "CI compiles it"
reduces to "`lattice-plugin-host/build.rs` builds it". One core plugin,
`comment`, was built only by the release workflow; it is now built there
too, and a test keeps every `plugins/*` crate in that list.

Guards (§5): an example naming a target that does not exist fails; a
malformed region (unterminated, nested, empty, uncaptioned, duplicate) fails;
a region in a guest `lattice-plugin-host` does not build fails.

### 2.3 Pages

- `docs/dev/reference/plugin-api.md` — the index: the worlds a plugin can
  target and what each imports/exports, a table of every seam (direction,
  capability, function and type counts, a one-line summary), and the
  conventions a reader needs once (resources, `result<_, string>` errors,
  sync vs async seams).
- `docs/dev/reference/plugin-api/<interface>.md` — one page per seam: full
  docs, every function with its signature and examples, every type with its
  fields/cases, and the worlds it appears in.

One page per seam because both readers need it: a person lands on the seam
they searched for; an agent loads the one page it needs rather than a
10k-line file. `render::markdown()` still produces the single-document form
(the index followed by every seam), because `:export-plugin-api` and the
agent bundle want exactly that.

The per-seam pages are generated, so they are validated by the generator's
test rather than listed by hand in `site/data/dev-nav.toml`; the site sync
treats the `plugin-api/` directory as one generated subsection.

### 2.4 Accuracy of the WIT prose itself

Generation makes the reference faithful to the WIT; it cannot make the WIT's
prose true. The doc comments carry slice IDs, `file.rs:294` line references
and names of deleted plugins. `tests/wit_prose.rs` checks every `///` line
for four kinds of pointer: a repository path must exist, a design-doc name
must be a file under `docs/` or the root, a `lattice_*::…::Item` path must
name a crate that defines `Item`, and a `file.rs:NNN` reference is refused
outright — no test can say whether line NNN is still the right line. Slice
IDs stay — they are how a reader finds the rationale.

**Only `///` is documentation.** `wit-parser` attaches *every* comment run
before an item to it, `//` included, so a raw parse published section
dividers (`// ---- Effect payload mirrors ----`) as the next record's docs
and a field's trailing `// note` as the following field's. The WIT authors
write to the Rust convention — `//` for maintainers, `///` for readers — so
`build.rs` parses a copy of the sources with non-doc comments blanked (line
numbers preserved for parse errors). Where a `//` comment was in fact an
item's only documentation, the fix is to promote it to `///` in the WIT,
which makes the intent explicit rather than inferred.

### 2.5 Usage patterns

The reference answers "what is this call". Patterns answer "how do I build a
picker / a mode with its own keymap / an operator / an event subscriber", end
to end: the world to target, the manifest, the registration call, the
lifecycle, the failure modes, and what the host does with the result.
They live in `docs/dev/guides/plugin-patterns.md` (hand-written; the
judgement is not derivable), and they quote code only through synced blocks —
an HTML comment `<!-- example: <id> -->` immediately before a fenced block.

A test replaces each synced block with the current extraction and fails on any
difference (`UPDATE_SITE_REFERENCE=1` rewrites them). A second test resolves
every `` `interface.item` `` reference in the guides against the catalog, so
renaming a WIT function breaks the guide's test, not its reader.

## 3. The Rust API

### 3.1 Coverage, ratcheted per crate

A crate opts in with `#![warn(missing_docs)]` at its root once every public
item is documented. `scripts/doc-coverage.sh` finds opted-in crates by that
attribute (there is no second list to drift) and fails CI on any
`missing documentation` or unresolved intra-doc-link warning in them.
`precommit.sh` already treats a rustc warning in a touched crate as a failure,
so the local loop catches it first.

Not `deny`: it would break a contributor's build between writing a `pub fn`
and writing its doc, which is the wrong moment for a hard stop, and the CI
gate gives the same guarantee at the moment that matters. Not workspace-wide:
~1,200 items in seven crates alone, so a global switch would be a flood that
teaches people to ignore it.

### 3.2 Examples are doctests

Key types and seams get `# Examples` sections that compile and run under
`cargo test`. Twenty-two crates set `doctest = false` to skip an empty doctest
binary (~100s of link time per invocation on a comparable crate); a crate that
gains examples turns it back on, as those manifests' comments already ask.
That cost is real and accepted per crate: an unverified example is the thing
this design exists to prevent.

### 3.3 Crate overviews and the crate map

Every crate root carries an overview in a fixed shape: what it owns, what it
must not depend on and why (the structural boundary — heuristic #6's reason it
is a crate), its main types, one worked example, and its design fragment. From
those roots a generated `docs/dev/reference/crates.md` maps the workspace:
layer, dependencies, summary. A test in `xtask` fails when the map is stale.

### 3.4 Publishing

`deploy-docs.yml` runs `cargo doc --no-deps --workspace` and publishes the
output at `/api/` beside the Zola site, so rustdoc is linkable from the dev
docs and the crate map.

## 4. The agent layer

Agents read differently from people: they want one fetchable document, plain
text, stable URLs and a machine-readable schema. All of it is generated from
the artefacts above; none of it is written by hand.

- **`/llms.txt`** (the llmstxt.org convention) — what lattice is, and a link
  with a one-line summary for every user doc, dev doc and plugin-API page,
  generated from `nav.toml` / `dev-nav.toml` by `sync-docs.sh`.
- **`/llms-full.txt`** — the single-file bundle for writing plugins: the full
  plugin-API reference, the patterns guide and the authoring guide.
- **`/plugin-api.json`** (and `docs/dev/reference/plugin-api.json`) — the
  catalog as JSON: seams, worlds, signatures, types, capabilities, examples.
  A tool or agent that wants structure rather than prose reads this.
- **`AGENTS.md`** at the repo root — a short, tool-neutral orientation for
  agents working *in* the repository: where the authoritative docs are, how to
  build and test a crate, and the gates a change must pass. It points into the
  docs rather than restating them.

## 5. Guards, in one place

| Artefact | Guard | Fails when |
|---|---|---|
| plugin-API pages + JSON | `lattice-plugin-api` `site_reference_is_current` | `wit/` or an example changed and the files did not |
| example regions | `lattice-plugin-api` `tests/examples.rs` | target missing, region malformed, source guest not built by `lattice-plugin-host` |
| guest builds | `lattice-plugin-host/build.rs` (existing) + `every_core_plugin_is_compiled_in_ci` | a guest fails to compile with the wasm target installed; a `plugins/*` crate is not in the build list |
| example coverage | ratchet list in the example test | a guest-facing seam has no example and is not on the (shrink-only) pending list |
| WIT prose | `tests/wit_prose.rs` | a WIT doc names a missing path, design doc or Rust item, or cites a source line number |
| doc-comment semantics | `only_triple_slash_comments_are_documentation` | a `//` note leaks into the reference, or a `///` doc is lost |
| guides | synced-block + reference tests | a quoted example or an `interface.item` reference no longer matches |
| Rust coverage | `scripts/doc-coverage.sh` in CI | an opted-in crate gains an undocumented item or a broken intra-doc link |
| Rust examples | doctests under `cargo test` | an example stops compiling or asserting |
| crate map | `xtask` test | a crate root's overview changed and the map did not |
| site nav | `sync-docs.sh` (existing) | a dev doc is not in `dev-nav.toml` |

## 6. Paramount-goal alignment

- **#2 Extensibility** is the reason for §2: a plugin API nobody can learn
  without reading the host's source is not an extension substrate. The WIT
  stays canonical — the reference is derived from it, and the SDK is
  documented as ergonomics over it, never as the API.
- **#1 Performance** is untouched at runtime: everything happens at build or
  test time. The catalog grows (types, examples) inside a crate the host
  already links, read only by introspection commands, never per frame.
  The accepted cost is compile time: doctests re-enabled per crate.
- **Heuristic #6:** no new crate. Generation stays in `lattice-plugin-api`
  (which owns the catalog), the crate map in `xtask` (which owns workspace
  tooling), the site outputs in `sync-docs.sh` (which owns the site).

## 7. Rejected alternatives

- **OpenAPI/Swagger.** Lattice has no HTTP API; its APIs are WIT and Rust.
- **A hand-written reference with an examples appendix.** It is how the current
  guides went stale.
- **`rustdoc --output-format json` for the crate map and agent bundle.**
  Nightly-only; the workspace builds on stable.
- **Examples in a new `examples/` plugin tree.** Compiles but proves nothing
  at runtime; the fixtures are already exercised by guest↔host tests (§2.2).
- **`#![deny(missing_docs)]`.** See §3.1.
