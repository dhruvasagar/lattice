# API documentation — Slice Plan (AD)

Sequencing for the generated, example-backed, guarded API documentation:
the plugin-API reference, the Rust API docs, and the agent layer.

- **Design:** [`api-docs.md`](../../../architecture/api-docs.md) (what is
  generated from what, the guards, rejected alternatives).
- **Builds on:** the PI series (`slice-plans/archive/plugin-host.md` — the
  build-time `PluginApiCatalog`, `:describe-plugin-api`, the PI.6 site page);
  `site/scripts/sync-docs.sh` (site sync + nav validation).
- **Audit references:**
  - `crates/lattice-plugin-api/build.rs` — the one parse of `wit/`.
  - `crates/lattice-plugin-api/src/render.rs` — markdown rendering.
  - `crates/lattice-plugin-api/tests/site_reference_is_current.rs` — the
    staleness guard, and the PI.7 gap it pins.
  - `crates/lattice-plugin-host/build.rs` — `build_guest`, which silently
    skips a guest that fails to build.

Status icons: 📝 planned · 🚧 in progress · ✅ landed · ⛔ deferred · ❌ dropped.

---

## Plugin-API reference

| Slice | What | Status |
|---|---|---|
| **AD.0** | Design fragment + this plan | ✅ |
| **AD.1** | Catalog carries types, `use`s and full signatures; single-page render shows them (closes PI.7) | ✅ |
| **AD.2** | Per-seam pages + index page; JSON export; site sync of the generated subsection | ✅ |
| **AD.3** | Example regions: extraction, render, validation tests; `comment` compiled in CI | ✅ |
| **AD.4** | Seed examples across the guest-facing seams; shrink-only pending list | ✅ |
| **AD.5** | WIT prose accuracy: path-rot guard + fix stale references | ✅ |
| **AD.6a** | World-level functions (the `register-*` entry points) in the catalog, a worlds page, the JSON | ✅ |
| **AD.6** | `plugin-patterns.md` guide with synced examples + reference guard; fix `plugin-authoring.md` | ✅ |
| **AD.6b** | Site link integrity: same-directory dev-doc links resolve; out-of-tree links must exist | ✅ |

## Agent layer

| Slice | What | Status |
|---|---|---|
| **AD.7** | `llms.txt`, `llms-full.txt`, `plugin-api.json` on the site; `AGENTS.md` | ✅ |

## Rust API

| Slice | What | Status |
|---|---|---|
| **AD.8** | Publish rustdoc at `/api/`; `scripts/doc-coverage.sh` + CI step | ✅ |
| **AD.9** | Generated `reference/crates.md` (layers, deps, dependents, summaries) + xtask staleness test; every crate opens with a real summary | ✅ |
| **AD.10** | `lattice-plugin-sdk` — full docs + examples, opt in | ✅ |
| **AD.11** | `lattice-keymap` — full docs + examples, opt in | ✅ |
| **AD.12** | `lattice-core` — full docs + examples, opt in | ✅ |
| **AD.13** | `lattice-config` — full docs + examples, opt in | ✅ |
| **AD.14** | `lattice-mode` — full docs + examples, opt in | ✅ |
| **AD.15** | `lattice-protocol` — full docs + examples, opt in | ✅ |
| **AD.16** | `lattice-grammar` — full docs + examples, opt in | ✅ |

All seven landed on 2026-09-30, one commit per crate, each opted in to
`scripts/doc-coverage.sh` (with `lattice-plugin-api` from AD.8): ~1,200
previously undocumented public items, 135 doctests (several converted from
`ignore`), and every crate overview extended with what it owns, what it must
not depend on and why, a worked example and its design docs. Written by
parallel agents on disjoint crates (grammar split by file), each diff
verified to touch doc comments only, coverage and doctests re-run before
commit, and the crate map regenerated per commit from HEAD plus that crate.

The pass surfaced dozens of docs that contradicted the code (corrected) and
latent code issues (recorded in the docs and the commit messages, not
fixed — out of scope for a docs series). The notable ones: a
`ServiceRegistry` TypeId mismatch that leaves compilation output and the
project diff uncoloured (`PendingSyntheticHighlights` registered bare,
looked up as its `Arc` handle); `Event::DocumentClosed` never published
outside tests; `DocumentOpened.id` minted from the buffer registry's counter
rather than the document's; `Document::apply_edit_batch` not atomic;
`try_push_layer` not scope-checking mode layers against the capability.

Crates not yet opted in follow the same shape; `doc-coverage.sh` discovers
them from the attribute, so there is no list here to extend.

---

## Slice notes

### AD.1 — types and signatures in the catalog

`ApiInterface` gains `types: Vec<ApiType>` and `uses: Vec<ApiUse>`;
`ApiFunction` gains `kind`, `params` and `result`. `ApiType` carries name,
doc and a `TypeKind` (record / variant / enum / flags / resource / alias) with
per-field / per-case docs. WIT type expressions are stored as their WIT
spelling. The PI.7 test inverts into one requiring record fields in the
render. `lattice-host`'s readers of the catalog compile unchanged (fields are
additive; the host only reads).

### AD.2 — pages, JSON, site section (as built)

`render::pages()` returns the whole set (index, `plugin-api/<seam>.md`,
`plugin-api.json`); the staleness test checks it as a set, so a stale,
missing or orphaned page (a removed seam) all fail, and a second test
resolves every generated link and anchor at `cargo test` time. On the site
the reference is its own dev section (`dev-nav.toml` `generated =
"plugin-api"`), not a subsection of "Plugins": a Zola subsection would have
turned that section's page list into a card grid. `sync-docs.sh` rewrites
the same-directory links to `@/` internal links, which makes Zola validate
every one, anchor included (verified by injecting a broken anchor).

Deviation: `*-fixture` worlds are now excluded from the catalog's world
list (previously only `trampoline-fixture`) — three test worlds were
listed as if a plugin should target them. They still count toward seam
direction.

### AD.6 — guides (as built)

`plugin-patterns.md` is new: eleven recipes (operator, action, motion / text
object, ex-command, mode + options, picker, events, buffer + tree, state,
help + logging, cross-cutting rules), every code block a synced quote — 30
examples from 9 guests plus the `comment` manifest and `Cargo.toml`.
`tests/guides.rs` rewrites/verifies them and resolves every
`` `seam.item` `` and `` `name` world `` mention in the plugin guides.

`plugin-authoring.md` was stale from "Lifecycle + manifest" on: a worked
example of the deleted `fuzzy-finder` in the pre-OR.5b picker shape,
`manifest.toml` (the loader reads `plugin.toml`), four capability forms of
six, `ui` called a type-mirror, a missing `.await` on the async
`instantiate_plugin`, "eight fixtures", and a "still ahead" list naming
shipped work. Rewritten: the toolchain / ABI sections (current) and the
sync-vs-async and runtime-contract sections are kept verbatim; the
per-seam status table — a second copy of what the generated reference
already says — became a goal → seam → pattern map, which is judgement the
reference cannot carry. The manifest example is parsed by the real parser
(`lattice-plugin-host/tests/documented_manifests_parse.rs`) and its keys
pinned to `RawManifest`'s (the parser ignores unknown keys).

### AD.9 — crate map (as built)

`xtask/tests/crate_map.rs` generates `docs/dev/reference/crates.md`: 41
crates in 14 dependency layers, each with its overview's first paragraph,
its `lattice-*` dependencies and dependents, and a rustdoc link. Published
first in the Foundations section; linked from `/api/` and `AGENTS.md`.

Scope changed from the plan: "a fixed overview shape on every crate root"
would have been a 41-crate rewrite with no guard that could check the
shape's content. Instead the guard checks what the map depends on — every
root opens with a paragraph saying what the crate is, not a slice ID — and
the fuller shape moves into the per-crate slices (AD.10+). Ten openings
failed that rule (`Phase 5.7: GPUI peer renderer scaffold` — stale as well
as opaque, `PO.4 —`, `PL8.H.2 —`, `NOTIF.1a —`, `IM.4 —`, `WT.1 —`, …) and
were rewritten from what each overview already said, IDs kept at the end.

### AD.8 — rustdoc + the coverage ratchet (as built)

`deploy-docs.yml` builds `cargo doc --no-deps --workspace` on pushes (not
PRs — `ci.yml`'s `doc` job already builds it per PR) and
`site/scripts/publish-rustdoc.sh` copies it to `/api/` with a crate index
(stable rustdoc has no workspace landing page).

`scripts/doc-coverage.sh`, run by `ci.yml`'s `doc` job, discovers opted-in
crates from their `#![warn(missing_docs)]` attribute. **The first version
reported "clean" over a crate with four gaps:** a crate's own lint
attribute outranks a command-line `-D missing_docs`. It now passes
`--force-warn` (which no attribute can lower), reads rustdoc's JSON
diagnostics, and fails on `missing_docs` / broken or private intra-doc
links in the opted-in crates; cached builds replay the diagnostics, so a
warm run is as strict as a cold one (verified). `lattice-plugin-api` is
the first crate opted in, its four undocumented render helpers documented.

### AD.7 — agent layer (as built)

`sync-docs.sh` writes `/llms.txt` (469 lines: every plugin-API page with
its summary, both plugin guides, every user doc with its summary, every
dev doc by section), `/llms-full.txt` (~430 KB: authoring guide, patterns
guide, the full reference — the one file an agent needs to write a
plugin), and mirrors every published Markdown source to `/md/<repo
path>` so every llms.txt link is plain Markdown. `AGENTS.md` at the root
points at `CLAUDE.md` for rules and at the docs for facts; its links are
checked by the same resolver (verified by injecting a dead one).
`deploy-docs.yml` now also triggers on `crates/**`, `plugins/**` and
`AGENTS.md`, because the link check covers links into code.

### AD.6b — site link integrity (carved while checking AD.6 on the site)

The patterns guide's links to its sibling rendered as
`/dev/plugins/plugin-patterns/plugin-authoring.md` — and so did every
same-directory link in the dev docs: the resolver matched only `../`-prefixed
links and assumed they were relative to `docs/`. Measured in the built HTML:
**398 page-relative `.md` hrefs before, 0 after.** It also sent
`../operations/…` links from architecture docs to GitHub without their
`docs/dev/` prefix, and never checked that an out-of-tree target existed
(`plugins/fuzzy-finder`).

`sync-docs.sh` now resolves every relative link against its source file:
published pages become `@/` links (which Zola validates, anchors included),
everything else a GitHub URL at its real path, and a missing target fails the
sync with the full list. Code — fenced and inline — is left alone. It
surfaced five dead links and two anchors that only GitHub's slugger
produced (numbered headings slug differently in Zola); all fixed at the
source.

### AD.6a — world entry points (carved while writing AD.6)

Found while checking the authoring guide against the WIT: every
contribution world declares freestanding functions
(`export register-grammar: func();`, `register-modes`,
`register-picker-sources`, …) — the entry points the host calls on a guest
at load, and the first thing an author writes. The catalog only recorded a
world's *interface* edges, so the reference omitted exactly these. The
catalog now carries `export_functions` / `import_functions` per world;
`plugin-api/worlds.md` renders each world with its imports, exports and
entry points (first in the site sidebar); the index links there; the JSON
carries them; `every_world_entry_point_is_in_the_reference` guards it.

### AD.3 — example regions (as built)

Marker: `// @example <interface>[.<item>]: <caption>` … `// @end-example`.
Scanned roots: `plugins/*/src`, `crates/lattice-plugin-host/tests/fixtures/*/src`.
Extraction problems (unterminated, nested, empty, uncaptioned, duplicate)
are collected as data, not panics, so the test reports every one at once.

Two deviations from the plan, both from reading the code rather than the
design's assumptions:

- **Extraction moved from `build.rs` to test time** (`examples::scan`).
  In `build.rs` every guest source becomes a build input of a crate
  `lattice-host` links, so any plugin edit relinks the host. Cost accepted:
  `:export-plugin-api` in the editor has no examples.
- **The "CI guest-build guard" was already there.** `build_guest` panics
  when a guest fails to compile and `wasm32-wasip2` is installed (OA.22).
  The real gap was a guest nobody built: `plugins/comment`, compiled only
  at release. It is now in `lattice-plugin-host/build.rs`, and
  `every_core_plugin_is_compiled_in_ci` keeps every `plugins/*` crate there.

Seeded with the five `comment` registrations/callback as the end-to-end
proof; broad seeding is AD.4.

### AD.4 — coverage (as built)

75 examples across 23 guests: every guest-facing seam has at least one,
and 74 of the 102 guest-facing functions have their own. The remaining 28
are on `tests/example_coverage.rs`'s shrink-only `PENDING` list, each with
a reason. Two kinds:

- **24 functions no guest calls at all** — no fixture, no plugin. That is
  untested API surface, not a documentation gap; an example for one means
  writing its guest-side test first. The list is its inventory.
- **4 called only inside another target's example** (`node.walk`,
  `tree-cursor.current-node`, `node.named-child-count`, `tree-snapshot.root`)
  — visible to a reader there, with no non-overlapping span to file under
  their own name.

Every added region was verified to be comment lines only (`git diff`
filtered to non-marker lines was empty).
