# Lattice 0.9 Alpha Launch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `v0.9.0` — the first release a stranger can install — with artefacts that actually contain the editor's bundled plugins and user-facing docs that are true.

**Architecture:** Nine slices, each its own commit. L.0 restores a green test baseline. L.1 gets the never-yet-successful release pipeline to complete one end-to-end preview run. L.2 fixes the artefact contract (core plugins + prefix-relocatable layout). L.3–L.7 make every user-facing surface honest and complete. L.8 tags and announces.

**Tech Stack:** Rust 1.94 / cargo, GitHub Actions, `cargo xtask` (wasm32-wasip2 component builds), `cargo-deb`, `linuxdeploy`, Zola + `site/scripts/sync-docs.sh`, POSIX shell (`install.sh`), `gh` CLI.

**Spec:** [`../../architecture/launch-0.9.md`](../../architecture/launch-0.9.md) — the launch contract. §3 of that document amends [`../../architecture/release-pipeline.md`](../../architecture/release-pipeline.md); this plan's L.2 lands that amendment. Pipeline sequencing history is in [`release-pipeline.md`](./release-pipeline.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

**Status:** 🚧 — **v0.9.0 shipped 2026-09-21.**
`https://github.com/dhruvasagar/lattice/releases/tag/v0.9.0`, 20 assets,
notes written by hand from `CHANGELOG.md`. Verified the way it matters: the
public `curl … | sh` one-liner resolved the tag through the API, checksummed
the archive, installed a prefix, and `lattice --version` printed
`lattice 0.9.0` with all nine plugin files present.

Still open, which is why this plan is NOT archived — every open item now has
a row in the table below rather than living only in this paragraph, because
prose-only open work is how `ML.4` and `DB.8` got buried:

- the **announcement** (L.8 Step 11, Dhruva's to make);
- **L.5**'s differentiator screenshot gallery ⛔ (1 of 6 captured) and the
  hero recapture Dhruva chose;
- **L.9b**, recording the demo video — the script is written and ✅, but the
  recording is what every motion clip is now cut from, so it gates L.5's
  moving assets as well as its own;
- **L.10** and **L.11**, added 2026-09-21: org-mode is the fourth
  differentiator in the launch contract (§10) and nothing user-facing says
  so. L.11 grows a `/plugins/` section that the community can be listed in
  later.

L.4b's GIF rendering is ❌ **dropped**, not deferred — headless Chrome's
screencast is broken on this machine, reproduced minimally on a real
terminal, so motion comes from the demo video instead (L.9). Nothing to do.

| Slice | What                                                        | Gate                                            | Status |
|-------|-------------------------------------------------------------|-------------------------------------------------|--------|
| L.0   | Green `lattice-ui-tui` baseline                             | both tests pass; LCP pinned by a test           | ✅     |
| L.1   | Pipeline: ARM artefact bug + first green preview run        | `publish` succeeds once                         | ✅     |
| L.2   | Core plugins in every artefact; prefix-relocatable layout   | extracted binary loads 3 plugins                | ✅     |
| L.3   | Version 0.9.0 + honesty pass                                | no false claim on any user surface              | ✅     |
| L.4   | `install.sh`                                                | installs from a real release on macOS + Linux   | ✅     |
| L.4b  | Positioning, re-cut shot list, VHS capture harness          | tapes keystroke-verified; GIFs ⛔ (VHS sandbox)  | ✅     |
| L.5   | README restructure + hero                                   | README 143 lines, hero wired, paths verified    | ✅     |
| L.5b  | Differentiator screenshot gallery + hero recapture          | 5 landing shots captured; README gallery wired  | ⛔     |
| L.6   | known-limitations, troubleshooting, cheatsheet, dashboard   | sync + zola clean; budget 12.2% headroom        | ✅     |
| L.7   | CONTRIBUTING / SECURITY / CoC / issue templates / CHANGELOG | YAML validates; 9 silent 404s fixed             | ✅     |
| L.8   | Tag `v0.9.0`, notes, announce                               | released; announcement is Dhruva's              | 🚧     |
| L.9   | Demo video script + clip plan                               | script written; recording is Dhruva's           | ✅     |
| L.9b  | Record the demo video; cut the clips                        | video published; clips under 4 MB committed     | ⛔     |
| L.10  | Org-mode in the launch communications                       | README + landing card + org repo discoverable   | ✅     |
| L.11  | A `/plugins/` section: index + a page per plugin             | bundled/external split legible; guard bites     | ✅     |

---

## Global Constraints

Every task's requirements implicitly include this section.

- **Version:** `0.9.0`; tag `v0.9.0`. All 41 crates inherit via `version.workspace = true`, so the only edit is `[workspace.package] version` in the root `Cargo.toml` (line 49). `prepare` fails unless the tag equals that value exactly (`release.yml:43-48`).
- **"Alpha" appears in prose only** — never in the version string. No `-alpha.N` suffix (spec §2).
- **Binary name is `lattice` for both flavours.** TUI = `cargo build -p lattice-cli --release --target <t>`; GUI = the same plus `--features gui`. Both write `target/<t>/release/lattice`, so **the TUI archive MUST be created before the GUI build runs.**
- **Archive layout is prefix-relocatable** (spec §3): `bin/lattice` plus `share/lattice/plugins/<name>/{<name>.wasm,plugin.toml,.source}`, plus `LICENSE` and `README.md` at the root. Artefact *filenames* do not change, so the `publish` manifest stays valid.
- **Core plugins are exactly** `auto-pair`, `treesitter-context`, `project` (`xtask/src/main.rs:19`, `CORE_PLUGINS`).
- **All CI `cargo` invocations use `--locked`.** All `setup-rust-toolchain` steps set `rustflags: ""` (the action otherwise injects `-D warnings`, overriding the workspace's intentionally-relaxed lint gate). Every build leg runs `rm -f rust-toolchain.toml` before installing a toolchain.
- **`actions/upload-artifact@v4` excludes dotfiles unless `include-hidden-files: true`.** The core plugins carry a `.source` marker; without that flag it is silently dropped and `:plugins` shows `—` instead of `bundled`.
- **A new `docs/user/*.md` page needs four things** or the build breaks: a leading `summary:` YAML frontmatter block (`crates/lattice-help/build.rs` reads it), a row in `docs/user/README.md`'s Topics table, an entry in `site/data/nav.toml` (`site/scripts/sync-docs.sh` hard-fails when `docs/user/` and `nav.toml` disagree **in either direction**), and it must keep `embedded_user_docs_stay_under_size_budget` green (384 KiB packed, `crates/lattice-help/src/topics.rs:388`).
- **A new `docs/dev/<subdir>/*.md` page needs an entry in `site/data/dev-nav.toml`** — same both-directions hard fail (`sync-docs.sh:425-456`). Pages under `docs/dev/operations/slice-plans/` are **not** synced (`collect_dev_pages` globs one level, `sync-docs.sh:458-463`), so this plan file needs no entry.
- **Pre-commit gate, before every commit:** `scripts/precommit.sh <touched-crate>...` — fmt-clean, zero new rustc warnings in touched code, no new clippy warnings, targeted tests green. Wait for it to finish before committing.
- **This machine:** prefix heavy cargo runs with `CARGO_BUILD_JOBS=3 nice`. Never launch a multi-crate gate without asking Dhruva first. Gate `lattice-ui-gpui` and `lattice-ui-tui` in **separate** runs (combined load times out `settle_mode`).
- **`rtk` rewrites cargo test output** — `grep "test result"` finds nothing. Match `"passed|failed|^error"`, or run `rtk proxy cargo test`.
- **One slice, one commit.** Commit each as it goes green.

---

### Task L.0: Green `lattice-ui-tui` baseline ✅

> **Premise correction (2026-09-18, after execution).** The two tests were
> NOT red: the bug was fixed on 2026-09-08 by `af719434` ("a fuzzy action id
> no longer suppresses the `<Tab>` LCP"), and `focused-surface.md` §7 — the
> source this slice was written from — was simply stale. Step 1's
> reproduction found 2 passed, which is the signal the brief told the
> implementer to stop on.
>
> What L.0 actually delivered, and why it was still worth doing:
> `af719434` shipped **without a test at the layer that computes the
> extension**, so the behaviour was guarded only by two TUI-level tests that
> happened to notice. L.0 added
> `opening_the_popup_extends_the_line_to_the_longest_common_prefix` in
> `lattice-host`'s dispatch path (proven load-bearing: RED with `af719434`
> reverted, GREEN restored) and retired the stale note. No production code
> changed. See spec §9 for the pattern this is the second instance of.
>
> The task text below is left as written, as the record of what was planned.

Two tests are red on clean HEAD and have been since before 2026-09-04. Fixing them first means every later slice can trust its own test run, and the first stranger who clones the repo does not have to rediscover which failures are "normal".

Documented root cause (`docs/dev/architecture/focused-surface.md:200-206`): *command-line completion stopped extending `descr` to `describe-`* — the longest-common-prefix rewrite that `open_completion_popup` is supposed to apply.

**Files:**
- Investigate: `crates/lattice-ui-tui/src/app/cmdline.rs:294` (`open_completion_popup`, which delegates via `self.mutate_editor(|e| e.open_completion_popup())`)
- Fix: whichever function the delegation lands in (located in Step 3)
- Test: `crates/lattice-ui-tui/src/app/options.rs:1026` (`typing_after_popup_open_live_refilters_candidates`), `crates/lattice-ui-tui/src/app/edit.rs:1527` (`backspace_after_popup_open_live_refilters`)
- Update: `docs/dev/architecture/focused-surface.md:200-206`

**Interfaces:**
- Consumes: nothing from earlier tasks (this is the first).
- Produces: a green `lattice-ui-tui` suite. Every later task's `scripts/precommit.sh` run depends on this being true.

- [ ] **Step 1: Reproduce both failures**

```bash
cd /Users/dhruva/src/dhruvasagar/lattice
CARGO_BUILD_JOBS=3 nice cargo test -p lattice-ui-tui --lib -- --test-threads=1 popup_open_live_refilters
```

Expected: 2 failures. Both should fail on the same assertion shape — `assert_eq!(a.editor.command_line(), "describe-")` getting the un-extended input back (`"descr"` and `"describ"` respectively).

Record the actual left-hand values. If they instead fail on the candidate-count assertions, the root cause in `focused-surface.md` is wrong and you are debugging something else — say so before proceeding.

- [ ] **Step 2: Confirm it predates the launch work**

```bash
git stash -u && CARGO_BUILD_JOBS=3 nice cargo test -p lattice-ui-tui --lib -- --test-threads=1 popup_open_live_refilters ; git stash pop
```

Expected: the same 2 failures on clean HEAD. (There should be nothing to stash at this point — run it anyway, because the whole slice rests on this being a pre-existing failure rather than something L.0 introduced.)

- [ ] **Step 3: Locate the LCP extension site**

```bash
grep -rn "fn open_completion_popup" crates/ --include="*.rs"
```

`crates/lattice-ui-tui/src/app/cmdline.rs:296` calls `e.open_completion_popup()` on the editor, so the second hit is the real implementation. Read its body and find where it computes the longest common prefix of the candidates and assigns `original_line`. The bug is that the extension is computed but not written back to the command line, or is skipped by a guard.

- [ ] **Step 4: Write a focused unit test at the layer that is broken**

Add this beside the other tests in the file that owns `open_completion_popup`'s implementation (the crate found in Step 3). Adjust the constructor call to match that module's existing test helper — read a neighbouring test first and copy its setup shape verbatim.

```rust
#[test]
fn opening_the_popup_extends_the_line_to_the_longest_common_prefix() {
    // Every `describe-*` command shares `describe-`, so opening the
    // popup on `descr` must rewrite the line (vim-wildmenu style)
    // while preserving `descr` as `original_line` for dismiss-restore.
    let mut e = editor_in_command_mode("descr");
    e.open_completion_popup();
    let state = e
        .completion_state
        .as_ref()
        .expect("popup must open with multiple describe-* matches");
    assert_eq!(e.command_line(), "describe-");
    assert_eq!(state.original_line, "descr");
}
```

- [ ] **Step 5: Run it and watch it fail**

```bash
CARGO_BUILD_JOBS=3 nice cargo test -p <crate-from-step-3> --lib -- --test-threads=1 opening_the_popup_extends_the_line
```

Expected: FAIL, `assertion `left == right`` with left `"descr"`.

- [ ] **Step 6: Fix the extension**

Make the minimal change that writes the longest common prefix back to the command line while setting `original_line` to what the user typed. Do not restructure the completion pipeline — the two red tests describe behaviour that used to work, so this is a regression repair, not a redesign.

- [ ] **Step 7: Verify all three tests pass**

```bash
CARGO_BUILD_JOBS=3 nice cargo test -p <crate-from-step-3> --lib -- --test-threads=1 opening_the_popup_extends_the_line
CARGO_BUILD_JOBS=3 nice cargo test -p lattice-ui-tui --lib -- --test-threads=1 popup_open_live_refilters
```

Expected: 1 passed, then 2 passed.

- [ ] **Step 8: Prove the whole crate is green**

Ask Dhruva before running this — it is a whole-crate gate on a ~1860-test crate.

```bash
scripts/precommit.sh lattice-ui-tui <crate-from-step-3>
```

Expected: fmt clean, no new warnings, zero failures. If a *different* test now fails, re-run that one alone before believing it (`--test-threads=1`); parts of this suite settle by polling and time out under load.

- [ ] **Step 9: Retire the known-red note**

In `docs/dev/architecture/focused-surface.md`, replace the bullet at lines 200-206 with:

```markdown
- **Fixed 2026-09-18 (L.0).** `typing_after_popup_open_live_refilters_candidates`
  and `backspace_after_popup_open_live_refilters` were red on clean HEAD because
  command-line completion had stopped extending `descr` to `describe-`. The
  extension is now pinned by a unit test at the layer that computes it, so the
  crate's green baseline is zero failures again.
```

- [ ] **Step 10: Commit**

```bash
git add crates/lattice-ui-tui docs/dev/architecture/focused-surface.md
git add <crate-from-step-3>
git commit -m "fix(completion): restore the longest-common-prefix extension on popup open

The command line stopped extending \`descr\` to \`describe-\` when the
completion popup opened, leaving two lattice-ui-tui tests red on clean
HEAD since before 2026-09-04 — a baseline every contributor had to
rediscover. Pinned at the layer that computes the prefix, not just
through the two TUI tests that noticed.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

### Task L.1: Pipeline — fix the ARM artefact bug and get one green preview run

`release.yml` has run exactly twice, both failures. **`publish` has never executed once**, so the source archive, `SHA256SUMS`, the manifest check, provenance and the release upload are all code that has never run. Before spending a run on it, fix the bug the audit already found.

`release.yml:250-251` lists `lattice-gui-<ver>-aarch64.AppImage` and `…-aarch64.deb` as **required**, but both are produced only when the GUI build succeeds, and the `aarch64-linux` leg is `gui_best_effort: true`. So the exact case the design says must never block a release — ARM GUI failing to link — makes `publish` exit 1 and kills the whole release.

**Files:**
- Modify: `.github/workflows/release.yml:237-265` (the `required` / `best_effort` arrays)
- Update: `docs/dev/operations/slice-plans/release-pipeline.md` (status block, Task 5)

**Interfaces:**
- Consumes: nothing from L.0 (independent, but sequenced after it so the repo is green first).
- Produces: a `release-preview` artefact containing every archive plus `SHA256SUMS`. L.2 modifies the same workflow and re-runs this preview; L.4's installer is tested against this bundle's successor.

- [ ] **Step 1: Move the ARM Linux GUI bundles to best-effort**

In `.github/workflows/release.yml`, delete these two lines from the `required=(` array:

```bash
            "lattice-gui-${VERSION}-aarch64.AppImage"
            "lattice-gui-${VERSION}-aarch64.deb"
```

and extend the `best_effort=(` array so it reads:

```bash
          best_effort=(
            "lattice-gui-${VERSION}-aarch64-linux.tar.xz"
            "lattice-gui-${VERSION}-aarch64-windows.zip"
            "lattice-gui-${VERSION}-aarch64.AppImage"
            "lattice-gui-${VERSION}-aarch64.deb"
          )
```

- [ ] **Step 2: Assert the invariant in the workflow itself**

Immediately after the `best_effort=(...)` array, insert this guard so the pairing can never drift again:

```bash
          # Every artefact produced by a `gui_best_effort` leg must be
          # best-effort here too. Listing one as required is what made a
          # failed ARM GUI link kill the entire release.
          for f in "${required[@]}"; do
            case "$f" in
              *aarch64-linux* | *aarch64-windows* | *-aarch64.AppImage | *-aarch64.deb)
                if [[ "$f" != *"lattice-${VERSION}-aarch64"* ]]; then
                  echo "::error::$f is produced by a best-effort leg but listed as required"
                  exit 1
                fi
                ;;
            esac
          done
```

(The inner test allows the ARM **TUI** archives, which are genuinely required — only the GUI artefacts are best-effort.)

- [ ] **Step 3: Lint the workflow**

```bash
actionlint .github/workflows/release.yml
```

Expected: no output, exit 0. If `actionlint` is missing: `brew install actionlint`.

- [ ] **Step 4: Verify the arrays with a parse check**

```bash
python3 - <<'EOF'
import re
s = open('.github/workflows/release.yml').read()
req = re.search(r'required=\((.*?)\)', s, re.S).group(1)
best = re.search(r'best_effort=\((.*?)\)', s, re.S).group(1)
assert 'aarch64.AppImage' not in req, 'ARM AppImage still required'
assert 'aarch64.deb' not in req, 'ARM deb still required'
assert 'aarch64.AppImage' in best and 'aarch64.deb' in best, 'ARM bundles not best-effort'
print('artefact classes OK:', len(req.split()), 'required,', len(best.split()), 'best-effort')
EOF
```

Expected: `artefact classes OK: 13 required, 4 best-effort`.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): ARM GUI bundles are best-effort, not required

publish listed lattice-gui-*-aarch64.{AppImage,deb} as required while
the aarch64-linux leg builds its GUI with continue-on-error, so the one
failure the design says must never block a release would have killed it.
Adds a guard so the pairing cannot drift again.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

- [ ] **Step 6: Trigger a preview run on this branch**

```bash
git push -u origin HEAD
gh workflow run release.yml --ref "$(git rev-parse --abbrev-ref HEAD)"
```

`workflow_dispatch` runs in preview mode: version becomes `dev-<short-sha>`, no tag, no release.

- [ ] **Step 7: Watch it**

```bash
rid="$(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')"
gh run watch "$rid" --exit-status
```

Expected: `prepare`, all six `dist` legs, and `publish` green. The two ARM GUI steps may show `continue-on-error` warnings — acceptable; the legs themselves must be green.

This step is expected to iterate. The `x86_64-linux` leg previously died on `-fuse-ld=mold` (re-applied from `.cargo/config.toml` because release legs build with an explicit `--target`); `120b27e9` installs `mold` to fix it and **that fix has never been exercised**. On failure: read the log, fix, commit the fix as its own commit, re-run from Step 6. Do not batch pipeline fixes into one commit — each is a separate diagnosis.

- [ ] **Step 8: Download the preview bundle and verify checksums**

```bash
rid="$(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')"
rm -rf /tmp/lattice-preview && gh run download "$rid" -n release-preview -D /tmp/lattice-preview
cd /tmp/lattice-preview && ls -1 && shasum -a 256 -c SHA256SUMS
```

Expected: TUI archives for all 6 platforms, GUI archives for at least the 4 proven platforms, `.deb` + `.AppImage` + `.zsync` for x86_64 Linux, a source archive, `SHA256SUMS` — and every checksum `OK`.

- [ ] **Step 9: Record that Task 5 finally passed**

In `docs/dev/operations/slice-plans/release-pipeline.md`, replace the `**Status:**` paragraph (the block beginning "🚧 in progress (audited 2026-08-27)") with:

```markdown
**Status:** ✅ complete. Tasks 1–4 landed (`233dc3d1`, `036b9dd3`,
`ef9523fa`, `d1cf810c`, `d9dd6d01`, `120b27e9`); **Task 5 — the
integration run — passed on 2026-09-18** under launch slice L.1, which
also fixed the required/best-effort mismatch the 2026-08-27 audit found
(`lattice-gui-*-aarch64.{AppImage,deb}` were required while the leg that
builds them is best-effort) and added a guard against its recurrence.
The packaging contract this plan designed is amended by launch slice L.2
— see `../../architecture/launch-0.9.md` §3 — because the archives as
designed here carry no core plugins.
```

(Present tense deliberately: L.1 runs before L.2, so this sentence must
not claim an amendment that has not landed yet. L.2 Step 13 writes the
amendment itself.)

Then tick Task 5's five checkboxes.

- [ ] **Step 10: Commit**

```bash
git add docs/dev/operations/slice-plans/release-pipeline.md
git commit -m "docs(release-pipeline): Task 5 passed — the pipeline has run end to end

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

### Task L.2: Core plugins in every artefact

Spec §3. Every artefact the pipeline currently produces would ship with **zero** core plugins: the legs never run `cargo xtask build-core-plugins`, `/runtime` is gitignored, and the flat archive layout puts nothing on any of discovery's four search paths. `default_core_plugins_dir()` returns `None`, which the code treats as a benign skip — so auto-pair, treesitter-context and project are silently absent, with no error for the user to search for.

Start with the test, because the layout the entire release will depend on is the one discovery path that has no test: the four existing tests cover the `$LATTICE_RUNTIME` override, the baked prefix, the dev fallback, and the none case — but not `<exe-dir>/../share/lattice/plugins`.

**Files:**
- Test: `crates/lattice-plugin-loader/src/discovery.rs:220-300` (the `mod tests` block)
- Modify: `.github/workflows/release.yml` (new `core-plugins` job; `dist` gains `needs` + a download step; both packaging steps and both Linux bundle steps rewritten for the prefix layout)
- Modify: `crates/lattice-cli/Cargo.toml` (`[package.metadata.deb]` assets)
- Modify: `docs/dev/architecture/release-pipeline.md` (land the spec §3 amendment)

**Interfaces:**
- Consumes: L.1's green pipeline. The `required`/`best_effort` artefact *names* are unchanged by this task — only archive contents change — so `publish`'s manifest stays valid.
- Produces: `core_plugins_dir_from(None, None, Some("<prefix>/bin/lattice"))` → `Some("<prefix>/share/lattice/plugins")`, pinned by test. A `core-plugins` CI artifact containing `runtime/plugins/<name>/{<name>.wasm,plugin.toml,.source}`, downloaded by every `dist` leg to `runtime/plugins`. L.4's installer relies on the archive's `bin/` + `share/` shape.

- [ ] **Step 1: Write the failing test for the relocatable layout**

Add to the `mod tests` block in `crates/lattice-plugin-loader/src/discovery.rs`, after `falls_through_to_the_dev_runtime_dir`:

```rust
    #[test]
    fn resolves_a_relocatable_install_from_the_bin_dir() {
        // The release-archive layout (launch-0.9.md §3): the user extracts
        // `lattice-<ver>-<platform>/` anywhere and runs `bin/lattice`, which
        // must find `../share/lattice/plugins` beside it. No baked prefix —
        // the archive is relocatable, so the prefix isn't known at build time.
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        let plugins = prefix.join("share").join("lattice").join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();

        let got = core_plugins_dir_from(None, None, Some(&prefix.join("bin").join("lattice")));

        assert_eq!(
            got.as_deref().map(std::path::Path::to_path_buf),
            Some(plugins),
            "an extracted archive must find the plugins shipped beside its binary"
        );
    }
```

- [ ] **Step 2: Run it**

```bash
CARGO_BUILD_JOBS=3 nice cargo test -p lattice-plugin-loader --lib -- --test-threads=1 resolves_a_relocatable_install
```

Expected: PASS. The search path already supports this layout (`discovery.rs:92`) — the test exists to *pin* it, because everything below depends on it and nothing was guarding it. If it FAILS, stop: the archive layout in the spec is wrong and must be revisited before touching the workflow.

Note the `Path::canonicalize` trap on macOS: `tempdir()` returns `/var/...` which is a symlink to `/private/var/...`. The assertion above compares unresolved paths on both sides, so it is unaffected — do not "fix" it by canonicalizing one side only.

- [ ] **Step 3: Commit the test**

```bash
git add crates/lattice-plugin-loader/src/discovery.rs
git commit -m "test(discovery): pin the relocatable install layout

The release archives are about to depend on \`<exe>/../share/lattice/plugins\`,
and it was the one discovery path with no test — override, baked prefix,
dev fallback and none were all covered.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

- [ ] **Step 4: Add the `core-plugins` job**

In `.github/workflows/release.yml`, insert this job between `prepare` and `dist`:

```yaml
  # WASM components are platform-independent, so the three core plugins are
  # built ONCE here and downloaded by every dist leg. Per-leg builds would be
  # six identical artefacts and would drag a wasm32-wasip2 toolchain onto the
  # ARM Windows runner for nothing. See docs/dev/architecture/launch-0.9.md §3.
  core-plugins:
    name: core-plugins
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Remove rust-toolchain.toml
        shell: bash
        run: rm -f rust-toolchain.toml

      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          toolchain: stable
          target: wasm32-wasip2
          rustflags: ""

      - uses: Swatinem/rust-cache@v2
        with:
          key: core-plugins

      - name: Build and stage the core plugin components
        run: cargo xtask build-core-plugins

      - name: Verify the staged layout
        shell: bash
        run: |
          set -euo pipefail
          for p in auto-pair treesitter-context project; do
            for f in "$p.wasm" plugin.toml .source; do
              if [[ ! -f "runtime/plugins/$p/$f" ]]; then
                echo "::error::missing runtime/plugins/$p/$f"
                exit 1
              fi
            done
          done
          ls -lR runtime/plugins

      - uses: actions/upload-artifact@v4
        with:
          name: core-plugins
          path: runtime/plugins
          # `.source` is a dotfile and upload-artifact@v4 drops hidden files
          # by default. Without this the marker vanishes and `:plugins` shows
          # `—` instead of `bundled` for every core plugin.
          include-hidden-files: true
          if-no-files-found: error
```

- [ ] **Step 5: Make `dist` consume it**

Change the `dist` job's `needs:` line from:

```yaml
    needs: prepare
```

to:

```yaml
    needs: [prepare, core-plugins]
```

and insert this step into `dist` immediately after the `Swatinem/rust-cache@v2` step:

```yaml
      - name: Download the core plugin components
        uses: actions/download-artifact@v4
        with:
          name: core-plugins
          path: runtime/plugins
```

- [ ] **Step 6: Repackage the TUI archive as a prefix tree**

Replace the body of the `Package TUI archive` step's `run:` block with:

```bash
          set -euo pipefail
          mkdir -p dist
          pkg="lattice-${VERSION}-${{ matrix.build }}"
          # Prefix-relocatable layout: `bin/lattice` finds `../share/lattice/plugins`
          # wherever the user extracts this. See launch-0.9.md §3.
          mkdir -p "$pkg/bin" "$pkg/share/lattice"
          cp -R runtime/plugins "$pkg/share/lattice/plugins"
          cp LICENSE README.md "$pkg/"
          if [[ "${{ runner.os }}" == "Windows" ]]; then
            cp "target/${{ matrix.target }}/release/lattice.exe" "$pkg/bin/"
            7z a -r "dist/$pkg.zip" "$pkg" >/dev/null
          else
            cp "target/${{ matrix.target }}/release/lattice" "$pkg/bin/"
            chmod +x "$pkg/bin/lattice"
            tar cJf "dist/$pkg.tar.xz" "$pkg"
          fi
          rm -rf "$pkg"
```

- [ ] **Step 7: Repackage the GUI archive the same way**

Replace the body of the `Package GUI archive` step's `run:` block with the identical shape, differing only in the package name:

```bash
          set -euo pipefail
          mkdir -p dist
          pkg="lattice-gui-${VERSION}-${{ matrix.build }}"
          mkdir -p "$pkg/bin" "$pkg/share/lattice"
          cp -R runtime/plugins "$pkg/share/lattice/plugins"
          cp LICENSE README.md "$pkg/"
          if [[ "${{ runner.os }}" == "Windows" ]]; then
            cp "target/${{ matrix.target }}/release/lattice.exe" "$pkg/bin/"
            7z a -r "dist/$pkg.zip" "$pkg" >/dev/null
          else
            cp "target/${{ matrix.target }}/release/lattice" "$pkg/bin/"
            chmod +x "$pkg/bin/lattice"
            tar cJf "dist/$pkg.tar.xz" "$pkg"
          fi
          rm -rf "$pkg"
```

- [ ] **Step 8: Verify each archive carries the plugins, in the leg that built it**

Insert this step after `Package GUI archive` and before `Install cargo-deb`:

```yaml
      - name: Verify archives carry the core plugins
        shell: bash
        env:
          VERSION: ${{ needs.prepare.outputs.version }}
        run: |
          set -euo pipefail
          # A missing plugin is silent at runtime (discovery treats an absent
          # dir as a benign skip), so it has to be caught here or not at all.
          check_tar() {
            for p in auto-pair treesitter-context project; do
              for f in "$p.wasm" plugin.toml .source; do
                tar tJf "$1" | grep -q "share/lattice/plugins/$p/$f" \
                  || { echo "::error::$1 is missing $p/$f"; exit 1; }
              done
            done
            tar tJf "$1" | grep -q "bin/lattice" || { echo "::error::$1 has no bin/lattice"; exit 1; }
            echo "ok: $1"
          }
          check_zip() {
            local listing; listing="$(7z l -ba "$1")"
            for p in auto-pair treesitter-context project; do
              for f in "$p.wasm" plugin.toml .source; do
                echo "$listing" | grep -q "$p.$f" \
                  || { echo "::error::$1 is missing $p/$f"; exit 1; }
              done
            done
            echo "ok: $1"
          }
          shopt -s nullglob
          for a in dist/*.tar.xz; do check_tar "$a"; done
          for a in dist/*.zip; do check_zip "$a"; done
```

- [ ] **Step 9: Put the plugins in the `.deb`**

In `crates/lattice-cli/Cargo.toml`, extend the `[package.metadata.deb]` `assets` array with nine explicit entries (a glob would depend on `require_literal_leading_dot` being false to catch `.source` — too subtle for a gate). Insert them before the closing `]`:

```toml
    # Core plugin components, staged into runtime/plugins/ by the pipeline's
    # core-plugins job. An installed /usr/bin/lattice resolves these through
    # discovery path 3 (`<exe>/../share/lattice/plugins`).
    ["../../runtime/plugins/auto-pair/auto-pair.wasm", "usr/share/lattice/plugins/auto-pair/auto-pair.wasm", "644"],
    ["../../runtime/plugins/auto-pair/plugin.toml", "usr/share/lattice/plugins/auto-pair/plugin.toml", "644"],
    ["../../runtime/plugins/auto-pair/.source", "usr/share/lattice/plugins/auto-pair/.source", "644"],
    ["../../runtime/plugins/treesitter-context/treesitter-context.wasm", "usr/share/lattice/plugins/treesitter-context/treesitter-context.wasm", "644"],
    ["../../runtime/plugins/treesitter-context/plugin.toml", "usr/share/lattice/plugins/treesitter-context/plugin.toml", "644"],
    ["../../runtime/plugins/treesitter-context/.source", "usr/share/lattice/plugins/treesitter-context/.source", "644"],
    ["../../runtime/plugins/project/project.wasm", "usr/share/lattice/plugins/project/project.wasm", "644"],
    ["../../runtime/plugins/project/plugin.toml", "usr/share/lattice/plugins/project/plugin.toml", "644"],
    ["../../runtime/plugins/project/.source", "usr/share/lattice/plugins/project/.source", "644"],
```

- [ ] **Step 10: Put the plugins in the AppImage**

In the `Build AppImage (GUI)` step, after the `cp "target/.../lattice" AppDir/usr/bin/lattice` line, add:

```bash
          mkdir -p AppDir/usr/share/lattice
          cp -R runtime/plugins AppDir/usr/share/lattice/plugins
```

- [ ] **Step 11: Lint and structurally check the workflow**

```bash
actionlint .github/workflows/release.yml
python3 - <<'EOF'
import yaml
d = yaml.safe_load(open('.github/workflows/release.yml'))
jobs = d['jobs']
assert 'core-plugins' in jobs, 'core-plugins job missing'
assert jobs['dist']['needs'] == ['prepare', 'core-plugins'], jobs['dist']['needs']
up = [s for s in jobs['core-plugins']['steps'] if 'upload-artifact' in str(s.get('uses',''))][0]
assert up['with']['include-hidden-files'] is True, 'hidden files would drop .source'
names = [s.get('name','') for s in jobs['dist']['steps']]
assert names.index('Download the core plugin components') < names.index('Package TUI archive')
assert names.index('Package TUI archive') < names.index('Build GUI binary'), 'GUI build would overwrite the TUI binary'
print('workflow shape OK')
EOF
```

Expected: no actionlint output, then `workflow shape OK`.

- [ ] **Step 12: Verify the deb asset sources exist locally**

```bash
cargo xtask build-core-plugins
python3 - <<'EOF'
import tomllib, pathlib
crate = pathlib.Path("crates/lattice-cli")
deb = tomllib.loads((crate/"Cargo.toml").read_text())["package"]["metadata"]["deb"]
missing = [s for s,_,_ in deb["assets"] if not s.startswith("target/") and not (crate/s).exists()]
assert not missing, missing
print(f'{len(deb["assets"])} deb asset sources all exist')
EOF
```

Expected: `17 deb asset sources all exist` (8 pre-existing + 9 plugin files). If `cargo xtask build-core-plugins` fails on a missing target, run `rustup target add wasm32-wasip2`.

- [ ] **Step 13: Land the design amendment**

In `docs/dev/architecture/release-pipeline.md`, find the section describing the archive contents and add:

```markdown
## Amendment (2026-09-18) — archives are prefix-relocatable

The layout above packaged a bare binary, which shipped every artefact with
zero core plugins: the legs never ran `cargo xtask build-core-plugins`, and a
flat archive puts nothing on any of discovery's four search paths, so
`default_core_plugins_dir()` returned `None` — a silent skip.

Archives now carry `bin/lattice` plus
`share/lattice/plugins/<name>/{<name>.wasm,plugin.toml,.source}`, and a
`core-plugins` job builds the three components once (WASM is
platform-independent) for every leg to download. The `.deb` installs the same
tree under `/usr`; the AppImage stages it under `AppDir/usr`. Artefact
filenames are unchanged, so `publish`'s manifest is unaffected.

Rationale, rejected alternatives (baked `LATTICE_INSTALL_PREFIX`, a
`LATTICE_RUNTIME` wrapper script, per-leg builds):
`launch-0.9.md` §3.
```

- [ ] **Step 14: Commit**

```bash
git add .github/workflows/release.yml crates/lattice-cli/Cargo.toml docs/dev/architecture/release-pipeline.md
git commit -m "ci(release): ship the core plugins with every artefact

Every artefact would have shipped with zero plugins: the legs never ran
\`cargo xtask build-core-plugins\`, /runtime is gitignored, and the flat
archive layout put nothing on any of discovery's four search paths — so
auto-pair, treesitter-context and project were silently absent from
every download, with no error to search for.

Archives are now prefix-relocatable (bin/ + share/lattice/plugins/),
built once in a core-plugins job because WASM components are
platform-independent, and each leg verifies its own archives rather than
trusting the copy.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

- [ ] **Step 15: Preview run**

```bash
git push
gh workflow run release.yml --ref "$(git rev-parse --abbrev-ref HEAD)"
rid="$(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')"
gh run watch "$rid" --exit-status
```

Expected: `core-plugins` green, all six `dist` legs green with the new verify step passing, `publish` green.

- [ ] **Step 16: Prove it on a real extracted archive (the gate for this slice)**

```bash
rid="$(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')"
rm -rf /tmp/lattice-preview && gh run download "$rid" -n release-preview -D /tmp/lattice-preview
cd /tmp/lattice-preview
tar xJf lattice-dev-*-aarch64-macos.tar.xz
cd lattice-dev-*-aarch64-macos
find share -type f | sort
./bin/lattice --version
```

Expected: nine files under `share/lattice/plugins/` (three per plugin, `.source` among them), and a version line.

Then the part no CI step can assert — open the editor and confirm the plugins actually loaded:

```bash
./bin/lattice
```

In the editor run `:plugins`. Expected: `auto-pair`, `treesitter-context` and `project` each listed with SOURCE `bundled`. Then open a file and type `(` — expected: `()` with the cursor between them. If `:plugins` is empty, discovery did not resolve: check `find . -name '.source'` inside the extracted tree (a dropped dotfile means `include-hidden-files` regressed) before touching discovery code.

---

### Task L.3: Version 0.9.0 and the honesty pass

Spec §4. Every user-facing claim must be true at tag time. Issues and discussions are enabled, so a false doc becomes a filed bug.

**Files:**
- Modify: `Cargo.toml:49` (`version = "0.1.0"` → `"0.9.0"`), `Cargo.lock`
- Rewrite: `site/content/install.md`
- Modify: `README.md` (Rust version line only — the restructure is L.5)
- Modify: `docs/dev/operations/implementation.md` (three stale blocks)

**Interfaces:**
- Consumes: nothing structural.
- Produces: `[workspace.package] version = "0.9.0"`, which all 41 crates inherit and from which `site/scripts/sync-docs.sh:57-66` generates `site/data/version.toml` (the site hero). L.8's tag `v0.9.0` must match this exactly.

- [ ] **Step 1: Bump the workspace version**

```bash
python3 - <<'EOF'
import pathlib, re
p = pathlib.Path('Cargo.toml')
s = p.read_text()
s2, n = re.subn(r'(\[workspace\.package\]\nversion = )"0\.1\.0"', r'\1"0.9.0"', s, count=1)
assert n == 1, 'workspace version line not found in the expected shape'
p.write_text(s2)
print('bumped')
EOF
grep -m1 -E '^version[[:space:]]*=' Cargo.toml
```

Expected: `version = "0.9.0"` — and it must be the **first** line-anchored `version =` in the file, because that is exactly what `release.yml:44` greps for.

- [ ] **Step 2: Refresh the lockfile**

```bash
CARGO_BUILD_JOBS=3 nice cargo check -p lattice-cli --locked 2>&1 | tail -5
```

Expected: this **fails** — `--locked` refuses to update the lock while 41 crate versions changed. That is the signal; now update it deliberately:

```bash
CARGO_BUILD_JOBS=3 nice cargo check -p lattice-cli 2>&1 | tail -3
git diff --stat Cargo.lock
```

Expected: `Cargo.lock` shows 41 version changes and nothing else.

- [ ] **Step 3: Verify the site hero picks it up**

```bash
python3 site/scripts/sync-docs.sh 2>&1 | grep -E "version|ERROR"
cat site/data/version.toml
```

Expected: `latest = "0.9.0"`. (`version.toml` is gitignored and generated — do not stage it.)

- [ ] **Step 4: Rewrite the install page**

Replace `site/content/install.md` entirely:

```markdown
+++
title = "Installation"
description = "Install Lattice from a release archive, the install script, or source"
+++

Lattice 0.9 is an **alpha**. The editor is usable; the distribution is new.
Binaries are unsigned — see [Gatekeeper](#macos-gatekeeper) below if macOS
refuses to open one.

## Install script (macOS, Linux)

```sh
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
```

Installs into `~/.local` (`~/.local/bin/lattice` plus the bundled plugins
under `~/.local/share/lattice`). Override with `--prefix`:

```sh
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh -s -- --prefix /usr/local
```

Add `~/.local/bin` to your `PATH` if it isn't there already.

## Release archives

Download from the [releases page](https://github.com/dhruvasagar/lattice/releases).

| Platform | Architectures | TUI | GUI |
|---|---|---|---|
| macOS | x86_64, aarch64 | `.tar.xz` | `.tar.xz` |
| Linux | x86_64, aarch64 | `.tar.xz` | `.tar.xz`, `.AppImage`, `.deb` |
| Windows | x86_64, aarch64 | `.zip` | `.zip` (x86_64) |

The `lattice-*` archives are the terminal build — the one to use over SSH or
on a server. The `lattice-gui-*` archives are the same editor plus the
GPU-rendered window, opened with `--gui`. ARM Linux and ARM Windows GUI
builds are best-effort and may be absent from a given release.

Every archive unpacks to a relocatable prefix — put it anywhere:

```
lattice-0.9.0-aarch64-macos/
  bin/lattice
  share/lattice/plugins/…   # bundled plugins; keep these beside bin/
```

`bin/lattice` finds its plugins through `../share/lattice/plugins`, so move
the whole directory rather than just the binary. Verify your download against
the release's `SHA256SUMS`:

```sh
shasum -a 256 -c SHA256SUMS
```

### macOS Gatekeeper

Archives downloaded in a browser are quarantined, and macOS will refuse to
run an unsigned binary. Clear the flag:

```sh
xattr -dr com.apple.quarantine lattice-0.9.0-aarch64-macos
```

Downloads made with `curl` — including the install script — are not
quarantined, so this step only applies to browser downloads. There is no
signed installer at 0.9; notarisation needs a paid certificate.

## From source

```sh
git clone https://github.com/dhruvasagar/lattice
cd lattice
cargo build --release
cargo xtask build-core-plugins   # builds the bundled plugins
./target/release/lattice
```

The GPU renderer is behind a cargo feature:

```sh
cargo run --features gui -- --gui
```

Requires Rust **1.94+** (edition 2024, pinned in `rust-toolchain.toml`) and
the `wasm32-wasip2` target for the plugin build:
`rustup target add wasm32-wasip2`. Install the toolchain via
[rustup](https://rustup.rs/).

`cargo xtask build-core-plugins` is not optional — without it the editor
starts with no bundled plugins and no error, because an absent plugin
directory is indistinguishable from an empty one.

## Requirements

- **macOS** 14+, or **Linux** with kernel 5.10+, or **Windows** 10+
- **Build:** Rust 1.94+, `clang` (tree-sitter), `cmake` (some native deps)
- **GPU mode (optional):** Metal on macOS, Vulkan on Linux

## Verify your install

```sh
lattice --version
```

Prints the version. To confirm the bundled plugins were found, open the
editor and run `:plugins` — `auto-pair`, `treesitter-context` and `project`
should each be listed as `bundled`.

## Not yet available

Homebrew, `cargo install`, `.dmg` and `.msi` are all post-0.9. See
[known limitations](./docs/known-limitations/).

## Next steps

- [Getting started](./docs/getting-started/) — ten-minute orientation
- [Modal editing](./docs/modal-editing/) — the vim grammar
- [Known limitations](./docs/known-limitations/) — what doesn't work yet
```

(The `known-limitations` links resolve once L.6 lands. Run the site build in L.6, not here.)

- [ ] **Step 5: Fix the README's Rust version**

```bash
grep -n "1\.85\|1\.94" README.md
```

Ensure every occurrence reads `1.94+`. There should be no `1.85` anywhere in the repo's user-facing docs:

```bash
grep -rn "1\.85" README.md site/content/ docs/user/ || echo "clean"
```

Expected: `clean`.

- [ ] **Step 6: Retire the stale magit audit block**

The 2026-07-26 functional audit in `implementation.md` marks MG.5–MG.9 broken. All six were verified implemented and tested on HEAD (2026-09-18), and the same slice plan's 2026-07-27 close-out already superseded the audit — the prose was simply never removed.

```bash
grep -n "MG.5\|MG.6\|MG.7\|MG.8\|MG.9" docs/dev/operations/implementation.md | sed -n '1,20p'
```

Locate the audit block (around lines 4320-4405), read it, and replace the MG.5–MG.9 paragraphs with:

```markdown
**MG.5–MG.9 — closed.** The 2026-07-26 functional audit that used to be
quoted here was superseded by the magit plan's own 2026-07-27 close-out and
by MG.13 (action handlers registered at boot rather than on activation),
which invalidated its cross-buffer-hijack and dropped-registration findings
outright. Re-verified against source 2026-09-18: magit-diff populates and
refreshes, magit-log `<CR>` opens a synthetic revision buffer with no temp
file, blame `<CR>`/`p` resolve and re-blame, root/file transient items
resolve to real actions (guarded by
`every_root_dispatch_item_resolves_to_a_real_action_not_a_flag_fallback`),
branch `c` runs a pick-base → prompt-name wizard, and the rebase todo is
built from `git log --reverse` with `C-c C-c` running a real `git rebase -i`.
See `slice-plans/magit.md` for the per-slice record.
```

- [ ] **Step 7: Fix the other two stale claims**

```bash
grep -n "^## In-progress" docs/dev/operations/implementation.md
sed -n '1335,1360p' docs/dev/operations/implementation.md
```

Two fixes:
- The `## In-progress` section (near line 5222) still describes Phase 4.2/4.3 as in flight. Both are ✅ in the phase table. Update the section to name the actual active frontier — reconcile it with the phase table's `5.8.AF.5` row and README's claim, and state one of them rather than three.
- The help table (near lines 1340-1356) marks `:describe-option` and the help major mode ⛔ while line 1167 marks `:describe-option` ✅ and `docs/user/help-mode.md` exists. Change the table rows to ✅.

- [ ] **Step 8: Housekeeping**

```bash
git rm --cached assets/.DS_Store 2>/dev/null || true
git mv pickers.md docs/dev/notes/pickers.md
grep -rn "pickers.md" --include="*.md" docs/ README.md site/ | grep -v "notes/pickers.md" || echo "no inbound refs"
```

`docs/dev/notes/` **is** synced, so add `"notes/pickers"` to the `Reviews & notes` section of `site/data/dev-nav.toml`. Then remove the stale comment in `site/config.toml` that says to uncomment `deploy_ref` "when the deploy workflow is ready" — it has been ready since `deploy-docs.yml` landed.

- [ ] **Step 9: Verify the site still builds**

```bash
python3 site/scripts/sync-docs.sh 2>&1 | tail -20
```

Expected: no `ERROR`. A nav mismatch names the exact file — fix it rather than removing the page.

- [ ] **Step 10: Gate and commit**

```bash
scripts/precommit.sh lattice-cli
```

```bash
git add Cargo.toml Cargo.lock README.md site/content/install.md site/config.toml \
        site/data/dev-nav.toml docs/dev/operations/implementation.md docs/dev/notes/pickers.md
git commit -m "chore(release): 0.9.0, and make every user-facing claim true

Bumps the workspace version (all 41 crates inherit it; the site hero is
generated from it) and removes the false claims a stranger would hit
first: install.md advertised signed .tar.gz binaries, a working curl
one-liner, a Homebrew tap and Rust 1.85 — none of which were true — and
implementation.md still carried a magit audit its own next-day close-out
had superseded, plus two Phase-4 rows contradicted by its own tables.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

### Task L.4: The install script

Spec §5. `install.sh` resolves the latest release, picks the right archive, verifies it against `SHA256SUMS`, and lays down a relocatable prefix.

**Files:**
- Create: `install.sh` (repo root — the URL in the docs points at `main`)
- Modify: `README.md` (install section; full restructure is L.5)

**Interfaces:**
- Consumes: L.2's archive layout (`bin/lattice`, `share/lattice/plugins/`) and the release's `SHA256SUMS`.
- Produces: `install.sh` accepting `--prefix <dir>` (default `$HOME/.local`), `--version <tag>` (default: latest release), and `--gui` (install the GUI build instead of the TUI build).

- [ ] **Step 1: Write the script**

Create `install.sh`:

```sh
#!/bin/sh
# Lattice installer. Resolves a release archive for this platform, verifies it
# against the release's SHA256SUMS, and installs a relocatable prefix.
#
#   curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
#   ... | sh -s -- --prefix /usr/local --gui --version v0.9.0
#
# The archive layout matters: `bin/lattice` discovers its bundled plugins
# through `../share/lattice/plugins`, so both trees are installed together.
set -eu

REPO="dhruvasagar/lattice"
PREFIX="${LATTICE_PREFIX:-$HOME/.local}"
VERSION="${LATTICE_VERSION:-}"
FLAVOUR="lattice"

die() { printf 'install.sh: %s\n' "$1" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

while [ $# -gt 0 ]; do
	case "$1" in
		--prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; PREFIX="$2"; shift 2 ;;
		--version) [ $# -ge 2 ] || die "--version needs a tag"; VERSION="$2"; shift 2 ;;
		--gui) FLAVOUR="lattice-gui"; shift ;;
		-h|--help)
			cat <<'EOF'
usage: install.sh [--prefix DIR] [--version TAG] [--gui]

  --prefix DIR    install root (default: ~/.local)
  --version TAG   release tag, e.g. v0.9.0 (default: latest)
  --gui           install the GPU-rendered build instead of the terminal build
EOF
			exit 0 ;;
		*) die "unknown option: $1" ;;
	esac
done

have curl || die "curl is required"
have tar || die "tar is required"

case "$(uname -s)" in
	Darwin) os="macos" ;;
	Linux) os="linux" ;;
	*) die "unsupported OS $(uname -s) — see https://github.com/$REPO/releases for Windows archives" ;;
esac

case "$(uname -m)" in
	x86_64|amd64) arch="x86_64" ;;
	arm64|aarch64) arch="aarch64" ;;
	*) die "unsupported architecture $(uname -m)" ;;
esac

if [ -z "$VERSION" ]; then
	VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
		| sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
	[ -n "$VERSION" ] || die "could not resolve the latest release tag; pass --version"
fi

ver="${VERSION#v}"
archive="$FLAVOUR-$ver-$arch-$os.tar.xz"
base="https://github.com/$REPO/releases/download/$VERSION"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

printf 'Downloading %s (%s)…\n' "$archive" "$VERSION"
curl -fSL --progress-bar -o "$tmp/$archive" "$base/$archive" \
	|| die "no such archive: $base/$archive
The GUI build is best-effort on ARM Linux; try without --gui, or see
https://github.com/$REPO/releases"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "could not fetch SHA256SUMS"

printf 'Verifying checksum…\n'
if have sha256sum; then sum_cmd="sha256sum"
elif have shasum; then sum_cmd="shasum -a 256"
else die "neither sha256sum nor shasum is available"
fi
want="$(grep " $archive\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
[ -n "$want" ] || die "$archive is not listed in SHA256SUMS"
got="$(cd "$tmp" && $sum_cmd "$archive" | awk '{print $1}')"
[ "$want" = "$got" ] || die "checksum mismatch for $archive
  expected $want
  got      $got"

printf 'Installing to %s…\n' "$PREFIX"
tar xJf "$tmp/$archive" -C "$tmp"
root="$tmp/$FLAVOUR-$ver-$arch-$os"
[ -x "$root/bin/lattice" ] || die "archive has no bin/lattice — layout changed?"
[ -d "$root/share/lattice/plugins" ] || die "archive carries no bundled plugins — refusing to install a crippled editor"

mkdir -p "$PREFIX/bin" "$PREFIX/share/lattice"
rm -rf "$PREFIX/share/lattice/plugins"
cp -R "$root/share/lattice/plugins" "$PREFIX/share/lattice/plugins"
cp "$root/bin/lattice" "$PREFIX/bin/lattice"
chmod +x "$PREFIX/bin/lattice"

printf '\nInstalled lattice %s to %s/bin/lattice\n' "$ver" "$PREFIX"
case ":$PATH:" in
	*":$PREFIX/bin:"*) ;;
	*) printf '\n%s/bin is not on your PATH. Add it:\n    export PATH="%s/bin:$PATH"\n' "$PREFIX" "$PREFIX" ;;
esac
printf '\nNext: run `lattice` and press <CR> on "Tutor", or `lattice --scaffold-init` to start a config.\n'
printf 'Confirm the bundled plugins loaded with `:plugins` — three rows marked `bundled`.\n'
```

Note the deliberate refusal: if the archive carries no `share/lattice/plugins`, the script **fails** rather than installing a binary with no plugins. That is the spec §3 failure mode, and it must be loud here even though it is silent in the editor.

- [ ] **Step 2: Make it executable and lint it**

```bash
chmod +x install.sh
shellcheck install.sh || true
sh -n install.sh && echo "syntax OK"
```

Expected: `syntax OK`. Address any shellcheck error (not style warnings); if `shellcheck` is absent, `brew install shellcheck`.

- [ ] **Step 3: Test argument handling without touching the network**

```bash
./install.sh --help
./install.sh --bogus 2>&1 | head -2
./install.sh --prefix 2>&1 | head -2
```

Expected: the usage block; `install.sh: unknown option: --bogus`; `install.sh: --prefix needs a directory`. Each non-help case exits non-zero.

- [ ] **Step 4: Test the install path against the L.2 preview bundle**

There is no release yet, so the script's network path cannot run end to end — it resolves a tag from the GitHub releases API, and there are no releases. Prove the **install block** (the part that is script-specific and easy to get wrong) against the L.2 preview bundle by performing exactly what the script performs:

```bash
rm -rf /tmp/fake-prefix && mkdir -p /tmp/fake-prefix
cd /tmp/lattice-preview
tar xJf lattice-dev-*-aarch64-macos.tar.xz
root="$(ls -d lattice-dev-*-aarch64-macos)"
mkdir -p /tmp/fake-prefix/bin /tmp/fake-prefix/share/lattice
cp -R "$root/share/lattice/plugins" /tmp/fake-prefix/share/lattice/plugins
cp "$root/bin/lattice" /tmp/fake-prefix/bin/lattice
/tmp/fake-prefix/bin/lattice --version
```

Expected: a version line, and `:plugins` inside that binary shows three `bundled` rows. This mirrors exactly what the script's install block does; the script's own network path is proven for real in L.8 Step 7 against the actual release.

- [ ] **Step 5: Point the README at it**

Replace the README's build-from-source-first opening of the Quick start section with the install script as the first option, keeping the from-source instructions below it (and adding the `cargo xtask build-core-plugins` step, which the current README already has). The full restructure is L.5 — this step only ensures the README is not wrong in the interim.

- [ ] **Step 6: Commit**

```bash
git add install.sh README.md
git commit -m "feat(dist): add install.sh

Resolves the latest release archive for the platform, verifies it against
the release SHA256SUMS, and installs bin/ + share/lattice/plugins/ as a
relocatable prefix. Refuses to install an archive that carries no bundled
plugins — silent in the editor, loud here.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

### Task L.4b: Positioning, the re-cut shot list, and the capture harness

Spec §10. `docs/media/screenshot-ideas.md` has two problems, and the second is worse than the first.

**It is mis-organised.** It lists 13 screenshots and 8 screencasts by feature, and contains **none** of the four differentiators the launch leads with: magit, everything-is-a-buffer, config-as-Rust-WASM (programmable, not just keybindings), and org-mode + agents-as-buffers.

**It is stale.** It was written long before the features it omits existed, and nothing updated it as they landed. This is the *third* stale-doc instance found preparing 0.9, after `focused-surface.md` §7 (L.0) and the magit audit block in `implementation.md` (L.3) — so the slice does not re-cut the old list from memory. It rebuilds the inventory from the sources that cannot drift: `docs/user/`, which has one topic page per shipped feature and is enforced against `site/data/nav.toml` by a build that fails when they disagree.

A feature-organised gallery shows Lattice is real; a differentiator-organised one shows why it exists.

This slice also makes TUI demos **regenerable**: declarative VHS `.tape` files committed to the repo and rendered per release, so an asset cannot silently rot when the UI moves. VHS drives a terminal, so GPUI shots stay manual.

**Files:**
- Rewrite: `docs/media/screenshot-ideas.md`
- Create: `docs/media/tapes/{magit,buffers,config,org-agents}.tape`
- Create: `docs/media/README.md` (how to regenerate, and the demo-fixture convention)
- Create: `assets/media/demos/` (rendered GIFs, committed)

**Interfaces:**
- Consumes: nothing from earlier slices.
- Produces: `assets/media/demos/<name>.gif` and `assets/media/screenshots/<name>.png` paths that L.5 references from the README and the site, and the differentiator ordering L.5's gallery follows.

- [ ] **Step 1: Install VHS and confirm it runs**

```bash
brew install vhs
vhs --version
```

Expected: a version line. VHS renders a terminal session from a `.tape` file to GIF/MP4 with no screen recorder and no manual timing.

- [ ] **Step 2: Confirm the real ex-command names before writing any tape**

Do not invent commands. `magit-status` and `search` are confirmed to exist (`crates/lattice-magit/src/lib.rs:948`, `crates/lattice-host/src/dispatch.rs`). Find the rest:

```bash
grep -rhoE '"(org-agenda|org-capture)[a-z-]*"' /Users/dhruva/src/dhruvasagar/lattice-org-plugin/src/*.rs | sort -u
grep -rhoE 'register(_ex)?_command\w*\(\s*"[a-z][a-z0-9-]*"' crates/lattice-agent/src/*.rs | sort -u
grep -rn "reload-config\|scaffold-init" crates/lattice-cli/src/*.rs crates/lattice-host/src/*.rs | head -5
```

Report every command name you will type in a tape and where you confirmed it. If a command you need does not exist, say so and leave that tape out rather than typing a command that will error on screen — a demo showing an unknown-command message is worse than no demo.

- [ ] **Step 3: Rebuild the feature inventory from source, not from the old list**

The old list predates a great deal of what shipped. Derive the current set from the authoritative places rather than trusting it:

```bash
# One topic page per shipped user-facing feature (~147 of them).
ls docs/user/*.md | sed 's|docs/user/||; s|\.md$||' | sort > /tmp/topics.txt
wc -l /tmp/topics.txt

# What the old shot list already covers.
grep -oiE '(magit|org|agent|lsp|picker|diff|terminal|multibuffer|narrow|fold|snippet|theme|tutor|plugin|dashboard|which-key|surround|table|repl|media|compilation|blame|rebase)' docs/media/screenshot-ideas.md | tr 'A-Z' 'a-z' | sort -u > /tmp/covered.txt

# Subsystem crates — each is a feature area that may deserve a shot.
ls crates | sed 's/^lattice-//' | sort
```

Read `/tmp/topics.txt` against `/tmp/covered.txt` and list, in your report, every substantial shipped feature the old doc does not mention. Expect a long list — magit alone has ~22 topic pages, and org, agents, narrowing, folding, multibuffer, which-key, surround, table-mode, snippets, compilation, REPL and media are all likely absent.

Then group what you found into: **priority** (a differentiator from spec §10), **second tier** (a strong feature worth a page shot), and **skip** (real but not visually distinctive — an option, a keybinding nicety). Judgement call: a shot has to show something a still image can carry.

- [ ] **Step 4: Re-cut the shot list**

Rewrite `docs/media/screenshot-ideas.md`. Keep the Technical Notes and Screencast sections largely as they are (they are good), but replace the flat screenshot list with a table whose every row names the differentiator **and the editors it differentiates against**, ordered by launch priority:

```markdown
## Priority shots — the differentiators

These are the shots the README gallery and the site lead with. Each exists
to answer "why this and not Zed / Helix / Neovim / VS Code", not "what
features does it have". See `../dev/architecture/launch-0.9.md` §10.

| # | Shot | Shows | Absent from | File |
|---|---|---|---|---|
| 1 | Magit status with staged + unstaged hunks and a transient popup open | a real magit port inside a modal editor | Zed, Helix, Neovim (fugitive is not magit) | `assets/media/screenshots/magit.png` |
| 2 | Four-way split: file tree, code, terminal, search results — all real buffers | everything is a buffer; the same grammar works in all of them | all of them; the others have panels | `assets/media/screenshots/buffer-splits.png` |
| 3 | `init.rs` beside the editor, defining a custom command and a hook, then `:reload-config` applying it live | config is Rust compiled to WASM, and it is programmable — not a settings file | Zed (JSON), Helix (TOML), Neovim (Lua), VS Code (JSON+TS) | `assets/media/screenshots/config-init-rs.png` |
| 4 | Org agenda beside a coding-agent buffer under interactive diff review | org-mode and agents-as-editable-buffers, in one editor | everything outside Emacs; Zed's agent is not a buffer | `assets/media/screenshots/org-and-agents.png` |
| 5 | The same file in the TUI and the GPU window, side by side | one core, two first-class renderers | Zed (no TUI), Helix (no GPU) | `assets/media/screenshots/two-renderers.png` |

Shot 3 must show something genuinely programmatic — a custom command or a
hook — not a keybinding one-liner. A remapped key looks like every other
editor's config; a compiled function does not.

## Supporting shots

[the second tier: the existing hero / LSP / picker / diff / help / tutor /
 theme entries, PLUS everything Step 3's inventory found missing that you
 graded second tier — magit sub-views, org capture/clocking, narrowing,
 folding, multibuffer, which-key, surround, table mode, snippets,
 compilation, REPL, media. Used on feature pages, not the landing gallery.]
```

- [ ] **Step 5: Write the demo-media README**

Create `docs/media/README.md`:

```markdown
# Demo media

Screenshots and demo GIFs for the README and the website.

## Regenerating the TUI demos

The `.gif` files under `assets/media/demos/` are **generated**, not
recorded. Each has a declarative `.tape` beside it in `tapes/`:

```sh
brew install vhs
vhs docs/media/tapes/magit.tape     # writes assets/media/demos/magit.gif
```

Re-render them whenever the UI they show changes. A recorded video would
silently go stale; a tape fails loudly or shows the new UI.

## The demo fixture

Tapes run against **this repository**. It is a real Rust project with real
git history, which is what makes the magit demo honest — staged hunks from
an actual working tree rather than a contrived fixture. Before rendering,
ensure the tree is clean and no editor is already running.

## GPUI shots

VHS drives a terminal, so it cannot capture the GPU renderer. Those shots
are captured by hand — see `screenshot-ideas.md` Technical Notes for the
resolution, theme and font conventions.
```

- [ ] **Step 6: Write the magit tape**

Create `docs/media/tapes/magit.tape`. Fill the command names from Step 2; `magit-status` is confirmed.

```tape
# Regenerate: vhs docs/media/tapes/magit.tape
# Shows: a real magit port inside a modal editor (differentiator 1).
Output assets/media/demos/magit.gif

Require lattice

Set Shell "bash"
Set FontSize 16
Set Width 1400
Set Height 800
Set Padding 20
Set TypingSpeed 60ms

Hide
Type "cd /Users/dhruva/src/dhruvasagar/lattice && clear"
Enter
Show

Sleep 500ms
Type "lattice"
Enter
Sleep 3s

Type ":magit-status"
Enter
Sleep 2500ms

# Walk to a hunk and stage it — the thing no other modal editor does.
Type "jjj"
Sleep 1s
Type "s"
Sleep 2s

Escape
Sleep 500ms
Type ":q!"
Enter
Sleep 1s
```

- [ ] **Step 7: Write the remaining three tapes**

`buffers.tape`, `config.tape` and `org-agents.tape`, following the same header shape (Output/Require/Set block identical; only the body differs). Each body should be under ~20 seconds of screen time — a landing-page GIF that runs longer than that does not get watched.

- `buffers.tape` — open the file tree, open a file from it, `:search` for a symbol, jump to a result, open a terminal buffer in a split, then `:ls` to show every one of them is a listed buffer.
- `config.tape` — show `init.rs` defining a custom command, then `:reload-config`, then invoke that command. This is the shot that must be programmatic, not a keybinding.
- `org-agents.tape` — org agenda, then an agent buffer with a diff under review. Leave this tape out and say so if Step 2 could not confirm the command names.

- [ ] **Step 8: Render and inspect**

```bash
mkdir -p assets/media/demos
for t in docs/media/tapes/*.tape; do echo "--- $t"; vhs "$t" || echo "FAILED: $t"; done
ls -lh assets/media/demos/
```

Expected: one GIF per tape, each **under 4 MB** (they ship in every clone and load on the landing page). If one is larger, shorten the tape or drop `Set Width`/`Height` — do not commit a 20 MB GIF.

Then watch each GIF. A tape that ran without erroring can still show a blank editor, an error message, or a popup that never opened. Confirm each one actually shows the thing its comment claims, and say so per tape in your report.

- [ ] **Step 9: Commit**

```bash
git add docs/media/screenshot-ideas.md docs/media/README.md docs/media/tapes assets/media/demos
git commit -m "docs(media): re-cut the shot list around differentiators, add VHS tapes

The shot list was organised by feature and contained none of the four
things that separate lattice from the editors it is compared to: magit,
everything-is-a-buffer, programmable Rust-WASM config, and org plus
agents-as-buffers. Re-cut so every priority shot names both the
differentiator and the editors that lack it.

TUI demos are now declarative VHS tapes rendered per release rather than
one-off recordings, so a demo cannot silently rot when the UI moves.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

### Task L.5: README restructure and a hero screenshot

The README is 653 lines opening on a 47-line internal phase ledger ("Phase 4 wind-down ~90%", "Phase 5.8 GPUI feature-parity"), and there is not one screenshot anywhere in the repo — only the SVG wordmark. It reads as a changelog for people who already know what Lattice is.

**Files:**
- Rewrite: `README.md`
- Create: `assets/media/screenshots/hero-dark.png`
- Modify: `site/templates/index.html` (hero image)
- Modify: `docs/media/screenshot-ideas.md` (mark the hero captured)
- Move into: `docs/dev/operations/implementation.md` (the phase ledger and feature checklist)

**Interfaces:**
- Consumes: L.3's version truth and L.4's `install.sh`.
- Produces: a README under ~200 lines whose first screen is pitch → screenshot → install.

- [ ] **Step 1: Capture the hero screenshot**

`docs/media/screenshot-ideas.md` already specifies the shot. Capture at a 16:10 ratio, dark theme, a Rust file open with syntax highlighting, the gutter and modeline visible, and a real project in the file tree — no lorem, no empty buffer.

```bash
mkdir -p assets/media/screenshots
# Capture with the OS tool, save to assets/media/screenshots/hero-dark.png, then:
file assets/media/screenshots/hero-dark.png
du -h assets/media/screenshots/hero-dark.png
```

Expected: a PNG, ideally under 400 KB (it ships in every clone). If it is larger, downscale to 2000px wide rather than committing a 2 MB file.

- [ ] **Step 2: Move the contributor-facing bulk out of the README**

The Status block (README:10-56), the crate map, and the 136-line feature checklist (README:426-562) are contributor content. `docs/dev/operations/implementation.md` already owns the phase ledger — append the feature checklist to it under a `## Feature checklist (moved from README, 2026-09-18)` heading rather than deleting it.

- [ ] **Step 3: Rewrite the README**

Target structure, in order — first screen is pitch, image, install:

```markdown
<p align="center">
	<img src="./assets/readme-banner.svg" alt="Lattice" width="720" />
</p>

<p align="center">
	A modal, GPU-accelerated, plugin-first text editor written in Rust.
</p>

<p align="center">
	<img src="./assets/media/screenshots/hero-dark.png" alt="Lattice editing a Rust file" width="900" />
</p>

> **0.9 — alpha.** The editor is usable; the distribution is new. Expect
> rough edges in install and first-run rather than in editing. Please file
> what you hit: [issues](https://github.com/dhruvasagar/lattice/issues).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
```

Or grab an archive from [releases](https://github.com/dhruvasagar/lattice/releases).
Full instructions, including from source: [install guide](https://dhruvasagar.github.io/lattice/install/).

## What works today

| | |
|---|---|
| **Modal editing** | Vim grammar — operators, motions, text objects, registers, counts, macros, folds, marks |
| **Code intelligence** | LSP: completion, diagnostics, hover, rename, references, inlay hints, symbols, code actions |
| **Syntax** | Tree-sitter, 19 languages, incremental and O(viewport) |
| **Git** | A magit port — status, stage/unstage by hunk, commit, rebase, blame, log, branches, stashes |
| **Extensibility** | WASM Component Model plugin host; config is Rust compiled to WASM |
| **Two renderers** | Terminal (first-class, for SSH) and GPU (`--gui`) |
| **AI agents** | Claude Code over MCP, and opencode over ACP, both as buffers |

## Rough edges at 0.9

Unsigned binaries (macOS quarantines browser downloads); LSP servers must be
installed by hand; syntax colours are not yet fully themeable; ARM Linux and
ARM Windows GUI builds are best-effort; `--gui` is opt-in, not the default.
The full list is [known limitations](https://dhruvasagar.github.io/lattice/docs/known-limitations/).

## Why

[Four paramount goals, ~120 words — keep the existing prose, trimmed]

## Documentation

[Keep the existing documentation table]

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md).

## License

MIT — see [LICENSE](./LICENSE).
```

Keep the "Why another editor" and paramount-goals prose (it is good and it is the pitch); drop "Versus Zed", the crate map, the roadmap and the checklist, each of which is a link away in `docs/dev/`.

- [ ] **Step 4: Check the length and the links**

```bash
wc -l README.md
grep -oE '\]\(\./[^)]+\)' README.md | tr -d '](.)' | while read -r p; do
	[ -e "$p" ] || echo "BROKEN: $p"
done; echo "link check done"
```

Expected: under 200 lines, and no `BROKEN` lines. (`CONTRIBUTING.md` will be broken until L.7 — that is the one acceptable miss, and L.7 closes it.)

- [ ] **Step 5: Put the screenshot in the site hero**

In `site/templates/index.html`, add the screenshot below the hero's Install CTA, referencing it from `static/`:

```bash
mkdir -p site/static/media
cp assets/media/screenshots/hero-dark.png site/static/media/hero-dark.png
```

```html
<img class="hero-shot" src="{{ get_url(path='media/hero-dark.png') }}"
     alt="Lattice editing a Rust file" loading="lazy" width="1200" />
```

Add a `.hero-shot` rule to the relevant file in `site/sass/` with `max-width: 100%; height: auto; border-radius: 8px;`.

- [ ] **Step 6: Record the capture**

In `docs/media/screenshot-ideas.md`, mark the hero shot captured, with its committed path.

- [ ] **Step 7: Commit**

```bash
git add README.md assets/media/screenshots/hero-dark.png site/static/media/hero-dark.png \
        site/templates/index.html site/sass docs/media/screenshot-ideas.md \
        docs/dev/operations/implementation.md
git commit -m "docs(readme): lead with the pitch, a screenshot, and install

The README opened on a 47-line internal phase ledger and carried a
136-line feature checklist, with no screenshot anywhere in the repo — a
changelog for people who already knew what this was. Contributor content
moves to the implementation ledger that already owns it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

### Task L.6: Known limitations, troubleshooting, cheatsheet, dashboard hints

Three new user docs and two dashboard rows. Note the four-part contract for every new `docs/user/` page (Global Constraints) — frontmatter, index row, nav entry, size budget.

**Files:**
- Create: `docs/user/known-limitations.md`, `docs/user/troubleshooting.md`, `docs/user/cheatsheet.md`
- Delete: `site/content/cheatsheet.md` (its content moves into the synced doc)
- Modify: `docs/user/README.md` (three Topics rows), `site/data/nav.toml` (three entries), `site/content/faq.md` (the chat question)
- Modify: `crates/lattice-dashboard/src/sections.rs` (+ its tests)

**Interfaces:**
- Consumes: L.3's install page, which links `./docs/known-limitations/`.
- Produces: `:help known-limitations`, `:help troubleshooting`, `:help cheatsheet`, and two new dashboard rows.

- [ ] **Step 1: Write `docs/user/known-limitations.md`**

```markdown
---
summary: "What doesn't work yet at 0.9, and what is deliberately absent."
---

# Known limitations

Lattice 0.9 is an alpha. This page is the honest list — check it before
filing an issue, and file anyway if your case is worse than what's written
here.

## Distribution

- **Binaries are unsigned.** macOS quarantines browser downloads; clear it
  with `xattr -dr com.apple.quarantine <dir>`. No `.dmg`, no `.msi`, no
  notarisation — those need a paid certificate.
- **No Homebrew, no `cargo install`.** Use the install script or a release
  archive. See [installation](https://dhruvasagar.github.io/lattice/install/).
- **ARM Linux and ARM Windows GUI builds are best-effort** and may be missing
  from a release. The terminal build is available on every platform.
- **Windows executables have no embedded icon.**

## Editor

- **`--gui` is opt-in.** The terminal renderer is the default and is a
  first-class peer, not a fallback. The GPU renderer is not yet at parity.
- **Syntax colours are not fully themeable.** UI theming works; several
  syntax-style consumers still read a hardcoded palette.
- **LSP servers must be installed by hand.** There is no server manager or
  installer; point lattice at servers already on your `PATH`.
- **No crash reporter**, and no accessibility work has been done yet.

## Grammar and commands not yet implemented

- `!` (filter through an external command) and `gq` (format motion).
- The `'<` / `'>` visual marks.
- Ex ranges are partially implemented.
- Command line: `<C-b>` / `<C-e>` cursor movement, `<C-r>` register paste,
  and completion inside `:s/…/…/`.
- `:customize`, `:autocmd` / `:add-hook`, `:describe-event`,
  `:describe-mode`, `:history-*`.
- Terminal buffers: mouse passthrough, and word motions in Terminal Visual.

## Deliberately absent

- **No vimscript, no Lua, no elisp.** WASM is the single extension
  substrate; your config is Rust compiled to WASM
  ([init](help:init)). This is a design decision, not a gap.
- **No vim/emacs config compatibility.** Explicit non-goal.
- **Rich inline media** (images, embedded widgets) is post-1.0.

## Reporting

[Open an issue](https://github.com/dhruvasagar/lattice/issues) with your
platform, `lattice --version`, and whether `:plugins` lists the three bundled
plugins. If it doesn't, say so — that is its own bug and it hides others.
```

- [ ] **Step 2: Write `docs/user/troubleshooting.md`**

```markdown
---
summary: "When something doesn't work: plugins, LSP, colours, logs, and first-run problems."
related: [plugins, lsp-status, messages]
---

# Troubleshooting

Start with [known limitations](help:known-limitations) — if it's listed
there, it isn't broken, it's absent.

## The bundled plugins aren't working

Symptom: typing `(` doesn't insert a closing paren, or there's no
tree-sitter context header.

```
:plugins
```

`auto-pair`, `treesitter-context` and `project` should each be listed with
SOURCE `bundled`. If the list is empty, the editor found no plugin
directory. Lattice looks in this order:

1. `$LATTICE_RUNTIME/plugins`
2. `<install-prefix>/share/lattice/plugins`
3. `<directory-of-the-binary>/../share/lattice/plugins`
4. `<workspace>/runtime/plugins` (when running from `target/`)

The usual causes are moving `bin/lattice` out of its extracted archive
(which orphans it from `../share/lattice/plugins`), or building from source
without running `cargo xtask build-core-plugins`. An absent plugin directory
is treated as "no plugins installed" and does not raise an error, which is
why this fails silently.

## An LSP server won't start

```
:lsp-status
```

Lattice does not install servers — the binary must already be on your
`PATH`. Check the server's own log:

```
:lsp-log
```

and the editor's messages buffer:

```
:messages
```

For a deeper trace, start with `--log-level debug`. Per-keystroke and
per-frame diagnostics are debug-level by design, so `info` stays readable.

## Colours look wrong

If the UI is themed but code is monochrome, the buffer's language may have
no tree-sitter grammar — check `:set filetype?`. If glyphs in the file tree
are boxes, your font isn't a Nerd Font: leave `ui.nerd_fonts` off and the
BMP fallback palette is used instead. Both palettes are the same cell width,
so nothing shifts.

## A key does nothing

See [troubleshooting keys](help:troubleshooting-keys), and:

```
:describe-key
```

Then press the chord. Note that a bound prefix consumes its longer chords —
if `gD` is bound, `gDd` can never fire.

## Where the logs are

`:messages` is the in-editor log. For a file, redirect stderr:

```sh
lattice --log-level debug 2>/tmp/lattice.log
```

Never use `println!`/`eprintln!` while the terminal UI is up — it corrupts
the alternate screen.

## Filing a bug

Include your platform, `lattice --version`, whether `:plugins` shows three
`bundled` rows, and the relevant `:messages` output.
[Open an issue](https://github.com/dhruvasagar/lattice/issues).
```

- [ ] **Step 3: Move the cheatsheet into the doc set**

```bash
git mv site/content/cheatsheet.md docs/user/cheatsheet.md
```

Replace its Zola `+++` frontmatter with the doc-set YAML form, keeping the body:

```markdown
---
summary: "One-page keystroke reference: modes, motions, operators, git, LSP."
related: [modal-editing, ex-commands]
---

# Cheatsheet
```

Then repoint inbound links:

```bash
grep -rn "/cheatsheet" site/templates/ site/content/ docs/ README.md | grep -v "docs/user/cheatsheet"
```

Every hit must become `/docs/cheatsheet/` (the synced URL) or `help:cheatsheet` inside a user doc.

- [ ] **Step 4: Register all three pages**

Add a row per page to the Topics table in `docs/user/README.md`, matching the existing format:

```markdown
| Known limitations          | [`known-limitations`](help:known-limitations) | ✅      |
| Troubleshooting            | [`troubleshooting`](help:troubleshooting)     | ✅      |
| Cheatsheet                 | [`cheatsheet`](help:cheatsheet)               | ✅      |
```

Then add each topic to the right group in `site/data/nav.toml` — `cheatsheet` and `troubleshooting` under the getting-started section, `known-limitations` alongside them.

- [ ] **Step 5: Verify the sync contract and the size budget**

```bash
python3 site/scripts/sync-docs.sh 2>&1 | tail -20
CARGO_BUILD_JOBS=3 nice cargo test -p lattice-help -- --test-threads=1 embedded
```

Expected: no `ERROR` from the sync (a missing nav entry names the file), and both `embedded_user_docs_stay_under_size_budget` and `embedded_bodies_are_actually_compressed` pass. If the budget test fails, the fix is **not** a bigger number — trim the new pages.

- [ ] **Step 6: Answer the FAQ's chat question honestly**

In `site/content/faq.md`, make the "is there a chat or forum" answer read:

```markdown
[GitHub Discussions](https://github.com/dhruvasagar/lattice/discussions) and
[issues](https://github.com/dhruvasagar/lattice/issues). There's no Discord or
Matrix room at 0.9 — an empty chat room is worse than none, and discussions
keep answers searchable.
```

- [ ] **Step 7: Write the failing test for the dashboard rows**

In `crates/lattice-dashboard/src/sections.rs`, read the existing tests for the `Links` and `Tutor` sections and copy their shape. Add:

```rust
    #[test]
    fn the_dashboard_tells_a_first_time_user_how_to_leave_and_how_to_configure() {
        // A brand-new user's two dead ends: not knowing how to quit, and not
        // knowing a config exists. Both are one row each.
        let rendered = render_all_sections_for_test();
        assert!(
            rendered.contains(":q"),
            "the dashboard must say how to quit"
        );
        assert!(
            rendered.contains("--scaffold-init"),
            "the dashboard must point at config scaffolding"
        );
    }
```

Replace `render_all_sections_for_test()` with whatever helper the neighbouring tests use — read one first and match it exactly.

- [ ] **Step 8: Run it and watch it fail**

```bash
CARGO_BUILD_JOBS=3 nice cargo test -p lattice-dashboard -- --test-threads=1 first_time_user
```

Expected: FAIL on the first assertion.

- [ ] **Step 9: Add the rows**

Extend the `Links` section (or add a small `Survival` section beside it, following the `DashboardSection` pattern at `sections.rs:97-127`) with two rows:

- `:q` — quit (and `:` opens the command line)
- `lattice --scaffold-init` — start a config

Follow the existing row-building helpers; do not hand-format strings that the section helpers already align.

- [ ] **Step 10: Verify and gate**

```bash
CARGO_BUILD_JOBS=3 nice cargo test -p lattice-dashboard -- --test-threads=1 first_time_user
scripts/precommit.sh lattice-dashboard lattice-help
```

Expected: PASS, then a clean gate.

- [ ] **Step 11: Commit**

```bash
git add docs/user/known-limitations.md docs/user/troubleshooting.md docs/user/cheatsheet.md \
        docs/user/README.md site/data/nav.toml site/content/faq.md site/templates \
        crates/lattice-dashboard
git commit -m "docs(user): known limitations, troubleshooting, and an in-editor cheatsheet

Three pages a stranger needs and none of which existed: the honest
absent-features list (so issues aren't filed against known gaps), a
general troubleshooting page (the only one was about keys), and the
cheatsheet, which lived on the website but not in \`:help\`. Plus the two
dashboard rows a first-time user actually needs: how to quit, and that a
config exists.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

### Task L.7: Repo civics

The repo has no `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `CHANGELOG.md` or issue templates, and issues are enabled on a public repo about to be announced.

**Files:**
- Create: `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `CHANGELOG.md`
- Create: `.github/ISSUE_TEMPLATE/bug_report.yml`, `.github/ISSUE_TEMPLATE/feature_request.yml`, `.github/ISSUE_TEMPLATE/config.yml`

**Interfaces:**
- Consumes: L.5's README, which links `./CONTRIBUTING.md`.
- Produces: `CHANGELOG.md` with a `## 0.9.0` section — L.8's release notes are written from it.

- [ ] **Step 1: Write `CONTRIBUTING.md`**

Move the README's existing Contributing content (good-first-issues, dev workflow) into it, and add what a stranger actually needs:

```markdown
# Contributing to Lattice

Lattice is 0.9 — alpha. The most useful contribution right now is a bug
report from an install that went wrong.

## Setup

```sh
git clone https://github.com/dhruvasagar/lattice
cd lattice
rustup target add wasm32-wasip2     # for the bundled plugins
cargo build
cargo xtask build-core-plugins      # NOT optional — see below
cargo run
```

Without `cargo xtask build-core-plugins`, the editor starts with no bundled
plugins and says nothing about it: an absent plugin directory is
indistinguishable from an empty one.

The GPU renderer is behind a cargo feature and is **not** in a plain build:

```sh
cargo run --features gui -- --gui
```

A plain `cargo build -p lattice-cli` does not compile `lattice-ui-gpui` at
all, so edits there appear to succeed while never being built.

## Before you push

```sh
scripts/precommit.sh <crate-you-touched>...
```

That runs three gates: `cargo fmt --check` (strict), zero new rustc warnings
in code you touched, and the targeted tests. `unwrap_used` / `panic` / `todo`
warnings are `warn` on purpose and overwhelmingly test code — don't try to
zero them. The blocking lints are `unsafe_code` and `unused_must_use`.

Whole-workspace test runs are slow (`lattice-host` alone is ~1670 tests).
Run the tests your change touches; leave the full sweep to CI.

## Commits

`<type>: <description>` — `feat`, `fix`, `refactor`, `docs`, `test`,
`chore`, `perf`, `ci`. One logical change per commit.

## Design

Read [the design spec](./docs/dev/architecture/design.md) before proposing
architecture. Four paramount goals govern, in order: performance,
extensibility, vim modal editing, asynchronicity — with user experience
above all four. A change that ships only code is incomplete: the doc, the
test, and the benchmark are part of the deliverable.

## Docs

User docs live in `docs/user/` and are embedded in the binary as `:help`
topics. A new page needs `summary:` frontmatter, a row in
`docs/user/README.md`, and an entry in `site/data/nav.toml` — the site build
fails if the last two disagree with the directory.
```

- [ ] **Step 2: Write `SECURITY.md`**

```markdown
# Security Policy

## Supported versions

Lattice is pre-1.0. Only the latest release receives fixes.

## Reporting a vulnerability

Report privately through
[GitHub Security Advisories](https://github.com/dhruvasagar/lattice/security/advisories/new).
Please don't open a public issue for a vulnerability.

Expect an acknowledgement within a week. As a one-maintainer alpha project
there is no formal SLA beyond that.

## Scope worth noting

Lattice runs plugins as WebAssembly components, capability-gated and
fuel-limited, each in its own store. A sandbox escape, a capability that
grants more than it declares, or a plugin reading outside its granted paths
is in scope and interesting. So is anything in the config path: a user's
`init.rs` is compiled to WASM and loaded with boot capabilities.

Binaries are unsigned at 0.9 — that is a known gap, documented in
[known limitations](./docs/user/known-limitations.md), not a vulnerability
report.
```

- [ ] **Step 3: Add `CODE_OF_CONDUCT.md`**

Use Contributor Covenant 2.1 verbatim, with `dhruva.sagar@gmail.com` as the contact.

```bash
curl -fsSL -o CODE_OF_CONDUCT.md \
  https://raw.githubusercontent.com/EthicalSource/contributor_covenant/release/content/version/2/1/code_of_conduct.md
grep -n "\[INSERT CONTACT METHOD\]" CODE_OF_CONDUCT.md
```

Replace the placeholder with the contact address and verify none remain.

- [ ] **Step 4: Write the bug report template**

`.github/ISSUE_TEMPLATE/bug_report.yml`:

```yaml
name: Bug report
description: Something behaved wrongly
labels: [bug]
body:
  - type: markdown
    attributes:
      value: |
        Please check [known limitations](https://dhruvasagar.github.io/lattice/docs/known-limitations/)
        first — if it's listed there it's absent rather than broken.
  - type: textarea
    id: what
    attributes:
      label: What happened
      description: What you did, what you expected, what you got.
    validations:
      required: true
  - type: input
    id: version
    attributes:
      label: Version
      description: Output of `lattice --version`
      placeholder: lattice 0.9.0
    validations:
      required: true
  - type: dropdown
    id: platform
    attributes:
      label: Platform
      options:
        - macOS (Apple silicon)
        - macOS (Intel)
        - Linux x86_64
        - Linux aarch64
        - Windows x86_64
        - Windows aarch64
    validations:
      required: true
  - type: dropdown
    id: renderer
    attributes:
      label: Renderer
      options:
        - Terminal (default)
        - GPU (--gui)
    validations:
      required: true
  - type: dropdown
    id: plugins
    attributes:
      label: Does `:plugins` list auto-pair, treesitter-context and project as `bundled`?
      description: If not, that is its own bug and it hides others.
      options:
        - "Yes, all three"
        - "No, the list is empty or incomplete"
        - "Haven't checked"
    validations:
      required: true
  - type: dropdown
    id: install
    attributes:
      label: How did you install it?
      options:
        - install.sh
        - Release archive
        - .deb or AppImage
        - Built from source
    validations:
      required: true
  - type: textarea
    id: messages
    attributes:
      label: Relevant `:messages` output
      render: text
```

- [ ] **Step 5: Write the feature and config templates**

`.github/ISSUE_TEMPLATE/feature_request.yml`:

```yaml
name: Feature request
description: Something missing you'd like to exist
labels: [enhancement]
body:
  - type: markdown
    attributes:
      value: |
        Lattice has four paramount goals in priority order — performance,
        extensibility, vim modal editing, asynchronicity — with user
        experience above all of them. A request that names the goal it
        serves is much easier to act on.
  - type: textarea
    id: problem
    attributes:
      label: What are you trying to do?
      description: The problem, not the solution.
    validations:
      required: true
  - type: textarea
    id: prior-art
    attributes:
      label: Prior art
      description: How vim, emacs, helix or zed handle this, if they do.
```

`.github/ISSUE_TEMPLATE/config.yml`:

```yaml
blank_issues_enabled: true
contact_links:
  - name: Question or idea
    url: https://github.com/dhruvasagar/lattice/discussions
    about: Discussions are the place for questions, ideas and show-and-tell.
  - name: Documentation
    url: https://dhruvasagar.github.io/lattice/
    about: The user guide, install instructions, and known limitations.
```

- [ ] **Step 6: Seed the changelog**

`CHANGELOG.md`:

```markdown
# Changelog

## 0.9.0 — 2026-09-18

The first installable release. Alpha: the editor is usable; the
distribution is new.

### Editing
- Vim modal grammar: operators, motions, text objects, registers, counts,
  macros recorded as command invocations, folds, marks, a unified position
  history (jump list + mark ring), surround, narrowing, soft wrap.
- Unified command dispatch — the `:` line, the palette, plugin
  contributions and the grammar all flow through one registry.

### Code intelligence
- LSP: completion, diagnostics, hover, rename, references, inlay hints,
  document symbols, code actions, signature help, semantic tokens,
  selection ranges, folding ranges.
- Tree-sitter highlighting for 19 languages, incremental and O(viewport).

### Git
- A magit port: status, hunk-level staging, commit, amend, rebase, blame,
  log, branches, stashes, submodules, notes, cherry-pick, with transients.
- A diff and merge subsystem — inline, side-by-side, three-way, `]c` / `[c`,
  `do` / `dp`.

### Extensibility
- A WebAssembly Component Model plugin host: capability-gated, fuel-limited,
  crash-isolated, one store per plugin instance.
- Configuration is Rust compiled to WASM (`lattice --scaffold-init`), with
  TOML for static option overrides.
- Three bundled plugins ship with every build: auto-pair,
  treesitter-context, project.

### Interface
- Two renderers: a first-class terminal peer and a GPU-rendered window
  (`--gui`, opt-in at 0.9).
- Everything is a buffer — file tree, diagnostics, search results, terminal,
  git views, help.
- Pickers, which-key, a dashboard, notifications, themes, a tutor
  (`:tutor`), and self-documenting help for every command, option, mode and
  key.
- Coding-agent integrations: Claude Code over MCP and opencode over ACP,
  both as buffers with interactive diff review.

### Distribution
- Release archives for macOS, Linux and Windows on x86_64 and aarch64, plus
  Linux `.AppImage` and `.deb`, checksums and build provenance.
- `install.sh` for macOS and Linux.

### Known limitations
See [known limitations](./docs/user/known-limitations.md). The short version:
binaries are unsigned, LSP servers must be installed by hand, syntax colours
are not fully themeable, and the GPU renderer is not yet at parity with the
terminal one.
```

- [ ] **Step 7: Verify the templates parse**

```bash
python3 - <<'EOF'
import glob, yaml
for f in glob.glob('.github/ISSUE_TEMPLATE/*.yml'):
    yaml.safe_load(open(f))
    print('ok', f)
EOF
grep -c "INSERT CONTACT METHOD" CODE_OF_CONDUCT.md || echo "no placeholders"
ls CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md CHANGELOG.md
```

Expected: three `ok` lines, `no placeholders`, four files listed.

- [ ] **Step 8: Re-run the README link check**

```bash
grep -oE '\]\(\./[^)]+\)' README.md | tr -d '](.)' | while read -r p; do
	[ -e "$p" ] || echo "BROKEN: $p"
done; echo "link check done"
```

Expected: no `BROKEN` lines — `CONTRIBUTING.md` now exists.

- [ ] **Step 9: Commit**

```bash
git add CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md CHANGELOG.md .github/ISSUE_TEMPLATE
git commit -m "docs: contributing, security, conduct, changelog, issue templates

Issues are enabled on a public repo about to be announced and there was
no template, no contributing guide and no security policy. The bug form
asks the three things that otherwise cost a round trip each: platform,
version, and whether the bundled plugins loaded.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

### Task L.8: Tag, publish, announce

**Files:**
- Modify: `docs/dev/operations/releasing.md` (known gaps now that the pipeline has run)
- Modify: `docs/dev/operations/slice-plans/launch-0.9.md` (this file — final statuses)

**Interfaces:**
- Consumes: everything above. The tag must equal `[workspace.package] version` from L.3 exactly.
- Produces: the `v0.9.0` GitHub Release.

- [ ] **Step 1: Confirm the tree is release-ready**

```bash
git status --short
grep -m1 -E '^version[[:space:]]*=' Cargo.toml
python3 site/scripts/sync-docs.sh 2>&1 | tail -5
```

Expected: clean tree, `version = "0.9.0"`, no sync `ERROR`.

- [ ] **Step 2: Run the full gate**

Ask Dhruva first, and run it when the machine is free — this is the whole workspace.

```bash
scripts/precommit.sh
```

Expected: fmt clean, no new warnings, all tests pass. A failure here stops the tag. Re-run any single failure alone before believing it.

- [ ] **Step 3: Merge to main**

```bash
gh pr create --fill --base main --head "$(git rev-parse --abbrev-ref HEAD)"
```

Wait for CI, then merge. `deploy-docs.yml` publishes the site from `main`, so the install page and the three new docs go live at this point.

- [ ] **Step 4: Verify the deployed site**

```bash
gh run list --workflow=deploy-docs.yml --limit=1
open https://dhruvasagar.github.io/lattice/install/
```

Check: the hero shows `v0.9.0` and the screenshot, the install page's script block is correct, and `/docs/known-limitations/` and `/docs/troubleshooting/` resolve.

- [ ] **Step 5: Tag and push**

```bash
git checkout main && git pull
git tag v0.9.0
git push origin v0.9.0
```

- [ ] **Step 6: Watch the release run**

```bash
rid="$(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')"
gh run watch "$rid" --exit-status
```

Expected: `prepare` confirms the tag matches `0.9.0`, `core-plugins` green, six `dist` legs green, `publish` creates the release. If `prepare` fails on the version assertion, delete the tag (`git push --delete origin v0.9.0`), fix, and re-tag.

- [ ] **Step 7: Verify the release as a user would**

```bash
gh release view v0.9.0
rm -rf /tmp/lattice-install && sh install.sh --prefix /tmp/lattice-install
/tmp/lattice-install/bin/lattice --version
find /tmp/lattice-install/share/lattice/plugins -type f | wc -l
```

Expected: the release lists every artefact plus `SHA256SUMS`; the installer downloads, verifies and installs; the version prints `0.9.0`; nine plugin files. Then open `/tmp/lattice-install/bin/lattice`, run `:plugins`, and confirm three `bundled` rows. **This is the launch's actual gate** — everything else is preparation for this working.

- [ ] **Step 8: Write the release notes**

Replace the auto-generated notes (3395 commits of changelog is noise) with the `CHANGELOG.md` 0.9.0 section plus an install block:

```bash
python3 - <<'EOF' > /tmp/notes.md
import re
s = open('CHANGELOG.md').read()
body = re.search(r'## 0\.9\.0.*?(?=\n## |\Z)', s, re.S).group(0)
print(body.strip())
print("""
## Install

```sh
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
```

Or download an archive below. Each unpacks to a relocatable prefix —
keep `share/lattice/plugins/` beside `bin/lattice`. Verify with
`shasum -a 256 -c SHA256SUMS`.

macOS quarantines browser downloads of unsigned binaries:
`xattr -dr com.apple.quarantine <extracted-dir>`.

Feedback is the point of this release — please
[open an issue](https://github.com/dhruvasagar/lattice/issues) or
[start a discussion](https://github.com/dhruvasagar/lattice/discussions).
""")
EOF
gh release edit v0.9.0 --notes-file /tmp/notes.md
gh release view v0.9.0 --web
```

- [ ] **Step 9: Update the release runbook**

In `docs/dev/operations/releasing.md`, add the core-plugins step to the process description and update Known gaps: the pipeline has now run end-to-end, ARM GUI bundles are best-effort in both the leg and the manifest, and `.dmg`/`.msi`/signing remain out.

- [ ] **Step 10: Close out this plan**

Mark every slice ✅ in the status table at the top of this file, and set `**Status:** ✅ complete`. Note in `docs/dev/operations/implementation.md` that 0.9.0 shipped, with the date.

**Do NOT archive this plan.** The archiving rule in CLAUDE.md is explicit that a ⛔ deferred slice keeps a plan active, naming `ML.4`/`ML.6` and `DB.8` as the work that got buried by ignoring it. Still carries: the **announcement** (Step 11, Dhruva's), L.5's feature-screenshot gallery ⛔ (1 of 6 shots exists) and the hero recapture Dhruva chose, and L.4b's GIF rendering ❌ — **dropped**, not deferred: VHS writes no file because headless Chrome 153's screencast is broken on this machine (`CVDisplayLinkCreateWithCGDisplay failed`), reproduced with a minimal tape on a real terminal, so motion now comes from the demo video instead (L.9). Leave this plan in `slice-plans/`; archive it when the announcement and the screenshots settle.

- [ ] **Step 11: Commit and announce**

```bash
git add docs/dev/operations/releasing.md docs/dev/operations/implementation.md \
        docs/dev/operations/slice-plans/
git commit -m "docs: 0.9.0 shipped

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
git push
```

Then announce — soft launch, so: your own channels, and one post to r/rust. Lead with what it is and one honest sentence about the alpha; link the release and the site, not the repo root. No Hacker News at 0.9.

---

### Task L.9: The demo video script

Post-tag; does not gate the release. A **differentiator-led 6-8 minute** script Dhruva records from — it opens on what nobody else has rather than touring features.

**Files:**
- Create: `docs/media/demo-script.md`

**Interfaces:**
- Consumes: L.4b's differentiator ordering and confirmed command names; L.2's shipped plugins (the demo runs a real installed build).
- Produces: nothing other slices depend on.

- [ ] **Step 1: Write the script**

Create `docs/media/demo-script.md` with this structure. Every keystroke must be one confirmed to exist in L.4b Step 2 — a demo that shows an unknown-command error is worse than no demo.

```markdown
# Lattice — introductory demo script (6-8 min)

Differentiator-led: open on what no other editor has, then earn the
feature tour. Record in sections; each has a reset point so one section
can be re-recorded without redoing the take.

## Before recording

- Fresh terminal, 1400×800, 16pt Nerd Font, default dark theme.
- `cd` to a clean checkout of this repo with a few uncommitted edits
  staged and unstaged (the magit section needs real hunks).
- Run the installed 0.9 build, not `cargo run` — the demo should be the
  thing a viewer can download.
- Confirm `:plugins` lists auto-pair, treesitter-context and project as
  `bundled` before you start. If it does not, the build is wrong and the
  auto-pair beat in section 3 will not work.

## 0. Cold open (0:00-0:30)

Editor already open on a Rust file. Say what it is in one sentence:
a modal, GPU-accelerated, plugin-first editor in Rust — vim's grammar,
emacs's extensibility, on a core where the UI thread does no I/O.

Then immediately: `:magit-status`.

## 1. Magit (0:30-2:00) — the thing nobody else has

...
```

Write all seven sections in full: cold open, magit, everything-is-a-buffer, config as Rust-WASM, org + agents, two renderers, and a close that says plainly it is an alpha and points at the install page and the issue tracker. For each section give: the exact keystrokes in order, what to say over each beat (as prose to paraphrase, not a teleprompter), the duration budget, and the reset command to return to a known state.

- [ ] **Step 2: Verify every command in the script exists**

```bash
grep -oE '^\s*:[a-z][a-z0-9-]*' docs/media/demo-script.md | tr -d ' :' | sort -u > /tmp/script-cmds.txt
cat /tmp/script-cmds.txt
```

For each, confirm it is registered (the greps from L.4b Step 2). Report any that you could not confirm, and remove them from the script rather than leaving them in.

- [ ] **Step 3: Commit**

```bash
git add docs/media/demo-script.md
git commit -m "docs(media): differentiator-led demo script for the intro video

Opens on magit in a modal editor rather than touring features, then
everything-is-a-buffer, programmable Rust-WASM config, org and agents,
and the two renderers. Sectioned with reset points so a single section
can be re-recorded without redoing the take.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01L1Ac4ezt9RY2UVPr1372zL"
```

---

## Addendum (2026-09-21) — the remaining launch work, made visible

The three slices below were added after `v0.9.0` shipped. L.9b and L.5b
existed already as prose deferrals; they are written out here so the plan's
open work is greppable rather than remembered. L.10 and L.11 are new, and
their premise is a gap found by reading the launch contract against the
shipped surfaces: **§10 names org-mode as one of four differentiators, and
not one user-facing surface mentions it.** `README.md` (143 lines): zero.
The site landing page: zero. `CHANGELOG.md`'s 0.9.0 section, which is now
the GitHub release body: zero.

The argument itself is already written, and well — `docs/user/org.md`
("the deepest test of those seams, and the reason several of them exist"),
`docs/user/plugins.md:272` ("a complete worked example"), `demo-script.md`
§4, and the plugin's own README, titled *"org — the reference plugin"*,
carrying a seam-by-seam table. So this is a **pointing** job, not a writing
one, and the second audience is the one that makes it urgent: a developer
deciding whether lattice's plugin API is deep enough to build on. Org is
the honest answer to that question, and right now nothing leads them to it.

Org is **not** becoming a bundled plugin — its tree-sitter grammar is 2.2 MB
of generated C, and design §1's argument is that such a grammar is the
plugin's build artefact, not the editor's. Every surface below says so.

### Task L.9b: Record the demo video and cut the clips ⛔

Dhruva's, and it gates more than itself: since VHS is ❌ dropped, **every
motion clip for the README and the site is cut from this recording**. One
session produces both.

**Files:**
- Create: `assets/media/demos/*.{mp4,gif}` (clips, each under 4 MB — `docs/media/README.md` size budget)
- Modify: `README.md`, `site/templates/index.html` (embed points)

- [ ] **Step 1: Record** against `docs/media/demo-script.md`, section by section — each has a reset point, so one section can be re-taken without redoing the whole run.
- [ ] **Step 2: Publish the full video** somewhere hosted; the size budget says link it, do not commit it.
- [ ] **Step 3: Cut the clips** named in the script's "Clips to cut" section.
- [ ] **Step 4: Wire them in**, keeping each committed clip under 4 MB.

### Task L.5b: The differentiator screenshot gallery ⛔

1 of 6 captured (`hero-dark.png`). The five landing shots and the ~20
supporting shots are listed in `docs/media/screenshot-ideas.md`, which is
derived from `docs/user/` rather than remembered. Shots 1-3 have verified
choreography in `docs/media/tapes/`; shots 4 and 5 are hand-only. Dhruva
also chose to recapture the hero.

### Task L.10: Org-mode in the launch communications ✅

**Files:**
- Modify: `README.md` (a new section after *What works today*)
- Modify: `site/templates/index.html` (the Extensibility feature card)
- External: `dhruvasagar/lattice-org-plugin` repo metadata

**Interfaces:**
- Consumes: `docs/user/org.md`, `docs/user/plugins.md` §"A plugin can ship a whole language", the plugin's own README.
- Produces: the inbound links L.11's page and the announcement both rely on.

**Decisions taken 2026-09-21 (Dhruva):**
- **`CHANGELOG.md`'s 0.9.0 section and the published release body are left alone.** Org lands in 0.10.0's section instead. A shipped release is not rewritten to improve its marketing.
- The README gets its **own section**, not a table row — "nothing in lattice knows what a headline is" does not fit in a cell, and the plugin-developer argument is the one being made.
- The org repo gets a **description and topics** so it is findable; **no tag** is cut for it here (that is its own repo's call, and it would be a release of code not reviewed in this plan).

- [ ] **Step 1: README section.** After *What works today*. Two audiences, named separately: what you get if you use org, and what it proves if you want to write a plugin. State plainly that it is not bundled and why.
- [ ] **Step 2: Landing-page card.** Extend the Extensibility card's copy with the org clause and link it to `/plugins/` (L.11).
- [ ] **Step 3: Org repo metadata.** `gh repo edit` — description and topics.
- [ ] **Step 4: Verify.** Links resolve; `README.md` stays close to its 143-line discipline; no claim that org ships with lattice.

> **Accuracy note.** The plugin's own README says "the `language` seam, and
> by now of twelve others" (13) while its table lists 15 rows. Do not
> propagate either number — write "more than a dozen" until that repo
> settles it.

### Task L.11: A `/plugins/` section — index plus a page per plugin ✅

**Files:**
- Create: `site/content/plugins/_index.md` → `/plugins/`
- Create: `site/content/plugins/{auto-pair,treesitter-context,project,org}.md`
- Create: `site/templates/plugins-index.html`
- Modify: `site/templates/base.html` (nav + footer)
- Modify: `site/scripts/sync-docs.sh` (the drift guard)

**Interfaces:**
- Consumes: `xtask/src/main.rs:19` `CORE_PLUGINS`, `docs/user/core-plugins.md`, `docs/user/plugins.md`, `docs/user/org.md`, L.10's org copy.
- Produces: `/plugins/` and `/plugins/<name>/`, the link targets L.10's landing card and README point at.

**Scope, revised 2026-09-21 (Dhruva, three passes):** not a single page and
not an org-only feature. `/plugins/` is an **index** that will grow —
eventually with plugins written by other people — and each plugin gets its
**own page** for detail, screenshots and demo clips. The index **clearly
demarcates bundled from external**, because that distinction is the one a
visitor most needs and most easily gets wrong: bundled plugins arrive with
the binary and are on by default; external ones you install yourself.

An earlier draft of this slice settled on "org as the featured reference,
not a directory". That answer was given for a *single page*; the page is no
longer single, and a section with one page per plugin carries a directory
without the thinness that objection was about.

**Structure:**

```
site/content/plugins/
  _index.md              -> /plugins/            (grouped card index)
  auto-pair.md           -> /plugins/auto-pair/
  treesitter-context.md  -> /plugins/treesitter-context/
  project.md             -> /plugins/project/
  org.md                 -> /plugins/org/
```

**Grouping mechanism:** each page carries `extra.kind` in its frontmatter —
`"bundled"` or `"external"` — and `plugins-index.html` partitions
`section.pages` on it, rendering one card grid per group under its own
heading, plus a per-card badge so the distinction survives being skimmed. A
group with no pages renders **nothing**, so the future `"community"` group
stays invisible until a third-party plugin exists rather than shipping an
empty shelf. `extra.kind` is also what the drift guard reads.

Chosen over a Zola taxonomy: a taxonomy would generate its own listing pages
and need a `config.toml` change, to give exactly one filter this template
does in two lines.

**The single-source constraint — reference content does NOT move.**
`docs/user/` *is* the offline `:help` corpus embedded in the binary
(`sync-docs.sh`'s header states this, and `crates/lattice-help/build.rs`
reads it). So `/plugins/<name>/` is a **showcase** surface — what it is, what
it looks like, how to get it — that links into `/docs/` for the reference.
It must not restate keybinding tables or option lists; those have one home
and it is not this page.

**Demo assets are blocked, and the pages ship without them.** Screenshots and
clips come from L.5b and L.9b. No page gets a placeholder `<img>` for an
asset that does not exist — a broken image is worse than an absent one. The
asset slots go in when the captures land.

- [ ] **Step 1: The template.** `plugins-index.html`, partitioning on `extra.kind`, reusing the existing `.subsection-grid` / `.subsection-card` classes (`site/sass/style.scss:603,610`) rather than inventing card CSS.
- [ ] **Step 2: `_index.md`.** The lead: config, bundled plugins and third-party plugins are the same mechanism at different trust tiers — one substrate, capability-gated, crash-isolated. Then the groups.
- [ ] **Step 3: The four plugin pages.** Each: what it is, why you would want it, how to get it (bundled ⇒ the `<id>.enabled` gate; external ⇒ the install route), and **Full documentation →** into `/docs/`. Org additionally carries the reference-implementation argument from L.10 and the "never bundled, here is why" note.
- [ ] **Step 4: Nav + footer** links in `base.html`, between *Why Lattice* and *Docs*.
- [ ] **Step 5: The drift guard.** Extend `sync-docs.sh` to hard-fail when the set of `extra.kind = "bundled"` pages and `CORE_PLUGINS` in `xtask/src/main.rs` disagree **in either direction** — the same both-directions check `nav.toml` already gets. This section restates a list whose truth lives in code, and §10's rule is *bind the artefact to its source, or accept that it will drift*; 0.9 prep turned up three stale docs that each failed exactly this way. External pages are deliberately unguarded — there is no in-tree source of truth to bind them to.
- [ ] **Step 6: Prove the guard bites.** Add a bundled page for a plugin not in `CORE_PLUGINS`, run the sync, watch it fail; delete one that is, watch it fail the other way. A guard never seen red is not a guard.
- [ ] **Step 7: Verify.** `python3 site/scripts/sync-docs.sh` clean, `zola build` clean, every link resolves, and the bundled/external split is legible without reading the prose.

---

## Self-Review

**Spec coverage** (against `../../architecture/launch-0.9.md`):
- §1 what 0.9 claims → L.5 README status block, L.8 release notes. ✓
- §2 version policy → L.3 Steps 1-3; tag asserted in L.8 Step 6. ✓
- §3 artefact contract (core plugins, prefix-relocatable, build-once, rejected alternatives) → L.2 entire; design amendment landed in L.2 Step 13. ✓
- §4 honesty requirement, all six false claims → L.3 Steps 4-7 (install.md rewrite covers claims 1-3; hero version via Step 3; magit audit Step 6; Phase 4.2/4.3 and the help table Step 7). ✓
- §5 install channels, in and out → L.4 (`install.sh`); "not yet available" section in L.3's install.md; L.6 known-limitations names each exclusion. ✓
- §6 feedback surface → L.7 issue templates + `config.yml` discussions link; L.6 Step 6 answers the FAQ honestly. ✓
- §7 explicitly not in 0.9 → L.6 known-limitations page enumerates all nine items. ✓
- §8 paramount-goal alignment → no task enters a hot path; L.2 is the goal-#2 precondition. ✓
- §9 green baseline → L.0. ✓

**Placeholder scan:** one deliberate deferral remains — L.0's fix site (Step 6) is named by a `grep` in Step 3 rather than a file:line, because the delegation target of `e.open_completion_popup()` could not be resolved without reading the editor crate. That is a located-by-command step, not a "TODO". L.6 Step 7 and L.0 Step 4 both instruct copying a neighbouring test's helper shape rather than asserting a helper name that may not exist. Every other step carries its literal content.

**Type/name consistency:** `core-plugins` is the artifact name in L.2 Step 4 (upload) and Step 5 (download), path `runtime/plugins` in both, which is what L.2 Steps 6/7/9/10 copy from. `core_plugins_dir_from` (L.2 Step 1) matches `discovery.rs:72`. `CORE_PLUGINS` names match `xtask/src/main.rs:19`. The archive root name `$FLAVOUR-$ver-$arch-$os` in L.4's script matches `lattice-${VERSION}-${{ matrix.build }}` from L.2 Step 6 (where `matrix.build` is `<arch>-<os>`). `install.sh` lives at the repo root in L.4 Step 1 and is fetched from `main` at that path by L.3's install.md and L.5's README. `known-limitations` / `troubleshooting` / `cheatsheet` are the topic slugs used in L.3's install.md links, L.5's README links, L.6's pages, `nav.toml`, and L.7's bug template.

**Sequencing check:** L.3's install.md links `./docs/known-limitations/`, which does not resolve until L.6 — flagged inline in L.3 Step 4, and the site build that would catch it runs in L.6 Step 5. L.5's README links `CONTRIBUTING.md`, which arrives in L.7 — flagged in L.5 Step 4 and re-checked in L.7 Step 8. Both are forward references within one plan, not gaps.
