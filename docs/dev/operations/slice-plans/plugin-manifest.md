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
| PMF.8 | Org adopts: embedded manifest, `{org.directory\|~/org}` / `{org.roam-directory\|~/org/roam}`, defaults registered to match; `plugin.toml` removed; `org.md` and README updated. | lattice-org-plugin + lattice docs | 📝 |
| PMF.9 | Revise the distribution design and plan to the one-file package. | lattice | ✅ |
| PMF.10 | A source with no `plugin.toml` builds: an in-place project is a cargo project; ids come from the built component; a staged copy left by an older build is removed. | lattice | ✅ |
| PMF.11 | Bundled plugins embed their manifests (`manifest!`); their `plugin.toml` files, the core-plugin staging and release-archive checks that expected them, and the tests that copied them, go. | lattice | ✅ |
| PMF.12 | `--scaffold-init` / `--scaffold-plugin` write `manifest!` instead of `plugin.toml`. Existing configs keep theirs and keep working. | lattice | 📝 |

## Sequencing

	PMF.1 ──► PMF.2 ──────────────────────────────┐
	PMF.3 ──► PMF.4 ──► PMF.5 ──► PMF.6 ──► PMF.7 ─┴─► PMF.8
	PMF.1 ──► PMF.10 ──► PMF.11 ──► PMF.12 ──► PMF.8
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
current value does not reload; `lattice.toml` read after boot reloads once.

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
with `manifest!`, depend on `lattice-plugin-sdk`, and write no `plugin.toml`.

## PMF.9 — distribution follows

`plugin-distribution.md` §2 package → the component and its `.sha256`;
§3 drops `grants` from `release-spec`; §5 points at PMF; PRL.6 marked ❌
(superseded by PMF.3–PMF.5).
