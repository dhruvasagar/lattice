# Plugin distribution — slice plan (PRL)

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/plugin-distribution.md`](../../architecture/plugin-distribution.md).
> Amended by [`plugin-manifest.md`](plugin-manifest.md) (2026-10-17): the
> release asset is the component alone (no archive), and PRL.6 is superseded.
> Builds on the plugin manager's user track (PM.5–PM.9, tracked in
> [`../implementation.md`](../implementation.md) and
> [`archive/plugin-loader.md`](archive/plugin-loader.md)) and reuses
> lighthouse's LH.0 download core ([`lighthouse.md`](lighthouse.md)).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).
Every non-trivial slice ships the four artefacts (doc + bench where perf-relevant
+ tests including failure modes + graceful error handling).

**Status: 🚧 in progress (2026-10-19).**

| Slice | What | Repo | Status |
|---|---|---|---|
| PRL.1 | Factor LH.0's download core out of its job wrapper, host-callable. No behaviour change. | lattice | ✅ |
| PRL.2 | `Prebuilt{url}` bug fixes: no re-download when present; timeouts and a byte ceiling via PRL.1. | lattice | 📝 |
| PRL.3 | The package installer: `PluginSource::Release`, convention URL, sha256, id check, swap into place, `.source` kind `release`. | lattice | 📝 |
| PRL.4 | Compatibility and resolution: API generation in the asset name, the named refusal, newest-compatible lookup for unpinned specs and `update`. | lattice | 📝 |
| PRL.5 | WIT `require-release` + host wiring; `:plugins` source label. | lattice | 📝 |
| PRL.6 | User grants on `release-spec`. | lattice | ❌ superseded by option-bound grants, [`plugin-manifest.md`](plugin-manifest.md) PMF.3–PMF.5 |
| PRL.7 | Publishing: one shared plugin build script; reusable `plugin-release.yml`; `release.yml`'s core-plugin job calls the same script. | lattice | 📝 |
| PRL.8 | Org adopts the workflow; first published release. | lattice-org-plugin | 📝 |
| PRL.9 | User and author docs; fix the `plugin-authoring.md` contradiction. | lattice | 📝 |
| PRL.10 | Signed packages (attestations or minisign). | lattice | ⛔ after PRL.8 has shipped real releases — the threat model needs a real publisher to be written against (design §7). |
| PRL.11 | A plugin index resolving an id to a `Release` spec. | — | ⛔ until there are enough third-party plugins for discovery to be the problem (design §3). |

## Sequencing

	PRL.1 ──► PRL.2                              (bug fixes, ship alone)
	   └────► PRL.3 ──► PRL.4 ──► PRL.5 ──► PRL.6
	                                 │
	PRL.7 ───────────────────────────┴──► PRL.8 ──► PRL.9

PRL.7 is independent of PRL.1–6 and can run in parallel: it produces packages,
the others consume them. PRL.8 needs both — a published package and an editor
that installs one — because PRL.8 is the end-to-end proof. PRL.9 lands last,
so the docs describe what shipped rather than what was planned.

## PRL.1 — the download core, host-callable

`download_host` in `lattice-plugin-host` holds the hardened logic (timeouts,
byte ceiling, sha256 before the file is kept). It is reachable today only
through a guest job carrying a `CapabilityGrant`. Split it into a plain
function over a request and its limits that the job wrapper calls, and export
it for `lattice-plugin-loader`. (The release asset is a single component since
PMF, so the extract core is no longer needed here.)

**Done when:** every LH.0 test passes unchanged, the core has direct unit
tests, and nothing outside the module changes behaviour.

## PRL.2 — `Prebuilt{url}` fixes

Independent bugs in the escape hatch that stays:

- `resolve_prebuilt` ignores `RefreshPolicy` and downloads on every resolve,
  boot included. Under `UseCache` with the artifact present: no fetch. Fix the
  `resolve.rs` doc comment that already claims this.
- `HttpFetcher` has no timeout or ceiling: route it through PRL.1's core.

**Tests:** warm boot makes zero fetcher calls (a counting `FakeFetcher`);
`update` fetches; a stalled server times out (a local `TcpListener` that
accepts and never answers) — the first test to run the real HTTP path.
**CHANGELOG:** a Fixed entry.

## PRL.3 — the package installer

`PluginSource::Release { repo, version, sha256 }` in the loader.
`resolve_release`:

1. Build the asset URL from the convention (design §3) and the host's API
   generation.
2. Download the `.sha256`, then the component, through PRL.1; verify — and
   when the spec pins a `sha256`, against that too.
3. Write it into a sibling directory; read its embedded manifest (PMF.1) —
   refuse a component without one — and check the manifest `id` equals the
   spec's `name`.
4. Swap into `<user_root>/<id>/` — rename the old directory aside, the new one
   in, then remove the old — so a failure at any step leaves the previous
   version loading.
5. Write `.source` with kind `release` (design §6).

Warm boot with a matching `.source` resolves to the artifact with no network.

**Tests** (fixture components with an embedded manifest, built in-test): the
happy path; a hash mismatch; a spec-pinned hash mismatch; a component with no
manifest; an id mismatch; a truncated download; the previous version still
loads after each failure; warm boot makes no fetch; one end-to-end install
over a local HTTP server. **Bench:** none — the path is off-thread and, warm,
never runs; the warm-boot no-fetch test is the guard that matters.

## PRL.4 — compatibility and resolution

- The asset name carries `api<M>.<m>` from the host's `lattice-wit`.
- A 404 on a pinned version fetches the release's asset list once and refuses
  with what it does ship (design §4), surfaced as a `FailedLoad` in `:plugins`
  and a `*messages*` line.
- Unpinned: one GitHub releases API call picks the newest non-draft,
  non-prerelease release with a matching asset; the resolved tag goes in
  `.source`, and only `update` asks again.
- API failures (rate limit, offline) keep what is installed, logged at
  `warn` — never a failed boot.

**Tests:** a fake releases listing — the newest compatible release chosen over
a newer incompatible one; drafts and prereleases skipped; none compatible →
the named refusal; an offline `update` keeps the installed version.

## PRL.5 — `require-release` on the WIT

Add `record release-spec` and `require-release: func(spec) -> bool` to
`plugin-manager` (design §3). Additive, so no package bump (`releasing.md`) —
but `ABI_FINGERPRINT` moves, so source-built plugins rebuild once; the
CHANGELOG says so. The host mirrors `require`: validate `name`, record, drain
after the export returns. The `:plugins` source column reads
`release owner/name@v0.2.0`.

**Tests:** the `require_seam.rs` pattern — a guest calls `require-release`, the
spec crosses intact, a bad `name` is rejected and the rest of `init.rs` still
runs. The generated plugin-API reference picks the function up and its guard
test passes.

## PRL.6 — user grants

When `grants` is given it replaces the manifest's `capabilities`: each grant
must parse, and its kind must be one the manifest requests (design §5).
`proc:spawn` stays denied to user-installed plugins. A refused grant fails that
plugin's install with the grant named — not a silent drop, which would leave a
plugin running with less than the user believes it has.

**Tests:** retarget `fs:write`; narrow to fewer kinds; add a kind → refused and
named; `proc:spawn` → refused; `grants: None` → the manifest's capabilities, as
today; an `update` that replaces the package keeps the user's grants.

## PRL.7 — publishing

- `scripts/build-plugin.sh <dir>`, the one recipe: `cargo build --release
  --target wasm32-wasip2`, the single-`.wasm` and non-empty checks, the API
  generation from the locked `lattice-wit`, the embedded-manifest check, and
  the renamed component and its `.sha256`.
- `release.yml`'s `core-plugins` job calls it for its build step. What the
  editor's archives contain does not change; their existing verify step
  proves it.
- `.github/workflows/plugin-release.yml` (`on: workflow_call`): checkout, tag
  equals `Cargo.toml` version, toolchain + `wasm32-wasip2` + clang, the
  script, then a GitHub release with the tag message as its body and both
  files attached.

**Tests:** CI runs the script against `plugins/auto-pair` and PRL.3's installer
installs the result from a local URL — producer and consumer agreeing is the
test that matters.

## PRL.8 — org adopts it

In `lattice-org-plugin`: a five-line `.github/workflows/release.yml` calling the
reusable workflow (`scripts/release.sh` already tags). Cut `v0.1.0`. Then, on a
machine with no Rust toolchain, `require-release` org and confirm it loads, its
seams drain, and `:plugins` shows the release source. Pushing the tag is the
maintainer's act.

## PRL.9 — docs

- `docs/user/plugins.md` and `docs/user/init.md`: installing a released plugin
  first, with grants; building from source as the alternative.
- `docs/dev/guides/plugin-authoring.md`: a "Publishing" section; replace the
  "does not download a prebuilt `.wasm`" paragraph.
- `docs/user/org.md` and org's README: lead with `require-release`.
- `plugin-manager.md`: point §4 / §7 at the design fragment; fix its stale
  "📝 designed, not built" status line.
