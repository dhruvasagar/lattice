# Release plugins declared in `lattice.toml`

**Status: proposal, under review (2026-11-25). Nothing here is built or
decided.** Drafted by a design pass over the source; every `file:line` was
read there. Section 7 lists what could not be verified.

## The ask

> We can add plugin configuration in lattice.toml, however we still need
> init.rs to register plugins, which requires rust toolchain. I want to
> explore if we could potentially support adding plugins (only release spec)
> via lattice.toml. This wouldn't change what init.rs can do, just add an
> option to allow users to be lean and not require a rust toolchain to still
> be able to refer plugins. — Dhruva

A user declares **release** plugins (prebuilt `.wasm` from a published
release; not git, path or build-from-source) in `lattice.toml`, and needs no
Rust toolchain. `init.rs` keeps every capability it has. This is a second,
leaner front-end over the same install path, not a replacement.

## 1. How it works today

**Registration from `init.rs`.**
- The `register-plugins` export calls `plugin_manager::require(PluginSpec)` or
  `require_release(ReleaseSpec)` (`crates/lattice-wit/wit/plugin-manager.wit:30-110`).
- `require-release` is a new function, not a fourth variant case, so old
  compiled `init.rs` files still link (`plugin-distribution.md` §3).
- The host only records (`crates/lattice-plugin-host/src/lib.rs:2326-2360`):
  - an unsafe name returns `false` and is logged, never trapped;
  - a release is recorded with `pinned: false`, since it is never built.
- The record is `RequiredPlugin { name, source: RequiredSource, enable_mode, pinned }`
  (`plugin_manager_host.rs:44-80`). It is mirrored into the loader's
  `RequiredSpec` / `PluginSource::Release { repo, version, sha256 }`
  (`lattice-plugin-loader/src/pipeline.rs:40-82`, `resolve.rs:36-55`).
  Mirroring rather than sharing the type keeps a WIT-facing type safe from
  loader refactors.

**Boot order** (`lattice-plugin-loader/src/install.rs:433-560`). One spawned
task, off the boot thread:
1. Core plugins are discovered.
2. `init.rs` is built, loaded and awaited.
3. `run_required(Boot)` takes the queue (`lib.rs:3012`) and calls `install_all`
   on `spawn_blocking` with `RefreshPolicy::UseCache` (`install.rs:679-722`).
4. The user-root scan loads everything installed.
5. `enable_required_modes` runs, then `report_unclaimed_options`.

**Per release spec** (`pipeline.rs:119`, `resolve.rs:374-440`):
1. Build the asset name `<id>-<ver>-api<M>.<m>.wasm` for this editor's API
   generation.
2. Check the embedded manifest id against the name.
3. Fetch `<asset>.sha256` and the asset through the hardened
   `download_to_file`, then verify the digest, and the spec's `sha256` if
   pinned.
4. Stage into `<user_root>/<id>/` and write the `.source` marker.

A failure is `Install::Skipped` plus `record_failure`, which `:plugins`
shows. One bad spec costs only that plugin.

**Cache and lock.**
- There is no lockfile. The record is the per-plugin `.source` marker
  (`source_record.rs:41-72`): `kind=release, repo, version, sha256, api, latest`.
- A warm boot with the artifact present and the marker matching does no
  network I/O.
- `:plugin-update` works from the marker and declines on a pinned tag
  (`lib.rs:2779-2785`).
- `sha256` is recorded only when the user pinned it (`resolve.rs:431`). An
  unpinned install does not record the digest it got.

**`lattice.toml` today.**
- `lattice_config::load_default_paths` reads the user's `lattice.toml`, then
  the project's `.lattice/config.toml` (`lattice-config/src/loader.rs:285-328`).
  It is called only from `Editor::load_persistent_config`.
- Keys under an unloaded plugin's namespace (`[org]`) are held and applied
  when the plugin declares the option (`loader.rs:736-760`). A plugin's
  *options* already work in TOML.
- An untrusted project config refuses some tables (`PROJECT_REFUSED_TABLES`,
  `loader.rs:331`).
- Nothing in it declares a plugin.

**The two gaps for a toolchain-free user.**
1. `run_required` runs only inside `if let Some(init_dir)`
   (`install.rs:467-543`). A TOML-fed queue would never run without an init
   directory.
2. `:reload-config` reconciles requires only through `sync_init`
   (`lib.rs:3125-3144`).

**Existing position.** `plugin-manager.md` §3 / §11 reject a TOML list as the
*canonical* registration but allow it later "as declarative sugar over the
same `require` for the trivial case". `plugin-manager.md` §12.1 and
`config-and-init.md` §5 state the same split. This proposal is that sugar.

## 2. Static setting, or logic?

A release declaration is `{id, repo, tag, hash, enable-mode}`. It has no
conditionals, no ordering semantics, no callbacks and no computation, so it
is a static setting by CLAUDE.md's own test ("static settings stay
declarative; logic stays code").

Two tensions, stated:

- **Wording.** "TOML covers static option overrides only" (CLAUDE.md, One
  extension substrate). A plugin declaration is not an option override. The
  principle holds; that sentence, and `plugin-manager.md` §12.1, need a
  one-line amendment.
- **Trust.** `lattice.toml` is inert today; this makes it something that
  fetches and runs third-party code at boot. That code is still
  capability-gated and sandboxed, but the vector is new. It drives three
  rules below:
  - project config may not declare plugins;
  - key validation is strict;
  - the `sha256` pin is documented.

## 3. Options

All three lower to the existing `RequiredSpec` / `PluginSource::Release`.
None adds a WIT function or a crate.

### (A) A `[[plugins]]` array of tables

```toml
[[plugins]]
name = "org"
repo = "owner/lattice-org-plugin"
version = "v0.2.0"
```

> **UX (higher court):** neutral. It installs with the same eventual
> consistency as `init.rs` and is off the keystroke path.
> **Paramount goals:** protects #2 (no-toolchain users get plugins) and #1
> (warm boot stays network-free); sacrifices none.
> **Heuristic #1:** the weaker fit.
> - Arrays allow duplicate `name`s silently, so uniqueness is hand-written
>   code.
> - The id is a value here, but the *key* everywhere else (`[org]`,
>   `org.enabled`, the install directory).
>
> **Heuristic #2:** anchored on #2, not on other tools.
> **Heuristic #3:** see B and C.
> **Heuristic #6:** no new crate.
> **Mode ownership:** no chord, command or action touched; `enable-mode`
> stays an opaque string on the existing path.

### (B) `[plugins.<id>]` keyed tables, with a one-line shorthand

```toml
[plugins.org]
release = "owner/lattice-org-plugin"   # required: owner/name
version = "v0.2.0"                     # optional; omitted = newest built for this editor
sha256  = "3f9a…"                      # optional; pins the bytes, not just the tag
enable-mode = "org-mode"               # optional; the same string init.rs takes

[plugins]
auto-pair-extras = "someone/lattice-auto-pair-extras@v1.2.0"   # shorthand: repo[@tag]

[org]                                  # the plugin's own options, which work today
directory = "~/notes"
```

> **UX:** as A, and it reads beside the plugin's own `[org]` options under
> the same key.
> **Paramount goals:** as A.
> **Heuristic #1:** the best fit.
> - A duplicate id is a TOML parse error by construction.
> - The id is the key, as everywhere else.
> - The string shorthand is the trivial case and the table the full one,
>   both lowered through one path.
>
> **Heuristic #2:** anchored on #2.
> **Heuristic #3:** the shorthand makes it a mild superset of A.
> **Heuristic #6:** no new crate.
> - Parse into a neutral `PluginDeclaration` in `lattice-config`, which
>   already depends on `toml`; the loader does not
>   (`lattice-plugin-loader/Cargo.toml:45-63`).
> - The loader lowers it to `RequiredSpec`, the same mirror-not-share
>   pattern as `to_required_spec`.
>
> **Mode ownership:** as A.
> **Cost:** `toml::Table` iterates alphabetically (a `BTreeMap`, unless
> `preserve_order` is on; unverified), not in file order. Harmless, since
> installs are independent.

### (C) A separate `plugins.toml`

> **UX:** neutral to slightly better.
> - `lattice.toml` stays pure settings.
> - A shared dotfile fragment cannot install code by accident.
> - The table cannot collide with the option walker.
>
> **Paramount goals:** #2 as A; it costs discoverability, a second file to
> learn.
> **Heuristic #1:** cleaner on trust and on lockfile placement
> (`plugins.lock` beside it), and it resolves the CLAUDE.md wording rather
> than amending it.
> **Heuristic #2:** anchored on a trust argument, not a paramount goal.
> **Heuristic #3:** this is the third option.
> **Heuristic #6:** no new crate.
> **Cost:** the ask names `lattice.toml`, and it adds a file and a config
> path to document.

## 4. Behaviour, under (B)

**Schema and validation.**
- A table must have `release`, and may have `version`, `sha256` and
  `enable-mode`.
- A string is `repo` or `repo@tag`.
- The id must pass `is_safe_plugin_name` (`plugin_manager_host.rs`), the same
  gate `init.rs` passes.
- `release` must be `owner/name`; `sha256` must be 64 hex characters.

**A non-release spec in TOML.**
- `git`, `path`, `url` and `build` are not release keys. The entry is
  skipped with one `warn!`: "builds from source; declare it in init.rs".
- An unknown key also skips the whole entry. A typo such as `sha265`, dropped
  quietly, would install unpinned and defeat the pin.
- Each rejection is also `record_failure`d, so `:plugins` shows it.
- Log and skip, never panic. One bad entry does not stop the others, as
  `pipeline.rs`'s `one_broken_spec_does_not_stop_the_others` already pins
  for `init.rs`.

**`init.rs` and TOML naming the same plugin.**
- `init.rs` wins. It is the superset and the more specific layer.
- TOML specs are queued first and deduplicated by id, the last one winning.
- One `warn!` names both when they differ; a `debug!` when they are
  identical.
- Never an error, never a refused boot.
- The whole spec is replaced, not merged field by field: "TOML's version,
  init.rs's sha256" would surprise.

**Options and grants.**
- Nothing new for options: `[org]` is already held and applied.
- No `grants` key. Grants follow the plugin's own options
  (`plugin-manifest.md` §2), and the distribution design already rejected a
  `grants` field, so TOML cannot widen a plugin past its manifest.

**Enable and disable.**
- `<id>.enabled` keeps working (`lib.rs:1664-1726`; registered for plugins
  that declare a default mode).
- No declaration-level `enabled = false` in v1. The user-root scan loads
  every installed plugin whether or not it is declared, so removing or
  disabling a declaration unloads nothing. That is true of `init.rs` today.
  See question 3.

**Pinning and the lockfile.**
- v1 reuses `version` + `sha256` in the declaration and the existing
  `.source` markers; there is no lockfile.
- The guide must say that an unpinned declaration records no digest, so it
  is not reproducible across machines.
- A `lattice.lock` (resolved tag and digest, perhaps written by a
  `:plugin-pin`) is deferred, PT.7.

**`:plugin-*` and the manager view.**
- TOML specs join the same `required` queue. From there, `apply_required`,
  the markers, `:plugins`, `:plugin-update`, `:plugin-load` / `-unload` /
  `-reload` and describe work unchanged.
- One addition: the view shows where a plugin was declared (`lattice.toml`
  or `init.rs`). Nothing records that today (PT.5).

**No `init.rs` at all.** Boot works with no Rust toolchain once two things
change:
1. `run_required` moves out of the `init_dir` gate, with the TOML specs
   queued before it.
2. `:reload-config` re-reads TOML without `sync_init`.

Then a toolchain-free user needs `lattice.toml` and, for the first install
only, the network. Warm boots do no network I/O and no build.

**Project config.**
- `plugins` joins `PROJECT_REFUSED_TABLES` (`loader.rs:331`): a cloned
  repository's `.lattice/config.toml` never installs code, the same rule as
  for `llm`.
- The option walker must not treat `plugins` as an option namespace, or it
  would hold the entries and report them unclaimed.

## 5. Recommendation

**(B)**, on heuristic #1:
- the id is already the key for options, the gate and the install directory;
- TOML gives uniqueness free;
- the shorthand covers the common case without a second mechanism.

It protects paramount #2 (a no-toolchain user gets plugins) and keeps #1
(the warm boot does no network I/O; installs stay off the boot thread).

Choose **(C)** if "`lattice.toml` stays inert data" outweighs "one file". The
lowering is identical; only the file differs.

**Impact surface.**

| artefact | what |
|---|---|
| Design | This fragment. Amendments to `plugin-manager.md` §3 / §11 / §12, `config-and-init.md` §5, `plugin-distribution.md` (the TOML front-end) and the CLAUDE.md sentence. `implementation.md`, the user guide and a `:help` topic. |
| Crates | `lattice-config`: parse, `PluginDeclaration`, project refusal, walker reservation. `lattice-plugin-loader`: lowering, enqueue, hoisted `run_required`, reload path, provenance. `lattice-plugin-manager`: provenance in `:plugins`. `lattice-cli/src/scaffold.rs`: a comment pointing at the TOML path. No WIT change, no new crate. |
| Tests | Parser table tests: valid table, shorthand, non-release keys, unknown key, unsafe name, bad repo, bad sha256, empty `[plugins]`. Project refusal. `init.rs`-vs-TOML precedence, including the warning. A boot with no `init.rs` installing a TOML release through a fake fetcher. A warm boot asserting zero fetches with a counting fetcher. One broken entry not stopping the others. `:reload-config` with no `init.rs`. Patterned on `lattice-plugin-loader/tests/require_drain.rs` and `release_round_trip.rs`. |
| Bench | A `plugins_table` parse-and-lower bench in `lattice-config/benches/options.rs` (N entries). The real regression guard, "a warm boot does no network I/O", is a test. |
| Errors | Every rejection is a `warn!` plus `record_failure`, never a panic. Installs stay on `spawn_blocking`. |

## 6. Slice plan (proposed IDs; check for collisions)

| slice | what |
|---|---|
| PT.0 | This fragment made final, the doc and CLAUDE.md amendments, a slice plan under `docs/dev/operations/slice-plans/`. No code. |
| PT.1 | The parser in `lattice-config`: `parse_plugin_declarations(&toml::Table) -> (Vec<PluginDeclaration>, Vec<Diagnostic>)`, with its table tests. No wiring. |
| PT.2 | The walker reserves `plugins` (not held, not reported unclaimed). Project config refuses it with a warning. |
| PT.3 | Boot wiring: the install task reads the user's `lattice.toml`, lowers it and queues it before `init.rs`, deduplicating with `init.rs` winning; `run_required` hoisted out of the `init_dir` gate. Tests: a boot with no `init.rs`, and a warm boot with no fetch. |
| PT.4 | `:reload-config` re-reads TOML (the after-boot reconcile) with no `init.rs`. |
| PT.5 | `:plugins` shows provenance and the rejected entries. |
| PT.6 | Docs sweep: guide, `:help`, the scaffold comment, the ledger, `BENCHMARKS.md`. |
| PT.7 | ⛔ A lockfile or a `:plugin-pin` write-back; needs a decision. |

## 7. Open questions

1. **(B) or (C)?** Is amending "TOML = static option overrides only"
   acceptable, or should the plugin list live in its own file?
2. **The name.** `[plugins.<id>]` sits beside the structural `plugin` prefix
   and `plugin.trace-level`. Is that acceptable, or should it be
   `[plugin.<id>]`, `[install.<id>]` or similar? What consumes the structural
   `plugin` sections (`pending_config_structural_sections`) was not verified.
3. **Removal.** Removing a declaration, or disabling it, does not unload an
   installed plugin, because the user-root scan loads everything on disk.
   Should a declaration-level disable also gate the scan, or does disabling
   stay `<id>.enabled`?
4. **Conflict.** "`init.rs` wins, warn on a difference", or an error that
   skips both?
5. **Lockfile.** Is v1 acceptable with only an explicit `sha256` pin, given
   an unpinned declaration is not reproducible across machines? Promote
   PT.7?
6. **Not verified, to check before PT.3:**
   - whether `load_persistent_config` runs before the loader task reads the
     file (proposed: the loader reads it itself, so the order cannot matter);
   - whether `default_init_dir()` is `Some` when the directory is absent;
   - how a duplicate `require` of one name from `init.rs` behaves today
     (`install_all` maps over the list, so presumably both run);
   - whether `toml` has `preserve_order` on.
