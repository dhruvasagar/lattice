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
| PT.1 | The parser: `PluginDeclaration` + `parse_plugin_declarations`, table tests, a parse bench. No wiring. | `lattice-config` | 📝 |
| PT.2 | The walker reserves `plugins`; project config refuses it; `[plugin.<x>]` with `release` warns. | `lattice-config` | 📝 |
| PT.3 | Boot: one `declare` path with `DeclaredBy` origin; the install task reads `lattice.toml`, lowers, merges with `init.rs` (init wins), installs. | `lattice-plugin-loader` | 📝 |
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

- `RequiredSpec` gains `declared_by: DeclaredBy` (`InitRs` / `LatticeToml`).
  `to_required_spec` sets `InitRs`.
- One `PluginLoader::declare(specs)` fills `required` and `declared`; the
  plugin-manager drain calls it instead of its inline locking.
- `lower_declarations(PluginDeclarations) -> (Vec<RequiredSpec>, Vec<(String, String)>)`:
  `is_safe_plugin_name`, then `PluginSource::Release`, `pinned: false`.
- The install task reads `default_user_config_path()` at its top, parses,
  lowers, `declare`s, and `record_failure`s each rejection with a `warn!`.
  A read or parse error is `debug!`.
- At drain (`run_required`), `merge_declared(specs)`: dedup by name,
  `InitRs` beats `LatticeToml`, `warn!` on a differing pair, `debug!` on an
  identical one. The log line stops saying "declared by init.rs".

**Tests**: `merge_declared` unit tests (init wins, warn on difference, no
warn on identical, TOML-only passes through); lowering rejects an unsafe id;
an integration test that boots with no `init.rs` and a `lattice.toml`
declaring a release, through a fake fetcher, and finds it installed and
loaded; a warm boot with the marker present that asserts zero fetches with a
counting fetcher; one broken entry not stopping the others. Pattern on
`tests/require_drain.rs` and `tests/release_round_trip.rs`. The integration
tests need the fetcher injectable at the `run_required` seam; if it is not,
the slice adds that seam first.

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
