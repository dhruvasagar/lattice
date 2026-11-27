# Release plugins declared in `lattice.toml`

**Status: decided 2026-11-26.** Option (B), `[plugins.<id>]` keyed tables.
Sequencing and status: [`../operations/slice-plans/plugin-toml.md`](../operations/slice-plans/plugin-toml.md).

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
leaner front-end over the same install path, not a replacement:
`plugin-manager.md` §3 / §11 already allow a TOML list "as declarative sugar
over the same `require` for the trivial case", and this is that sugar.

## 1. How it works today

**Registration from `init.rs`.**
- The `register-plugins` export calls `plugin_manager::require(PluginSpec)` or
  `require_release(ReleaseSpec)` (`crates/lattice-wit/wit/plugin-manager.wit`).
- The host only records. An unsafe name (`is_safe_plugin_name`,
  `plugin_manager_host.rs`) returns `false` and is logged, never trapped.
- The record is mirrored into the loader's `RequiredSpec` /
  `PluginSource::Release { repo, version, sha256 }` (`pipeline.rs`,
  `to_required_spec`). Mirroring rather than sharing keeps a WIT-facing type
  safe from loader refactors.
- `PluginLoader` puts each declaration in two places
  (`lib.rs`, the plugin-manager drain):
  - `required`, the queue the install step drains once;
  - `declared`, a name → `SourceRecord` map that outlives the drain, so that
    `:plugins` can rebuild a plugin whose first install never staged a
    `.source` marker.

**Boot order** (`install.rs`). One spawned task, off the boot thread:
1. Core plugins are discovered and loaded.
2. `init.rs` is built, loaded and awaited.
3. `run_required(Boot)` drains the queue and calls `install_all` on
   `spawn_blocking` with `RefreshPolicy::UseCache`.
4. The user-root scan loads everything installed.
5. `enable_required_modes`, then `report_unclaimed_options`.

Step 2 sits inside `if let Some(init_dir)`, but `default_init_dir()` is `Some`
whenever a config root resolves, whether or not the directory exists. With no
`init.rs` the load fails at `debug!` and step 3 still runs, on an empty queue.
The boot path is therefore already toolchain-free; it only lacks a second
source for the queue.

**Per release spec** (`pipeline.rs`, `resolve.rs`):
1. `check_release_spec` refuses a repo that is not `owner/name` or a tag with
   characters GitHub does not allow.
2. Build the asset name `<id>-<ver>-api<M>.<m>.wasm` for this editor's API
   generation; an unpinned spec takes the newest compatible release.
3. Fetch `<asset>.sha256` and the asset through the hardened
   `download_to_file`; verify the digest, and the spec's `sha256` if pinned.
4. Check the embedded manifest id against the name, stage into
   `<user_root>/<id>/`, write the `.source` marker.

A failure is `Install::Skipped` plus `record_failure`, which `:plugins`
shows. One bad spec costs only that plugin.

**Cache and lock.** No lockfile; the record is the per-plugin `.source` marker
(`kind=release, repo, version, sha256, api, latest`). A warm boot with the
artifact present and the marker matching does no network I/O.
`:plugin-update` works from the marker and declines on a pinned tag. `sha256`
is recorded only when the user pinned it.

**`lattice.toml` today.**
- `lattice_config::load_default_paths` reads the user's `lattice.toml`, then
  the project's `.lattice/config.toml`. Its production caller is
  `Editor::load_persistent_config`, once at boot. **Nothing re-reads it after
  boot**: `:reload-config` rebuilds and reloads `init.rs` only. (Since
  superseded: the whole file reloads, [`config-and-init.md`](config-and-init.md) §6a.)
- Keys under an unloaded plugin's namespace (`[org]`) are held and applied
  when the plugin declares the option (`awaits_plugin`, `loader.rs`). A
  plugin's *options* already work in TOML.
- An untrusted project config refuses some tables (`PROJECT_REFUSED_TABLES`).
- The `plugin` name is taken twice: `plugin.trace-level` is a registered option
  group, and the host passes `"plugin"` as a structural prefix, so a
  `[plugin.X]` table is bucketed into `pending_config_structural_sections`,
  which no production code drains.

## 2. Static setting, or logic?

A release declaration is `{id, repo, tag, hash, enable-mode}`. It has no
conditionals, no ordering semantics, no callbacks and no computation, so it
is a static setting by CLAUDE.md's own test ("static settings stay
declarative; logic stays code").

Two tensions, stated:

- **Wording.** CLAUDE.md said "TOML covers static option overrides only". A
  plugin declaration is not an option override. The principle holds; the
  sentence is amended to "static option overrides and release-plugin
  declarations", and `plugin-manager.md` §3 / §11 / §12 and
  `config-and-init.md` §5 point here.
- **Trust.** `lattice.toml` was inert; it now fetches and runs third-party
  code at boot. That code is still capability-gated and sandboxed by its
  manifest, but the vector is new. It drives three rules:
  - project config may not declare plugins;
  - key validation is strict;
  - the `sha256` pin is documented.

## 3. Options considered

All three lower to the existing `RequiredSpec` / `PluginSource::Release`.
None adds a WIT function or a crate.

### (A) A `[[plugins]]` array of tables — rejected

```toml
[[plugins]]
name = "org"
repo = "owner/lattice-org-plugin"
version = "v0.2.0"
```

> **UX (higher court):** neutral; installs with the same eventual
> consistency as `init.rs`, off the keystroke path.
> **Paramount goals:** protects #2 (no-toolchain users get plugins) and #1
> (warm boot stays network-free); sacrifices none.
> **Heuristic #1:** the weaker fit. Arrays allow duplicate `name`s silently,
> so uniqueness is hand-written code; and the id is a value here but the
> *key* everywhere else (`[org]`, `org.enabled`, the install directory).
> **Heuristic #2:** anchored on #2. **Heuristic #3:** see B and C.
> **Heuristic #6:** no new crate. **Mode ownership:** no chord, command or
> action touched.

### (B) `[plugins.<id>]` keyed tables, with a one-line shorthand — chosen

```toml
[plugins.org]
release = "owner/lattice-org-plugin"   # required: owner/name
version = "v0.2.0"                     # optional; omitted = newest built for this editor
sha256  = "3f9a…"                      # optional; pins the bytes, not just the tag
enable-mode = "org-mode"               # optional; the same string init.rs takes

[plugins]
auto-pair-extras = "someone/lattice-auto-pair-extras@v1.2.0"   # shorthand: repo[@tag]

[org]                                  # the plugin's own options, unchanged
directory = "~/notes"
```

> **UX:** as A, and it reads beside the plugin's own `[org]` options under
> the same key.
> **Paramount goals:** as A.
> **Heuristic #1:** the best fit. A duplicate id is a TOML parse error by
> construction; the id is the key, as everywhere else; the shorthand is the
> trivial case and the table the full one, lowered through one path.
> **Heuristic #2:** anchored on #2. **Heuristic #3:** the shorthand makes it
> a mild superset of A.
> **Heuristic #6:** no new crate. `lattice-config` parses into a neutral
> `PluginDeclaration` (it already depends on `toml`; the loader does not);
> `lattice-plugin-loader` lowers it to `RequiredSpec`, the same
> mirror-not-share pattern as `to_required_spec`.
> **Mode ownership:** as A; `enable-mode` stays an opaque string.

`toml` 0.8 without `preserve_order` iterates a table alphabetically, not in
file order. Harmless: installs are independent.

### (C) A separate `plugins.toml` — rejected

> **UX:** neutral to slightly better on trust (a shared dotfile fragment
> cannot install code by accident); worse on discoverability (a second file
> to learn).
> **Paramount goals:** #2 as A.
> **Heuristic #1:** cleaner on trust and on lockfile placement, but the
> argument is trust, not a paramount goal. **Heuristic #2:** not anchored on
> a paramount goal.
> **Heuristic #6:** no new crate.

Rejected because the ask names `lattice.toml`, the trust argument is answered
by the project-config refusal (§4), and the lowering is identical, so moving
the table later costs only a parser entry point.

## 4. Behaviour

### Schema and validation

`lattice-config` checks shape; the existing pipeline checks meaning. One
validation path for each fact, shared with `init.rs`:

| check | where | why there |
|---|---|---|
| Value is a table or a string | `lattice-config` parser | shape |
| Table keys ⊆ `release`, `version`, `sha256`, `enable-mode`; `release` present; every value a string | parser | shape; an unknown key is a typo (`sha265`) that, dropped, would install unpinned |
| String is `repo` or `repo@tag` | parser | shape |
| `sha256`, when present, is 64 hex characters | parser | a malformed pin must never read as "no pin" |
| Id is a safe path component | loader lowering, `is_safe_plugin_name` | the gate `init.rs` passes; `lattice-config` cannot depend on `lattice-plugin-host` |
| Repo is `owner/name`, tag is well-formed | `resolve::check_release_spec`, at install | already there, already records failures |

**A non-release key** (`git`, `path`, `url`, `build`, `rev`) skips the entry
with a message naming why: "builds from source; declare it in init.rs". Any
other unknown key also skips the whole entry.

Every rejection is a `warn!` and a `record_failure` under the declared id, so
`:plugins` shows it. Log and skip, never panic. One bad entry does not stop
the others.

### The `plugins` table is not an option namespace

`lattice-config`'s walker reserves the top-level `plugins` key in every load:
it is not applied, not held, and never reported unclaimed. The reservation
lives in the walker, not in the host's structural-prefix list, so every
caller of `load_default_paths` gets it.

A `[plugin.<x>]` table carrying a `release` key gets a warning, "did you mean
`[plugins.<x>]`?", since the structural bucket it would otherwise land in is
never read.

### Project config refuses it

`plugins` joins `PROJECT_REFUSED_TABLES`: a cloned repository's
`.lattice/config.toml` never installs code. Same rule, same warning shape as
`llm`.

### Who reads the file

The install task reads the user's `lattice.toml` itself
(`lattice_config::default_user_config_path`, through
`lattice_config::read_plugin_declarations`) before `init.rs` loads,
so the order relative to `Editor::load_persistent_config` cannot matter. A
parse error is `debug!` on this side; the editor's own load already reports
it to the user, and reporting it twice is noise.

### `init.rs` and TOML naming the same plugin

- **`init.rs` wins.** It is the superset and the more specific layer.
- The whole spec is replaced, never merged field by field: "TOML's version,
  init.rs's sha256" would surprise.
- One `warn!` names both when the specs differ; `debug!` when identical.
- Never an error, never a refused boot.

TOML ids are unique by construction (a shorthand and a table under one id is a
TOML duplicate-key error).

### One declaration path, with provenance

Both front-ends go through one `PluginLoader::declare` that fills `required` and
`declared` together, tagging each queue entry with its origin
(`DeclaredBy::InitRs` / `DeclaredBy::LatticeToml`). `RequiredSpec` itself does
not change: it mirrors a WIT-facing type, and nothing after the merge reads the
origin. The merge happens when the
queue is drained, after `init.rs` has loaded, so precedence is decided in one
place with both sets visible. `:plugins` shows the origin of each declared
plugin and the TOML entries the parser rejected.

### Options and grants

- Nothing new for options: `[org]` is already held and applied.
- No `grants` key. Grants follow the plugin's own options
  (`plugin-manifest.md` §2), so TOML cannot widen a plugin past its manifest.

### Enable and disable

- `<id>.enabled` keeps working for plugins that declare a default mode.
- No declaration-level `enabled = false`. The user-root scan loads every
  installed plugin whether or not it is declared, so removing a declaration
  unloads nothing. That is true of `init.rs` today, and the two front-ends
  must not differ on it. Uninstalling stays the `:plugins` view's `clean`
  verb.

### `:reload-config`

`:reload-config` re-reads the `[plugins]` table and reconciles it through
`run_required(AfterBoot)`: a newly declared plugin is installed and loaded, an
unchanged one left alone. With no `init.rs` on disk, the init rebuild step is
a benign skip, not a recorded `init` failure.

The rest of `lattice.toml` is re-applied by the same command, before this
reconcile: see [`config-and-init.md`](config-and-init.md) §6a.

### Pinning

v1 reuses `version` + `sha256` and the existing `.source` markers; there is no
lockfile. The guide says plainly that an unpinned declaration records no
digest and is not reproducible across machines. A lockfile is deferred (slice
plan PT.7).

## 5. Paramount-goal alignment

- **#1 Performance.** Parsing one table at boot is microseconds and off the
  boot thread; a warm boot does no network I/O, pinned by a counting-fetcher
  test. Installs stay on `spawn_blocking`.
- **#2 Extensibility.** The point of the feature: plugins without a toolchain.
  WIT unchanged; the SDK gains nothing the wire lacks.
- **#3 Modal editing.** Untouched.
- **#4 Asynchronicity.** Same off-thread install task; nothing new on the
  actor or UI thread.

## 6. Settled decisions

1. **Surface** — (B) `[plugins.<id>]` in `lattice.toml`, with a `repo[@tag]`
   shorthand. (C) rejected (§3).
2. **Name** — `plugins`. `plugin` is taken by an option group and a
   structural prefix (§1).
3. **Removal** — no declaration-level disable; `<id>.enabled` and `clean`
   cover it, as for `init.rs`.
4. **Conflict** — `init.rs` wins, whole spec, `warn!` on a difference.
5. **Lockfile** — deferred; v1 is `version` + optional `sha256`.
6. **`:reload-config`** — reconciles `[plugins]`; the rest of the file is
   re-applied by the whole-config reload ([`config-and-init.md`](config-and-init.md) §6a).

## 7. Rejected alternatives

- **A `[[plugins]]` array** (§3 A): uniqueness by hand, id as a value.
- **A separate `plugins.toml`** (§3 C): a second file for a trust problem the
  project-config refusal already solves.
- **Merging TOML and `init.rs` specs field by field**: a spec assembled from
  two files is one nobody wrote.
- **Accepting `git` / `path` sources in TOML**: they build from source, which
  needs the toolchain this feature exists to avoid; and a build step in a
  static file is logic.
- **Validating repo and tag in `lattice-config`**: duplicates
  `check_release_spec`, and two validators drift.
