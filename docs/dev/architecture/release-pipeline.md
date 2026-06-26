# Release pipeline

> Design fragment. The "what" and "why" of how Lattice produces downloadable
> release artefacts. Slice sequencing lives in the slice plan
> (`docs/dev/operations/slice-plans/release-pipeline.md`) once carved.

## Goal

A tag-driven GitHub Actions pipeline that, on every `v*` tag, builds and
publishes downloadable release artefacts for **Linux, macOS, and Windows**
(x86_64 + aarch64) plus a **source** archive, with checksums and supply-chain
provenance. The pipeline is testable without cutting a release (preview mode).

This is build/release infrastructure, not editor runtime. The four paramount
goals do not directly govern it; the relevant standing rules are **no silent
failures** (a dropped artefact must be logged, never silently absent) and
**UX follows convention** (artefact shapes match what users expect from
Helix / Neovim / Zed).

## Reference survey (substrate-weighted)

Per the editor-references rule, Helix is weighted heaviest here — same
substrate (Rust), same class of tool, no signing infrastructure assumed. Live
release assets at time of writing:

| Platform | Helix (Rust)            | Zed (Rust, GUI)        | Neovim (C)              |
| -------- | ----------------------- | ---------------------- | ----------------------- |
| Linux    | tar.xz + AppImage + deb | tar.gz                 | tar.gz + AppImage       |
| macOS    | tar.xz                  | .dmg (signed+notarised)| tar.gz                  |
| Windows  | .zip                    | .exe installer (signed)| .zip + .msi             |
| Source   | source.tar.xz           | —                      | —                       |
| Arch     | x86_64 + aarch64        | x86_64 + aarch64       | x86_64 + arm64          |

Conclusions that shaped this design:

- **Archives (tar/zip) are the universal baseline** — every editor ships them
  on every platform. Non-negotiable core.
- **Linux native bundles are cheap and unsigned-friendly** — AppImage (Helix +
  Neovim) and .deb (Helix) need no certificates and are a real UX win.
- **macOS .dmg and Windows .msi are gated on code-signing.** Only Zed ships
  them, because Zed holds an Apple Developer cert (+ notarisation) and a
  Windows Authenticode cert. An *unsigned* .dmg/.msi is a **worse** UX than an
  archive — Gatekeeper reports "damaged, can't be opened"; SmartScreen blocks
  the installer. Helix and Neovim deliberately ship plain archives on macOS;
  Helix ships only a zip on Windows. Lattice follows suit: **no macOS/Windows
  installers until signing certs exist.**
- Helix uses **native runners per arch** (no cross-compilation) and free
  build-provenance attestation. Both adopted here.

## Decisions

| Decision        | Choice                                                          |
| --------------- | -------------------------------------------------------------- |
| Trigger         | Push of `v*` tag → real release. `workflow_dispatch` + PR touching the workflow → **preview** mode (artefacts only, no release). |
| Architectures   | x86_64 + aarch64 on all three OSes (6 build legs).             |
| Binary flavors  | **TUI** (`lattice`, default features) *and* **GUI** (`lattice` built `--features gui`) shipped as **separate** archives. |
| Packaging       | Helix model: archives everywhere + Linux AppImage + .deb + `SHA256SUMS` + source archive + provenance attestation. No .dmg / .msi / .rpm in v1. |
| Linux bundles   | Wrap the **GUI** binary only (desktop-install formats). TUI ships as a plain archive (headless/SSH/server audience). |
| Build profile   | `--release` — already dist-grade (`panic=abort`, thin LTO, `codegen-units=1`). No separate `dist` profile. |

## Build matrix

Native runner per arch — free for this public repo, avoids cross-compilation:

| build          | runner            | target                       |
| -------------- | ----------------- | ---------------------------- |
| x86_64-linux   | ubuntu-22.04      | x86_64-unknown-linux-gnu     |
| aarch64-linux  | ubuntu-22.04-arm  | aarch64-unknown-linux-gnu    |
| x86_64-macos   | macos-13          | x86_64-apple-darwin          |
| aarch64-macos  | macos-latest      | aarch64-apple-darwin         |
| x86_64-windows | windows-latest    | x86_64-pc-windows-msvc       |
| aarch64-windows| windows-11-arm    | aarch64-pc-windows-msvc      |

`ubuntu-22.04` is pinned (not `-latest`) for an older glibc → binaries that run
on a wider range of distros, matching Helix's portability note.

The `rust-toolchain.toml` is removed at the start of each build leg (it pins
`1.94.0` with `profile=minimal` and would otherwise override the action's
toolchain + target installation), mirroring Helix.

### TUI vs GUI coverage

- **TUI** binary builds on **all 6** legs (pure Rust, no display-lib deps,
  low risk).
- **GUI** binary (`--features gui`) needs GPUI to link. Current CI
  (`gpui-window-build`) only proves linkage on **x86_64-linux**,
  **aarch64-macos** (macos-latest is Apple Silicon), and **x86_64-windows**.
  **aarch64-linux** and **aarch64-windows** GUI linkage is *unproven*.

  **Decision (a): build GUI on all 6 legs, but mark `aarch64-linux` and
  `aarch64-windows` GUI steps `continue-on-error`.** A GPUI link failure on
  those two legs therefore does not sink the release: their GUI artefacts are
  simply absent and the leg logs a warning (**no silent drop** — the publish
  job's manifest names every expected artefact and flags any missing). All TUI
  artefacts (6 legs) and the four proven GUI artefacts always publish. When a
  preview run proves the two ARM GUI legs link, drop the `continue-on-error`.

  Linux GUI legs install the same display libs the CI `gpui-window-build` job
  uses (`libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev
  libfontconfig1-dev`).

## Artefacts

`<ver>` = tag with the leading `v` stripped (e.g. tag `v0.2.0` → `0.2.0`).

| Artefact                                     | Flavor | Legs                  |
| -------------------------------------------- | ------ | --------------------- |
| `lattice-<ver>-<build>.tar.xz` / `.zip`      | TUI    | all 6                 |
| `lattice-gui-<ver>-<build>.tar.xz` / `.zip`  | GUI    | all 6 (2 best-effort) |
| `lattice-gui-<ver>-<arch>.AppImage` + `.zsync` | GUI  | linux (x86_64, aarch64)|
| `lattice-gui_<ver>_<arch>.deb`               | GUI    | linux (x86_64, aarch64)|
| `lattice-<ver>-source.tar.xz`                | —      | publish job           |
| `SHA256SUMS`                                  | —      | publish job           |

- Each archive contains the binary + `LICENSE` + `README.md`.
- The source archive is a clean `git archive` export (deterministic, excludes
  `target/` and untracked files). GitHub additionally auto-attaches its own
  "Source code (zip/tar.gz)" to every release; the named `source.tar.xz` is the
  convention-matching, reproducible one.
- Unix archives use `.tar.xz`; Windows uses `.zip` (7-Zip).

## Icons (native bundles)

The only signed-or-unsigned **native apps** shipped in v1 are the Linux
AppImage + .deb. Their icon must match the existing desktop entry
`assets/linux/com.lattice-editor.lattice.desktop`, which declares
`Icon=com.lattice-editor.lattice` and `StartupWMClass=lattice`.

- **Icon source:** `assets/lattice.iconset/icon_NxN.png` — clean **square**
  PNGs at 16/32/64/128/256/512/1024. These are the same sources that build the
  macOS `.icns`.
- **Trap:** `assets/lattice-mark-512.png` is **512×614 (non-square)** and must
  **not** be used for freedesktop icons — it would be stretched/rejected. The
  workflow and `[package.metadata.deb]` reference the `iconset` PNGs only.
- **`.deb`:** a new `[package.metadata.deb]` section in
  `crates/lattice-cli/Cargo.toml` installs:
  - the binary → `/usr/bin/lattice`
  - `com.lattice-editor.lattice.desktop` → `/usr/share/applications/`
  - each square PNG → `/usr/share/icons/hicolor/<size>x<size>/apps/com.lattice-editor.lattice.png`
    (renamed from `icon_NxN.png` to the `Icon=` key).
- **AppImage:** built with `linuxdeploy`, passing
  `-d assets/linux/com.lattice-editor.lattice.desktop` and
  `-i <icon_512x512.png renamed to com.lattice-editor.lattice.png>`. The icon
  basename must equal the desktop `Icon=` key or linuxdeploy rejects it. The
  `.zsync` companion enables AppImage delta-updates
  (`gh-releases-zsync|dhruvasagar|lattice|latest|lattice-gui-*-<arch>.AppImage.zsync`).
- **macOS `.icns`:** ready (`assets/lattice.icns`) but unused in v1 (no shipped
  `.dmg`). Wired in when signing is added.
- **Windows:** the zipped `.exe` carries **no embedded icon** (a bare release
  binary has the default Windows icon). Embedding one requires a `winresource`
  build script — out of scope for v1; documented as a known gap.

## Jobs

1. **`dist`** (matrix, 6 legs) — checkout; remove `rust-toolchain.toml`;
   install toolchain + target; (Linux) install GPUI display libs; build TUI
   binary; build GUI binary (`continue-on-error` on the two unproven ARM legs);
   assemble TUI + GUI archives; (Linux) build AppImage + .deb for the GUI
   binary; `upload-artifact` per leg.
2. **`publish`** (`ubuntu-latest`, `needs: dist`) — download all leg artefacts;
   create the clean source archive via `git archive`; generate `SHA256SUMS`
   over every artefact; verify the expected-artefact manifest and **log any
   missing** (the best-effort ARM GUI legs); run
   `actions/attest-build-provenance` (free, supply-chain signing — *not* code
   signing); in tag mode, `softprops/action-gh-release@v2` creates the release
   with auto-generated notes and uploads everything; in **preview** mode, skip
   release creation and `upload-artifact` the assembled `dist/` instead.

Permissions: `dist` needs only `contents: read`; `publish` needs
`contents: write` (release upload) + `id-token: write` + `attestations: write`
(provenance).

Concurrency group keyed on the workflow + ref, `cancel-in-progress: false`
(don't cancel a half-published release).

## Version safety

A step in `publish` asserts the tag matches the workspace version:
`v<X> ` from `GITHUB_REF_NAME` must equal `workspace.package.version`
(`grep`-extracted from the root `Cargo.toml`). Mismatch fails the job loudly —
prevents tagging `v0.3.0` while `Cargo.toml` still says `0.2.0`. Skipped in
preview mode (no tag).

## Repo additions

| File                                              | Purpose                                  |
| ------------------------------------------------- | ---------------------------------------- |
| `.github/workflows/release.yml`                   | the pipeline                             |
| `crates/lattice-cli/Cargo.toml` `[package.metadata.deb]` | .deb file mapping + icon install  |
| `docs/dev/operations/releasing.md`                | how to cut a release (tag, bump, preview)|

## Out of scope (v1)

- macOS `.dmg` / `.app` and Windows `.msi` (need signing certs; unsigned is
  worse UX — see survey).
- `.rpm` (Helix doesn't ship it; add via `cargo-generate-rpm` later if asked).
- Windows `.exe` embedded icon (`winresource` build script).
- Homebrew tap / AUR / winget / Flatpak / crates.io publish (distribution
  channels — separate follow-on work).
- Bench-regression gating and code signing/notarisation.

## Failure modes & handling

- **ARM GUI link failure** — non-blocking (`continue-on-error`); artefact
  absent, logged in the publish manifest. Never silent.
- **Tag/version mismatch** — hard fail in `publish` before any upload.
- **Partial leg failure (TUI)** — `fail-fast: false` so one leg's failure does
  not cancel the others; the release publishes what succeeded and the failed
  leg is visible in the Actions UI and the missing-artefact log.
