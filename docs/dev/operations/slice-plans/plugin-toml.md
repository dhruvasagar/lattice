# Release plugins in `lattice.toml` — slice plan (PT)

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/plugin-toml.md`](../../architecture/plugin-toml.md).
> Builds on the plugin manager's `require` path
> ([`../../architecture/plugin-manager.md`](../../architecture/plugin-manager.md))
> and the release source kind ([`plugin-distribution.md`](plugin-distribution.md)).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).
Every non-trivial slice ships the four artefacts (doc + bench where perf-relevant
+ tests including failure modes + graceful error handling).

**Status: 🚧 in progress.**

| Slice | What | Crates | Status |
|---|---|---|---|
| PT.0 | Design made final; CLAUDE.md, `plugin-manager.md`, `config-and-init.md` amended; this plan; ledger entry. No code. | docs | ✅ |
| PT.1 | The parser: `PluginDeclaration` + `parse_plugin_declarations`, table tests, a parse bench. No wiring. | `lattice-config` | ✅ 18 tests; `config::plugins_table` 41 ns / 3.2 µs / 23 µs at 1 / 16 / 128 entries |
| PT.2 | The walker reserves `plugins`; project config refuses it; `[plugin.<x>]` with `release` warns. | `lattice-config` | ✅ 3 tests |
| PT.3 | Boot: one `declare` path with `DeclaredBy` origin; the install task reads `lattice.toml`, lowers, merges with `init.rs` (init wins), installs. | `lattice-config`, `lattice-plugin-loader` | ✅ 9 tests (5 unit, 4 integration + 1 fixture-gated); whole-crate suites pending the next batch run |
| PT.4 | `:reload-config` reconciles `[plugins]`; no `init.rs` is benign. | `lattice-plugin-loader` | 📝 |
| PT.5 | `:plugins` shows each plugin's origin and the rejected TOML entries. | `lattice-plugin-loader` (view) | 📝 |
| PT.6 | Docs sweep: user guide, `:help`, scaffold comment, `BENCHMARKS.md`, CHANGELOG. | docs, `lattice-cli` | 📝 |
| PT.7 | A lockfile, or a `:plugin-pin` write-back of the resolved tag and digest. | — | ⛔ until an unpinned declaration's non-reproducibility bites a real user; needs a format decision and an answer for how `:plugin-update` moves it (design §4 Pinning). |

## Sequencing

PT.1 → PT.2 → PT.3 → PT.4 → PT.5 → PT.6. PT.2 can land before PT.3 safely:
reserving the table only stops a spurious "no plugin declares
`plugins.org.release`" warning. PT.3 is the slice that makes the feature work;
everything after it is reconcile, visibility and docs.

## PT.1 — the parser

`lattice-config/src/plugins.rs`:

```rust
pub struct PluginDeclaration {
	pub id: String,
	pub repo: String,
	pub version: Option<String>,
	pub sha256: Option<String>,
	pub enable_mode: Option<String>,
}
pub struct RejectedDeclaration { pub id: String, pub reason: String }
pub struct PluginDeclarations {
	pub declared: Vec<PluginDeclaration>,
	pub rejected: Vec<RejectedDeclaration>,
}
pub fn parse_plugin_declarations(root: &toml::Table) -> PluginDeclarations;
```

Reads `root["plugins"]` if present. Shape checks only (design §4 table).
`rejected` carries the reason text the loader will log and record.

**Tests** (unit, table-driven): full table; table with only `release`;
shorthand `repo`; shorthand `repo@tag`; empty `[plugins]`; no `[plugins]`;
`plugins` not a table; value neither string nor table; missing `release`;
non-string value; each non-release key (`git`, `path`, `url`, `build`, `rev`)
with the "declare it in init.rs" reason; unknown key (`sha265`); malformed
`sha256` (short, non-hex); shorthand with an empty tag (`repo@`); one bad
entry beside good ones (the good ones survive).

**Bench**: `plugins_table/<N>` in `lattice-config/benches/options.rs`, parse of
N ∈ {1, 16, 128} entries.

## PT.2 — the table is not an option namespace

- The walker skips the top-level `plugins` key in `load_file` /
  `load_project_file`: not applied, not held, not unclaimed.
- `plugins` joins `PROJECT_REFUSED_TABLES` with a reason naming the user
  config.
- A `[plugin.<x>]` table with a `release` key warns "did you mean
  `[plugins.<x>]`?".

**Tests**: a user config with `[plugins.org]` produces no messages and holds
nothing (`unclaimed_held_options` stays empty after a load); a project config
with `[plugins.org]` produces the refusal warning and is not parsed as a
declaration; `[plugin.org] release = …` warns.

## PT.3 — boot

As built:

- `lattice-config::read_plugin_declarations(path)` reads and parses the file,
  quiet on a missing or broken one (the editor's config load reports that), so
  the loader stays free of a `toml` dependency.
- `lattice-plugin-loader/src/declare.rs`: `DeclaredBy` (`InitRs` /
  `LatticeToml`), `lower_declarations` (the `is_safe_plugin_name` gate, then
  `PluginSource::Release`, `pinned: false`) and `merge_declared` (one spec per
  name, `init.rs` beats `lattice.toml`, later beats earlier within one file,
  `warn!` naming both files on a differing pair).
- The origin rides on the queue entry, `(DeclaredBy, RequiredSpec)`, and in
  the `declared` map, not as a `RequiredSpec` field: nothing after the merge
  reads it, and `RequiredSpec` mirrors a WIT-facing type with a dozen literal
  sites. `take_required` merges as it drains, so precedence is decided in one
  place with both files' declarations visible.
- `PluginLoader::declare(by, specs)` is the one path both front-ends use; the
  plugin-manager drain calls it. `declare_from_config_file(path)` reads,
  lowers, declares, and `warn!` + `record_failure`s each rejection with the
  file as its location.
- The boot task calls `declare_from_config_file(default_user_config_path())`
  first. `run_required` keeps the real git / network / toolchain and user
  roots and delegates to the new public `run_required_in`, the seam the tests
  drive. Nothing was hoisted out of the init-dir gate: it was never a gate
  (design §1).
- A repeated `require` of one name in `init.rs` now installs once, the later
  spec, rather than twice.

**Tests** (`tests/toml_plugins.rs`, `declare.rs` unit tests): lowering and the
unsafe-id gate; merge precedence both orders, pass-through order, repeat within
a file; a declaration queued as a release spec; rejections recorded with the
file as their location; a missing file; no `init.rs`, a release installed
through a fake fetcher with no git or toolchain, a broken entry costing only
itself, and a warm boot doing zero fetches; `init.rs` winning a shared name
against the real plugin-manager fixture guest.

## PT.4 — `:reload-config`

- `reload_config` re-reads `[plugins]`, `declare`s, and runs
  `run_required(AfterBoot)`.
- With no init directory content, the init build and load are skipped
  without `record_failure(INIT_PLUGIN_ID)`, and the report says so.

**Tests**: no `init.rs`, a declaration added after boot, `:reload-config`
installs and loads it; no `init.rs` records no `init` failure; an unchanged
declaration is left alone.

## PT.5 — `:plugins` provenance

Each declared plugin's row names `lattice.toml` or `init.rs`; a rejected TOML
entry is a failure row with the parser's reason. Origin comes from `declared`,
so a plugin that never built still shows it.

**Tests**: the view model for one of each origin and one rejection.

## PT.6 — docs sweep

User guide section with the schema and the reproducibility caveat; a `:help`
topic or section; the `init.rs` scaffold comment pointing at the TOML path;
`BENCHMARKS.md` row for `plugins_table`; CHANGELOG `## Unreleased` entry (if
PT.3 did not already add it).
