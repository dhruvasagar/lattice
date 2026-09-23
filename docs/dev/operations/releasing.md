# Cutting a release

The release pipeline (`.github/workflows/release.yml`) is driven by `v*` tags.
Design: `docs/dev/architecture/release-pipeline.md`.

## Steps

1. **Write the changelog entry.** Add a `## X.Y.Z — YYYY-MM-DD` section at the
   top of `CHANGELOG.md`, in theme-grouped prose (see the 0.9.0 entry). This
   is not optional bookkeeping: it becomes the annotated tag's message *and*
   the GitHub Release body. If you skip it, `scripts/release.sh` inserts a
   dated stub and stops.
2. **Cut it.**
   ```bash
   scripts/release.sh              # current version, and what each bump gives
   scripts/release.sh minor --dry-run
   scripts/release.sh minor        # 0.9.0 -> 0.10.0
   ```
   The script checks you are on `main`, clean (bar `CHANGELOG.md`) and in sync
   with `origin/main`; that the tag is free locally and on the remote; and that
   the changelog section exists and is not still the stub. Then it rewrites the
   one `[workspace.package] version` line, runs `cargo update --workspace` so
   `Cargo.lock`'s member entries follow, commits `chore(release): vX.Y.Z` from
   explicit paths, and creates the annotated tag.
3. **Push.** The script never pushes — it prints the line. Pushing the tag is
   what publishes:
   ```bash
   git push origin main && git push origin v0.10.0
   ```
   To undo before pushing: `git tag -d v0.10.0 && git reset --hard HEAD~1`.
4. The pipeline builds 6 platform legs, packages TUI + GUI archives, Linux
   AppImage/.deb, a source archive, `SHA256SUMS`, attests provenance, and
   creates the GitHub Release.

### Why a script rather than `cargo-release`

38 of the 41 crates are `version.workspace = true`, so the multi-crate version
interdependence that `cargo-release` and `release-plz` exist to solve barely
arises here. What *is* worth enforcing is
local policy — the changelog gate and the refusal to push — which a generic
tool makes you work around. Helix, Alacritty and Zed all cut releases the same
way, by hand or from a small in-repo script; the generic-tool adopters are
overwhelmingly library crates.

## Release notes

The GitHub Release body is the tag's `CHANGELOG.md` section, extracted by the
`Release notes from CHANGELOG.md` step in `release.yml`. It replaced
`generate_release_notes: true`, whose raw commit list is strictly worse reading
for users. Both ends refuse to ship an empty body: the script will not tag
without a written section, and the workflow step fails the release if the
section is missing at publish time.

## Testing without releasing (preview mode)

Run the workflow from the Actions tab (**Run workflow** / `workflow_dispatch`),
or open a PR that touches `.github/workflows/release.yml`. Preview mode builds
everything and uploads a `release-preview` artefact — no tag, no release. Use
this to validate a pipeline change before tagging.

## Artefacts

- `lattice-<ver>-<platform>.{tar.xz,zip}` — TUI binary (headless/SSH/server).
- `lattice-gui-<ver>-<platform>.{tar.xz,zip}` — GUI binary (TUI + `--gui`).
- `lattice-gui-<ver>-<arch>.{AppImage,deb}` — Linux desktop installs.
- `lattice-<ver>-source.tar.xz`, `SHA256SUMS`.

## The core plugins ship with the artefacts

Since 0.9.0 the archives are **prefix-relocatable** — `bin/lattice` plus
`share/lattice/plugins/<name>/` — and a `core-plugins` job builds the three
WASM components once for every leg to download. Before that the legs
packaged a bare binary, so every artefact shipped with no plugins and the
editor said nothing about it (discovery treats an absent plugin directory
as a benign skip). Each leg now asserts the **contents** of the archives it
built, because a green build proves nothing about this failure.

Design: `../architecture/launch-0.9.md` §3, which amends
`../architecture/release-pipeline.md`.

## The three published crates are released separately

`lattice-wit`, `lattice-plugin-sdk` and `lattice-plugin-sdk-derive` go to
crates.io. They are **not** part of the tag flow above and `scripts/release.sh`
does not touch their versions — an editor patch release must not push a new
version at every plugin author for a crate that did not change.

They exist for one consumer: a plugin built outside this tree. Before they were
published, the only way to name lattice's ABI was a path into a checkout, which
is why the org plugin built on exactly one machine.

Publish in dependency order — `lattice-plugin-sdk` will not resolve until the
derive crate is on the index:

```bash
cargo publish -p lattice-plugin-sdk-derive
cargo publish -p lattice-plugin-sdk        # after the index updates
cargo publish -p lattice-wit               # independent of the other two
```

Dry-run each first (`--dry-run`); it catches a missing `description`, a path
dep without a `version`, and — the one that actually bit — a build script
reading files that are not inside the package.

Two things to know:

- **Bumping a version is a decision, not bookkeeping.** These are `0.x`, so
  Cargo treats `0.1 -> 0.2` as breaking. That is the right signal when the WIT
  changes shape and the wrong one when it does not.
- **`wit/` lives in `crates/lattice-wit/wit/`**, not at the workspace root, so
  the crate that publishes the ABI contains it. Everything else in the tree
  reads it from there — there is exactly one copy, and no guard keeping two in
  step.

## Changing the plugin seam

Editing anything under `crates/lattice-wit/wit/` changes the API every plugin
compiles against. Two separate consequences, and conflating them is the mistake
to avoid:

| Change | Effect | Needs a package bump? |
|---|---|---|
| Any edit at all, including a doc comment | `ABI_FINGERPRINT` moves → every plugin's `.build-stamp` goes stale → they all **rebuild** | No |
| Adding an interface, or a function to one | Existing plugins still compile and still link — they import only what they use | No |
| Changing a signature, renaming a record field, removing a function | Existing plugins fail to **compile** on rebuild | Usually yes |
| Bumping `package lattice:plugin-host@X.Y.Z` | Every existing component fails to **instantiate**, because the version is in the imported interface names | — |

A rebuild is cheap and automatic. A package bump is neither: it strands every
plugin pinned to the old generation until its author bumps and releases. Spend
it when the shape genuinely changed, not to mark that something was added.

### Doing the bump

Never by hand — the version is stated in 37 places (36 `.wit` files plus three
crate manifests), and one left behind produces a package that fails to parse or
links half its interfaces:

```bash
cargo xtask bump-plugin-api 0.2.0
cargo test -p lattice-wit      # the guard proves it landed everywhere
```

That rewrites every `package` declaration, all three published crate versions,
the SDK's `version` on its dependency on the derive crate, and refreshes
`Cargo.lock`. Then publish the three crates (above), because a plugin cannot
target a generation that is not on the index.

The rule the guard enforces: **the crates' `major.minor` equals the WIT
package's `major.minor`**, so `lattice-wit = "0.2"` means
`lattice:plugin-host@0.2.x` and an author tracks one number. Patch is the
crates' own — use it for a packaging fix that leaves the ABI alone.

### After a bump

- In-tree plugins and fixtures rebuild on the next `cargo xtask
  build-core-plugins` / test run; nothing to do.
- Out-of-tree plugins that **do not** declare a `lattice-wit` build-dependency
  follow automatically — the loader refreshes their `wit/` from the running
  editor.
- Out-of-tree plugins that **do** declare one are pinned to the old generation
  and will warn (`warn_if_abi_skewed`) and then fail to instantiate. That is
  working as designed; the author bumps their pin. `lattice-org-plugin` is one
  of these, so bump it in the same sitting.

## Known gaps

- aarch64-linux / aarch64-windows **GUI** builds are best-effort
  (`continue-on-error`); if GPUI fails to link there the release still ships
  every other artefact and the publish log warns. When a preview run proves
  they link, drop `continue-on-error` on those legs in `release.yml`.
- No macOS `.dmg` / Windows `.msi` (need signing certs; unsigned is worse UX).
- Windows `.exe` has no embedded icon (needs a `winresource` build script).
- **Intel macOS is the slow leg** — roughly 65 minutes, two full release
  builds (`codegen-units = 1` + thin LTO, the deliberate trade for
  keystroke latency). A post-0.9 improvement worth taking: cross-compile
  `x86_64-apple-darwin` on the arm64 `macos-latest` runner and retire
  `macos-15-intel`. Apple's SDK cross-compiles both arches natively, arm64
  runners are faster and more plentiful, and GitHub is retiring the Intel
  class anyway. Unproven risk: GPUI/Metal linking and the tree-sitter `cc`
  builds under cross-compilation. Deliberately not done during the launch —
  it would have introduced unproven cross-compilation into a pipeline that
  had never once completed.
