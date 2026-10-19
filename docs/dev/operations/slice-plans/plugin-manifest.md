# Plugin manifest — slice plan (PMF)

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/plugin-manifest.md`](../../architecture/plugin-manifest.md).
> Changes the plan in [`plugin-distribution.md`](plugin-distribution.md):
> its package becomes the component alone and PRL.6 is replaced by PMF.3–PMF.5.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).
Every non-trivial slice ships the four artefacts (doc + bench where perf-relevant
+ tests including failure modes + graceful error handling).

**Status: 🚧 in progress (2026-10-17).** The embedding spike passed (design §1).
Branch `plugin-manifest`, merged by PR; targets **0.9.10**.

| Slice | What | Repo | Status |
|---|---|---|---|
| PMF.1 | Read a `lattice-manifest` custom section from the component; embedded wins over `plugin.toml`; `plugin.toml`-only plugins unchanged; `min_lattice` refuses a plugin that needs a newer editor. | lattice | ✅ |
| PMF.2 | `lattice-plugin-sdk::manifest!` emits the section (and hides the unsafe attribute). | lattice | ✅ |
| PMF.3 | Option-bound grants: `fs:read/write:{option\|default}` parsed, resolved at grant time from the user's value or the default; same-plugin rule. | lattice | ✅ |
| PMF.4 | A grant-bearing option changing reloads its plugin when the resolved path differs; default-vs-registered mismatch warns. | lattice | ✅ |
| PMF.5 | Grant-bearing options cannot be set by plugins (except `init.rs`), refused by name. | lattice | ✅ |
| PMF.6 | `:plugins` shows expanded grants and their origin; out-of-grant refusals name the option. | lattice | ✅ |
| PMF.7 | Docs: `:help plugins` "Capabilities and your configuration", plugin-authoring guide, manifest custom-section schema beside the WIT. | lattice | ✅ |
| PMF.8 | Org adopts: embedded manifest, `{org.directory\|~/org}` / `{org.roam-directory\|~/org/roam}`, defaults registered to match; `plugin.toml` removed; `org.md` and README updated. | lattice-org-plugin + lattice docs | ✅ |
| PMF.9 | Revise the distribution design and plan to the one-file package. | lattice | ✅ |
| PMF.10 | A source with no `plugin.toml` builds: an in-place project is a cargo project; ids come from the built component; a staged copy left by an older build is removed. | lattice | ✅ |
| PMF.11 | Bundled plugins embed their manifests (`manifest!`); their `plugin.toml` files, the core-plugin staging and release-archive checks that expected them, and the tests that copied them, go. | lattice | ✅ |
| PMF.12 | `--scaffold-init` / `--scaffold-plugin` write `manifest!` instead of `plugin.toml`. Existing configs keep theirs and keep working. | lattice | ✅ |
| PMF.13 | Loads, reloads and the scan's check-then-load are serialised (`load_ops`): no caller sees a plugin in the gap of another's reload. | lattice | ✅ |
| PMF.14 | `require` reconciles instead of loading: at boot it installs and the user-root scan is the one loader; after boot (`:reload-config`) it loads what is new, reloads what was rebuilt, and is a no-op otherwise. | lattice | ✅ |

## Sequencing

	PMF.1 ──► PMF.2 ──────────────────────────────┐
	PMF.3 ──► PMF.4 ──► PMF.5 ──► PMF.6 ──► PMF.7 ─┴─► PMF.8
	PMF.1 ──► PMF.10 ──► PMF.11 ──► PMF.12 ──► PMF.8
	PMF.4 ──► PMF.13 ──► PMF.14
	PMF.9 (docs only, any time after the design is accepted)

PMF.1 and PMF.3 are independent: one is where the manifest comes from, the
other what a grant means. PMF.8 needs both, because org drops `plugin.toml`
and moves its grants to options in the same release.

## PMF.1 — the embedded manifest

`lattice-plugin-host::manifest` gains `from_component_bytes(&[u8]) ->
Option<PluginManifest>`: walk the component's sections, descend into the
wrapped core module, find `lattice-manifest`, parse its payload with the
existing manifest parser. The loader asks it before looking for
`plugin.toml`; a staged directory with no `plugin.toml` but an embedded
manifest is discovered as a plugin.

`min_lattice` (either source of manifest): a version newer than the running
editor (`CARGO_PKG_VERSION`) refuses the plugin with
`"<id> needs lattice ≥ <v>; this is <running>"`, recorded as a failed load so
`:plugins` shows it. Absent or unparseable-as-semver: absent is accepted,
unparseable is a named manifest error.

**Tests:** a fixture component with the section (built in-test by appending a
custom section — valid at the component's top level too, which the reader also
accepts); embedded wins over a conflicting file; a file-only plugin loads as
before; a malformed payload is a named load error, never a panic;
`min_lattice` above the running version refused with the message, equal and
below accepted.

## PMF.2 — `manifest!`

A declarative macro in `lattice-plugin-sdk` producing the TOML payload and the
`#[unsafe(link_section = "lattice-manifest")] #[used]` static, with
`#[allow(unsafe_code)]` on the item. Published crates: a **patch** bump of
all three (`lattice-wit`, `lattice-plugin-sdk`, `lattice-plugin-sdk-derive`
→ `0.3.1`). They share one version whose `major.minor` is the WIT
generation; the patch is the crates' own, so the WIT package stays `@0.3.0`
and no plugin's ABI moves. Not `cargo xtask bump-plugin-api`, which moves
the WIT package too. Publishing (`scripts/publish-plugin-api.sh`) runs from
`main` after the merge.

Shape as built: `manifest!(<TOML text>)`, the same schema as `plugin.toml`,
validated by the loader at load time (a named error in `:plugins`). A
field-syntax proc macro that checks the manifest at compile time is possible
later; it would need the host's capability grammar in the derive crate.

**Tests:** a compile test that the macro builds under `#![deny(unsafe_code)]`;
the payload round-trips through the host parser.

## PMF.3 — option-bound grants

`Capability` gains a templated form for `fs:read` / `fs:write`:
`{name|default}`. Granting resolves it against the user's layers for `name`,
else the default, `~`-expanded; explicit empty → no grant. After the plugin's
`register-options`, a template naming an option the plugin did not register is
a load error.

**Tests:** default used when unset; user value used when set; empty grants
nothing; another plugin's option refused; malformed template a parse error.

## PMF.4 — reload on change

The loader subscribes to option changes for every grant-bearing name; on a
change it re-resolves, and reloads the plugin only when the resolved path
differs. A default that disagrees with the registered option default warns in
`:plugins` and `*messages*`.

**Tests:** `:set org.directory=…` reloads with the new grant; setting it to its
current value does not reload; a value `init.rs` sets during the load reloads
once. (Not `lattice.toml`: a plugin's options are not settable there — it is
read before any plugin registers them; `init.rs` is the place.)

## PMF.5 — plugins cannot widen their grant

`set-option` / `set-option-value` on a grant-bearing option from any plugin
other than `init.rs` is refused with the reason.

**Tests:** org's own `set-option("org.directory", "/")` refused; `init.rs`
allowed; a non-grant option unaffected.

## PMF.6 — visible grants

The `:plugins` capability column shows each resolved grant and its origin
(`org.directory` / `org.directory, default`). Refusals for paths outside a
templated grant name the option and its current value.

## PMF.7 — docs

`docs/user/plugins.md` gains "Capabilities and your configuration": what a
grant is, that some follow an option, how to see them (`:plugins`), and that
setting the option is how you move them. The authoring guide documents
`manifest!` and templated grants; the custom-section schema is documented
beside the WIT reference.

## PMF.8 — org adopts

In `lattice-org-plugin`: `manifest!` replacing `plugin.toml`; grants
`fs:write:{org.directory|~/org}` and `fs:write:{org.roam-directory|~/org/roam}`;
`org.directory` / `org.roam-directory` registered with the same defaults.
Its manifest declares `min_lattice = "0.9.10"`. Released as a minor version. Lattice's `org.md` names the two options as the
ones that decide where org may write.

As built (org `aacd323`): defaults are emacs's — `~/org` and `~/org-roam`
(not `~/org/roam`), so roam needs its own grant. Roam's "unset means inert"
became "absent means inert": a directory that does not exist keeps it off,
which a missing preopen makes true inside the guest too. The section is
written directly, not with `manifest!`, so org builds from crates.io until
plugin-API 0.3.1 is published. Its integration tests strip the embedded
manifest (`without_embedded_manifest`) to load under their own.

## PMF.10 — sources without a `plugin.toml`

Until now a plugin source was recognised by `plugin.toml` beside
`Cargo.toml`. A `manifest!` plugin has no `plugin.toml` before it is built —
or after — so `is_in_place_project` becomes "a cargo project in the plugins
directory", `manifest_id` falls back to the built component's embedded
manifest, and the init directory counts as configured when it holds a cargo
project. Staging a source that dropped its `plugin.toml` removes the copy an
earlier build left in the staging directory.

**Tests:** a `Cargo.toml`-only directory is in place; a source that moves its
manifest into the component stages without the file and is still named.

## PMF.11 — bundled plugins embed

Each of auto-pair, treesitter-context, project, comment and lighthouse gains a
`manifest!` carrying its `plugin.toml` (comments preserved as Rust comments)
and loses the file. `xtask build-core-plugins` stages the component alone;
`release.yml`'s archive checks and `the_deb_carries_every_core_plugin` stop
expecting `plugin.toml`; tests and the TUI's comment-plugin loader stop
copying it.

## PMF.12 — scaffolds embed

`--scaffold-init` and `--scaffold-plugin` write the manifest into `src/lib.rs`
and write no `plugin.toml`.

As built: the section is written directly (`#[unsafe(link_section =
"lattice-manifest")] static … = *MANIFEST.first_chunk().unwrap()`), not with
`manifest!`. The scaffolds depend on `wit-bindgen` alone, and `manifest!`
ships in plugin-API 0.3.1, which is published from `main` after this branch
merges — a scaffold needing it would fail `the_scaffolds_compile` on the PR
that introduces it. Switching the scaffolds to `manifest!` is a one-line
follow-up once 0.3.1 is on crates.io, if wanted.

## PMF.13 — loads are serialised

**Found in use (2026-10-18).** Boot has two paths into a `require`d plugin's
directory: `install_required_plugins` loads it, and the user-root scan then
walks the same root, skipping anything loaded. A user's `init.rs` sets
`org.roam-directory` on `pre-plugin-loaded` — mid-load, after the grant was
fixed — so PMF.4 schedules a reload. When the scan reached `org/` between that
reload's unload and its load, "is org loaded?" answered no and it loaded a
second copy: two `org` rows in `:plugins`, or one beside a failure ("declares
no loadable seam" — the second copy's `org-completion-mode` collided).

`PluginLoader::load_ops`, a `tokio` mutex held by `load_path`, by `reload`
across its unload *and* load, and by the scan around each check-then-load.
Not boot-specific: `:plugin-load`, `:plugin-reload` and the plugins-view
chords can each meet a reload's gap the same way.

**Tests:** `a_scan_during_a_grant_reload_does_not_load_a_second_copy` boots
`load_path` then the scan eight times; without the lock round 0 ends with two
copies.

## PMF.14 — `require` reconciles

Today `install_required_plugins` runs once, at boot, and calls `load_path` for
every spec whether or not anything was installed or rebuilt. After boot it is
never drained, so a `require` added before `:reload-config` is recorded and
not installed until a restart.

`require` becomes a declaration the loader reconciles against what is on disk
and loaded:

| When | What happens |
|---|---|
| Boot | Install / build / stage only. The user-root scan, which runs next, is the single loader; `enable_mode` is applied after it. |
| After boot, newly installed | Loaded. |
| After boot, loaded and rebuilt | Reloaded onto the new build. |
| After boot, loaded and unchanged | Nothing. |

One loader per directory at boot removes the duplicate by construction; PMF.13
stays for every path that is not boot. `:reload-config` drains the specs its
`init.rs` records.

**Tests:** a warm boot loads a required plugin once and builds nothing; a
`require` added before `:reload-config` is installed and loaded without a
restart; a rebuilt required plugin reloads; an unchanged one is not touched;
`enable_mode` applies to a plugin the scan loaded.

As built: `PluginLoader::apply_required(installs, user_root, phase)` holds
the rules and `enable_required_modes` applies boot's deferred modes after the
scan; `Install::Ready` gained `rebuilt` (a fresh build). The post-boot drain
runs at the end of `sync_init`, so `:reload-config` and init's file watcher
both reconcile. `tests/require_reconcile.rs` drives the decisions with
pipeline results, no git or cargo. A `require` *removed* from `init.rs` still
leaves its plugin loaded until restart.

**Not this slice:** org still loads twice per boot when `init.rs` sets a
grant-bearing option on `pre-plugin-loaded` — the grant is fixed before the
option exists. Removing that reload needs `init.rs` to be able to set an option
before the plugin registers it (held, applied at registration); it is a
`set-option` contract change and is decided separately.

## PMF.9 — distribution follows

`plugin-distribution.md` §2 package → the component and its `.sha256`;
§3 drops `grants` from `release-spec`; §5 points at PMF; PRL.6 marked ❌
(superseded by PMF.3–PMF.5).
