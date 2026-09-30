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
| **AD.2** | Per-seam pages + index page; JSON export; site sync of the generated subsection | 📝 |
| **AD.3** | Example regions: extraction in `build.rs`, render, validation tests, CI guest-build guard | 📝 |
| **AD.4** | Seed examples across the guest-facing seams; shrink-only pending list | 📝 |
| **AD.5** | WIT prose accuracy: path-rot guard + fix stale references | 📝 |
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

### AD.3 — example regions

Marker: `// @example <interface>[.<item>]: <caption>` … `// @end-example`.
Scanned roots: `plugins/*/src`, `crates/lattice-plugin-host/tests/fixtures/*/src`.
Extraction problems (unterminated, nested, empty) are collected into the
catalog as data, not panics, so the test reports every one at once.
`lattice-plugin-host/build.rs` records failed guest builds in an env var; a
test fails on a non-empty list when `CI` is set.
