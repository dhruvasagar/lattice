+++
title = "Cutting a release"
+++


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

## Known gaps

- aarch64-linux / aarch64-windows **GUI** builds are best-effort
  (`continue-on-error`); if GPUI fails to link there the release still ships
  every other artefact and the publish log warns. When a preview run proves
  they link, drop `continue-on-error` on those legs in `release.yml`.
- No macOS `.dmg` / Windows `.msi` (need signing certs; unsigned is worse UX).
- Windows `.exe` has no embedded icon (needs a `winresource` build script).
