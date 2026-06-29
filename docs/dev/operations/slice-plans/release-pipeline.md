# Release Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Implements the design fragment `docs/dev/architecture/release-pipeline.md`
> (read it first for the "what" and "why"). This file owns sequencing only.

**Goal:** A tag-driven GitHub Actions pipeline that builds and publishes cross-platform release artefacts (Linux/macOS/Windows × x86_64/aarch64), Linux AppImage/.deb bundles, a source archive, checksums, and provenance attestation.

**Architecture:** Three jobs — `prepare` (compute version/preview, assert tag↔Cargo.toml on tags), `dist` (6-leg native-runner matrix building TUI + GUI binaries and packaging them), `publish` (collect artefacts, source archive, `SHA256SUMS`, provenance, GitHub Release). Helix's pipeline is the reference (same Rust substrate).

**Tech Stack:** GitHub Actions, `actions-rust-lang/setup-rust-toolchain`, `Swatinem/rust-cache`, `cargo-deb`, `linuxdeploy`, `actions/attest-build-provenance@v2`, `softprops/action-gh-release@v2`. Local lint gate: `actionlint`.

## Global Constraints

- Binary name is `lattice` for **both** flavors. TUI = `cargo build -p lattice-cli --release --target <t>`; GUI = same `+ --features gui`. Both write `target/<t>/release/lattice` — **the TUI archive MUST be created before the GUI build runs**, or the GUI binary overwrites it.
- All `cargo` invocations use `--locked`.
- All `setup-rust-toolchain` steps set `rustflags: ""` (the action otherwise injects `-D warnings`, overriding the workspace's intentionally-relaxed lint gate — see `.github/workflows/ci.yml` top comment).
- Each build leg removes `rust-toolchain.toml` before installing the toolchain (it pins `1.94.0`/`profile=minimal` and would override the action's target install).
- `<ver>` = tag name with leading `v` stripped (tag `v0.2.0` → `0.2.0`). Preview runs use `dev-<short-sha>`.
- Workspace version lives at `Cargo.toml:33` (`version = "0.1.0"` under `[workspace.package]`).
- Desktop entry `assets/linux/com.lattice-editor.lattice.desktop` declares `Icon=com.lattice-editor.lattice` — every installed icon file MUST be named `com.lattice-editor.lattice.png`.
- Icon sources are the **square** PNGs in `assets/lattice.iconset/` (`icon_NxN.png`). NEVER use `assets/lattice-mark-512.png` (it is 512×614, non-square).
- aarch64-linux and aarch64-windows GUI builds are unproven; their GUI steps are `continue-on-error` and must never block the release.
- `fail-fast: false` on the `dist` matrix.
- No `.dmg`, `.msi`, `.rpm` in v1.

---

### Task 1: `prepare` + `dist` jobs (build + archives, no bundles yet)

Builds both binaries on all 6 legs and produces the TUI + GUI archives. No Linux bundles yet (Task 2). End state: a valid, lintable workflow that, in preview mode, produces archive artefacts.

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Produces: workflow with jobs `prepare` (outputs `version`, `preview`) and `dist` (uploads artifact `artifacts-<build>` containing a `dist/` dir of archives). Task 2 adds steps to `dist`; Task 3 adds the `publish` job consuming `artifacts-*`.

- [ ] **Step 1: Install the local lint gate**

```bash
# macOS dev box
brew install actionlint
actionlint --version   # expect e.g. 1.7.x
```

- [ ] **Step 2: Create the workflow with `prepare` + `dist`**

Create `.github/workflows/release.yml`:

```yaml
# Release pipeline. See docs/dev/architecture/release-pipeline.md for design;
# docs/dev/operations/slice-plans/release-pipeline.md for sequencing.
name: release

on:
  push:
    tags: ['v*']
  workflow_dispatch: {}
  pull_request:
    paths: ['.github/workflows/release.yml']

concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: false

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  prepare:
    name: prepare
    runs-on: ubuntu-latest
    outputs:
      version: ${{ steps.v.outputs.version }}
      preview: ${{ steps.v.outputs.preview }}
    steps:
      - uses: actions/checkout@v4
      - id: v
        shell: bash
        run: |
          set -euo pipefail
          if [[ "${GITHUB_REF}" == refs/tags/v* ]]; then
            preview=false
            version="${GITHUB_REF_NAME#v}"
          else
            preview=true
            version="dev-${GITHUB_SHA:0:7}"
          fi
          echo "preview=$preview" >> "$GITHUB_OUTPUT"
          echo "version=$version" >> "$GITHUB_OUTPUT"
          echo "Resolved version=$version preview=$preview"
          if [[ "$preview" == "false" ]]; then
            cargo_ver=$(grep -m1 -E '^version[[:space:]]*=' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
            if [[ "$version" != "$cargo_ver" ]]; then
              echo "::error::tag v$version does not match Cargo.toml workspace version $cargo_ver"
              exit 1
            fi
            echo "Tag matches Cargo.toml version $cargo_ver"
          fi

  dist:
    name: dist (${{ matrix.build }})
    needs: prepare
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - { build: x86_64-linux,   os: ubuntu-22.04,     target: x86_64-unknown-linux-gnu,   gui_best_effort: false }
          - { build: aarch64-linux,  os: ubuntu-22.04-arm, target: aarch64-unknown-linux-gnu,  gui_best_effort: true  }
          - { build: x86_64-macos,   os: macos-13,         target: x86_64-apple-darwin,        gui_best_effort: false }
          - { build: aarch64-macos,  os: macos-latest,     target: aarch64-apple-darwin,       gui_best_effort: false }
          - { build: x86_64-windows, os: windows-latest,   target: x86_64-pc-windows-msvc,     gui_best_effort: false }
          - { build: aarch64-windows,os: windows-11-arm,   target: aarch64-pc-windows-msvc,    gui_best_effort: true  }
    steps:
      - uses: actions/checkout@v4

      - name: Remove rust-toolchain.toml
        shell: bash
        run: rm -f rust-toolchain.toml

      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          toolchain: stable
          target: ${{ matrix.target }}
          rustflags: ""

      - uses: Swatinem/rust-cache@v2
        with:
          key: release-${{ matrix.build }}

      - name: Install Linux display libs (for GUI link)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
            libwayland-dev libfontconfig1-dev

      - name: Build TUI binary
        run: cargo build -p lattice-cli --release --locked --target ${{ matrix.target }}

      - name: Package TUI archive
        shell: bash
        env:
          VERSION: ${{ needs.prepare.outputs.version }}
        run: |
          set -euo pipefail
          mkdir -p dist
          pkg="lattice-${VERSION}-${{ matrix.build }}"
          mkdir -p "$pkg"
          cp LICENSE README.md "$pkg/"
          if [[ "${{ runner.os }}" == "Windows" ]]; then
            cp "target/${{ matrix.target }}/release/lattice.exe" "$pkg/"
            7z a -r "dist/$pkg.zip" "$pkg" >/dev/null
          else
            cp "target/${{ matrix.target }}/release/lattice" "$pkg/"
            chmod +x "$pkg/lattice"
            tar cJf "dist/$pkg.tar.xz" "$pkg"
          fi
          rm -rf "$pkg"

      - name: Build GUI binary
        id: gui_build
        continue-on-error: ${{ matrix.gui_best_effort }}
        run: cargo build -p lattice-cli --release --locked --features gui --target ${{ matrix.target }}

      - name: Package GUI archive
        if: steps.gui_build.outcome == 'success'
        shell: bash
        env:
          VERSION: ${{ needs.prepare.outputs.version }}
        run: |
          set -euo pipefail
          mkdir -p dist
          pkg="lattice-gui-${VERSION}-${{ matrix.build }}"
          mkdir -p "$pkg"
          cp LICENSE README.md "$pkg/"
          if [[ "${{ runner.os }}" == "Windows" ]]; then
            cp "target/${{ matrix.target }}/release/lattice.exe" "$pkg/"
            7z a -r "dist/$pkg.zip" "$pkg" >/dev/null
          else
            cp "target/${{ matrix.target }}/release/lattice" "$pkg/"
            chmod +x "$pkg/lattice"
            tar cJf "dist/$pkg.tar.xz" "$pkg"
          fi
          rm -rf "$pkg"

      - uses: actions/upload-artifact@v4
        with:
          name: artifacts-${{ matrix.build }}
          path: dist
          if-no-files-found: error
```

- [ ] **Step 3: Lint the workflow**

Run: `actionlint .github/workflows/release.yml`
Expected: no output (exit 0). If it reports an expression or shell error, fix it before committing.

- [ ] **Step 4: Sanity-check job/matrix shape**

Run: `python3 -c "import yaml,sys; d=yaml.safe_load(open('.github/workflows/release.yml')); legs=d['jobs']['dist']['strategy']['matrix']['include']; print('legs',len(legs)); assert len(legs)==6; print('best_effort',[l['build'] for l in legs if l['gui_best_effort']])"`
Expected: `legs 6` and `best_effort ['aarch64-linux', 'aarch64-windows']`

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: release pipeline prepare + dist jobs (build + archives)"
```

---

### Task 2: Linux GUI bundles (.deb + AppImage) with correct icons

Adds the `[package.metadata.deb]` file mapping and the two Linux-only bundle steps to the `dist` job. End state: Linux GUI legs additionally emit `.deb` and `.AppImage` (+ `.zsync`) with the `com.lattice-editor.lattice` icon installed from the square iconset.

**Files:**
- Modify: `crates/lattice-cli/Cargo.toml` (add `[package.metadata.deb]` after the existing `[package.metadata.bundle]` block, ~line 47+)
- Modify: `.github/workflows/release.yml` (insert two steps into `dist`, after "Package GUI archive")

**Interfaces:**
- Consumes: GUI binary at `target/<target>/release/lattice`; `assets/linux/com.lattice-editor.lattice.desktop`; `assets/lattice.iconset/icon_*.png`.
- Produces: `dist/lattice-gui-<ver>-<arch>.deb`, `dist/lattice-gui-<ver>-<arch>.AppImage`, `dist/lattice-gui-<ver>-<arch>.AppImage.zsync` (arch ∈ {x86_64, aarch64}).

- [ ] **Step 1: Add `[package.metadata.deb]` to lattice-cli**

Append to `crates/lattice-cli/Cargo.toml` (after the `[package.metadata.bundle]` block). Asset source paths are relative to this crate dir, hence `../../`:

```toml
# cargo-deb config — used by the release pipeline (`cargo deb --no-build`).
# Installs the binary, the freedesktop desktop entry, and square hicolor
# icons renamed to the desktop's Icon= key. Icons come from the square
# iconset, NOT the non-square assets/lattice-mark-512.png.
[package.metadata.deb]
maintainer = "Dhruva Sagar <dhruva.sagar@gmail.com>"
copyright = "Copyright © 2024 Dhruva Sagar"
license-file = ["../../LICENSE", "0"]
extended-description = "Combines vim's modal editing power with emacs's extensibility model on a non-blocking, multi-threaded core."
section = "editors"
priority = "optional"
assets = [
    ["target/release/lattice", "usr/bin/", "755"],
    ["../../assets/linux/com.lattice-editor.lattice.desktop", "usr/share/applications/com.lattice-editor.lattice.desktop", "644"],
    ["../../assets/lattice.iconset/icon_16x16.png",   "usr/share/icons/hicolor/16x16/apps/com.lattice-editor.lattice.png",   "644"],
    ["../../assets/lattice.iconset/icon_32x32.png",   "usr/share/icons/hicolor/32x32/apps/com.lattice-editor.lattice.png",   "644"],
    ["../../assets/lattice.iconset/icon_32x32@2x.png","usr/share/icons/hicolor/64x64/apps/com.lattice-editor.lattice.png",   "644"],
    ["../../assets/lattice.iconset/icon_128x128.png", "usr/share/icons/hicolor/128x128/apps/com.lattice-editor.lattice.png", "644"],
    ["../../assets/lattice.iconset/icon_256x256.png", "usr/share/icons/hicolor/256x256/apps/com.lattice-editor.lattice.png", "644"],
    ["../../assets/lattice.iconset/icon_512x512.png", "usr/share/icons/hicolor/512x512/apps/com.lattice-editor.lattice.png", "644"],
]
```

- [ ] **Step 2: Verify the manifest parses and asset sources exist**

Run:
```bash
cd /Users/dhruva/src/dhruvasagar/lattice
cargo metadata --format-version 1 --no-deps >/dev/null && echo "metadata OK"
python3 - <<'EOF'
import tomllib, pathlib
crate = pathlib.Path("crates/lattice-cli")
deb = tomllib.loads((crate/"Cargo.toml").read_text())["package"]["metadata"]["deb"]
missing = [s for s,_,_ in deb["assets"] if not s.startswith("target/") and not (crate/s).exists()]
print("MISSING:", missing); assert not missing, missing
print("all deb asset sources exist")
EOF
```
Expected: `metadata OK`, `all deb asset sources exist`.

- [ ] **Step 3: Insert the bundle steps into `dist`**

In `.github/workflows/release.yml`, immediately AFTER the "Package GUI archive" step and BEFORE the `actions/upload-artifact` step, insert:

```yaml
      - name: Install cargo-deb
        if: runner.os == 'Linux' && steps.gui_build.outcome == 'success'
        uses: taiki-e/install-action@v2
        with:
          tool: cargo-deb

      - name: Build .deb (GUI)
        if: runner.os == 'Linux' && steps.gui_build.outcome == 'success'
        shell: bash
        env:
          VERSION: ${{ needs.prepare.outputs.version }}
        run: |
          set -euo pipefail
          # cargo-deb's [assets] reference target/release/lattice; stage the
          # GUI binary there so --no-build picks it up.
          mkdir -p target/release
          cp "target/${{ matrix.target }}/release/lattice" target/release/lattice
          arch="${{ matrix.build }}"; arch="${arch%-linux}"
          cargo deb -p lattice-cli --no-build --output "dist/lattice-gui-${VERSION}-${arch}.deb"

      - name: Build AppImage (GUI)
        if: runner.os == 'Linux' && steps.gui_build.outcome == 'success'
        shell: bash
        env:
          VERSION: ${{ needs.prepare.outputs.version }}
        run: |
          set -euo pipefail
          sudo apt-get install -y --no-install-recommends libfuse2
          arch="${{ matrix.build }}"; arch="${arch%-linux}"   # x86_64 | aarch64
          # linuxdeploy requires the icon basename to equal the desktop Icon= key.
          cp assets/lattice.iconset/icon_256x256.png com.lattice-editor.lattice.png
          mkdir -p AppDir/usr/bin
          cp "target/${{ matrix.target }}/release/lattice" AppDir/usr/bin/lattice
          curl -fsSLo linuxdeploy.AppImage \
            "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${arch}.AppImage"
          chmod +x linuxdeploy.AppImage
          export ARCH="$arch"
          export VERSION="$VERSION"
          export OUTPUT="lattice-gui-${VERSION}-${arch}.AppImage"
          export UPDATE_INFORMATION="gh-releases-zsync|dhruvasagar|lattice|latest|lattice-gui-*-${arch}.AppImage.zsync"
          ./linuxdeploy.AppImage \
            --appdir AppDir \
            -e AppDir/usr/bin/lattice \
            -d assets/linux/com.lattice-editor.lattice.desktop \
            -i com.lattice-editor.lattice.png \
            --output appimage
          mkdir -p dist
          mv "lattice-gui-${VERSION}-${arch}.AppImage" dist/
          mv "lattice-gui-${VERSION}-${arch}.AppImage.zsync" dist/
```

- [ ] **Step 4: Lint**

Run: `actionlint .github/workflows/release.yml`
Expected: no output (exit 0).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-cli/Cargo.toml .github/workflows/release.yml
git commit -m "ci: Linux GUI .deb + AppImage bundles with hicolor icons"
```

---

### Task 3: `publish` job (source archive, checksums, provenance, release)

Collects all leg artefacts, builds the source archive and `SHA256SUMS`, verifies the expected-artefact manifest (logging best-effort gaps, failing on required gaps), attests provenance, and either creates the GitHub Release (tag mode) or uploads the bundle as a CI artefact (preview mode).

**Files:**
- Modify: `.github/workflows/release.yml` (append the `publish` job)

**Interfaces:**
- Consumes: `prepare` outputs `version`/`preview`; all `artifacts-<build>` from `dist`.
- Produces: a GitHub Release with all artefacts + `SHA256SUMS` (tag mode), or a `release-preview` CI artefact (preview mode).

- [ ] **Step 1: Append the `publish` job**

Append to `.github/workflows/release.yml`:

```yaml
  publish:
    name: publish
    needs: [prepare, dist]
    runs-on: ubuntu-latest
    permissions:
      contents: write       # upload to the release
      id-token: write       # provenance signing
      attestations: write   # write provenance
    steps:
      - uses: actions/checkout@v4

      - uses: actions/download-artifact@v4
        with:
          pattern: artifacts-*
          path: staging

      - name: Assemble dist, source archive, checksums, manifest
        shell: bash
        env:
          VERSION: ${{ needs.prepare.outputs.version }}
        run: |
          set -euo pipefail
          mkdir -p dist
          find staging -mindepth 2 -type f -exec mv -t dist {} +

          # Deterministic source export (GitHub also auto-attaches its own).
          git archive --format=tar --prefix="lattice-${VERSION}/" HEAD \
            | xz -9 > "dist/lattice-${VERSION}-source.tar.xz"

          ( cd dist && sha256sum lattice-* > SHA256SUMS )

          echo "## Artefacts" >> "$GITHUB_STEP_SUMMARY"
          ls -1 dist >> "$GITHUB_STEP_SUMMARY"

          # Manifest: required must exist; best-effort (arm GUI) only warned.
          required=(
            "lattice-${VERSION}-x86_64-linux.tar.xz"
            "lattice-${VERSION}-aarch64-linux.tar.xz"
            "lattice-${VERSION}-x86_64-macos.tar.xz"
            "lattice-${VERSION}-aarch64-macos.tar.xz"
            "lattice-${VERSION}-x86_64-windows.zip"
            "lattice-${VERSION}-aarch64-windows.zip"
            "lattice-gui-${VERSION}-x86_64-linux.tar.xz"
            "lattice-gui-${VERSION}-x86_64-macos.tar.xz"
            "lattice-gui-${VERSION}-aarch64-macos.tar.xz"
            "lattice-gui-${VERSION}-x86_64-windows.zip"
            "lattice-gui-${VERSION}-x86_64.AppImage"
            "lattice-gui-${VERSION}-x86_64.deb"
            "lattice-gui-${VERSION}-aarch64.AppImage"
            "lattice-gui-${VERSION}-aarch64.deb"
            "lattice-${VERSION}-source.tar.xz"
          )
          best_effort=(
            "lattice-gui-${VERSION}-aarch64-linux.tar.xz"
            "lattice-gui-${VERSION}-aarch64-windows.zip"
          )
          fail=0
          for f in "${required[@]}"; do
            if [[ ! -f "dist/$f" ]]; then echo "::error::missing required artefact $f"; fail=1; fi
          done
          for f in "${best_effort[@]}"; do
            if [[ ! -f "dist/$f" ]]; then echo "::warning::best-effort artefact absent (ARM GUI unproven): $f"; fi
          done
          [[ $fail -eq 0 ]] || exit 1

      - name: Attest build provenance
        if: needs.prepare.outputs.preview == 'false'
        uses: actions/attest-build-provenance@v2
        with:
          subject-path: 'dist/*'

      - name: Create GitHub Release
        if: needs.prepare.outputs.preview == 'false'
        uses: softprops/action-gh-release@v2
        with:
          tag_name: ${{ github.ref_name }}
          files: dist/*
          generate_release_notes: true
          fail_on_unmatched_files: true

      - name: Upload preview bundle
        if: needs.prepare.outputs.preview == 'true'
        uses: actions/upload-artifact@v4
        with:
          name: release-preview
          path: dist/*
          if-no-files-found: error
```

- [ ] **Step 2: Lint**

Run: `actionlint .github/workflows/release.yml`
Expected: no output (exit 0).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: release publish job (source, checksums, provenance, release)"
```

---

### Task 4: Release runbook doc

**Files:**
- Create: `docs/dev/operations/releasing.md`

- [ ] **Step 1: Write the runbook**

Create `docs/dev/operations/releasing.md`:

```markdown
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

## Known gaps

- aarch64-linux / aarch64-windows **GUI** builds are best-effort
  (`continue-on-error`); if GPUI fails to link there the release still ships
  every other artefact and the publish log warns. When a preview run proves
  they link, drop `continue-on-error` on those legs in `release.yml`.
- No macOS `.dmg` / Windows `.msi` (need signing certs; unsigned is worse UX).
- Windows `.exe` has no embedded icon (needs a `winresource` build script).
```

- [ ] **Step 2: Commit**

```bash
git add docs/dev/operations/releasing.md
git commit -m "docs: release runbook"
```

---

### Task 5: Integration — preview run end-to-end

The real gate. A preview run exercises every job without creating a release.

**Files:** none (operational verification).

- [ ] **Step 1: Push the branch and trigger a preview run**

```bash
git push -u origin feat/release-pipeline
gh workflow run release.yml --ref feat/release-pipeline
```
(`workflow_dispatch` runs in preview mode — no tag, no release.)

- [ ] **Step 2: Watch the run**

```bash
gh run watch "$(gh run list --workflow=release.yml --branch=feat/release-pipeline --limit=1 --json databaseId --jq '.[0].databaseId')" --exit-status
```
Expected: `prepare`, all 6 `dist` legs, and `publish` succeed. The two ARM GUI legs may show a `continue-on-error` warning on the "Build GUI binary" step — that is acceptable; the leg itself must still be green.

- [ ] **Step 3: Download and inspect the preview artefact**

```bash
rid="$(gh run list --workflow=release.yml --branch=feat/release-pipeline --limit=1 --json databaseId --jq '.[0].databaseId')"
gh run download "$rid" -n release-preview -D /tmp/lattice-release-preview
ls -1 /tmp/lattice-release-preview
sha256sum -c /tmp/lattice-release-preview/SHA256SUMS  # run from inside that dir
```
Expected: TUI archives for all 6 platforms, GUI archives for the 4 proven platforms (+ ARM GUI if they linked), `.deb` + `.AppImage` + `.zsync` for both Linux arches, `lattice-dev-<sha>-source.tar.xz`, `SHA256SUMS`. Checksums verify.

- [ ] **Step 4: Verify the .deb icon wiring**

```bash
cd /tmp/lattice-release-preview
dpkg-deb -c lattice-gui-dev-*-x86_64.deb | grep -E 'com\.lattice-editor\.lattice\.(png|desktop)'
```
Expected: the `.desktop` under `usr/share/applications/` and `com.lattice-editor.lattice.png` under multiple `usr/share/icons/hicolor/<size>/apps/` paths. (Skip if not on a machine with `dpkg-deb`; otherwise inspect via `ar x` + `tar`.)

- [ ] **Step 5: Open the PR**

```bash
gh pr create --fill --base main --head feat/release-pipeline
```
The PR itself re-triggers a preview run (the `pull_request` path filter), giving a second clean signal.

---

## Self-Review

**Spec coverage** (against `docs/dev/architecture/release-pipeline.md`):
- Trigger (tag + workflow_dispatch + PR preview) → Task 1 `on:` + `prepare`. ✓
- 6-leg native matrix → Task 1 `dist.matrix`. ✓
- TUI + GUI separate archives, TUI-before-GUI ordering → Task 1 steps. ✓
- aarch64 GUI best-effort, no silent drop → Task 1 `continue-on-error` + Task 3 manifest warnings. ✓
- Linux AppImage + .deb, GUI only, correct icons → Task 2. ✓
- Source archive, SHA256SUMS, provenance, release, preview fallback → Task 3. ✓
- Version assertion → Task 1 `prepare`. ✓
- Repo additions (workflow, metadata.deb, releasing.md) → Tasks 1–4. ✓
- Out-of-scope items (.dmg/.msi/.rpm, win icon) → documented in Task 4. ✓

**Placeholder scan:** none — all YAML/TOML/shell is complete and literal.

**Type/name consistency:** artifact names `artifacts-<build>` (Task 1 upload) match `pattern: artifacts-*` (Task 3 download); `needs.prepare.outputs.{version,preview}` defined in Task 1 and consumed in Tasks 1–3; `steps.gui_build.outcome` set in Task 1, gated in Task 2; deb/AppImage names in Task 2 match the manifest list in Task 3.
