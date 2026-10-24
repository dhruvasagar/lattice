# Plugin distribution — installing a plugin without a toolchain

Status: design fragment (2026-10-16). Slice plan:
[`../operations/slice-plans/plugin-distribution.md`](../operations/slice-plans/plugin-distribution.md).

> **Amended 2026-10-17** by [`plugin-manifest.md`](plugin-manifest.md): a
> component now carries its own manifest, so a release asset is the component
> alone, and filesystem grants follow the plugin's own options instead of a
> `grants` field. §2, §3 and §5 below are revised to match; the rest stands.

Anchors: [`plugin-manager.md`](plugin-manager.md) §4 (the source model this
adds a kind to), §7 (the no-toolchain promise this keeps), §8 (`:plugins`);
[`release-pipeline.md`](release-pipeline.md) (the editor's own release, whose
core-plugin job is the build recipe reused here);
[`wit-ownership.md`](wit-ownership.md) (ABI skew); paramount goal #2.

Every user plugin today is built on the user's machine. `require` with a `Git`
or `Local` source runs `cargo build --release --target wasm32-wasip2`, so a
user needs rustup, the `wasm32-wasip2` target and — for a plugin that compiles
a tree-sitter grammar, as org does — clang and network access during the
build. That is right for someone developing a plugin and a wall for someone
who wants to use one.

This fragment adds a way to install a plugin its author has already built. It
**adds** a path; it changes none. `Local`, `Git`, build-on-boot, the
`.build-stamp` staleness rule and the bundled core plugins work exactly as
they do.

## 1. What exists, and why it is not enough

`PluginSource::Prebuilt { url }` was designed as this path (plugin-manager.md
§7: "the no-toolchain path for user plugins too") and built as far as a
download. As of this writing:

| Gap | Consequence |
|---|---|
| Downloads a bare `.wasm`; synthesizes a `plugin.toml` holding only `id` | No `provides`, so the loader drains no seams: a real plugin loads and contributes **nothing**. No `capabilities` either. |
| Re-downloads on every resolve, boot included | A network round trip per plugin per start, and no offline boot. The doc comment says "whenever the artifact is missing"; the code does not. |
| `ureq` GET with no timeout, size cap, or hash | A stalled server hangs the install task; a truncated or substituted file is loaded as-is. |
| No version, no ABI record | A component built for another `lattice:plugin-host` generation fails at instantiation with an error that names neither generation. Source builds at least get the `.build-stamp` fingerprint check. |
| No end-to-end test | `HttpFetcher` has never run under test. |

And the docs disagree: `plugin-authoring.md` says the manager "does not
download a prebuilt `.wasm`"; `plugin-manager.md` says it is the path for
users without a toolchain.

The fix is not to patch `Prebuilt{url}` into shape. A URL names bytes; it does
not name a plugin, a version, or the API generation those bytes target — and
those three are what a user means when they ask for "org, 0.2". The source
kind has to carry them.

## 2. The artefact: the component, carrying its manifest

A release publishes, per plugin API generation it supports:

	<id>-<version>-api<MAJOR>.<MINOR>.wasm
	<id>-<version>-api<MAJOR>.<MINOR>.wasm.sha256

e.g. `org-0.2.0-api0.3.wasm`. The component carries its manifest in a
`lattice-manifest` custom section
([`plugin-manifest.md`](plugin-manifest.md) §1), so it is the whole plugin:
the installer writes it into `~/.config/lattice/plugins/<id>/` — through a
temporary sibling renamed into place once complete, so an interrupted install
leaves the previous version loadable — and discovery reads the manifest from
it. The publishing workflow (§8) refuses a component with no embedded
manifest: a released plugin is one file or it is not released.

**Why `api<MAJOR>.<MINOR>` in the name.** It lets the installer choose — and
refuse — *before* downloading (§4), and lets one release carry builds for two
API generations across a host bump. Major.minor is the whole compatibility
key: `releasing.md` holds `lattice-wit`'s major.minor equal to the WIT
package's, and a patch never changes the ABI.

**Superseded:** this section first specified a `.tar.gz` holding
`plugin.toml` + the component. An archive was needed only because the
manifest was a second file; with it embedded, the archive, its extractor and
its entry-validation rules are unnecessary.

## 3. The source kind: `Release`

	Release {
		repo:    "owner/name",        // a GitHub repository
		version: Option<"vX.Y.Z">,    // a tag; None = newest compatible
		sha256:  Option<String>,      // pin the bytes, not just the tag
	}

With a version, the asset URL is a convention, not a lookup:

	https://github.com/<repo>/releases/download/<version>/<id>-<ver>-api<M>.<m>.wasm

so a pinned install makes no API call and meets no rate limit. Without one,
the installer asks the GitHub releases API once for the newest non-draft,
non-prerelease release carrying an asset for this editor's API generation,
installs it, and records the resolved tag (§6). Boot never asks again; only
`update` does.

`<id>` is the spec's `name`, and before installing the installer checks the
component's embedded manifest declares the same `id`. A mismatch is refused:
the user asked for one plugin and the release holds another.

**Why GitHub releases, and not an index.** A central index — `id → repo,
versions, hashes` — is the better end state for discovery ("what plugins
exist?") and is deliberately **not** this fragment. It is a service someone
has to run and moderate; a release source needs nothing but the host every
plugin already lives on. The index, when it comes, resolves an id to a
`Release` spec: it sits on top of this, it does not replace it.
GitHub-specific URLs are confined to one resolver function, so a second forge
is a second function.

### The WIT shape — a new function, not a new case

The obvious change is a fourth case on `variant plugin-source`. It is also a
breaking one: a variant's shape is part of the component type, so every
compiled `init.rs` would fail to link against a host whose variant had grown —
and `releasing.md`'s policy is that a package bump strands every plugin pinned
to the old generation. Adding a *function* to an interface is additive — a
guest that does not import it is unaffected — so the seam is:

	record release-spec {
		name: string,
		repo: string,
		version: option<string>,
		sha256: option<string>,
		enable-mode: option<string>,
	}

	require-release: func(spec: release-spec) -> bool;

It records and returns exactly as `require` does (plugin-manager.wit,
"Recording, not doing"); resolution runs off-thread after the export returns.

There is no `grants` field: filesystem grants follow the plugin's own options
(§5), which the user sets in their config whatever the source kind.

## 4. Compatibility is checked before the download

The host knows its own generation (`lattice:plugin-host@0.4.x`) and builds the
asset name from it, so a release with no build for this editor is a **named**
refusal in `:plugins` and `*messages*`:

	org v0.2.0 has no build for plugin API 0.4 (it ships: api0.3)

— the release's asset list fetched only on that failure, to say what *is*
available — instead of a download followed by an instantiation error that
names nothing.

What this does not cover: a package whose name claims a generation its
component was not built for. The publishing workflow (§8) derives the label
from the build's own locked `lattice-wit`, so the label is not hand-written;
if it is still wrong, instantiation fails as for any skewed component, and
`:plugins` reports it with the generation the package claimed. A source
build's `.build-stamp` fingerprint is not available here — it is written by
the editor's build service from the editor's own `lattice-wit`, and a CI build
has neither.

## 5. Capabilities: the user's configuration decides

Superseded by [`plugin-manifest.md`](plugin-manifest.md) §2. This section
first proposed a `grants` list on `release-spec` replacing the manifest's
capabilities. Instead, a filesystem grant names one of the plugin's own
options with a default — `fs:write:{org.directory|~/org}` — so the path comes
from the user's configuration for every source kind, release installs
included, and no user path ships in any plugin.

## 6. On disk, and when the network is touched

The package unpacks to the same `<user_root>/<id>/` a build stages into. The
`.source` marker gains a kind:

	kind = release
	repo = owner/name
	version = v0.2.0
	sha256 = 3f9a…
	api = 0.3
	latest = true

`latest` marks a record whose spec named no version: `version` is the tag it
resolved to, not a pin. Without it the record reads back as pinned to that
tag, and `update` — which works from the record — would fetch the same
release forever.

| Caller | Network |
|---|---|
| Boot, artifact present, `.source` matches the spec | **none** |
| Boot, artifact absent, or the spec's version / sha256 changed | download |
| `update`, unpinned spec | one API call; a download only if a newer compatible release exists |
| `update`, pinned spec | none — it declines, naming the pin, as a pinned git rev does |

This is `RefreshPolicy` (plugin-manager.md §4) applied as written — the rule
`Git` already follows.

**Not a second downloader.** Lighthouse's LH.0 already built this, hardened,
in `lattice-plugin-host`: `download_host` (connect / response / body timeouts,
a byte ceiling, sha256 verified before the file is kept). It is reachable today
only as a guest-facing, capability-checked job. The installer calls the same
core from the host side, run on `spawn_blocking`; the slice that does this
factors it out of its job wrapper rather than copying it.

## 7. Integrity

Every package is checked against its `.sha256`. That catches truncation and
corruption; it does **not** catch a substituted release, since both files come
from the same place. The spec's `sha256` field is the stronger form: the user
pins the bytes, and a re-published tag is refused. Signing — GitHub artifact
attestations or minisign — is the real answer to substitution and is
deferred, not dismissed.

## 8. Publishing: a reusable workflow

Authors get the toolchain problem solved once, in CI. Lattice publishes a
reusable workflow, `.github/workflows/plugin-release.yml`, that a plugin
repository calls on a tag push:

	on:
		push:
			tags: ["v*"]
	jobs:
		ci:
			uses: ./.github/workflows/ci.yml
			secrets: inherit
		release:
			needs: ci
			uses: dhruvasagar/lattice/.github/workflows/plugin-release.yml@<ref>

It checks out the tag; refuses unless the tag equals `Cargo.toml`'s version;
builds `wasm32-wasip2` with clang available (org's grammar); refuses an empty
artefact; reads the API generation from the build's locked `lattice-wit`;
packs, hashes and attaches both files to a GitHub release whose body is the
tag message.

`needs: ci` gates it on the plugin's own CI, run against the tagged commit.
This workflow cannot see that CI — it belongs to the plugin — so the caller
joins the two; otherwise the tag push and the branch push run unconnected
and a release can publish beside a red build. Lattice gates its own releases
the same way (`release-pipeline.md`).

Its build step is the core-plugin job's recipe in `release.yml`. The two stay
one recipe — a script both call — because two copies of "how a plugin is built
for release" will drift, and the symptom (a package that behaves unlike a
source build) is invisible until a user meets it.

A plugin that does not opt in loses nothing: its users still `require` it from
git.

## 9. Paramount goals

- **#2 extensibility** — the reason for the feature: a plugin becomes
  installable by anyone who can edit `init.rs`, which is what "plugins ship in
  any language" has to mean for users, not only for authors.
- **#1 performance** — protected: a warm boot does no network I/O and no
  build (§6); every download and unpack is off-thread; a cold install is
  eventual, as a cold source build already is.
- **#4 asynchronicity** — install results reach `:plugins` through the
  install pipeline's existing wake, not a new path.

## 10. Rejected alternatives

- **Patch `Prebuilt{url}` instead.** It cannot carry a version or an API
  generation without parsing them out of a URL, and its variant case cannot
  gain fields without breaking the WIT (§3). It stays, as the "I have a URL"
  escape hatch, with its re-download and timeout bugs fixed on their own.
- **A central registry now.** §3: right later, a service to run now, and
  layered on `Release` when it comes.
- **Have the editor install a toolchain on demand.** Replaces "install a
  toolchain" with "the editor installs one" — the same hundreds of megabytes
  and the same clang problem, now on the user's first boot.
- **Ship plugins inside the editor's release.** That is what core plugins are,
  and plugin-manager.md rejects it for user plugins: it couples plugin
  releases to editor releases.
