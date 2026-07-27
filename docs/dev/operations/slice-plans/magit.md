# Magit — Slice Plan

**Status:** plan, not started. Slices sequenced by dependency.
Authoritative status per slice tracked here and in
[`../implementation.md`](../implementation.md) (under
`## vcs-and-magit`). Design fragment:
[`../../architecture/magit.md`](../../architecture/magit.md).

This plan owns *when* and *in what order*. The design fragment
owns *what* and *why*.

## Slices

| Slice | Description | Depends on | Status |
|---|---|---|---|
| VCS.1 | `lattice-vcs` crate — Layer 1 data API | None | ✅ |
| VCS.2 | Layer 2 auto-inline-diff subsystem | VCS.1 | ✅ |
| PICK.1 | Picker transient-mode extension | None (picker subsystem) | ✅ |
| MG.1 | `lattice-magit` crate scaffolding | VCS.2, mode-architecture | ✅ |
| MG.2 | magit-status buffer rendering | MG.1 | ✅ |
| MG.3 | magit-status actions (s/u/x, cc/ca, =, p, <CR>) | MG.2 | ✅ |
| MG.4 | magit-commit buffer | MG.2 | ✅ |
| MG.5 | magit-diff buffer | MG.2 | ✅ |
| MG.6 | magit-log buffer | MG.2 | ✅ |
| MG.7 | magit-blame buffer | VCS.2 | ✅ |
| MG.8 | Transient menus (picker transient mode) | MG.3, PICK.1 | ✅ |
| MG.9 | magit-stash, magit-branch, magit-rebase | MG.8 | ✅ |
| MG.10 | Polish + perf + edge cases | MG.1–MG.9 | ✅ |
| MG.11 | Cross-view uniformity: `<CR>`, highlighting, file-at-revision | MG.4–MG.9 | ✅ |
| MG.12 | Destructive-action parity — confirm before branch delete / stash drop | MG.9 | 📝 |
| MG.13 | Mode-keymap binding testability (harness) | MG.8 | 📝 |
| MG.14 | Headerline across every magit buffer | MG.4–MG.11 | 📝 |
| MG.15 | Stash detail view (`<CR>` in magit-stash) | MG.9, MG.14 | 📝 |
| MG.16 | Remote/stash ex-command parity (`:magit-push` etc.) | MG.8 | 📝 |
| MG.17 | Transient flags + arguments (`--force`, `--include-untracked`, …) | MG.8 | 📝 |
| MG.18 | Hunk-level staging | MG.5, MG.13 | 📝 |
| MG.19 | magit-diff side-by-side + `do`/`dp` | MG.18, D.4 | 📝 |
| MG.20 | Operation coverage — merge / tag / reset / revert / cherry-pick | MG.17 | 📝 |

**2026-07-26 audit correction:** this table (last synced when MG.1-3 landed) had
drifted from `implementation.md`'s per-slice status, which had marked MG.4-10 all
✅ — neither matched source. A functional audit (prompted by real-usage bugs in
MG.3) found MG.5 is a content-population stub, MG.6/MG.7's primary action
(`<CR>`) is dead on arrival (registration guard dropped / never registered),
MG.8's dispatch transients were never wired to their ex-commands, and MG.9's
branch-create and rebase are stubbed/disconnected from real git. See the dated
notes under each slice in `implementation.md` for specifics; MG.3 and MG.8 got
partial fixes in the same pass (see there).

**2026-07-27 close-out:** the gaps above are closed and MG.4–MG.9 move to ✅.
Landed in this pass:

- **MG.8 root cause** — `do_transient_trigger` built a throwaway
  `DispatchOutcome` and discarded its `.effects`, so *every* transient item's
  `Effect::OpenSyntheticBuffer` vanished before reaching the renderer: the menu
  closed and nothing happened. Now takes the caller's `&mut DispatchOutcome`.
  Compounding this, `MagitGlobalMode` registered its handlers from
  `on_activate` behind a `OnceLock`, which `ModeRegistry::spawn_cascade` could
  defer to an unfinished background task — moved to `Mode::action_handlers()`
  (synchronous, boot-time).
- **MG.8 breadth** — `C-c g` gained `d` (diff), `f` (fetch), a real `c` submenu
  (`c c` commit / `c a` amend) and `z` submenu (`z z` push / `z l` list);
  `C-c f` gained `u` (unstage), `x` (discard, with confirmation), `l`
  (file log), `b` (blame). Keys follow Emacs magit's own. Three regression
  tests guard the menus (no inert `Flag` leaves incl. submenus, no duplicate
  keys per level, plus an inverse vacuity check).
- **MG.11 (new)** — cross-view uniformity work driven by real usage:
  `<CR>` on a commit SHA opens the dedicated commit buffer in *every* view
  that shows one (status, log, blame, rebase); `<CR>` on a file line opens
  the file **at that revision** (new `magit-file-revision-mode`,
  `*magit:file:<ref>:<path>*`) wherever the surrounding buffer describes a
  fixed revision or the index, and the live working-tree file where it
  describes current state; per-view syntax highlighting for all eight
  non-status views via the new `highlight.rs` + five magit-owned theme
  elements (`magit.sha`, `magit.branch.current`, `magit.ref.decoration`,
  `magit.rebase.verb`, `magit.author`); and `d` on a status file entry opens
  a dedicated section-scoped diff buffer (`--cached` for Staged,
  working-tree-vs-index for Unstaged) as a scalable alternative to `=`.
- **Latent bug fixed** — `gix::discover` requires a *directory*; three sites
  passed a file path and always failed. One was `lattice-host`'s auto-head-diff
  subsystem, meaning gutter diff signs had never worked for any file.

## Dependency graph

```
VCS.1 → VCS.2 ─┬─→ MG.1 → MG.2 → MG.3 ─┬─→ MG.8 → MG.9
               │    │       ├── MG.4   │
PICK.1 ────────┘    │       ├── MG.5   │
                    │       ├── MG.6   │
                    │       └── MG.7 ──┘
                    │
                    └── (MG.7 also reads VCS.2 data for blame)

MG.9 ─→ MG.10 ─→ MG.11 ──┬─→ MG.12 (confirm parity)   ← do first: wiring only
                         ├─→ MG.13 (binding testability) ← then this: unblocks
                         │                                  every slice below
                         ├─→ MG.14 (headerline) ─┬─→ MG.15 (stash detail)
                         ├─→ MG.16 (ex-cmd parity)
                         └─→ MG.17 (flags/args) ─┬─→ MG.20 (operations)
                                                 │
                    MG.5 ─→ MG.18 (hunk staging) ─→ MG.19 (side-by-side)
```

**Recommended order: MG.12 → MG.13 → MG.14 → the rest.** MG.12 is pure
wiring over machinery that already exists and closes a real safety
inconsistency. MG.13 comes next because every slice after it adds chords,
and until mode-keymap bindings are testable they all ship on the blind
spot that already produced one live bug (MG.8). MG.14 is
user-facing polish with no dependants blocking it. MG.18 is the largest
functional gap but wants a design fragment first, so it runs on its own
track rather than gating the others.

VCS.1, VCS.2, and PICK.1 are independent and buildable concurrently.
MG.1 depends on VCS.2 (needs `RepositoryEvent` and git data types).
MG.7 depends on MG.1 (needs `MagitBlameMode` registration) and uses VCS.2
data for git blame output.
MG.4/MG.5/MG.6/MG.7 build in parallel after MG.1 lands (each is an
independent major mode definition + buffer provisioning).
MG.8 waits for both MG.3 and PICK.1.

MG.4/MG.5/MG.6/MG.7 can run in parallel after MG.1 lands.

## Per-slice detail

### VCS.1 — `lattice-vcs` data crate

- **Crate:** `crates/lattice-vcs/`
- **Deps:** `gix`, `ropey`, `smallvec`, `thiserror` (zero `lattice-*`)
- **Deliverables:**
  - `Repository::discover(path)`, `workdir()`, `gitdir()`
  - `GitBlob::read(repo, oid)` → `Bytes`
  - `Reference::resolve(repo, name)` → `Oid`
  - `WorkingTree::path_status(repo, path)` → `PathStatus`
  - `WorkingTree::statuses(repo)` → `Vec<(PathBuf, PathStatus)>`
  - `PathStatus` enum: Clean, Modified, Added, Deleted, Untracked, Ignored, Unmerged, Conflicted
  - `Index::stage_path`, `unstage_path`, `stage_hunk`, `unstage_hunk`
  - `Commit::create`, `Commit::amend`
  - `Branch::checkout`, `Branch::create`, `Branch::delete`
  - `Stash::list`, `Stash::apply`, `Stash::pop`, `Stash::drop`, `Stash::create`
  - `GitBaseline` struct, `impl DiffParticipantSource`
  - Unit tests in temp git repos
  - Bench: `git_status_p99_us` on 200-file repo
- **Tests:** `crates/lattice-vcs/tests/` — repo discovery, blob read, ref resolution, status classification, stage/unstage round-trip, commit create + amend, branch operations, stash operations
- **Special verification:** each operation verified against `git` CLI output on the same repo

### VCS.2 — Layer 2 auto-inline-diff subsystem

- **Location:** `crates/lattice-host/src/vcs/`
- **Deps:** `lattice-vcs`, `lattice-diff`, `lattice-runtime` (event bus)
- **Deliverables:**
  - `RepositoryWatcher` — tokio fs-watcher task, debounced, fires `RepositoryEvent`
  - `RepositoryEvent` enum: HeadChanged, IndexChanged, RefsChanged
  - Auto-register `DiffSession(GitBaseline(HEAD, path))` on `DocumentOpened`
  - Auto-teardown on `DocumentClosed`
  - Subscribe to `RepositoryEvent` → poke affected sessions
  - `git.auto-head-diff` typed option (default `true`)
  - Integration with `DiffSubsystem` (D.2–D.3)
- **Tests:** open file in temp git repo → gutter signs appear; `git checkout other-branch` from outside → gutter signs reflow; `DocumentClosed` → session torn down; `git.auto-head-diff = false` → no auto-session
- **Formerly:** D.7 (re-carved)

### PICK.1 — Picker transient-mode extension

- **Location:** `crates/lattice-picker/`
- **Deps:** `lattice-picker` (existing), `lattice-ui-tui`, `lattice-ui-gpui` (rendering)
- **Why a picker slice, not a magit slice:** The transient is a picker interaction mode. Magit is the first consumer, but which-key key hints, command palette drilldown, and future plugin transients all reuse it. Extending the picker avoids building a parallel rendering + input system.
- **Deliverables:**
  - `TransientSpec`, `TransientGroup`, `TransientItem`, `TransientItemKind`, `PreviewFn`, `TransientState` / `TransientValue` types in `lattice-picker`
  - Picker rendering: grouped entry layout with section headers, key+label+description row rendering, flag toggle indicators (`[x]` / `[ ]`), preview pane repurposed as command preview
  - Keyboard: single-key trigger dispatch (`TransientItemKind::Action` → `ActionHandlerRegistry`), flag toggle in-place, argument → minibuffer prompt → return, submenu open + `DEL`/`BS` back, `q`/`Esc`/`C-g` dismiss
  - `PickerRegistry::open_transient(spec: TransientSpec, display: PickerDisplay)` API
  - TUI + GPUI render parity
- **Tests:** single-group transient opens/closes; multi-group with scroll; flag toggle updates preview line; nested submenu opens → back; argument value entry → back to transient; dismiss with q/Esc/C-g; TUI + GPUI parity
- **Stretch (deferred):** WIT mirror for plugin-accessible transient

### MG.1 — `lattice-magit` crate scaffolding

- **Crate:** `crates/lattice-magit/`
- **Deps:** `lattice-vcs`, `lattice-diff`, `lattice-mode`, `lattice-runtime`, `lattice-core`, `lattice-protocol`, `lattice-grammar`, `lattice-keymap`
- **Deliverables:**
  - `MagitCoreMode` — `MinorMode`, `ActivationPolicy::OnMajorMatch(magit-*)`, shared keymap (`gr`, `q`, `]]`/`[[`, `]f`/`[f`, `]c`/`[c`, `TAB`, `S-TAB`), action handler stubs
  - `MagitStatusMode` — `MajorMode`, keymap + handlers stubbed, `on_activate` creates empty status buffer
  - `pub fn install(boot: &mut SubsystemBoot)` — registers all modes, commands, keymaps
  - `lattice-cli` boot wiring — one `lattice_magit::install(&mut boot)` call
  - Verify: `:magit-status` in a git repo opens an empty `*magit:status*` buffer; `:ls` shows it; `:bd` closes it
- **Tests:** activation/deactivation lifecycle, `magit-core` activates on `magit-status` major, `q` closes buffer, no `Editor::do_magit_*` methods exist

### MG.2 — magit-status buffer rendering

- **Deliverables:**
  - `SectionIndex` data structure — file paths + status labels only, no diff data
  - `DiffCache` — per-file lazy diff cache, keyed by `(path, section)`, invalidated on file status change
  - `MagitStatusRefreshTask` — runs `WorkingTree::statuses(repo)` (gitoxide), `Stash::list(repo)`, `git log --oneline -N` on `spawn_blocking`. No diff commands.
  - Buffer content formatting — section headers, file entries with status labels
  - `=` toggle — lazy diff loading: `git diff --cached <path>` / `git diff <path>` on `spawn_blocking`, inline insertion as local edit, hunk fold registration
  - Section fold provider — register fold ranges via fold overlay service
  - Headerline — branch name + status indicator
  - Auto-refresh on `RepositoryEvent` — re-runs fast path only; invalidates `DiffCache` entries for changed files
- **Tests:** open status in temp repo with staged + unstaged + untracked files → all sections present as file lists; `=` on a file toggles inline diff; `=` on a section toggles all files; auto-refresh after commit flushes stale diffs; headerline shows branch name

### MG.3 — magit-status actions

- **Deliverables:**
  - `s` — stage hunk at cursor (`git add -p`); file-level when cursor on file header
  - `u` — unstage hunk at cursor (`git reset -p`); file-level when cursor on file header
  - `x` — discard hunk at cursor (`git checkout --`); file-level when cursor on file header
  - `cc` — open commit buffer
  - `ca` — amend previous commit
  - `=` — toggle inline diff at cursor
  - `p` — stage hunk interactively (full `git add -p` interactive prompt)
  - `<CR>` — context-aware open/visit (open file, show commit, etc.)
  - `gr` — manual refresh (inherited from `magit-core`)
  - `q` — close buffer (inherited from `magit-core`)
  - Stale hunk boundary detection + user message
- **Tests:** stage hunk → `git diff --cached` shows it moved; unstage → back to unstaged; discard → file reverted; stale hunk → "refresh and retry" message; file-level s/u/x work; `cc` opens commit buffer with correct diff; `=` toggles inline diff visibility

### MG.4 — magit-commit buffer

- **Deliverables:**
  - `MagitCommitMode` major mode, `*magit:commit*` buffer
  - Staged diff preview (read-only top region, inline overlay)
  - Editable message region (bottom)
  - `C-c C-c` → `Commit::create(repo, message)` → close → refresh status
  - `C-c C-k` → close without commit
  - Amend: pre-populate previous message, use `Commit::amend`
  - Empty subject validation
- **Tests:** commit with message → HEAD advances; abort → no change; amend → commit count unchanged, HEAD message updated; empty subject → error in headerline

### MG.5 — magit-diff buffer

- **Deliverables:**
  - `MagitDiffMode` major mode, `*magit:diff*` buffer
  - Full `DiffSession` (HEAD vs working tree via `GitBaseline`)
  - Side-by-side presentation (reuses D.4 pane groups)
  - `s`/`u`/`x` on hunks in diff view (inherits `TAB` fold + `]c`/`[c` + `do`/`dp` from `magit-core` + `diff-mode`)
  - Hunk staging fires same `ActionId`s as `magit-status-mode`
- **Tests:** opens with side-by-side panes; scroll-binding works; stage hunk from diff → status buffer reflects; visual + `s` stages selected range

### MG.6 — magit-log buffer

- **Deliverables:**
  - `MagitLogMode` major mode, `*magit:log*` buffer
  - `git log --oneline --graph --decorate -N` → styled text
  - Commit SHA, ref decorations, graph styling
  - `<CR>` → open `*magit:commit:<sha>*`
  - Log arguments (count, `--all`, path filter) configurable via `C-c g` dispatch transient's Log submenu or `:magit-log` args
- **Tests:** log renders with graph; `<CR>` opens commit detail; log args change output

### MG.7 — magit-blame buffer

- **Deliverables:**
  - `MagitBlameMode` major mode, `*magit:blame:<path>*` buffer
  - `BlameLineMap` — per-file cache, populated from `git blame --line-porcelain`
  - Blame gutter column — SHA, author, date as styled cells
  - `<CR>` → open `*magit:commit:<sha>*`
  - `p` → re-blame at parent commit
  - `q` → close blame buffer
  - Cache invalidation on file change
- **Tests:** blame renders correct author/date per line; `<CR>` opens correct commit; `p` re-blames at parent; file edit invalidates cache

### MG.8 — Transient menus (picker transient mode)

- **Deps:** MG.3 (status actions), PICK.1 (picker transient-mode extension)
- **Deliverables:**
  - `TransientState` — `RefCell` on magit's Guard holding flag values, argument values
  - Root dispatch transient (`C-c g` global) — groups: Working tree / History / Branches / Stashing / Remotes / Misc
  - Branch transient — actions + arguments + live preview
  - Merge transient — merge target selection + `--no-ff` / `--squash` flags
  - Rebase transient — start/continue/abort/skip/edit-todo + interactive/autosquash/onto flags
  - Stash transient — create/apply/pop/drop/list + `--include-untracked` flag
  - Push/Pull transients — `--force`/`--set-upstream` / `--all`/`--prune` flags
  - `fn preview(state: &TransientState) -> String` — live command preview for each transient
  - Global bindings: `C-x g` → `:magit-status`, `C-c g` → `magit-dispatch`, `C-c f` → `magit-file-dispatch` (registered during `install(boot)`)
  - `C-c g` opens root dispatch via `ctx.picker.open_transient(spec, display)`; `C-c f` opens file-dispatch — resolves file path from current buffer (global) or `SectionIndex` entry (magit buffers)
  - Direct chords (§12.2) fire same `ActionId`s bypassing the transient (advanced-user fast path)
- **Tests:** dispatch transient opens with correct groups; flag toggle updates preview; nested submenu opens → `DEL` back; argument value set → preview reflects; file-dispatch from status resolves correct file; file-dispatch from code buffer resolves buffer path; direct chord and transient-submitted chord produce identical action; dismiss with q

### MG.9 — Remaining operation buffers

- **Deliverables:**
  - `MagitStashMode` — `*magit:stash*`, stash list, `<CR>` show, `a` apply, `p` pop, `d` drop, `z` create
  - `MagitBranchMode` — `*magit:branch*`, branch list, `<CR>` checkout, `c` create, `d` delete, `m` merge
  - `MagitRebaseMode` — `*magit:rebase*`, interactive rebase todo buffer, editable pick/reword/squash/fixup/drop, `C-c C-c` runs rebase, `C-c C-k` aborts
- **Tests:** create branch → visible in branch list; stash create → appears in stash list; rebase todo edit → rebase runs correctly

### MG.10 — Polish

- **Deliverables:**
  - Persistent state cache (last log args, last branch, last stash index)
  - Performance optimization (lazy diff caching, per-file on-demand loading)
  - Error handling: detached HEAD headerline, bare repo denial, no-repo message
  - `:help magit` — buffer-backed help view
  - GPUI renderer parity verification (TUI + GPUI inline diff, blame gutter, folds)
  - Manual QA pass against canonical magit workflows
- **Tests:** detached HEAD renders correctly; bare repo rejects writes; no-repo shows message; help buffer opens with keybinding table

---

The slices below were carved from a 2026-07-27 source audit (see
`implementation.md`). MG.1–MG.11 being ✅ means **each slice's declared
scope landed** — not that magit is at parity with Emacs magit. These
close the audited gap between the two.

### MG.12 — Destructive-action parity

magit-status's `x` (discard) asks first via `Effect::Confirm` →
`action:magit-discard-execute`. `d` in magit-branch calls
`Branch::delete` (force `-D`) **immediately**, and `d` in magit-stash
drops without asking. Same class of irreversible act, three different
safety postures — the inconsistency is the bug, not any one binding.

- **Deliverables:** route branch-delete and stash-drop through the
  existing `Effect::Confirm` two-step; audit every remaining magit
  mutation for the same gap; prompt text names the target
  (`Delete branch <name>?`) so the confirm is answerable without
  looking away.
- **Tests:** each destructive chord emits `Confirm` and mutates nothing
  until the yes-action fires; `n` leaves the repo untouched.
- **Note:** the confirm machinery already exists — this is wiring, not
  new mechanism. Highest value-to-effort of the open slices.

### MG.13 — Mode-keymap binding testability (harness)

Mode keymap layers are pushed by mode activation, which runs through
`ModeRegistry::spawn_cascade` (async). No synchronous App-level test can
observe them, so **no test can prove a mode's chord actually fires**.
This is not hypothetical: it is exactly what let the `C-c g` / `C-c f`
dead-transient bug ship (MG.8), and why MG.12's confirms and the
`search-line-mode` `<BS>` binding are handler-tested but binding-untested.

- **Deliverables:** a test seam that deterministically drives mode
  activation to completion (await the cascade, or a test-only
  synchronous activation path), so `press(chord)` resolves through the
  real layered keymap.
- **Tests:** a magit chord bound only by a mode's keymap fires via
  `press()`; the guard fails if the layer is absent.
- **Sequencing:** worth doing BEFORE the feature slices below — each one
  adds chords that would otherwise ship on the same blind spot.

### MG.14 — Headerline across every magit buffer

**Audit finding:** magit uses the `Headerline` trait **zero** times.
`SectionIndex::branch_status_line()` exists but has **no callers** — dead
code. `magit-status.md`'s claim that a branch/ahead-behind headerline "is
active" is false and must be corrected with this slice.

The mechanism already exists and is proven: `lattice_cells::headerline::
{Headerline, HeaderlineProvider, HeaderlineRow}`, consumed today by
`lattice-multibuffer` and `lattice-compilation`. `Headerline::version()`
is polled per tick and `render()` is only called when the version
advances, so the row costs nothing while static — it must not block, and
background refreshes bump the version rather than computing inline
(paramount goal #1).

Per-buffer content, so each view answers "what am I looking at?" without
re-deriving it from the body:

| Buffer | Headerline |
|---|---|
| magit-status | branch, ahead/behind, repo name, dirty counts |
| magit-commit | branch being committed to, staged file/insertion counts, `AMEND` marker |
| magit-revision | short SHA, author, relative date, subject |
| magit-file-revision | `<path> @ <short-sha>` (or `@ index` for the `staged` pseudo-ref) |
| magit-diff | scope (`HEAD` / `staged` / `unstaged`) + path when file-scoped |
| magit-log | ref being logged, commit count, path filter when file-scoped |
| magit-blame | path + revision currently blamed (walks back with `p`) |
| magit-branch | current branch, total branch count |
| magit-stash | stash count |
| magit-rebase | upstream, commit count, `REBASE IN PROGRESS` when applicable |

- **Deliverables:** one `MagitHeaderline` provider parameterised per mode
  (NOT one impl per buffer kind — the differences are data, not code, so
  this must not become a `match buffer_kind`); wire through each mode's
  `on_activate`; theme elements for the header's own styling reusing the
  MG.11 `magit.*` palette (`magit.sha` for the SHA field, etc.).
- **Tests:** each mode publishes a non-empty row carrying its identifying
  field; `version()` does not advance when nothing changed (the
  no-work-per-tick guarantee); a background refresh bumps it exactly once.
- **Docs:** correct the false "headerline is active" claim in
  `magit-status.md`; delete or wire `branch_status_line()`.

### MG.15 — Stash detail view

magit-stash has **no `<CR>`** — you cannot preview a stash from the stash
list, only from magit-status (where `<CR>` toggles its patch inline).
That is the one remaining inconsistency in MG.11's `<CR>` uniformity
rule: every other list view navigates to a detail buffer.

- **Deliverables:** `*magit:stash:<n>*` (`git stash show -p`), reusing
  `highlight::diff_styled_spans` and the MG.14 headerline; `<CR>` in
  magit-stash opens it. Keeps magit-status's inline toggle as-is (there
  the stash is one row among many).
- **Tests:** `<CR>` on a stash row opens the detail buffer with that
  stash's patch; the status buffer's inline toggle is unchanged.

### MG.16 — Remote/stash ex-command parity

Asymmetry: every buffer-opening operation has an ex-command, but
fetch / pull / push / stash-push are **transient-only** — reachable from
`C-c g` and nowhere else. Ex-commands are the scriptable surface and the
`:` discovery path; a transient-only operation is invisible to both.

- **Deliverables:** `:magit-fetch`, `:magit-pull`, `:magit-push`,
  `:magit-stash` sharing the transient handlers' bodies (one
  implementation, two front-ends — the unified-dispatch rule). Dashed
  namespaced names per the standing ex-command naming rule; no new 1–2
  letter shorts.
- **Tests:** each ex-command reaches the same handler its transient item
  fires.

### MG.17 — Transient flags and arguments

The only `TransientItemKind::Flag` in magit is the defensive fallback for
an unresolved id, so no shipped item is a real toggle: `z z` is always
bare `git stash push`, `P` always bare `git push`. `Argument` is unused
crate-wide, and no magit spec sets a `preview` closure. The picker
substrate supports all three; magit simply does not use them.

- **Deliverables:** `--force` / `--set-upstream` on push,
  `--include-untracked` on stash, `--all` / count / path-filter on log;
  a live preview line rendering the resolved git command.
- **Tests:** toggling a flag changes the preview string and the argv the
  handler builds; defaults round-trip.
- **Note:** `Argument` is currently a no-op in `do_transient_trigger`
  (§8.8) — that must land first or arguments silently do nothing, the
  same failure shape as the MG.8 bug.

### MG.18 — Hunk-level staging

**The largest divergence from Emacs magit.** `Index::stage_hunk` /
`unstage_hunk` exist in `lattice-vcs` with **zero callers**; every
`s`/`u`/`x` in every view is file-level. MG.5's original scope included
"hunk staging from diff buffer" and that part did not land. With `p`
(`git add -p`) deliberately disabled — genuinely blocked on terminal
suspend — there is currently **no path to partial staging at all**.

- **Deliverables:** hunk parsing over the diff text with a stable
  identity per hunk; `s`/`u`/`x` resolving hunk-at-cursor before falling
  back to file-at-cursor; visual-mode region staging (magit's
  most-used partial-stage path); the same behaviour in magit-status's
  inline diffs and magit-diff's buffer.
- **Tests:** staging one hunk leaves the file's other hunks unstaged;
  region staging splits a hunk correctly; cursor on a file header still
  stages the whole file (no regression).
- **Note:** wants its own design fragment before implementation — it
  touches the diff model, the section index, and all three staging
  surfaces. Sequence after MG.13 so the new chords are testable.

### MG.19 — magit-diff side-by-side + `do`/`dp`

magit-diff is a single-pane text view; MG.5's "reuse D.4 side-by-side"
did not land, and there is no `do`/`dp` hunk transfer.

- **Deliverables:** two-pane layout via D.4's pane-group machinery,
  scroll-bound; `do`/`dp` on top of MG.18's hunk identity.
- **Tests:** panes stay scroll-synced; `do`/`dp` move exactly one hunk.

### MG.20 — Operation coverage

Absent: merge (except magit-branch's `m`), tag, reset, revert,
cherry-pick, bisect, submodule, remote management. Deliberately omitted
from the transients so far, because a menu row that does nothing is worse
than an absent one — they appear only as they gain real implementations.

- **Deliverables:** prioritised by daily use — reset (`--soft`/`--mixed`/
  `--hard`, hard behind MG.12's confirm), revert, cherry-pick, tag, then
  the rest. Each lands with its transient entry and ex-command together.
- **Tests:** per operation, plus a transient-completeness check that the
  menu lists exactly the implemented set.

## Cross-references

- [`../../architecture/magit.md`](../../architecture/magit.md) — design fragment (what + why)
- [`../../architecture/diff-system.md`](../../architecture/diff-system.md) — diff subsystem Magit consumes
- [`../../architecture/diff-extraction.md`](../../architecture/diff-extraction.md) — `SubsystemBoot` pattern
- [`../../architecture/host-provider-boundary.md`](../../architecture/host-provider-boundary.md) — boundary that inverts Magit out
- [`../../architecture/mode-architecture.md`](../../architecture/mode-architecture.md) — Mode trait, ModeActivator, ActionHandlerRegistry
- [`../../architecture/compilation-mode.md`](../../architecture/compilation-mode.md) — synthetic-buffer + process spawning pattern
- [`../implementation.md`](../implementation.md) — per-slice status ledger
