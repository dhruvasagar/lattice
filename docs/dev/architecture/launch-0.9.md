# Lattice 0.9 — the alpha launch contract

> Sequencing lives in
> [`../operations/slice-plans/launch-0.9.md`](../operations/slice-plans/launch-0.9.md).
> The pipeline this builds on is designed in
> [`release-pipeline.md`](./release-pipeline.md); §3 below **amends** it.

## 1. What 0.9 claims

0.9 is the first version a stranger can install. It claims exactly one
thing: **the editor is usable; the distribution is new.** That sentence
is the launch's whole promise, and it is chosen because it is the part
we can actually stand behind — 3395 commits of editor against zero
successful release runs.

What it does *not* claim: stability guarantees, a settled plugin API, a
signed installer, or feature parity with the editors it borrows from.
Those are 1.0's business.

The reach is deliberately small: a **soft launch**. Tag, honest notes, a
polished install path, announce in one or two low-key places. No front
page. The thing being tested at 0.9 is whether a stranger can get from
"I read about this" to "I edited a file" without help — not whether the
editor impresses.

## 2. Version policy

The workspace version becomes `0.9.0` and the tag is `v0.9.0`. "Alpha"
is communicated **in prose** — release notes, README, the site — and
never in the version string.

**Rejected: `0.9.0-alpha.1`.** The pipeline's tag↔Cargo assertion
(`release.yml:43-48`) would accept it, so this is a choice rather than a
constraint. It loses on three counts: `0.x` already carries the
instability signal that a pre-release suffix would repeat; the string
propagates into the site hero (`site/data/version.toml`, generated from
`Cargo.toml` by `site/scripts/sync-docs.sh:57-66`), every artefact
filename, and the installer's asset resolution; and it forces a second
decision later about whether `0.9.0` proper is a separate release. A
version string is a name, and this one should be pronounceable.

## 3. The artefact contract — core plugins must ship

**This section amends [`release-pipeline.md`](./release-pipeline.md).**
The pipeline as designed packages a bare binary, and that is a defect,
not a simplification.

### The defect

Core-plugin discovery (`crates/lattice-plugin-loader/src/discovery.rs:54-96`)
searches, in order:

1. `$LATTICE_RUNTIME/plugins` — explicit override;
2. `<LATTICE_INSTALL_PREFIX>/share/lattice/plugins` — baked at build time;
3. `<exe-dir>/../share/lattice/plugins` — a relocatable install;
4. `<exe-dir>/../../runtime/plugins` — dev, running from `target/<profile>/`.

`/runtime` is gitignored, the release legs never invoke
`cargo xtask build-core-plugins`, and the archives are a flat
`lattice-<ver>-<platform>/lattice`. So `default_core_plugins_dir()`
returns `None` — which the code documents as "a benign skip, like an
absent user plugins dir" — and **auto-pair, treesitter-context and
project are silently absent from every download.**

`xtask/src/main.rs:1-10` already describes itself as "the dev equivalent
of the release/packaging step that stages the same artifacts into the
shipped runtime root". That packaging step was never built.

### Why this is a launch blocker and not a polish item

**UX is the higher court.** A user types `(`, gets no closing paren, and
concludes the editor is broken. There is no error, no warning, and
nothing to search for — the skip is silent by design, because in the dev
path it *is* benign. Silent absence is the worst available failure shape,
and it lands on feature #1 of the pitch.

**Paramount goal #2 (extensibility).** Shipping a plugin-first editor
with zero plugins loaded refutes the headline claim in the first minute.

### The contract

A released archive is **prefix-relocatable** — the layout discovery path
3 was designed for:

```
lattice-0.9.0-x86_64-macos/
  bin/lattice
  share/lattice/plugins/auto-pair/{auto-pair.wasm,plugin.toml,.source}
  share/lattice/plugins/treesitter-context/{...}
  share/lattice/plugins/project/{...}
  LICENSE
  README.md
```

`.deb` installs the same tree under `/usr/{bin,share}`; the AppImage
stages it under `AppDir/usr/`. The installer (§5) lays down a prefix,
not a loose binary.

The three components are built **once**, in a dedicated job, and
downloaded by each `dist` leg. WASM components are platform-independent:
six per-leg builds would be pure waste and would drag a
`wasm32-wasip2` toolchain onto the ARM Windows runner for no gain.

### Rejected alternatives

- **Bake `LATTICE_INSTALL_PREFIX` at build time** (path 2). Wrong for a
  tarball: the prefix is fixed at compile time, so an archive extracted
  anywhere but the baked path finds nothing. It is the right mechanism
  for a distro packager and the wrong one for us.
- **Ship a wrapper script that exports `LATTICE_RUNTIME`** (path 1).
  Adds a shell indirection to the binary the user invokes, breaks
  `exec`-based launchers, and papers over a layout problem with an
  environment variable.
- **Build the components per `dist` leg.** Six identical artefacts, six
  wasm toolchain installs, and a new failure mode on the two
  best-effort ARM legs for output that is byte-identical everywhere.

## 4. The honesty requirement

Every user-facing surface must be true at tag time. This is not a tone
preference: issues and discussions are enabled, so a false doc becomes a
filed bug, and a stranger who catches one stops trusting the rest.

Known-false claims as of 2026-09-18:

| Surface | Claim | Reality |
|---|---|---|
| `site/content/install.md` | signed, checksummed pre-built binaries; a `.tar.gz` release table; a working `curl` one-liner; a Homebrew tap "coming soon" | no tags, no releases, nothing signed; archives are `.tar.xz` |
| `site/content/install.md` | `lattice --version` "prints the version number and commit hash" | clap's `#[command(version)]` (`crates/lattice-cli/src/main.rs:46`) prints the version only |
| `site/content/install.md` | Rust 1.85+ | `rust-toolchain.toml` pins 1.94; README says 1.94+ |
| site hero | `v0.1.0` | the launch is 0.9 |
| `implementation.md:4320-4405` | magit MG.5–MG.9 broken (2026-07-26 audit) | **stale** — all six verified implemented and tested on HEAD; the same plan's 2026-07-27 close-out superseded the audit |
| `implementation.md:5222` | Phase 4.2/4.3 in flight | stale |

A **known-limitations page** is part of this requirement rather than an
apology for it. Naming what is absent is what stops duplicate issues and
sets expectations before the first `:q!`.

## 5. Install channels

**In:** GitHub Releases (the existing six-leg archives plus Linux
AppImage/`.deb`), and a `curl | sh` installer that resolves the latest
release, verifies against `SHA256SUMS`, and installs a prefix into
`~/.local`.

**Out, and documented as out:** Homebrew tap, crates.io
(`cargo install`), macOS/Windows code signing, `.dmg`/`.msi`.

crates.io is the most deceptive of these: it reads as a one-line job and
is not. All 40 workspace crates would need real published versions with
no path-only dependencies, and the core components staged by `xtask`
have no route into a registry install at all — the §3 defect again,
in a form we cannot fix from the release pipeline. A half-working
`cargo install lattice-cli` is worse than none.

Unsigned binaries mean macOS quarantines the download. The install page
and the troubleshooting page both spell out the `xattr` removal; hiding
it produces a first-run failure with a scary dialog and no explanation.

## 6. Feedback surface

GitHub **Issues** (with templates that ask for platform, `lattice
--version`, and whether core plugins loaded — the §3 symptom) and
**Discussions**. Both are already enabled.

**No chat platform at 0.9.** An empty Discord reads worse than no
Discord, and soft-launch volume does not need synchronous support.
`site/content/faq.md` already asks "is there a chat or forum" and must
answer it honestly rather than aspirationally.

## 7. Explicitly not in 0.9

Named here so the release notes and the known-limitations page agree,
and so none of it reads as an oversight:

- LSP **server installation** (`slice-plans/lighthouse.md`, LH.0–LH.2,
  not started) — users install servers by hand.
- Fully themeable syntax colours (`slice-plans/theme-system.md`
  T.5.b/T.5.c) — six `syntax_style` consumers still read a hardcoded
  palette. **Corrected 2026-09-23:** T.5.b had already deleted that
  `match`; every consumer resolves through `resolve_syntax_style`, and the
  remaining Catppuccin literals are the default theme's own palette. The
  entry was stale when this list was written, and the user-facing
  known-limitations page inherited it until an audit caught it.
- GPUI as the default renderer (Phase 5.last ⛔); `--gui` stays opt-in,
  and ARM GUI builds stay best-effort.
- `:autocmd` / `:add-hook` (verified unregistered).
- `:customize` **write-back**. Browsing and picking groups and modes
  works, and `:customize-edit <name>` opens an option in the `:` line;
  what is missing is writing the result back to the user's TOML.
- The dashed `:history-*` **spelling**. `:history` itself ships with
  three kinds.

> **These entries are source-verified, and the first revision of this
> list was not.** It was copied from ⛔ rows in `implementation.md` —
> which contradicts itself on all four, six thousand lines later — and
> `docs/user/known-limitations.md` then inherited the error, telling
> strangers that `:describe-event`, `:describe-mode`, `:customize` and
> `:history` do not exist when all four ship. §9 below states the rule
> this broke, in the course of reproaching an earlier instance of it;
> writing the rule down did not stop the next one. Verify against source,
> including when the source you are tempted to trust is this project's
> own ledger.
- Vim grammar gaps: `!` filter, `gq`, `'<`/`'>` marks, partial ex ranges.
- Terminal mouse passthrough; word motions in Terminal Visual.
- Rich buffer rendering (Phase 9, retired from v1 — concealment is the
  kept carve-out and is itself unimplemented).
- Crash reporter and accessibility work (Phase 10, not started).

## 8. Paramount-goal alignment

- **#1 Performance.** Untouched. Nothing here enters a hot path; the
  core-plugin staging happens at package time.
- **#2 Extensibility.** §3 is the goal's precondition — a plugin-first
  editor whose artefacts carry no plugins does not demonstrate the goal
  at all.
- **#3 Modal editing.** Untouched; §7 names the grammar gaps rather
  than closing them.
- **#4 Asynchronicity.** Untouched.

The higher court (UX) drives §3 and §4: silent plugin absence and a
lying install page are both first-contact failures, and first contact
is the only thing a soft launch actually measures.

## 9. Green baseline

**The baseline is already green** — and the way that was discovered is
itself a launch concern.

`focused-surface.md` §7 recorded that two `lattice-ui-tui` tests
(`typing_after_popup_open_live_refilters_candidates`,
`backspace_after_popup_open_live_refilters`) were red on clean HEAD
because command-line completion had stopped extending `descr` to
`describe-`, "re-proven by stashing, 2026-09-04". This document's first
revision took that at face value and made fixing them slice L.0.

They pass. The bug was fixed on 2026-09-08 by `af719434` ("a fuzzy action
id no longer suppresses the `<Tab>` LCP"), an ancestor of HEAD; the note
was never updated. L.0 became what the situation actually called for: a
regression test at the layer that computes the extension — which
`af719434` shipped without — plus retirement of the stale note.

**The pattern matters more than the instance.** This is the second stale
"X is broken" claim found while preparing 0.9, after the 2026-07-26 magit
audit block in `implementation.md` (§4). Both recorded a real
point-in-time problem and were never revised when it was fixed, and both
would have sent a contributor hunting a bug that does not exist. Only
`design.md` and `implementation.md` are authoritative, and even they
drift — so a claim that something is broken is verified against source
before it is planned around. The magit claims were verified that way; this
one was not, and it cost a slice's worth of work.

The green baseline still matters for the launch, which is why it is
checked rather than assumed: a red baseline makes "did I break it?"
unanswerable for the first contributor who clones the repo, and 0.9's
whole point is that strangers arrive.

## 10. Positioning, and why demo assets are artefacts

A soft launch is judged on first contact, and for most visitors first
contact is an image, not prose. So the demo assets are part of the launch
contract rather than decoration.

**The four differentiators the assets lead with**, each named against the
editors Lattice is actually compared to:

| Differentiator | Absent from |
|---|---|
| **Magit** — status, hunk staging, rebase, blame, transients | Zed, Helix, Neovim (fugitive is not magit) |
| **Everything is a buffer** — file tree, terminal, search results, git views all carry the full grammar | every one of them; the others have panels |
| **Config is Rust compiled to WASM** — one substrate for config and plugins, and programmable enough for custom commands and hooks, not just keybindings | Zed (JSON), Helix (TOML, no plugin host shipped), Neovim (Lua), VS Code (JSON + TS) |
| **Org-mode, and coding agents as editable buffers** | everything outside Emacs; Zed has agents but not as buffers you edit with the grammar |

A fifth, structural rather than feature-shaped: **one core, two
first-class renderers.** Zed has no terminal UI; Helix has no GPU
renderer. Nobody else ships both from one core, and it is why `--gui` is
a peer rather than a fallback.

The pre-existing shot list in `docs/media/screenshot-ideas.md` was
organised by feature and contained none of the four. A feature-organised
gallery shows that Lattice is real; a differentiator-organised one shows
why it exists. The list is re-cut against this table.

It was also **stale** — written before most of what it omits existed, and
never revised as features landed. That is the third stale-doc instance
found preparing 0.9, after `focused-surface.md` §7 and the magit audit
block in `implementation.md`, and the three share a cause: a document
that records a point-in-time state, maintained by hand, with nothing
tying it to the thing it describes. So the shot list is **derived, not
remembered**: its inventory comes from `docs/user/`, which carries one
topic page per shipped feature and is enforced against
`site/data/nav.toml` by a build that fails when the two disagree. A
feature cannot ship without a page, so it cannot be silently missing from
the inventory either. Same principle as the tapes in the paragraph
below, and as generating the site from `docs/` rather than copying into
it: bind the artefact to its source, or accept that it will drift.

**Demo assets are regenerable, not recorded.** TUI demos are declarative
VHS `.tape` files committed to the repo and rendered per release, so an
asset cannot silently rot when the UI moves — the same reason the docs are
generated from `docs/` rather than copied into the site. Manual capture
stays for the GPUI renderer, which VHS cannot drive.
