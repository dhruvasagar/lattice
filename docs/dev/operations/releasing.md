# Cutting a release

The release pipeline (`.github/workflows/release.yml`) is driven by `v*` tags.
Design: `docs/dev/architecture/release-pipeline.md`.

## Steps

1. Bump `[workspace.package] version` in the root `Cargo.toml` and commit.
   The tag must equal this value or `prepare` fails (e.g. `version = "0.2.0"`
   ⇒ tag `v0.2.0`).
2. Tag and push:
   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```
3. The pipeline builds 6 platform legs, packages TUI + GUI archives, Linux
   AppImage/.deb, a source archive, `SHA256SUMS`, attests provenance, and
   creates the GitHub Release with auto-generated notes.

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
