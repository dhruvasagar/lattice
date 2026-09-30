# API documentation — Slice Plan (AD)

Sequencing for the generated, example-backed, guarded API documentation:
the plugin-API reference, the Rust API docs, and the agent layer.

- **Design:** [`api-docs.md`](../../architecture/api-docs.md) (what is
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
| **AD.6** | `plugin-patterns.md` guide with synced examples + reference guard; fix `plugin-authoring.md` | 📝 |

## Agent layer

| Slice | What | Status |
|---|---|---|
| **AD.7** | `llms.txt`, `llms-full.txt`, `plugin-api.json` on the site; `AGENTS.md` | 📝 |

## Rust API

| Slice | What | Status |
|---|---|---|
| **AD.8** | Publish rustdoc at `/api/`; `scripts/doc-coverage.sh` + CI step | 📝 |
| **AD.9** | Crate-overview shape on every crate root; generated `reference/crates.md` + xtask staleness test | 📝 |
| **AD.10** | `lattice-plugin-sdk` — full docs + examples, opt in | 📝 |
| **AD.11** | `lattice-keymap` — full docs + examples, opt in | 📝 |
| **AD.12** | `lattice-core` — full docs + examples, opt in | 📝 |
| **AD.13** | `lattice-config` — full docs + examples, opt in | 📝 |
| **AD.14** | `lattice-mode` — full docs + examples, opt in | 📝 |
| **AD.15** | `lattice-protocol` — full docs + examples, opt in | 📝 |
| **AD.16** | `lattice-grammar` — full docs + examples, opt in | 📝 |

Order within the Rust series is smallest-gap-first so the ratchet mechanism
is proven on a small crate (SDK, 48 items; keymap, 26) before the large ones
(protocol 216, mode 224, grammar 486). Further crates follow the same shape
and get appended here as they are scheduled.

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
