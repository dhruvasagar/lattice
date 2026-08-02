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
| PICK.1a | Transient navigation as a selection, not a scroll offset | PICK.1 | ✅ |
| PICK.1b | Multi-key transient rows (`,k`, `=f`) reachable by keypress | PICK.1 | ✅ |
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
| MG.12 | Destructive-action parity — confirm before branch delete / stash drop | MG.9 | ✅ |
| MG.13 | Action handlers at boot, not activation (was: "binding testability") | MG.8 | ✅ |
| MG.14 | Headerline across every magit buffer | MG.4–MG.11 | ✅ |
| MG.15 | Stash detail view (`<CR>` in magit-stash) | MG.9, MG.14 | ✅ |
| MG.16 | Remote/stash ex-command parity (`:magit-push` etc.) | MG.8 | ✅ |
| MG.17a | Transient flags (`--force-with-lease`, `--prune`, …) + preview | MG.8 | ✅ |
| MG.17b | Transient `Argument` items (prompt → back to the menu) | MG.17a | ✅ |
| MG.18 | Hunk-level staging (sliced a–e) | MG.5, MG.13 | ✅ |
| MG.19 | magit-diff side-by-side + `do`/`dp` | MG.18, D.4 | ✅ |
| MG.20 | Operation coverage — reset / revert / cherry-pick | MG.17a | ✅ |
| MG.21 | Remaining operations — bisect, submodule, remotes (tag + merge landed in MG.23c) | MG.17b | ✅ |
| MG.21b | `lattice_vcs::Remote` — list/add/rename/remove/set-url/prune | — | ✅ |
| MG.21c | `magit-remote-mode` — the remote list buffer + its chords | MG.21b | ✅ |
| MG.21d | `M` on the root dispatch opens it | MG.21c | ✅ |
| MG.21e | `lattice_vcs::Bisect` — start/good/bad/skip/reset + state | — | ✅ |
| MG.21f | `BISECTING N left` on magit-status's headerline | MG.21e | ✅ |
| MG.21g | `B` sub-transient, gated on whether a bisect is running | MG.21e | ✅ |
| MG.21h | `lattice_vcs::Submodule` — list/add/update/sync/remove | — | ✅ |
| MG.21i | `magit-submodule-mode` + `o` on the root dispatch | MG.21h | ✅ |
| MG.23k | `D` — re-run a diff/log view with different git arguments | MG.17a | ✅ |
| MG.21a | Diff line-background tints in magit's diff views | MG.20 | ✅ |
| MG.22 | `magit-hunk-mode` — the mode owning diff *content* (chords + `<CR>` ✅; parser / options open) | MG.20 | 🚧 |
| MG.23 | magit-dispatch / file-dispatch parity (a–h done; j and i+ open) | MG.17b | 📝 |

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
                         ├─→ MG.13 (handlers at boot)   ← then this: unblocks
                         │                                  every slice below
                         ├─→ MG.14 (headerline) ─┬─→ MG.15 (stash detail)
                         ├─→ MG.16 (ex-cmd parity)
                         └─→ MG.17a (flags) ─┬─→ MG.17b (arguments)
                                             └─→ MG.20 (operations)
                                                 │
                    MG.5 ─→ MG.18 (hunk staging) ─→ MG.19 (side-by-side)
```

**Recommended order: ~~MG.12~~ → ~~MG.13~~ → ~~MG.14~~ (all done) → the rest.** MG.12 was pure
wiring over machinery that already existed and closed a real safety
inconsistency. MG.13 comes next because every slice after it adds chords,
and until a chord can be pressed in a test they all ship on the blind
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
  - `Index::stage_path`, `unstage_path`, `apply_patch` (MG.18a replaced the
    `stage_hunk` / `unstage_hunk` stubs, which discarded their hunk index
    and staged whole files)
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

#### PICK.1a — navigation is a selection, not a scroll offset ✅ (2026-07-31)

Reported in use: `<C-n>` past the last row kept going, and `<C-p>` then
took a run of presses before anything moved.

**The shape was wrong, not the arithmetic.** `transient_scroll` was a
raw row offset grown with `saturating_add`, and each renderer clamped
it *privately at paint time* — so the stored value drifted arbitrarily
far past anything renderable while the view sat still, and every
phantom step had to be walked back before `<C-p>` did anything visible.
A host-side clamp could not have fixed it: the true maximum is
`row_count - visible`, and `visible` is renderer geometry (the TUI
derives it from the terminal area, GPUI from a fixed budget).

The state is now `transient_selected`, an index over the spec's own
items — a bound the host *can* compute, which is exactly why the
regular picker (`selected` over `candidates`) never had this bug. It
wraps at both ends like `Picker::select_next`, so there is no
out-of-range value to represent. Renderers derive their scroll from the
selection every frame (`TransientSpec::scroll_for`), leaving no scroll
state to drift.

Two things came with it rather than as extras. The selection is
**visible** — a `❯` and a bold label, BMP-block so it works without a
patched font and one cell wide so the columns do not shift; a menu you
navigate but cannot see your position in was the other half of the
report. And `<CR>` fires the selected item, routed through
`do_transient_trigger` by the item's own key rather than a second
activation path, so submenus, flag toggles and argument prompts behave
identically however the item was reached.

`row_count` moved onto `TransientSpec` alongside `scroll_for` /
`row_of_item` / `selectable_count`: both peers had their own copy of
the row arithmetic and both got the group separator wrong the same way
(the bug fixed one commit earlier). One copy now.

- **Tests:** 9 — `row_of_item` skips headers and separators across
  uneven groups; `scroll_for` keeps the selection in view at every
  position × six window sizes and never scrolls past the end; walking
  off either end wraps; six `<C-n>`s on a six-item menu return to the
  top and one `<C-p>` then moves exactly one item (the reported
  symptom, stated directly); the walkers are inert with no transient
  open; `item_at` agrees with `row_of_item`'s ordering, so the marker
  and `<CR>` cannot disagree; and in the TUI, exactly one row is marked
  and it is the selected item, at every selection × four window sizes.
- **Renderer parity:** both peers in the same patch, per the
  lockstep rule.

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

**2026-07-28 — landed ✅.** Contract written up as
[`magit.md` §12.13](../../architecture/magit.md); the set lives in
`crates/lattice-magit/src/confirm.rs` as `DESTRUCTIVE_ACTIONS`.

- **Branch delete / stash drop** now split into ask + `-execute`. The
  ask half performs no git call at all, so "`n` leaves the repo
  untouched" holds structurally rather than by a guard: the code that
  mutates is unreachable from the ask path.
- **Audit of every remaining mutation** turned up one more: magit-rebase's
  `C-c C-k`. `git rebase --abort` discards everything the rebase has
  replayed — same class as discard — and asked nothing. It now asks, but
  *only* when a rebase is actually in progress; with nothing in flight
  `C-c C-k` is just closing a todo buffer nobody ran, and a prompt there
  would be noise. Gated on the same `rebase-merge`/`rebase-apply` check
  that already decided whether `--abort` runs, against a gitdir resolved
  once at activation (the check must be synchronous — the confirm *is*
  the effect the handler returns).
- **Deliberately left un-asking:** stage/unstage (index-only), checkout,
  merge, branch-create, stash apply/pop/create, commit/amend, rebase
  execute — each reversible, or itself the explicit confirm step of a
  dedicated buffer. Matches Emacs magit's `magit-no-confirm` defaults
  (UX convention over local rationale, per the standing rule).
- **Registration guard.** `confirm::ask` debug-asserts its `yes_action`
  is in `DESTRUCTIVE_ACTIONS`, and a test proves both halves of every
  row resolve in the command registry — an unregistered execute half
  otherwise fails at `confirm: unknown action` only when a user presses
  the key. This is the same class of blind spot MG.13 exists to close;
  it does not replace MG.13 (the *bindings* are still untested — these
  guards cover the action names and the ask-half effect, reached by
  calling the handler bodies' helpers directly).
- **Tests:** 10 new (2 branch, 2 stash, 3 rebase, 3 confirm — including a
  vacuity check that `ask` rejects an untabled yes-action). Both rebase
  backends (`rebase-merge`, `rebase-apply`) covered: missing either
  would abort real in-flight work without asking.

### MG.13 — Action handlers at boot, not activation

*(carved as "mode-keymap binding testability (harness)"; the premise
turned out to be wrong — see the correction below.)*

**2026-07-28 premise correction — verified against source.** This slice
was carved on the claim that "mode keymap layers are pushed by mode
activation, which runs through `ModeRegistry::spawn_cascade` (async), so
no synchronous App-level test can observe them." That is **not what the
code does**, and the distinction changes the deliverable:

- **Keymap layers are pushed at boot, not at activation.**
  `translate_mode_keymaps` (`lattice-host/src/keymap_mode_contributions.rs`)
  walks the *whole* mode registry and pushes every mode's layer once, from
  `lattice-ui-tui/src/input.rs:266` and `keymap_insert.rs:90`. The layer
  exists before any buffer opens.
- **Mode-active gating is synchronous.** `activate_major` sets
  `active.set_major(Some(mode))` in its sync prefix, *before* building the
  cascade plan — the source comment says so outright ("so
  `App::active_modes.has_major(mode)` is `true` the moment this call
  returns"). K.1.c's per-keystroke filter therefore passes immediately.
- **Only `on_activate`'s body is deferred** — and that is where magit's
  per-buffer modes (branch / stash / rebase / diff / commit / log)
  register their action handlers, because those handlers close over an
  `Arc<Mutex<…State>>` built during activation.

So a chord in a magit buffer **resolves** synchronously and finds **no
handler** until the cascade completes. That is precisely the MG.8 failure
(`MagitGlobalMode` registered handlers from `on_activate` behind a
`OnceLock`; the fix moved them to `Mode::action_handlers()`, synchronous
at boot). The blind spot is real — it is just one layer lower than
written, and "prove the layer is present" is the wrong test.

- **Deliverables:** a seam that makes an `on_activate`-registered handler
  deterministically observable from a test, so `press(chord)` resolves
  *and dispatches* end-to-end. Three candidate shapes, evaluated when the
  slice executes — see the session notes: (A) a test-only synchronous
  activation path, (B) a harness helper that pumps the runtime until the
  cascade's existing `MajorEntered` / `MinorEntered` /
  `ModeActivationFailed` signal lands, (C) move per-buffer handler
  registration to the synchronous `Mode::action_handlers()` seam with
  per-buffer state looked up by `BufferId` — attacking the root cause
  rather than the symptom, and the shape the mode-ownership standing rule
  already points at.
- **Tests:** a magit chord whose handler is registered only by
  `on_activate` fires via `press()`; the guard fails if the handler is
  absent (not merely if the layer is).
- **Sequencing:** worth doing BEFORE the feature slices below — each one
  adds chords that would otherwise ship on the same blind spot.

**2026-07-28 — landed ✅.** Option (C) was
chosen over the two test-seam options: the dead-chord window is a
*production* defect (a user pressing `d` quickly after `:magit-branch`
gets nothing), so a harness that observes or waits out the window is
instrumentation for a bug rather than a fix. Contract:
`crates/lattice-magit/src/buffer_state.rs`.

- **The shape.** Per-buffer modes register handlers via
  `Mode::action_handlers()` (boot-time, app-lifetime) and resolve their
  state through a `BufferStates<S>` service keyed by `BufferId` at call
  time. `on_activate` publishes state **above its first `.await`** —
  `spawn_cascade` polls the cascade future once synchronously on the App
  thread before spawning, so pre-await work lands before
  `activate_major` returns. That is what makes the handler *and* its
  state reachable on the very next keystroke.
- **Second defect, in the harness.** `test_helpers::press` passed
  `ActiveModes::minors()` where production passes `keymap_gated_ids()`
  — which includes the **major**. The harness was structurally unable to
  route ANY major-mode chord, so no test could have caught the first
  defect, nor a broken major-mode binding in oil / compilation /
  ai-conversation. Fixed; full TUI suite (1598 tests) green after.
- **Third defect, forced by the fix.** `action:magit-refresh` (`gr`) had
  **five** registrants. Per-activation registration hid the collision —
  one installed at a time. Boot registration does not:
  `ActionHandlerRegistry::register` inserts (last wins), and dropping a
  registration unregisters *by action id*, so a second registrant both
  shadows the first while active and deletes it on deactivation. Fixed
  with polymorphism rather than a central `match`: `magit-core-mode`
  (which binds `gr`) owns the single boot handler and dispatches through
  a new `MagitView` trait; each view mode publishes its own refresh
  body. Guarded by a test that fails if any two modes contribute the
  same boot `action_name`.
- **Every magit mode migrated.** State lives in `BufferStates<S>`
  services keyed by `BufferId`; handlers are contributed by
  `Mode::action_handlers()` and registered once at boot. **Zero
  `handlers.register(...)` calls remain in the crate** — that grep is
  the cheapest check that this has not regressed.
  `magit-commit` and `magit-rebase` exercise the late-resolved-field
  path — `diff_end_line` publishes as `0` (so `<CR>` declines rather
  than acting on a diff region that does not exist yet) and `upstream`
  publishes empty (which `confirm` already refuses to rebase onto).
  `magit-core-mode` needed no state at all: its handlers read the
  buffer through `BufferStoreHandle` + `ctx.buffer_id`.
- **Shared actions** — `gr`, `s`, `u` — are owned by `magit-core-mode`,
  which holds their single boot registration and dispatches per buffer
  through `MagitView`. The *binding* stays with whichever mode offers
  the chord (`s`/`u` are bound by status and diff, not by core), so a
  buffer whose mode does not bind `s` never routes one.
- **Two live bugs fixed as a consequence**, both pre-existing and
  neither introduced by this slice:
  - **`q` in `*magit:status*` was nondeterministic.**
    `action:magit-close` was registered by both `magit-status-mode`
    (`Effect::BufferDelete`) and `magit-core-mode`
    (`Effect::DismissPopup`). Same action id ⇒ last registrant won,
    decided by cascade ordering. Removed the status-side registration.
    This is a determinism fix, **not** a behaviour choice:
    `DismissPopup` was already the documented and tested intent —
    `q_on_magit_status_buries_it_and_never_quits_the_editor` (in
    `lattice-ui-tui`) asserts `q` restores the buffer that was active
    before magit-status opened, and core's handler records the
    live-reported bug (`q` quitting the whole editor) it fixed. The
    duplicate contradicted that test and voided its guarantee whenever
    it won the race.
  - **`s`/`u` across status and diff**, and **core-mode's own chords
    with two magit buffers open** — both collapsed the same way. The
    old `ActionRegsGuard` doc comment already described this hazard
    ("firing the chord in buffer A can execute buffer B's captured
    state against A's cursor"); holding the tokens bounded the damage
    but could not prevent it, because the registry has no buffer
    dimension. Boot registration removes it at the source.
- **Tests:** 5 chord-level tests in
  `lattice-ui-tui/src/app/magit_bindings.rs` (the first tests anywhere
  that press a magit chord) — including a negative case proving `c`
  does not fire outside a magit buffer, a guard on the harness fix
  itself, and a service-registration guard that already caught a real
  omission (`magit-log-mode`'s missing slot). Plus, in `lattice-magit`:
  a duplicate-boot-handler guard (which caught the `magit-close`
  collision above), a shared-action-ownership guard, and 4
  `BufferStates` unit tests.

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

**2026-07-28 — landed.** `crates/lattice-magit/src/headerline.rs`: one
`Headerline` impl, ten per-view field builders. The row is compact and
symbol-led (colour carries field identity, no `Head:` labels) — matching
the two headerlines lattice already ships rather than Emacs magit's
in-body header lines. Design: `../../architecture/magit.md` §4.11.

Choices worth recording, each on merit rather than on the shape of the
existing code:

- **Fields ride the existing builder.** `build_and_format`,
  `build_branch_list`, `build_stash_list`, `run_log` and peers now
  return their header fields alongside their text, so the row is a
  byproduct of work already done — no second refresh path to keep in
  sync, and `gr` / mutation refreshes update the header for free
  (paramount goal #1). Two honest exceptions: magit-revision runs one
  `git show -s --format=…` for author/date/subject (`--format` rather
  than scraping `git show`'s locale-dependent header), magit-commit one
  `rev-parse --abbrev-ref HEAD` — both inside a `spawn_blocking` that
  was already running git.
- **Theme-live, unlike its two predecessors.** Colours resolve inside
  `render()` and the theme's resolved version folds into the row's, so
  `:colorscheme` repaints the header. compilation and ai-conversation
  capture `u32`s at activation and go stale; heuristic #1 says take the
  better shape rather than copy the incumbent. Cost measured at 26ns per
  tick (bench below) — the read-lock this adds is not the hot path.
- **Two mode-owned theme elements.** `magit.headerline.alert` and
  `magit.headerline.label` are registered by the mode, not added to
  `lattice-theme`'s builtin block. The five MG.11 `magit.*` elements
  stay builtin because `lattice-syntax`'s styled-span table resolves
  them by builtin id — the header's own two have no such constraint, so
  mode ownership applies.
- **`branch_status_line()` deleted**, not wired: it flattened branch +
  ahead/behind into one `String`, and the header wants per-role coloured
  fields. `headerline::status_fields` renders the same data from the
  same `SectionIndex`.

- **Tests:** 22 in `headerline.rs` — a per-view test for each of the ten
  builders (each asserts the row is non-empty AND carries that view's
  identifying field), the no-work-per-tick guarantee both ways
  (identical fields do not bump; a changed refresh bumps exactly once),
  hide-when-empty, per-role colouring, and the teardown contract. Plus
  one in `magit_bindings.rs` that opens a real `*magit:status*` and
  reads the row back through the provider registry the cells worker
  reads — the wiring no pure test can see.
- **Bench:** `crates/lattice-magit/benches/headerline.rs` —
  `version()` 26ns (per tick), `render()` 581ns (only on change), an
  unchanged `set()` 129ns.

**Follow-up found, not fixed here (host, not magit):**
`Editor::do_buffer_delete` removes a buffer from the registry but never
removes its `active_modes` entry, so **no mode's `Guard::drop` runs on
`:bd`** — not magit's fold source, `MagitView`, or `BufferStates` entry
(MG.13), not ai-conversation's headerline + subscription, not diff
mode's fold sources. `gc_ephemeral_buffer` and
`dismiss_stale_popup_registry` both clear it; `do_buffer_delete` is the
outlier. Buffer ids come from a monotonic counter and are never reused,
so the effect is a bounded leak rather than a stale row over a live
buffer — which is why MG.14's teardown coverage is a unit test on
`HeaderlineRegistration::drop` rather than a `:bd` integration test.
The fix belongs in a host slice: it changes every mode's lifecycle at
once and wants its own test pass.

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

**2026-07-28 — landed, and it found a live bug first.**

`magit-stash`'s list rendered `  <message>` per row, while
`stash_index_at_cursor` — the function EVERY chord in that buffer calls
to find out which stash it is acting on — parsed `stash@{N}`. So `a`
(apply), `p` (pop) and `d` (drop) all resolved `None` and silently did
nothing: no error, no effect, indistinguishable from an unbound key.
`<CR>` would have shipped dead the same way. The same class as MG.6's
dead `<CR>` and MG.8's inert transients: a line format with a writer and
a reader that were only ever tested apart. `highlight::stash_styled_spans`
had even *documented* the mismatch ("no `stash@{N}` prefix in the CURRENT
buffer format") and treated it as intended, and the MG.12 confirm test
built its example row as a hand-written literal in the format the author
believed was rendered rather than calling the renderer.

Fixed by giving the list the `stash@{N}` label — matching the stash rows
magit-status already renders, so a stash reads the same in both views,
the drop prompt (`Drop stash@{2}?`) names what is on screen, and
`stash_styled_spans` stops being a no-op (same Keyword/Number/Comment
split `sections.rs` uses). `list_row` is now the single writer and
`parse_index` its single reader, with a round-trip test spanning them
plus an inverse test proving the old format is exactly what the parser
cannot read.

- **New mode:** `magit-stash-show-mode` (`crates/lattice-magit/src/
  magit_stash_show_mode.rs`) — `git stash show -p stash@{n}`, read-only,
  no mode-specific chords (`q`/`gr`/nav from magit-core), MG.14
  headerline showing `stash@{n}` + the stash's subject. Fixed-content
  like `magit-revision-mode`, so `gr` is a deliberate no-op.
- **Cross-cutting guard added:** `every_chord_every_mode_binds_reaches_a_
  registered_action_and_a_handler` walks every chord of every magit mode
  and asserts all three links — the `cmd:` names a registered action
  command (the MG.8 failure), some mode contributes a boot handler for it
  (the MG.13 failure), and no shared-action collision. Each prior slice
  bolted a bespoke test onto one link after a bug shipped through it;
  this covers all three for every chord at once, so the next chord added
  is covered by construction. It passes today, i.e. no OTHER magit chord
  is currently inert.
- **Tests:** 8 new in `lattice-magit` (round-trip + inverse + non-row
  lines, buffer-name parse/build agreement, 4 styler cases) plus the
  cross-cutting guard, and a chord-level test in `magit_bindings.rs`.

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

**2026-07-28 — landed.** The shared body is `magit_global_mode::
spawn_remote_op(RemoteOp)`, and the operation itself is a `RemoteOp`
constant (`PULL` / `PUSH` / `FETCH` / `STASH`) naming its argv and its
echo verb in exactly one place. The transient item and the ex-command
both resolve the same constant and call the same function, so the two
surfaces cannot drift in argv, in the `GIT_TERMINAL_PROMPT=0`
fail-fast handling, in the optimistic echo, or in how the outcome is
logged. The `remote_op!` macro that previously inlined the whole body
per item now only pushes the contribution.

`:magit-stash` creates a stash and `:magit-stash-list` opens the list —
mirroring Emacs magit's `z z` / `z l`, where the bare stash key is the
create. One name is a strict prefix of the other, which is safe:
`resolve_command_name_or_alias` is exact-match-then-alias with no prefix
fallback, and a test pins that the two resolve to distinct `CommandId`s
(a prefix fallthrough would make `:magit-stash` silently open the list
instead of stashing — a wrong action, not an error).

- **Tests:** both-surfaces-exist for all four operations, distinct-argv
  per `RemoteOp`, the prefix-distinctness guard, and a boot-level check
  in `magit_bindings.rs` that the commands reach the registry a real
  editor booted with (the unit tests build their own registry, so they
  prove `register_ex_commands` is correct, not that `install` calls it).
  None of them *execute* an operation: `:magit-stash` would stash the
  suite's own working tree and the other three would hit the network.

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

**Split into MG.17a (flags, done) and MG.17b (arguments) — the two
halves have very different risk.** Flags are self-contained; arguments
need a prompt round-trip that returns to an open transient, which is a
genuinely new interaction shape.

### MG.17a — transient flags + preview ✅ (2026-07-29)

**The blocker the plan didn't name.** `Argument` being a no-op was
known. What wasn't: even a *working* `Flag` toggle could not reach the
handler. `ActionContext` carried `buffer_id` / `cursor` / `services` /
`events` / `prompt_value` and no arguments, so a handler had no way to
ask "was `--force` set?". And it couldn't simply gain the state:
`TransientState` lives in `lattice-picker`, which `lattice-mode` (home
of `ActionContext`) does not and should not depend on.

**Resolved through `Args`,** the currency both crates already share and
the one ex-commands carry. `ActionContext` gained `args: Args`;
`do_transient_trigger` projects the toggled state onto the action's
`args_schema` positionally. That makes MG.16's "one body, two
front-ends" literal — `:magit-push --force-with-lease` and toggling
`-f` in the menu produce the *same* `Args::List` and run the same
`spawn_remote_op`. A test pins that equality directly, since it is the
claim the slice rests on.

**One table, four consumers.** `RemoteOp::flags` (a `&[RemoteFlag]` of
name / git-arg / key / doc) is read by the ex-command's `args_schema`,
the transient's `Flag` items, the preview string, and the argv builder.
Adding `--prune` to fetch is one row. This shape was chosen because
this crate has now produced the same class of bug three times (MG.6,
MG.8, MG.15) — a format with independent writers and readers that
drifted.

- **Shipped flags:** push `--force-with-lease` / `--set-upstream`,
  fetch `--all` / `--prune`, stash `--include-untracked`.
- **`--force-with-lease`, never bare `--force`** — it refuses when the
  remote moved since your last fetch, which is exactly when a bare
  force destroys someone else's commits. Pinned by a test; Emacs magit
  defaults the same way.
- **Push and fetch became submenus** to hold their flags; a flat menu
  fires on keypress, leaving no moment to toggle. One extra keystroke,
  in exchange for the flags existing. Pull deliberately stays a direct
  action — `--ff-only` is not optional, so it has no flags to hold.
- **Preview** is generated from the same table as the argv, and a test
  asserts the two agree across every flag combination. A preview that
  can disagree with what runs is worse than none before a force-push.

**The MG.8 guard needed teaching.** `every_root_dispatch_item_resolves_
to_a_real_action_not_a_flag_fallback` treated *any* `Flag` as an inert
placeholder, which was true when the only Flag was the unresolved-id
fallback. It now excludes flags declared in a `RemoteOp::flags` table —
precise without a hand-maintained exception list, since a placeholder
is named after the action it failed to resolve and can never match.

**Not shipped:** log's `--all` / count / path-filter. Log opens a
buffer whose scope rides its *name* (`*magit:log:<path>*`), so its
flags need that channel rather than the action-args one — a different
mechanism, deferred with MG.17b rather than bolted on.

- **Tests:** 8 in `lattice-magit` (argv per flag combination,
  argless-invocation regression, preview/argv agreement,
  force-with-lease pinning, schema/table alignment, both-front-ends-one-
  `Args`, unknown-token tolerance, flagless-op passthrough) + 3 in
  `lattice-host` driving the real projection (schema order beats map
  order, empty schema ⇒ `Args::None`, unset slot ⇒ declared default).

### MG.17b — transient arguments ✅ (2026-07-29)

`TransientItemKind::Argument` was a literal no-op in
`do_transient_trigger`. Now: press it, type a value, land back in the
menu with the value in the preview.

**What the slice is actually about.** A transient already *had* state —
`picker.transient_state` persists across flag toggles, and the preview
closure re-reads it every frame, which is why MG.17a's toggles update
live with no extra machinery. What state could not survive was a
**surface switch**: the prompt takes the editing buffer and the modal
state, so the picker is torn down. `Editor::pending_transient_argument`
owns the menu across the gap — spec, state, *and* the parent stack, so
an argument inside a submenu returns to that submenu rather than the
root.

- **`<Esc>` cancels the argument, not the menu.** Dropping the user out
  entirely would punish a typo by making them re-navigate.
- **An empty submit clears the argument** rather than storing `""`.
  `git stash push -m ""` labels a stash with nothing, which is worse
  than no label, so the empty string must never reach argv.
- **The prompt seeds from the current value**, so re-editing beats
  retyping.
- The submit path checks the parked menu *before* the action lookup,
  and the argument path deliberately registers no submit action —
  naming one would be a lie, since there is no handler to fire.

**Shipped consumer: `git stash push -m <message>`.** Deliberate — a
mechanism with no consumer is untested by construction, and an
unlabelled stash is findable only by position while positions renumber
on every drop. `RemoteFlag` grew a `kind` (`Flag` | `Value { prompt }`)
so the one table still feeds all four consumers; `argv` emits `-m
<text>` as two entries so a multi-word message survives as one
argument.

**Both guards needed teaching again, not silencing.** `Argument` had
been "unexpected item kind" in the inert-items walker; it is now
legitimate exactly when its name appears in a `RemoteOp` table — same
rule as `Flag`, so an argument nothing consumes still fails.

**Not shipped:** log's `--all` / count / path-filter. A log buffer's
scope rides its *name* (`*magit:log:<path>*`), so its arguments need
that channel, not this one.

- **Tests:** 3 in `lattice-magit` (unset value contributes nothing;
  multi-word message stays one argv entry; preview quotes it) and 4 in
  `lattice-host` driving the real resume path (submit writes the value
  and preserves flags toggled beforehand; cancel restores unchanged;
  empty submit clears; resume declines with nothing parked, which is
  what tells the submit path the prompt belonged to an action).

### Live bug — stale content after `q` ✅ (2026-07-29)

**Reported:** `q` in magit-status returned to the previous buffer but
the screen kept painting magit's content. Opening the command line
showed the file; `<Esc>` brought the stale content back; a forced
redraw did nothing.

**Root cause.** `open_synthetic_buffer` opens a full-pane buffer
through `activate_buffer`, which swaps the pane, the active-document
handle (`Editor::document`), the snapshot cache and the syntax state
together. magit's `q` returned via `Effect::DismissPopup` →
`dismiss_popup`, which hand-restores only `pane.buffer_id` /
`active_buffer` / cursor / scroll / modal. That restore is right for a
*popup* — a popup floats over the pane, so the document was never
swapped out — and wrong for a magit view, where it was.

So the pane named the file while `Editor::document` still held magit's
rope. The active render path paints from the document handle; inactive
panes render from registry snapshots, which is exactly why the command
line showed the right thing and `<Esc>` undid it; and a redraw could
not help because the *data* was stale, not the paint.

`document_buffer_id == pane_tree.active().buffer_id` is an **invariant**,
not two independent pieces of state: `Editor::document` is a live
handle cached so the keystroke path never does a registry lookup
(paramount goal #1), and `document_buffer_id` is its key. Panes are
many; the active document is one. This was a plain cache-invalidation
bug.

**Fix.** `Effect::BuryBuffer` + `Editor::bury_buffer`, distinct from
`DismissPopup` because the two are genuinely different operations —
dropping an overlay versus giving a pane its buffer back. `bury_buffer`
returns *through* `activate_buffer` rather than restoring a subset by
hand, so it cannot drift out of step with what opening does. No WIT
mirror yet (typed error at the boundary, the shape `Effect::Global`
uses); GPUI got its arm in the same patch per the parity rule.

**A test-harness hole this exposed, worth more than the bug.** Mode
activation runs as a spawned cascade — only the major lands
synchronously — and its results are applied by `run_tick_pending`,
which production reaches at `App::apply`'s tail, i.e. on the *next
keystroke*. A test that opened a buffer and only slept never applied
another action, so the cascade completed and was never drained.

Consequence: `magit-core-mode` looked inactive in every test, which
means **no test could reach `q`, `gr`, `]]`, `[[`, `]f`, `[f`, `]c`,
`[c`, `TAB` or `S-TAB` in any magit buffer** — the exact blind-spot
class MG.13 existed to close, reopened one layer down. `test_helpers`
gained `settle` / `settle_mode`, and the end-to-end `q` test uses them;
verified non-vacuous by reverting the fix and watching it fail with the
reported symptom (pane restored, document still magit's).

### MG.18 — Hunk-level staging (sliced 2026-07-29)

Design fragment:
[`../../architecture/magit-hunk-staging.md`](../../architecture/magit-hunk-staging.md).

| Slice | Scope | Depends | Status |
|---|---|---|---|
| MG.18a | Delete the `stage_hunk` / `unstage_hunk` stubs; add `Index::apply_patch` + `Repository::run_git_stdin` | — | ✅ |
| MG.18b | Hunk parser + patch synthesizer as pure functions, round-trip tested | MG.18a | ✅ |
| MG.18c | `s`/`u`/`x` resolve hunk-at-cursor, file-level fallback preserved | MG.18b | ✅ |
| MG.18d | Re-expand the entry + restore cursor to the next hunk after a mutation | MG.18c | ✅ |
| MG.18e | Region (visual-mode) staging — the hunk-splitting rewrite | MG.18c | ✅ |

MG.18a is independent of the hunk-identity choice and removes a
live landmine (the stubs silently stage whole files), so it lands
first regardless. Value arrives at MG.18c; MG.18d is not optional
polish — at hunk granularity, losing the user's place after every
stage defeats the feature.

**All five landed (2026-07-29 / 30).** Two follow-ups were left out
deliberately and are recorded in MG.18e: a region spanning several
hunks, and `apply_patch --index` so `x` on a staged hunk can discard
from index and worktree atomically instead of refusing.

#### MG.18c — `s` / `u` / `x` on the hunk at the cursor ✅ (2026-07-29)

Partial staging works. Put the cursor in a hunk — in magit-status's
inline diff or in a scoped `*magit:diff:*` buffer — and `s` stages that
hunk alone, `u` unstages it, `x` discards it after confirming.

**The resolution lives in `magit-core-mode`, not in each view.** A hunk
is a property of diff *text*, identical in every magit buffer — the
same argument §7.5 makes for `]c` / `[c` living there. `MagitView`
keeps only what genuinely differs per view: the file-level fallback,
and which tree the text was diffed against. A future view showing a
diff gains hunk staging by binding the chord, with no new code. Not a
`Document` trait method: the consumer is magit's own handler, not
generic host machinery (the substrate-vs-mode-helper rule).

**`MagitView::diff_source` — the new seam, and why it earns its place.**
Both `s` and `u` could have been left to fail loudly at `git apply`,
per the design's original table. `x` could not: on a **staged** hunk,
`git apply --reverse` against the worktree *succeeds* and removes the
change from the file while leaving it in the index — it disappears from
the buffer and is still committed by the next `cc`. So the operation
asks first. magit-status answers from the section header above the
cursor, magit-diff from the scope in its buffer name; a mismatch is
refused with what to press instead ("that hunk isn't staged"), not with
git's "patch does not apply".

`None` — `*magit:diff*` against HEAD (its hunks mix both sides), and a
commit's or stash's inline patch — refuses hunk staging **without**
falling through to file level. Falling through would turn a keypress
aimed at one hunk into staging the whole file, the same silent
over-staging MG.18a deleted the stubs over.

**Three parser corrections the buffer path exposed**, none reachable
while MG.18b's only callers were tests that placed the cursor inside a
hunk by construction:

- **Containment, not proximity.** `hunk_at` returned the hunk *above*
  any cursor, so `s` on the file entry below an expanded diff would
  have restaged that hunk instead of staging the file — the file-level
  fallback would never have fired in magit-status.
- **`\ No newline at end of file` after the last body line** is reached
  only once the declared counts are satisfied, so the body loop never
  saw it. Dropping it tells git to append a newline the file does not
  have.
- **Trailing whitespace was trimmed** off every stored line. `git apply`
  matches context byte-for-byte, so any diff touching a
  whitespace-dirty region produced a patch git refuses.

**Reads the hunk, not the document.** `hunk_at_with` pulls lines
through an accessor and stops at the `@@` header's counts, so
resolution costs ~0.63 µs whether the buffer holds 200 lines or 50,000
(`benchmarks.md` §MG.18c). The obvious simplification — collect the
buffer, slice it — is shorter, passes every correctness test, and puts
an O(document) copy on the actor thread for one keypress.

- **Tests:** 10 new. 6 wiring cases through a real document actor and a
  published view (Ready / FileLevel / three refusals), 5 parser cases
  (containment, the trailing marker, trailing whitespace, two
  `display_location` forms), the header→source and scope→source
  mappings, the apply-flag table, and a git round-trip proving discard
  reverses the worktree and leaves the index alone — the pairing with
  no second chance.
- **Not shipped:** the cursor lands wherever the refresh puts it
  (MG.18d), and region staging is MG.18e. `x` on a staged hunk refuses
  rather than discarding from index + worktree; that wants
  `apply_patch --index`; MG.18e shipped without it (see there).

#### MG.18d — the buffer and the cursor survive a mutation ✅ (2026-07-29)

Stage a hunk and the file's diff is still open, with the cursor on the
hunk that took the staged one's place. Staging four of a file's six
hunks is now four keypresses, not four keypresses and four searches for
your place.

**The rebuild carries the expansions.** `build_and_format` takes the
open entry keys and inlines their diffs into the text it builds, rather
than letting the refresh collapse everything and re-expanding
afterwards. One edit, one span vector, no splice arithmetic against a
buffer being replaced underneath it, and no collapse-then-expand
flicker. Counts are recomputed from the text actually written — a
carried-over count would collapse the wrong rows once the diff got
shorter. `gr` benefits identically: a manual refresh no longer throws
away every diff you had open.

**The cursor is restored by identity, not by row.** A mutation records
the file, the side, and the hunk's **ordinal** within that file.
Staging hunk *k* removes it, so ordinal *k* then names the next
remaining hunk — magit's behaviour and the restore rule are the same
arithmetic. Clamped to the last hunk when the final one was staged;
silent when the anchor is gone entirely (the file left its section),
because leaving the cursor where the refresh put it beats guessing at a
row. Naming the anchor stays per-view behind `MagitView`
(`refresh_restoring`): an entry row in magit-status, a `diff --git`
header in a diff buffer.

**Two host-side facts this needed, and one new effect.**

- **It must not wait for a keypress.** `run_tick_pending` runs from
  `App::apply`'s tail *and* from the actor's `async_landed` arm; a bare
  `tick_callback` has no wake of its own, so its results sit until the
  user presses something. The position travels on
  `SubsystemBoot::inbound`, whose `send` fires the wake from inside the
  sender. Now a standing rule in CLAUDE.md — it has been re-introduced
  enough times to earn one.
- **It must name its buffer.** Two git calls run before the position
  exists; a `q` in that window would land a bare `CursorMove` in
  whatever the user moved to. `Effect::CursorMoveIn { target, position }`
  (its own commit) is applied only while `target` is focused.

- **Tests:** 8 pure cases for the restore rule (next-hunk, clamp,
  collapsed entry, an entry not adopting its neighbour's diff, vanished
  anchor, the staged/unstaged split for one path, diff-buffer anchoring,
  hunk lists not running into the next file), 3 ordinal/`file_path`
  cases, 3 git round-trips proving an expansion survives a rebuild with
  a count equal to exactly the rows it added, and the wake test —
  verified non-vacuous by reverting the bus to a `tick_callback` and
  watching it fail on the message that names the bug.
- **Not shipped here:** region staging, which landed as MG.18e below.
  The cursor is not restored after a *file-level* stage — the entry is
  gone from its section by definition, and the anchor rule correctly
  declines.

#### MG.18e — region staging ✅ (2026-07-30)

Select lines inside a hunk, press `s` / `u` / `x`, and only those lines
move. Emacs magit's most-used partial-stage path, and the last piece of
MG.18.

**The rewrite is the slice.** A region is not a smaller hunk — the body
is rewritten so the patch still describes a complete transformation:
selected `+`/`-` stay, unselected `+` are **dropped**, unselected `-`
become **context**, and both counts are recounted from the result.
Reversed (`u` / `x`) the roles swap exactly, so it is one function with a
direction flag. The asymmetry is not a convention: applying forward the
target holds the old side, so an unselected `+` is not there and must
not appear, while an unselected `-` is there and survives.

Both `@@` **start** lines are kept verbatim — whichever side the target
matches is preserved line-for-line by those rules, and the other side's
start is not something git checks. The counts are what git validates
("corrupt patch"), which is why two round-trips against real git carry
the proof rather than the unit table alone.

**`ActionContext::selection`** is how the handler sees the region: the
Visual/Select extent, normalised, on the host-built context object.
Design §5.2 already says "Visual mode IS the active region" and makes
`Range::Selection` the default range argument for ex-commands; this is
that reaching mode action handlers. No visual kind — a diff line is the
unit of every consumer so far. No WIT impact: the mirrored
`ActionContext` is `lattice_grammar`'s, a different type.

**The transient path had to carry it too**, and this is the bit worth
remembering: `Effect::Confirm` opens a transient, so a destructive
action's execute half re-resolves through the transient-item context.
With `selection: None` there, a region `x` would have asked about 2
lines and then discarded the whole hunk — a silent escalation of the one
action that asks first. Safe to carry because `open_transient` touches
neither the modal state nor the anchor, and the transient owns every
keystroke while open (the same argument §12.13's cursor re-resolution
already rests on).

**Scoped to one hunk.** The region is intersected with the hunk under
the cursor; magit's multi-hunk region needs a multi-hunk patch builder,
so the echo names the count (`staged 3 lines of src/main.rs:42`) instead
of implying more. An empty selection is refused *before* the
staged/unstaged gate — the rows were picked deliberately. Acting on a
region ends Visual, like any vim operator on a selection.

- **Tests:** 9 pure rewrite cases (all-selected byte-identical to the
  whole hunk, context-only refused, adds-only, removes-only,
  interleaved, the reverse mirror, over-wide clamp, header suffix
  preserved, a `\ No newline` marker dropped with its line), 2 git
  round-trips (stage and unstage a single line of a two-change hunk),
  5 wiring cases through a real buffer, 3 for the echo/Visual-exit
  shape, and 3 for the host's `active_region` (normalised either way,
  none outside Visual, Select counts).
- **Deferred, deliberately:** a region spanning several hunks; and
  `apply_patch --index` so `x` on a *staged* hunk could discard from
  index and worktree atomically instead of refusing (MG.18c's note).
  Both are additive and neither blocks MG.19.

### MG.23 — magit-dispatch / file-dispatch parity (planned 2026-07-30)

Goal: every `magit-dispatch` and `magit-file-dispatch` entry that means
something in lattice, reachable from `C-c g` / `C-c f`. Pulled from
magit's own source (`lisp/magit.el`, `lisp/magit-files.el`), not from
memory.

**Scale, honestly.** ~39 entries across the two menus. ~30 need an
operation written. That is comparable in surface area to everything
MG.1–MG.21 shipped, so this is a phase, not a slice.

#### Policy decisions (2026-07-30)

1. **No inert rows.** A row appears only once its operation exists —
   the rule already in `transients.rs`, reaffirmed. Not greyed
   "coming soon" entries: that needs a `TransientItemKind::Unavailable`
   plus renderer support in both peers, and becomes dead weight the
   moment the last operation lands. Magit itself hides entries by
   predicate rather than disabling them, which is the same shape.
   **Keys still follow magit / evil-collection-magit from day one**, so
   a row landing later lands in the slot muscle memory already expects.
2. **`C-c f` always acts on the visited file.** No "which file?"
   prompt, which is magit's behaviour and the one deliberate deviation
   here. Destructive actions still **confirm** (`Delete src/foo.rs?`) —
   that is MG.12's ask/execute contract and a different axis from
   choosing a target.
3. **An explicit-target variant exists, surface still open.** Either
   `magit-other-file-dispatch` (a second menu whose rows ask for the
   file, unbound by default) or magit's own convention — the **capital
   of the same key in the same menu**, which is what magit-file-dispatch
   actually does: `d` "Diff" on the visited file vs `D` "Diff..." which
   asks; likewise `l`/`L` and `b`/`B` (the `...` is magit's mark for
   "prompts"). The capital-pair version needs no second menu and no new
   machinery. Not decided; **MG.23a is unblocked either way** because the
   seam is the same.

#### Deferred: a global `<C-u>` prefix mechanism (2026-07-30)

Wanted as a core feature, not a magit one — deferred, because magit does
not need it (the capital-pair or argument spellings cover the
explicit-target case). What the investigation established, so it is not
re-derived:

- **`<C-u>` is already bound in all three modes**: Normal scroll
  half-page up, Insert delete-to-line-start (readline/vim), Command
  clear the command line. All three are muscle memory.
- **A key with a standalone meaning cannot also be a zero-latency
  prefix.** Vim lives with this via `timeoutlen`; here it would mean
  half-page scroll waits on a timer. There is currently **no
  ambiguous-chord timeout at all** — `Action::AbsorbPartialChord` pushes
  onto `Editor::partial_chord` and waits for the next keystroke
  indefinitely, so a chord that is *both* terminal and a prefix has no
  resolution mechanism to hang the feature on. That machinery would have
  to be built first.
- **Emacs `C-u` ≈ vim's count.** `C-u` means "numeric argument 4"; the
  count is already plumbed (`pending_count` / `op_count`) and the
  grammar's `ActionContext` already carries one. The gap is that
  `lattice_mode::ActionContext` — the one mode handlers see — does not
  expose it. Whatever this becomes, it should be *one* prefix-argument
  concept and not a second one alongside counts.
- Likely shape when picked up: the mechanism core (a prefix-argument
  state, an `action:universal-argument` that sets it, exposure on both
  `ActionContext`s, cleared after the next command), with the **key a
  user choice** rather than one imposed — so `<C-u>` is available to
  anyone willing to accept the scroll latency without charging it to
  everyone.

#### The target seam

File actions resolve their target as **`ctx.arg_str(0)` if set, else
`active_file(ctx)`**. `C-c f` passes no argument, so it keeps acting on
the visited file; `magit-other-file-dispatch` fills the argument. Both
reuse MG.17a/b's transient argument machinery unchanged, and a future
universal-prefix would set the same argument rather than needing its own
mechanism.

#### Inventory

**Backed by an operation today, row missing.** `S` stage-all,
`U` unstage-all. (`A` cherry-pick / `_` revert / `O` reset are NOT free
despite MG.20 landing them: they act on the commit at the cursor, and
the root dispatch has no cursor context — see MG.20's own note. They
need either a commit picker or magit's `:if-derived` predicate.)

> **2026-07-31:** `S` / `U` landed in MG.23b. The parenthetical's
> either/or was resolved by checking magit rather than by choosing:
> magit puts these three in its **ungated** group because they prompt
> for a commit, so the answer is the picker, not the predicate. MG.23j.

**Maps to an existing lattice surface — wire, do not reimplement.**
`h` / `C-x i` → `:help`; `C-x m` → `:describe-mode`; `H` → the
`:describe-*` family; `e` / `E` ediff → lattice's own `diff-mode`
(D.4 / MG.19), not a reimplementation; `J` / `G` → `:ls` / `:b`;
`j` → `:magit-status`.

**Cheap-to-medium on machinery that exists.** `a` apply / `v` reverse
(MG.18's `apply_patch`); `t` tag, `i` gitignore, `I` init, `m` merge,
`Q` git-command (MG.17b's `Argument` prompt); file `untrack` / `rename`
/ `delete` / `checkout`; blame variants (`r` removal, `f` reverse,
`m` echo, `q` quit); blob `p` / `n` / `v` / `V` (magit-file-revision-mode
already exists); `D` / `L` diff- and log-arg refresh; `M` log-merged;
`e` edit-line-commit.

> **2026-07-31 — what is left of this bucket.** Landed: apply /
> reverse (MG.23g, as `a` / `-`), tag + gitignore (MG.23c1), init +
> merge (MG.23c2), the four file ops (MG.23d / MG.23d2), blob nav
> (MG.23f) and reverse blame (MG.23f2). Dropped after evaluation:
> `Q` git-command (`:terminal` covers it), and the `m` echo / `q` quit
> blame variants (no surface / duplicate). Still open: `r` blame
> removal, `D` / `L` diff- and log-arg refresh, `M` log-merged,
> `e` edit-line-commit.
>
> **2026-08-02:** `D` / `L` landed as **MG.23k**, merged into a single
> `D` (§8.10) because `L` is a motion we protect. Still open: `r` blame
> removal, `M` log-merged, `e` edit-line-commit.

**Genuinely new subsystems.** ~~`B` bisect~~ (MG.21e/f/g), ~~`M` remote
management~~ (MG.21b/c/d), ~~`o` submodule~~ (MG.21h/i), `O` subtree, `Z` worktree, `T` notes, `w` am /
`W` format-patch, `y` show-refs, `Y` cherries, `C` clone.

#### Known key collision — resolved by MG.23h, and it was mis-stated

This section used to read: "Our dispatch uses `s` for status; magit
uses `s` for **stage** and reaches status via `j`", framing it as a
collision to resolve once context-dependent menus existed.

**Checked against magit's source, that was wrong in both halves.**
`magit-dispatch`'s ungated group has no `s` at all — magit.el:328
carries `;; s ↓` as a placeholder comment pointing at the *gated*
"Applying changes" group below it. So magit's `s` = stage exists only
inside a magit buffer, and at the repo-wide level `s` is a slot magit
leaves empty. There was never a collision there to resolve.

MG.23h settled it by keeping `s` = status everywhere and declining to
import magit's `s` / `u` rows at all (their chords are what anyone
reaches for, so a menu path earns nothing and costs the key), while
swapping `s` to the section jump inside magit-status — where opening
the buffer you are already in is the actual no-op. See MG.23h's note.

| Slice | Scope | Depends | Status |
|---|---|---|---|
| MG.23a | File-target seam (`arg_str(0)` → `active_file`) + `:magit-other-file-dispatch` | MG.17b | ✅ |
| MG.23b | Repo rows with no target: `S` stage-all, `U` unstage-all | — | ✅ |
| MG.23c1 | The prompt-backed row shape + `t` tag, `i` gitignore | MG.17b, IX.5 | ✅ |
| MG.23c2 | `I` init, `m` merge (`Q` dropped — see below) | MG.23c1 | ✅ |
| MG.23d | File ops: untrack, delete, rename | MG.23a, IX.2 | ✅ |
| MG.23d2 | `,c` checkout a file from a revision | MG.23d | ✅ |
| MG.23e | Surface-mapping rows — **dropped**, see below | — | ⛔ |
| MG.23f | Blob navigation (blame variants evaluated out, one deferred) | — | ✅ |
| MG.23f2 | Reverse blame (`git blame --reverse`) from a blob buffer | MG.23f | ✅ |
| MG.23g | `a` apply / `-` reverse on a hunk of a commit | MG.18 | ✅ |
| MG.23h | Context-dependent menu content (magit's `:if-derived` / `:if-mode`) | — | ✅ |
| MG.23j | Repo-level `A` / `_` / `O` rows, via a commit picker (NOT the predicate — see MG.20's corrected note) | MG.20 | ✅ |
| MG.24b | Audit finding B: one `replace_buffer_text`, one `magit_workdir` | — | ✅ |
| MG.24c | Audit finding C2: `A`/`_`/`O` in the views the docs already claim | — | ✅ |
| MG.24a | Audit finding A1: `magit-hunk-mode` owns the diff-content chords | MG.22 | ✅ |
| MG.26 | `magit-blame-mode` as a minor on the file — retires the blame buffer | MG.7, MG.23f2 | 📝 |
| MG.27 | in-flight indicator in magit headerlines (a word, not `⟳` — see below) | — | ✅ |
| MG.26a | The blame model — porcelain → chunks → heading text (pure) | — | ✅ |
| MG.26b | `magit-blame-mode` as a minor + the chunk-heading provider; retires the major and both blame buffers | MG.26a | ✅ |
| MG.26c | Syntax highlighting for the blob buffer (reverse blame's content) | MG.26b | 📝 |
| NOTIF.1 | Notification subsystem (design.md §5.9.9) — magit remote ops as first consumer | — | 📝 |
| MG.23i+ | The new subsystems, one slice each, prioritised by daily use — the same set MG.21 still names | — | 📝 |

#### MG.23a — the file target seam + `:magit-other-file-dispatch` ✅ (2026-07-30)

`C-c f` acts on the visited file with no prompt — magit's one deliberate
deviation here — and `:magit-other-file-dispatch` is the stand-alone
command for a file you are *not* visiting. Tied to no buffer, bound to no
chord.

**The seam.** Every `C-c f` action declares an optional `file` argument
and resolves `ctx.arg_str(FILE_TARGET_SLOT)` before falling back to
`active_file(ctx)`. `C-c f` sets nothing, so its behaviour is byte-for-byte
what it was. The other-file menu carries an `Argument` row (`=f`) that
fills the slot. No new machinery: the host already projects transient
state onto an action's `args_schema`, and it does so **by name**
(`project_transient_state`) — so the schema's `"file"` and the
`Argument`'s `"file"` must match, and a mismatch would degrade silently
to "always the visited file" rather than failing. Two tests pin both
halves against that literal. This is also the seam a universal-prefix
would use if the deferred `<C-u>` work lands: set the same argument.

**Discard is absent from the other-file menu, and that is a limitation
worth naming.** `x` is destructive, so §12.13 routes it through
ask/execute — and `Effect::Confirm` opens a transient of its *own*,
replacing this menu and the target with it. The execute half would find
no `file` argument and fall back to the visited file: it would ask about
one file and act on another. Offering destructive rows for an explicit
target needs `Effect::Confirm` to carry arguments to its yes-action,
which is its own slice. Guarded by
`the_other_file_menu_has_no_destructive_row` — verified non-vacuous by
adding the row and watching it fail with that reasoning — so the hazard
cannot be reintroduced quietly.

The target is on the menu's preview line at all times, including when
unset ("target: (none set — rows act on the visited file)"), so a row
never fires at a file the user cannot see named.

- **Tests:** 3 — every file action declares the target under the name the
  menu uses; the menu offers an `Argument` by that name; no destructive
  row.

#### MG.23c1 — `t` tag, `i` gitignore ✅ (2026-07-30)

The prompt-backed shape, established on the two rows that need no
decisions: a menu opened from anywhere has no cursor to read a tag name
off, so both ask for their one value.

**The shape is the branch-create wizard's, generalised.** The row (or
chord) returns `Effect::OpenPrompt`; the `-finish` action named as its
submit target does the work with `ctx.prompt_value`. Two actions per
operation, no transient state to lose. A blank submission declines —
submitting nothing is how you back out, and `git tag ""` would fail
with a message about refs rather than about what the user did.

**One operation, two ways in.** `:magit-tag v1.2.0` acts immediately;
bare `:magit-tag` opens the same prompt targeting the same finish
action the menu row uses. The ex-command is the scriptable surface the
standing rule asks for, not a second implementation.

`i` is not a git subcommand — git has none for this — so the
`.gitignore` append is ours: skip a pattern already present (compared
as trimmed whole lines; pressing `i` twice on the same artefact is an
ordinary mistake), and add a newline first when the file lacks a
trailing one, or the new pattern fuses onto the last and neither is
ignored. `gitignore_append` is split out pure so the test exercises it
rather than a copy — the first draft of that test mirrored the logic,
which would have drifted.

- **Tests:** the append rules (idempotent, newline-safe, trimmed
  comparison), a blank prompt declining for both operations, and every
  prompt row targeting a finish action some mode actually contributes —
  without which the prompt accepts input and does nothing with it. Leaf
  count 14 → 16.
- **Docs:** both rows and their ex-commands. Also fixed the false
  auto-refresh section flagged earlier — `magit.auto-refresh`, the
  `RepositoryEvent` path and the `RepositoryWatcher` were all described
  as shipped and none exists. Two other pages called the
  action-triggered refresh "auto-refresh", which read as that same
  absent feature; both now say what actually refreshes.

#### MG.23e — dropped after evaluating the rows (2026-07-30)

The slice was "wire magit's entries that map onto lattice surfaces
rather than reimplementing them". Evaluated row by row, almost none of
them earns a place:

| magit | what it does | here | verdict |
|---|---|---|---|
| `j` Display status | opens the status buffer | our `s` row already does | **duplicate of a row we have** |
| `e` / `E` Ediff | diffs the thing at point | our `d` row opens magit-diff | overlapping, and "thing at point" does not exist in a menu opened from anywhere |
| `J` Display repo buffer | switch among magit buffers | `:ls` / `:b` / the buffer picker | duplicate of a general surface |
| `H` Section info | describes the section at point | *nothing* | nothing to map onto |
| `h` / `C-x i` Help | opens magit's manual | `:help magit` | one keystroke over an already-global command |
| `C-x m` describe-mode | the live keymap | `:describe-mode` | same |

Two are duplicates of rows already in our menu, two duplicate general
editor surfaces, one has no equivalent at all, and the rest save a
keystroke over commands that are already bound globally — each needing a
new action to wrap an ex-command, since transient rows resolve through
the action registry and never the ex-command path.

**And the framing was wrong.** Magit shows most of these in an
"Essential commands" group gated on `:if-derived magit-mode` — a
**contextual cheat-sheet**, not functionality. Our equivalent is
`:help magit-core-mode`, a full searchable page with every chord and
what it does, which is better than a menu row and already exists.

Dropped rather than deferred, on the policy this plan already carries: a
row that does nothing is worse than absent, and a row that duplicates
another row is not much better.

#### MG.23d2 — `,c` check a file out from a revision ✅ (2026-07-31)

Completes magit's `File` group. Prompt for the revision (seeded `HEAD`,
because "put back what I committed" is the common case and the one
revision you can name without looking anything up), then confirm naming
both the file and the revision.

**Why this one confirms where `,k` barely needs to.** `,k` is a plain
`git rm`, so git itself refuses a file with uncommitted changes — the
confirm there is a second line of defence. `git checkout <rev> -- <path>`
refuses nothing: it overwrites the working-tree file whatever state it
is in and keeps no copy of what it replaced. That is §12.13's bar
exactly, and it is the reason the flow is prompt → confirm → execute
rather than prompt → execute.

**Both halves are carried, and there is no re-derivation fallback.**
The other execute halves fall back to re-deriving their target, which is
safe because the fallback is "the file you are looking at". There is no
such guess for a revision, and checking out from the wrong one is
precisely the damage the confirm exists to prevent — so a missing slot
produces no git call at all. The path rides in the prompt buffer's name
(`*magit:checkout:<path>*`), the same carrier `,r` uses, because by
submit time the prompt buffer is the active one.

`--` before the path is load-bearing rather than tidy: without it a path
matching a ref name is ambiguous, and git resolves the ambiguity by
checking out the *branch* — a wrong action rather than an error. Pinned
by a test, along with the two carriers refusing to read each other's
buffer names and the execute half declining a half-carried confirm.

#### MG.23f — `gj` / `gk` walk a file's history ✅ (2026-07-31)

Scoped "blame variants + blob navigation". Blob navigation was the half
that earned its place, and it lands here; the blame variants were
evaluated row by row like MG.23e's.

**Blob navigation.** `magit-file-revision-mode` could be opened but not
walked — every step through a file's history meant going back to the
log and pressing `<CR>` again, which is a poor answer to the one
question that buffer exists for. `gk` / `gj` step older / newer through
`git log -- <path>`, so only commits that touched *this file* are in the
walk.

Three deliberate refusals, each an echo rather than a jump:

- **at either end** — the history has two ends, and wrapping from the
  first commit round to `HEAD` would read as a glitch;
- **from a `staged` blob** — the index is not a commit and has no place
  in the walk, which falls out of the same lookup rather than needing a
  special case;
- **across renames** — `--follow` is deliberately absent: the buffer
  name carries one path, and a step that silently changed which file you
  were reading is worse than stopping.

`blob_step` is pure (a slice, a ref, a direction), so the walk is
testable without a repository; a round-trip test against a real repo
covers the part that is not pure — that `git log -- <path>` skips the
unrelated commit and that stepping back shows the *earlier* content.

**On the keys.** magit uses `n` / `p`. Lattice follows
evil-collection-magit's `gj` / `gk` remap, and the reason the remap
exists is exactly the standing rule from MG.20's `V` mistake: a
read-only view of a file is where you search, so `n` is not free. `p`
alone would have been (paste is dead in a read-only buffer), but a
navigation pair split across two conventions is worse than either.

**The blame variants, evaluated:**

| magit | what it does | here | verdict |
|---|---|---|---|
| `b e` blame echo | blame for the current line, in the echo area | our blame is its own buffer, not an overlay on the file | **no surface to map onto** — needs inline virtual-text blame first |
| `b q` quit blaming | removes the blame overlay from the file buffer | `q` closes the blame buffer already | **duplicate** |
| `b r` removal / `b f` reverse | `git blame --reverse` — when a line *disappeared* | nothing | **real, and deferred** |

Reverse blame is the one that is genuinely absent rather than
duplicated, and it is now newly implementable: it needs a starting
revision, and the blob buffer this slice made navigable is exactly where
one is in hand ("when did each of these lines go away"). It is a slice
of its own, though — `*magit:blame:<path>*` carries no revision, so the
buffer name, the argv and the headerline all move together. Filed as
MG.23f2 rather than smuggled in here.

### MG.26 — magit-blame as a minor on the file 🚧 (2026-07-31; MG.26a/b landed 2026-08-02, MG.26c open)

Design fragment:
[`../../architecture/magit-blame.md`](../../architecture/magit-blame.md).
Shape decided 2026-07-31; implementation not started.

Prompted by "blame loses syntax highlighting". The highlighting is the
symptom — `blame_styled_spans` returns `Vec::new()` for the code
column, so the code is unstyled *by construction* and no work on that
function fixes it. The cause is the shape: `*magit:blame:<path>*`
replaces the file with a text rendering of the file.

**Cross-editor check was unanimous** and is recorded in the fragment:
magit (chunk headings, minor mode on the file buffer), fugitive
(scroll-bound split), Zed (column + inline), GitLens, JetBrains — every
one annotates the real buffer, and highlighting survives *because* the
file was never replaced. Helix has no blame. We are the only one that
builds a blame buffer.

**Decided:** a `magit-blame-mode` **minor**, chunk headings on
`VirtualRow` (the primitive `DiffOverlayVirtualRowProvider` already
drives), direction as mode state, buffer read-only while blaming so
the edit chords are free. Retires `*magit:blame:<path>*`,
`*magit:blame-reverse:<rev>:<path>*`, the blame *major*, and
`blame_styled_spans`; keeps `format_blame_porcelain`, whose extracted
fields are exactly what a heading needs.

**Reverse blame folds into the blob buffer** — `magit-file-revision-mode`
already shows a file at a revision, so reverse blame is that buffer
with this minor active in the reverse direction. Composes two existing
things instead of adding a third, and `gj`/`gk` keep working while
blaming. MG.23f2's dedicated buffer is superseded a day after landing;
its porcelain and argv work is not.

**Not blocked on a parser question, contrary to a first reading.** The
blob buffer has no highlighting today either (`wake()` with no spans),
which looked like MG.22's "a minor cannot get a parser" wrinkle. It is
not: `lattice-multibuffer` already gives a pathless synthetic buffer a
parser via `Lang::detect_from_path` on a path it knows *about*, and
`grep_highlight.rs` does the same. The blob buffer carries its path in
its name. MG.22's wrinkle — a minor supplying a *diff* parser for
content whose language is not the buffer's identity — stays open and
is a different problem.

### MG.24 — the duplication audit (2026-07-31)

Prompted by the `magit-diff-mode` missing-`x` report and Dhruva's rule
that shared behaviour belongs on a minor mode. Audited every chord,
handler and lifecycle across the 12 magit modes. Findings, sorted by
what each actually wants — which is **not** a minor mode in every case,
and the distinction is the point:

- **A1 → MG.24a. Wants a minor.** Five majors render unified diff;
  `s`/`u`/`x` are declared per-major and the set has drifted — status
  has all three, diff has two, and commit / revision / stash-show have
  none. 8 declarations covering 3 actions, 11 of 15 cells empty. Plus
  `]c`/`[c`/`a`/`-` sit on `magit-core-mode`, which activates on all
  **11** majors, so they are consumed dead keys in the six with no
  hunks.
- **A2. Wants a minor, later.** `<C-c><C-c>` / `<C-c><C-k>` declared
  four times each across magit-commit and magit-rebase — magit's own
  `with-editor-mode`. Two majors today; earns its keep when a third
  `$EDITOR` buffer lands (annotated `tag -a`, `config --edit`).
- **B → MG.24b. Wants helpers, not modes.** Below.
- **C1. Not duplication.** `<CR>` is nine majors and nine actions
  because each resolves a genuinely different target — a branch name,
  a stash index, a sha, a path. The *shape* repeats; the bodies do not.
  A view-seam question (MG.22's `diff_target`), not a mode question.
- **C2 → MG.24c. A live bug.** `A`/`_`/`O` resolve through
  `MagitView::commit_at_cursor`, implemented in exactly **two** places
  (status, log) — but `magit-core-mode.md` claims they work in four,
  naming the revision view and the rebase todo. magit-rebase publishes
  no `MagitView` at all; magit-revision's does not override
  `commit_at_cursor`. Two of four documented views have never worked.
  magit-blame and magit-commit publish no view either, which is also
  why hunk staging is refused in the commit buffer's staged region.

#### MG.24a — magit-hunk-mode owns the diff-content chords ✅ (2026-08-01)

Design fragment:
[`../../architecture/magit-hunk-mode.md`](../../architecture/magit-hunk-mode.md),
**amended** by this slice — its "What it owns" list did not include
`s` / `u` / `x`, which was an oversight rather than a decision. That
fragment and MG.18's hunk staging were designed the same week and
neither folded in the other: MG.18 centralised the machinery and left
the chords declared per-major; the fragment listed only the duplication
it had noticed (parsers, `<CR>`).

The omission had a cost. A live report found `x` missing from
magit-diff, and the audit found the whole set had drifted: 8
declarations covering 3 actions with 11 of 15 cells empty, and three
majors (commit, revision, stash-show) with no staging chords at all.
Nobody noticed, because a gap in a copied set does not announce itself.

**What moved:** `s` / `u` / `x` in Normal and Visual off magit-status
and magit-diff; `a` / `-` and `]c` / `[c` off **magit-core-mode**. The
last of those matters as much as the first — magit-core activates on
all eleven magit majors, so those four were consumed dead keys in the
six with no diff content (branch, log, stash list, rebase, blame,
blob).

**What did not move:** the machinery. `resolve_hunk`, `HunkOp`, the
`DiffSource` gate and MG.18e's region rewrite stay in
`magit_core_mode`; this mode contributes bindings and the handlers that
call them. The seam was already right — only the bindings were in the
wrong place.

**`<CR>` deliberately stayed.** In magit-status it is `magit-visit`,
dispatching on file / stash / commit rows, not only on diff content. A
minor's binding wins over a major's, so taking it before the
`diff_target` seam exists would replace status's context-aware visit
with a diff-only one. It moves with that seam.

**`magit-core-mode` now means what its name says** — every magit
buffer: `gr`, `q`, `]]`/`[[`, `]f`/`[f`, folds, and the commit
operations. Its module header said "]c/[c (hunks)" and no longer does.

**This completes items 1–2 of MG.22, not MG.22.** The mode it builds
is MG.22's mode; the parser, the `<CR>` seam and the `magit.hunk.*`
options remain. MG.22 moves 📝 → 🚧 rather than ✅ — recorded here
because building half a slice under a different id is exactly how a
status drifts without anyone noticing.

- **Tests:** 4 — activation is exactly the five diff-rendering majors,
  asserted in both directions (a missing one leaves that buffer without
  staging; an extra one puts the keys back where they are consumed to
  do nothing); every staging chord is bound in Normal *and* Visual,
  without which MG.18e's region staging is unreachable by its own
  gesture in some majors; `<CR>` is not taken. Plus an end-to-end
  binding test in `lattice-ui-tui` asserting the minor activates in
  magit-status and magit-diff and **not** in magit-log or magit-branch
  — the relocation is a binding change, so what needs proving is that
  the mode carrying them lands where it should and nowhere else.

#### MG.24c — the two views the docs promised and never had ✅ (2026-08-01)

`A` / `_` / `O` resolve the commit through
`MagitView::commit_at_cursor`. It was overridden in exactly **two**
places — magit-status and magit-log — while `magit-core-mode.md` named
**four**, adding the revision view and the rebase todo. Those two were
consumed dead keys for as long as the doc has claimed otherwise:
magit-rebase published no `MagitView` at all, and magit-revision's
(added by MG.23g for `a` / `-`) never overrode the method.

Nothing failed loudly. The trait default answers `None`, the handler
returns `None`, and a Normal-mode chord a mode binds is consumed
either way — so the keys did nothing, silently, in half the places the
documentation pointed at.

**The rebase todo already had the data.** `<CR>` reads the sha off the
same line with the same `extract_sha` the view now calls. Only the
seam was missing.

**The revision view ignores the cursor, deliberately.** A `git show`
buffer *is* one commit, so every line belongs to the sha in the
buffer's name. Reading a sha off the line under the cursor would have
been the wrong fix — the `--stat` rows and the diff body carry no sha,
so it would have worked on the header lines and nowhere else.

**`RebaseView::refresh` returns `None`, and that is load-bearing.**
`gr` means "rebuild this view from git" everywhere else; a rebase todo
is a file the user is part-way through editing, so rebuilding it would
silently discard their reordering. There is no refresh safe to offer
here.

- **Tests:** 1, structural. Every view the docs name must carry a
  `commit_at_cursor` override, with the trait default's `None` asserted
  first so the premise is explicit. Structural rather than
  chord-driven because what went wrong was a *missing override* —
  an override that exists and returns `None` for some cursor is a
  different and legitimate thing. Verified discriminating: the four
  named views have it, the three that show no commits (branch, diff,
  stash) do not.
- **Docs:** `magit-core-mode.md`'s claim is now a table naming what
  each view actually reads, rather than a prose list that outran the
  code.

#### MG.24b — one buffer write, one workdir lookup ✅ (2026-07-31)

**`replace_buffer_text` — 12 sites → 1.** Five private
`apply_full_replace` copies identical to the byte (md5 `ddbec26b…`
across blame/branch/diff/log/stash), a sixth near-copy in `refresh.rs`,
and six more inlined into `on_activate` without even a name. Now
`buffer_io::replace_buffer_text`. No behaviour change; −213 lines.

**`magit_workdir` — 11 sites → 5, and the 5 are a different
question.** What remains all wants the `Repository` *object* (for
`Branch::create`, `run_git`, gitignore's path join, rebase's `gitdir`),
not "where is the working tree" — factoring those would be `discover`
with extra steps, so the sweep stops there deliberately rather than
being driven to zero.

**Why this one is not tidying.** `gix::discover` takes a *directory*,
and a file path fails **silently**: `discover` returns `Err`, `.ok()`
swallows it, the caller takes its default. MG.11 found three sites
doing exactly that — one in `lattice-host`'s auto-head-diff subsystem,
which meant gutter diff signs had never worked for any file since they
landed. Two functions split by question (`magit_workdir()` vs
`workdir_for_file(path)`) make it unrepresentable: a caller holding a
file path cannot reach the directory-taking one.

**What was deliberately NOT done: a lifecycle framework.** The audit
also found all 11 `on_activate`s running the same nine steps — but they
*alternate* shared and mode-specific (boilerplate, parse the buffer
name, headerline, publish state, run git, apply text), so it is a
sandwich rather than a block. More importantly, step 7 carries MG.13's
**"publish BEFORE the first `.await`"** rule, commented at **9 of the
11 sites**, at the exact line it constrains. A builder or trait owning
that sequence would bury the one ordering constraint that has already
caused a real bug and make re-introducing it invisible. Extracting the
identical *leaves* captures most of the duplication and keeps the
invariant where it can be seen; the skeleton stays.

- **Tests:** 3 — `gix` discovery genuinely rejects a file path
  (asserted against `gix` itself, so a future version that accepts one
  says so rather than letting the split quietly become pointless);
  `workdir_for_file` resolves repo-relative; a parentless path is
  declined rather than silently discovering from the process's cwd.

#### PICK.1b — multi-key transient rows fire on their keys ✅ (2026-07-31)

Reported in use: `,k` (delete) in the file dispatch did nothing. The
row rendered, `<C-n>` reached it and `<CR>` fired it — its own keys did
not.

**Transient keys are strings; the host was comparing a char.**
`Action::PickerAppend(c)` did `let key = c.to_string()` and handed that
to `do_transient_trigger`, whose lookup is an exact match against each
row's key. So every multi-key row magit binds — `,x` untrack, `,r`
rename, `,k` delete, `,c` checkout, `=f` set-target — was unreachable
by keypress since the day it landed. Five rows across two menus, and
nothing failed loudly: the menu simply ignored the keystroke.

Keys now accumulate. `TransientSpec::resolve_key` answers `Fire` /
`Prefix` / `NoMatch` and the host holds a `transient_prefix` between
presses; `NoMatch` drops what was accumulated, because a stuck prefix
would make every later keystroke miss too — a menu gone quietly deaf.
`BS` clears a pending prefix before popping a submenu (a half-typed row
is what it most likely means to undo), and entering a submenu clears
it, since a prefix belongs to the level that was showing.

**Rows that can no longer match go dull** — magit's own behaviour, and
the only thing that says "`,` was received, keep going" rather than
leaving the menu looking inert. Both renderers.

**An exact match beats a prefix**, decided here rather than left
implicit. A key that both completes one row and begins another is
ambiguous, and vim resolves that with `timeoutlen` — machinery this
editor does not have: there is no ambiguous-chord timeout anywhere,
and `Action::AbsorbPartialChord` waits indefinitely. Firing the exact
match is the resolution that never hangs a key on a timer that does not
exist. No spec has such a pair today; this decides it if one appears.

- **Tests:** 4 — a multi-key row resolves one keystroke at a time and
  fires on the second; single-key rows still fire on the first; a key
  that begins nothing is a miss rather than a prefix (including a valid
  prefix followed by a wrong key); and the dim filter matches exactly
  the rows still reachable at each stage.

#### MG.23h — the dispatch is built for where it was opened ✅ (2026-07-31)

**This slice was nearly dropped on a false premise, which is worth
recording.** The assessment claimed magit never binds `magit-dispatch`
globally, making its `:if-derived` groups an artifact of the menu
normally already being in a magit buffer. That is wrong:
`magit-define-global-key-bindings` (magit.el:248) binds
`C-x g` / `C-c g` / `C-c f` under `'recommended` — **our binding set,
key for key**. The predicates are not an artifact; they exist precisely
*because* the menu is globally bound and has to degrade. Verified
against source after Dhruva corrected it.

**What magit's dispatch actually contains,** checked row by row
(magit.el:328): one ungated group of repo-wide transients, plus
"Applying changes" (`a`/`v`/`k`, `s`/`u`, `S`/`U`) and "Essential
commands" (`g`/`q`/`TAB`/`RET`/`C-x m`/`C-x i`), both
`:if-derived magit-mode`. The ungated group has **no `s`** — the source
carries `;; s ↓` as a placeholder pointing at the gated group. So our
`s` = status occupies a slot magit leaves empty at that level, and
there was never a collision to resolve there; magit reaches status via
`j` in the same ungated group.

**The mechanism: builders take a `TransientContext`.** See
`picker.md` §4bis.5ter for the design. Two axes as separate fields
because magit asks two different questions (`:if-mode` vs
`:if-derived`) about the same key, and a flat mode list answers only
one. Resolved at *build* time by each renderer rather than at *emit*
time by whatever produced the effect — otherwise `:magit-dispatch` on
the `:` line stays blind (`ExCommandContext` has no buffer), while in
Emacs `M-x magit-dispatch` is context-aware.

The rejected alternative was an action handler naming one of N
pre-registered specs, which needs no substrate change. It was rejected
on merit: it covers only the chord path, and N contexts multiply specs.
The objection that a context type would be "magit-shaped substrate" was
a strawman — `{ major_mode, minor_modes }` is exactly as generic as
`ActionContext`, and its consumer (the renderer, reading the Editor) is
generic host machinery, which is the side of the
substrate-vs-mode-helper rule that says it belongs there.

**What varies, and what does not:**

- **"Applying changes" gains `a` / `-` / `x` in any magit buffer.**
  They resolve the hunk under the cursor, so outside one there is no
  diff text to find it in. `S` / `U` stay unconditional — `add
  --update` and `reset` need no target, so gating them as magit does
  would be strictly less useful.
- **Magit's `s` / `u` rows are deliberately absent.** They would
  collide with the `s` row, and unlike `a`/`-`/`x` their chords are the
  first thing anyone reaches for in a magit buffer: a menu path earns
  nothing and costs the status key.
- **`s` becomes a section jump in magit-status only.** "Open the status
  buffer" is a no-op on the buffer you are in — magit swaps its `j` the
  same way (`magit-status-jump :if-mode` vs `magit-status-quick
  :if-not-mode`). The submenu has one row per section we render, on
  magit's keys where they coincide (`s`/`u`/`n`/`z`) plus `c` for
  Recent commits, which magit's status buffer has no counterpart for.
  In any other magit buffer `s` still opens status, which is useful
  there.
- **The file dispatch does not vary.** Its rows all act on the visited
  file, which is the same question wherever `C-c f` is pressed.

**The jump handlers scan for header text** rather than consulting the
`SectionIndex`, because `]]` / `[[` already locate sections that way
and two mechanisms for "where does this section start" is one more than
can stay in agreement. The prefixes come from
`sections::SECTION_HEADER_PREFIXES`, which is also what renders them —
so a test can assert the submenu covers every section rather than
whichever the author remembered. An empty section is not rendered at
all, so jumping to it echoes instead of leaving the cursor put.

**Rows reuse the chords' own actions** (`action:magit-apply-hunk`,
`action:magit-reverse-hunk`, `action:magit-discard`) rather than
declaring menu-only twins: a second action for "discard the thing at
cursor" is a second place for its confirm contract to drift. Verified
that this is safe — `do_transient_trigger` builds its `ActionContext`
from `self.document_buffer_id` / `self.cursor` / `self.active_region()`,
i.e. the underlying buffer, and the transient owns every keystroke
while open, so a Visual region even carries through to a menu-fired
`x`.

- **Tests:** 4 in magit, 2 in picker — the section-acting rows appear
  in a magit buffer and *not* outside one (both directions, since a
  gate that never opens and one that never closes each pass a one-sided
  test); `s` swaps only in magit-status, checked against a magit-log
  context so the two predicates cannot be conflated; no context
  produces a duplicate key at any level; the jump submenu has exactly
  one row per `SECTION_HEADER_PREFIXES` entry and none inert; the
  context reaches the builder and reaches it per build; and the two
  mode tests are independent. The existing no-inert-rows guard now runs
  over both shapes.
- **Renderer parity:** both peers in the same patch, per the lockstep
  rule.

#### MG.23g — `a` apply / `-` reverse one hunk of a commit ✅ (2026-07-31)

The hunk-scale peers of `A` cherry-pick and `_` revert: take the one
hunk under the cursor out of a commit and put it in the working tree,
or take it back out.

**It is a third `DiffSource`, not a new mechanism.** MG.18's gate
already asked "which tree was this hunk diffed from" and refused
anything it could not classify. `Committed` is the answer for a patch
already in history — a revision view's `git show`, a stash detail's
`git stash show -p` — and once the views answer it, `resolve_hunk`,
the region rewrite, the patch synthesis, the cursor restore and the
echo all work unchanged. The slice is one enum variant, two `HunkOp`
variants, two view impls and two chords.

**Both write the working tree and never the index**, which is what
makes `a` different from `s`: the question is about the file you would
edit, not about what is queued for the next commit. The result is an
ordinary unstaged change `s` then stages normally. Pinned against a
real repository by asserting `git diff --cached` is *empty* after `a`
— a `cached` slip would stage a commit's hunk invisibly and no
argv-shaped test would see it.

**Neither confirms, and that is the rule rather than an exception.**
Each is the other's exact inverse — `a` adds a change `-` removes, `-`
removes one the commit still holds — so both are recoverable without
consulting anything the user cannot see, which is §12.13's actual
test. `git apply` also refuses outright on drifted context.

**No file-level fallback, said out loud.** `s`/`u` fall through to the
view's whole-file path outside a hunk. Here the file-scale meaning is
cherry-pick and revert, so falling through would turn a missed cursor
into a far larger action than the key promises — and a bare `None`
would be worse, since a Normal-mode chord a mode binds is consumed
unconditionally and reads as a dead key. They echo.

**On the keys, checked against source rather than recall.** Magit's
`magit-mode-map` binds `a` to `magit-cherry-apply` and `v` to
`magit-revert-no-commit`; `magit-diff-section-base-map` then carries
`<remap> <magit-cherry-apply> → magit-apply` and
`<remap> <magit-revert-no-commit> → magit-reverse`. So the hunk pair
genuinely rides on the commit-level keys, which is why it belongs
beside `A` and `_`. `a` is magit's own and free here. `v` is not
available — Visual entry, which MG.18e needs — so this takes
evil-collection-magit's remap of the whole revert category
(`v`/`V` → `-`/`_`), verified against its README.org table, which is
where `_` already came from. Neither `-` nor `_` is a builtin motion
here yet, so the shadowing costs nothing today.

**Two modes gained a `MagitView`.** `magit-revision-mode` had state but
published no view; `magit-stash-show-mode` had neither, so it gained
`StashShowState` (just the workdir — the index is already in the buffer
name) and its `Guard` moved from a bare headerline registration to a
`BufferStateGuard`. Both return `None` from `refresh`, correctly: a
commit does not change because one of its hunks landed in the working
tree, and the buffer that did change is not the one on screen.

- **Tests:** 5 — `a`/`-` are the only ops a `Committed` hunk accepts
  and the only ops that accept one (both directions of the gate);
  `a` on a working-tree hunk says the change is already there and `-`
  names `x` instead; neither op's `apply_flags` touches the index; and
  against a real repo, applying one hunk of a two-hunk commit lands
  that hunk in the file, leaves the commit's other change out, leaves
  `git diff --cached` empty, and reverses back to a byte-identical
  file.
- **Docs:** `magit-hunk-staging.md` gains the `Committed` section and
  two rows in the gate table; `magit-core-mode.md` gains the chords,
  an "operating on one hunk of a commit" section, and two rows in the
  evil-magit key table.

#### MG.23f2 — `f` reverse blame ✅ (2026-07-31)

The one blame variant MG.23f found genuinely absent rather than
duplicated. `git blame --reverse <rev>..HEAD -- <path>`: for each line
of `<rev>`'s version of the file, the last commit it still existed in.
On magit's own key (`f`, "...reverse" in its file-dispatch Blame
group), with `:magit-blame-reverse <rev> <path>` as the scriptable
half.

**Two directions, one mode.** Only the argv and the header differ — the
rendering, the chords and the porcelain parser are identical — so a
second mode would have been a copy with a flag. The direction rides in
the buffer name (`*magit:blame-reverse:<rev>:<path>*`), which is what
makes it impossible for a buffer to be in one direction and labelled
the other. The two prefixes are distinct rather than one optional
field because the forward name's path runs to the closing `*` and may
itself contain `:` — no boundary rule could tell `*magit:blame:a:b.rs*`
from a rev-carrying name, and guessing wrong means blaming a file that
does not exist.

**The header says "reverse", and that is not decoration.** A reverse
body is indistinguishable from a forward one and every sha means the
opposite thing. Unlabelled, the buffer silently invites the wrong
reading of every row.

**Blob buffers only — arrived at, not copied.** Reverse blame needs a
revision as well as a path, and its output is the file *as it was at
that revision*. From a working-tree file it would replace what you are
reading with an older version of it, annotated with inverted shas. So
it resolves both halves from a `*magit:file:<rev>:<path>*` buffer and
refuses elsewhere — which turns out to be magit's own rule ("Only blob
buffers can be blamed in reverse"). `staged` is refused with it: the
index is not a commit, so there is no range to walk forward from, the
same exclusion `gj`/`gk` make. **The refusal is echoed and names what
is missing**, never a `None` — a menu row that silently does nothing
is the same failure the no-inert-rows policy forbids, reached from the
other direction.

Deliberately **not** in `FILE_TARGET_ACTIONS` and **not** on
`:magit-other-file-dispatch`: that seam names a target by *path*, and
a path cannot carry a revision. Equally, the ex-command requires both
arguments — `HEAD` is the one default that suggests itself and it is
wrong, since `HEAD..HEAD` is empty and would report every line as
still present: a plausible-looking answer that says nothing.

**`p` opens rather than re-blames.** In a forward buffer `p` walks the
revision back in place, which is fine because the name carries no
revision. In a reverse buffer it would make the name lie, so `p` opens
the parent's own reverse buffer instead — the shape `gj`/`gk` already
use. The `rev-parse` runs inline: the effect it returns *is* the
answer, and it is a cheaper call than the `git log` `gj`/`gk` already
run synchronously.

- **Tests:** 11 — the reverse argv walks forward from the named
  revision and the forward argv never reverses; `--` separates the path
  in both directions; the two buffer names parse to their own
  direction, a forward path containing `:` stays a path, and a
  half-formed reverse name is rejected; the built name round-trips;
  against a real repo, a line deleted in a later commit is annotated
  with the commit it last existed in while a surviving line names HEAD,
  and the *same* repo blamed forward says something different (without
  which the flag never taking would pass); and the handler resolves
  both halves from a blob buffer while refusing `staged`, a magit
  buffer and a plain file with an *echoed* reason. The pinned
  file-dispatch leaf count went 10 → 11.
- **Docs:** `magit.md` §4.4 gains the reverse section,
  `magit-transient.md` the `f` row and its own subsection,
  `docs/user/magit.md` the buffer row.

#### MG.23d — `,x` untrack, `,r` rename, `,k` delete ✅ (2026-07-30)

On magit's own `,` prefix, which is a signal rather than a key
shortage: these change what the file *is*, not what is staged of it.

**Two are gentler than they look, and the safety is in the argv rather
than in a prompt.** `,x` is `rm --cached`, so the file stays on disk and
only the index forgets it — `s` puts it back, which is why it does not
ask. `,k` is a plain `rm` with no `-f`, so git itself refuses a file
with uncommitted changes; the confirm is the second line of defence.
Both pinned by a test, along with `--` before every path so a file named
like a flag is not read as one.

**`,r` carries its source in the prompt buffer's name.** By submit time
the prompt buffer is the active one, so `active_target` would resolve
*it* rather than the file being renamed — the same problem the
branch-create wizard solved the same way. The prompt is pre-filled with
the current path (a rename within a directory is an edit, not a
retype), and submitting it unchanged cancels rather than asking git to
rename a file to itself.

All three read `active_target`, so they work from `C-c f` on the visited
file and from `:magit-other-file-dispatch` on a named one — and `,k`'s
confirm carries its path (IX.1), which is what makes the second of those
safe.

- **Tests:** untrack keeps the file / delete never forces / paths are
  `--`-separated; and the rename buffer-name carrier round-trips,
  rejecting both another buffer's name and an empty source. Leaf counts
  6 → 9 (file) and 16 → 18 (root).

#### MG.23d split: checkout-from-revision separated (2026-07-30)

Magit's `, c` is `magit-file-checkout` — restore a file's content **from
a chosen revision**, which needs a revision prompt on top of the file
target and a destructive confirm on top of that. It is also the one that
overlaps something we already have: `magit-file-revision-mode` shows a
file at a revision, and "restore what I am looking at" may be the better
shape than a second prompt chain. That deserves thinking about rather
than bolting on, so it is its own slice.

Note it is *not* the same as our `x` discard, which restores from the
index — same git verb, different question.

#### MG.23c2 — `m` merge, `I` init ✅ (2026-07-30)

Both on c1's prompt shape. `m` passes `--no-edit`, for the reason
`revert` does — git would otherwise open an `$EDITOR` that never
appears, and `run_remote_op`'s `Command::output()` cannot recover from
that wait. `I`'s prompt is **pre-filled with the working directory**:
that is the answer nearly every time, and a `.git` created in the wrong
place is annoying enough to be worth showing the path before it happens.

`m` prompts rather than picking, because picking already exists one
level down — `b` then `m` in the branch buffer, where the list is. The
repo-level row is the convenience for when you know the name.

**The argv builders are pure and separate from `spawn_git`.** The first
draft of the merge test reached the flags through the spawning path,
which needed a runtime — and would have run **real `git merge` against
the repository the tests live in**. `merge_argv` / `tag_argv` /
`init_argv` are the single copy the handlers, the ex-commands and the
test all use.

- **Tests:** no prompted operation can request an editor (argv-level, so
  nothing is executed); and the empty-prompt and prompt-target guards
  now iterate a `PROMPTED_OPS` table. That table is **hand-kept** — an
  earlier draft of this note claimed it covered new rows automatically,
  which is not true: a row added to production and not to the table is
  unchecked. Deriving it would mean invoking every contributed handler
  to see which return `OpenPrompt`, and some of them spawn git, so the
  test would run real commands against the repository it lives in.
  Leaf count 16 → 18.

#### `Q` git-command — dropped, not deferred (2026-07-30)

Magit's `Q` is `magit-git-command`: a minibuffer prompt pre-seeded with
`git ` (deletable, so really "async shell command"), run through the
shell, output into the process buffer, with `GIT_PAGER=cat` and
`magit-with-editor` so pagers and `$EDITOR`-spawning subcommands behave.

**We are not building it.** Anyone who wants to run a custom git command
can open a terminal and run it — `:terminal` is a real PTY, so it
already covers every case `Q` does, including the interactive ones, with
no new surface and no guard to get right. A `Q` row would be a second,
worse way to do something the editor already does well.

Recorded here rather than left as an open row so it does not get added
back as an oversight. The design question it raised — `:compile`
(captures output, has error navigation, no TTY) versus `:terminal`
(real TTY, unstructured output) — is answered for any future
command-running feature: they cover different halves, which half a
command needs is a property the user knows and we cannot infer, and
magit itself resolves this with an explicit menu rather than a guess.

#### A stale premise this uncovered: `p` (`git add -p`) is no longer blocked

§7.3 and the `p` handler say interactive staging is unsupported because
`git add -p` "is genuinely interactive over stdin, which the TUI's
raw-mode input loop already owns" and there is "no terminal-suspend
mechanism to route through". **`Effect::SpawnTerminal` is a real PTY**,
so the stated blocker no longer holds — `p` could spawn a terminal
running `git add -p`. Not scheduled; recorded because the reason
currently written in the code and the design doc is out of date.

#### MG.23c split into c1 / c2 (2026-07-30)

Amended rather than deviated from. The five rows share one mechanism
(prompt → finish), so coupling is not the reason; slice size is, plus
one hazard worth isolating:

- **`Q` git-command can hang the editor.** It runs whatever the user
  types, and `run_remote_op` uses `Command::output()`, which waits. A
  `rebase -i` or any editor-spawning subcommand would block on an
  `$EDITOR` that never opens — the same trap `git add -p` set (§7.3) and
  that `revert --no-edit` sidesteps. It needs `GIT_EDITOR` /
  `GIT_SEQUENCE_EDITOR` neutralised before it can ship, which is its own
  thing to get right and test.
- **`I` init and `m` merge carry target questions** (which directory,
  which branch) that `t` and `i` do not.

So c1 establishes the shape on the two unambiguous, high-daily-value
rows; c2 takes the three that each need a decision.

#### MG.23b — `S` stage-all, `U` unstage-all ✅ (2026-07-30)

The two repo-wide index rows, in magit's own "Applying changes" group
and on magit's own keys. Both reuse `RemoteOp` — already the one-shot-git
abstraction, not a remotes-only one (stash-push rides it too) — so this
is a table entry, a handler line, a menu row and its tests.

**`S` is `add --update`, not `add --all`.** Tracked modifications only.
"Stage everything" quietly adding a file git was never told about is how
build artefacts and secrets get committed; magit reaches the
include-untracked behaviour behind a prefix argument, which is the
deferred `<C-u>` work. Guarded by a test asserting `--all` / `-A` never
appear.

**`U` is a bare `git reset`,** so it does not ask: the index returns to
HEAD, the working tree is untouched, and every change is still there to
re-stage. Wider blast radius than one file, but fully reversible — which
is the actual test in §12.13's no-confirm set. Guarded by a test
asserting `--hard` / `--merge` never appear.

**Why these two and not `A` / `_` / `O`:** stage-all and unstage-all need
no target. The commit operations act on the commit under the cursor, and
`C-c g` opens from anywhere — including buffers with no commit in them.
They stay chords until the menu can either ask for a commit or hide rows
outside magit buffers (MG.23h). `I` init and `i` gitignore turned out to
need targets too — a directory and a pattern — so they moved to MG.23c.

- **Tests:** 3 (argv for each op, plus both taking no arguments). The
  pinned root-dispatch leaf count in
  `unresolved_ids_do_produce_inert_items_so_the_guard_is_not_vacuous`
  went 12 → 14; it is hardcoded deliberately, so a row added without a
  resolvable action id shows up as a permanently-inert placeholder
  instead of slipping in.
- **Docs, including two errors this pass caught:** `magit-transient.md`
  claimed revert / reset / cherry-pick had "no implementation behind
  them" — false since MG.20; they are absent for lack of *context*, a
  different reason now stated. And an earlier blanket `V`→`_` edit had
  corrupted the sentence describing *magit's own* keys, where revert
  really is `V`. `magit.md` also still claimed the dispatch had "no
  nested submenus yet", stale since MG.17a.

#### Original scoping notes

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

### MG.26b — blame as a minor ✅ (2026-08-02)

Design: [`../../architecture/magit.md`](../../architecture/magit.md)
§4.4 and
[`../../architecture/magit-blame.md`](../../architecture/magit-blame.md).
**The blame buffer is gone.** `magit-blame-mode` is a minor that
annotates the buffer you are already reading, so your code keeps its
own major, its parser and therefore its highlighting — which was the
whole complaint that started MG.26.

**Retired in this slice, as designed:** `*magit:blame:<path>*`,
`*magit:blame-reverse:<rev>:<path>*`, the blame *major*,
`blame_styled_spans`, and `format_blame_porcelain`'s row formatter.

**Three things worth keeping.**

*`Effect::ToggleMode` is the activation seam*, and it carries only a
mode name — correctly, since the grammar crate must not learn what a
blame direction is. Reverse blame therefore leaves a `BlameRequests`
entry keyed by the *buffer name* it will land on, consumed by
`on_activate` and removed on read. Keyed by name rather than
`BufferId` because the buffer may not exist yet: the reverse path opens
a blob buffer and activates the mode on it in one `Effect::Many`. The
ex-command captures the same `Arc` the handlers get as a service,
because an ex-command's `apply` receives the *grammar's*
`ActionContext`, which has no service registry.

*No `inbound` wiring was needed.* The cells worker polls
`VirtualRowProvider::version()` every tick, so bumping it **is** the
wake — blame results appear with no keypress, which is the rule
`feedback_async_results_need_the_inbound_primitive` exists to enforce,
satisfied here by the polling primitive rather than around it.

*`VirtualRowKind::Annotation` was added*, and both renderers wired in
the same patch. `Generic` carries the diff deletion-block backdrop in
TUI and GPUI alike, so a heading painted with it would read as a
removed line; `Filler` is blank padding; `Sticky` pins to the top of
the pane, and an annotation must scroll away with the lines it
describes.

**A collision the ordinary chord guard could not catch.** The first
draft bound `q` (magit's own key for stopping a blame). But this mode
can be active on a blob buffer, where `magit-core-mode` is *also*
active and already binds `q` — two minors, one chord, one buffer,
resolved by registration order. Both chords reach a registered action
and a handler, so the existing guard was silent. Dropped `q`; blame
toggles off the way it toggled on. Two new guards: no shared chord with
magit-core, and `magit-core`'s `Majors` allowlist names no minors — it
still listed `magit-blame-mode`, an entry that could never match again.

- **Tests:** 12 (argv incl. the `--` separator, provider anchoring and
  colouring, the uncommitted case, version bumps, chunk-at-cursor,
  request consumption, the two collision guards).
- **No bench:** `collect` returns rows built once per blame run; the
  git call is on `spawn_blocking` behind a detached task.
- **Deferred to MG.26c:** the blob buffer still has no syntax
  highlighting of its own (it calls `wake()` with no spans), so reverse
  blame annotates unhighlighted content. The fix is known and not a
  parser question — `Lang::detect_from_path` on the path in the
  buffer's name, exactly what `lattice-multibuffer` and
  `grep_highlight.rs` already do.

### MG.26a — the blame model ✅ (2026-08-02)

Design:
[`../../architecture/magit-blame.md`](../../architecture/magit-blame.md).
The pure core of MG.26, landed first — the same shape MG.21 used three
times (`lattice_vcs::Remote` / `Bisect` / `Submodule` before their
modes): the layer with the decisions in it, testable without a buffer.

`crates/lattice-magit/src/blame.rs` — `parse_blame_chunks`,
`BlameChunk`, `heading_text`, `relative_date`.

**Chunks, not rows.** The retired shape rendered one row per source
line, which is *why* blame lost highlighting: the buffer stopped being
the file. Headings need one entry per run of lines sharing a commit,
which is what this produces.

**Commit metadata is cached by sha, and that is the load-bearing
part.** Porcelain emits a full stanza the first time it sees a commit
and header-plus-content after that. Reading only the current stanza —
which the retired `format_blame_porcelain` effectively did, carrying
just `sha` and `author` forward — leaves every later occurrence with an
empty author. Guarded by
`a_repeated_commit_keeps_the_metadata_from_its_first_stanza`.

**A commit appearing twice in a file yields two chunks**, not one:
they are two runs and each wants its own heading.

**No date crate.** The units a relative date needs (minute, hour, day,
week) are fixed-length, so this is arithmetic; months and years are
approximated, which is what "relative" means. `now` is a parameter —
a function that reads the clock cannot be asserted against — and a
commit stamped in the future (ordinary clock skew in a shared repo)
reads as "just now" rather than a negative age.

**Uncommitted lines say so in words.** git attributes them to an
all-zero sha; rendering `00000000  Not Committed Yet` would put git's
internals in a row whose whole job is to be read at a glance.

- **Tests:** 11.
- **Next:** MG.26b wires this to a `magit-blame-mode` **minor** with a
  chunk-heading virtual-row provider; MG.26c retires the major, both
  blame buffers and `blame_styled_spans`, and folds reverse blame onto
  the blob buffer. Until MG.26b lands the existing blame *major* is
  untouched and still the only blame surface.

### MG.27 — the in-flight indicator ✅ (2026-08-02)

Design: [`../../architecture/magit.md`](../../architecture/magit.md)
§8.12.

**Deliverable changed from the slice title, deliberately.** The title
said `⟳`. The icon-degradation rule wants a BMP fallback for every
glyph surface, `⟳` (U+27F3) is not in the fallback set, and
`ui.nerd-fonts` — the toggle that would pick between glyph and
fallback — is buffer-local to the file tree, not a global option a
headerline can read. So the row says `refreshing`, which needs no
toggle and renders everywhere. Building the global toggle is a
different slice; shipping an unguarded glyph would have quietly
violated a standing rule.

**Two decisions worth keeping:** the flag is separate from `fields`,
because every refresh replaces that vector and would wipe a marker
stored in it; and it is raised by an RAII guard, because a refresh can
exit early, panic, or be cancelled and each would strand the row on
`refreshing`.

Applied to magit-status (the slice's subject) and to every list view
whose refresh spawns — branch, stash, log, remote, submodule. The
mutation spawners deliberately take no guard: their per-target
`refresh` calls raise and clear the flag on their own rows.

- **Tests:** 6.
- **No bench:** `set_busy` is one atomic swap and bumps the version
  only on a real change, so a refresh entirely between two ticks costs
  no repaint.

### MG.19 — side-by-side ✅ (2026-08-02)

Design: [`../../architecture/magit.md`](../../architecture/magit.md)
§8.11.

**The slice turned out to be far smaller than the plan assumed, and
finding that out was the work.** The plan said "two-pane layout via
D.4's pane-group machinery, scroll-bound; `do`/`dp` on top of MG.18's
hunk identity" — implying magit builds both. It builds neither.
`lattice-diff` already binds `do` / `dp` on `diff-mode` (they were
never magit's to add), and scroll binding, filler rows and `]c` / `[c`
all fall out of a registered `PaneGroup`. So MG.19 is: open the
baseline in the current pane, then `Effect::Diffsplit` the working-tree
file. Two existing effects, zero new API, and therefore no GPUI parity
work owed.

**Order is the correctness requirement.** `Diffsplit` diffs against the
*active* pane, so the baseline must be opened first — reversed, the
editable copy lands on the left and `do` / `dp` invert. Guarded.

**One deliberate asymmetry with hunk staging**, written up in §8.11:
`dv` succeeds on the unscoped `*magit:diff*` where `s` / `u` / `x`
refuse, because "which version do I show" is answerable where "which
tree do I write to" is not.

**`dv` is vim-fugitive's key** — magit has none, having no side-by-side
view. It joins the `d`-prefixed family `diff-mode` already owns.

- **Tests:** 5 (baseline per source, the `None`→HEAD asymmetry, a
  committed diff with no commit at cursor declining, effect order, and
  that magit does not rebind `do`/`dp`).
- **No bench:** no new work per frame or per keystroke; the session is
  the diff subsystem's existing one.
- **Deferred, named:** a deleted file has no working-tree side, so `dv`
  echoes instead of opening an empty pane. Three-way (`Diffsplit`'s
  `remote`) is unused — magit's conflict views would be its consumer.

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

### MG.20 — reset / revert / cherry-pick ✅ (2026-07-29)

`A` (cherry-pick), `_` (revert), `Os` / `Om` / `Oh` (reset
soft/mixed/hard) — **evil-collection-magit's** keys — working in **every view
that shows a commit**: the log, magit-status's Recent commits, the
revision view, the rebase todo.

**Reuses the MG.13 seam rather than adding a fifth special case.**
These operations all mean "act on the commit under the cursor", and
each view answers that differently (a log row, a `--stat` header, a
Recent-commits entry). `MagitView` gained `commit_at_cursor` +
`workdir`, so `magit-core-mode` owns one boot-registered handler per
operation and dispatches through the view. No per-view handler (which
MG.13 proved collides), no `match buffer_kind` in the host (which the
everything-is-a-buffer rule forbids). A row with no commit declines, so
`_` on a staged file or a `--graph` connector does nothing rather than
acting on a neighbour.

- **`Oh` is the only one that asks.** `reset --hard` discards
  uncommitted work irrecoverably; `--soft` and `--mixed` keep it.
  Prompting on the safe two would train the user to dismiss the prompt
  that matters. Registered in `confirm::DESTRUCTIVE_ACTIONS`, so the
  ask half performs no git call at all.
- **The keys are evil-collection-magit's, not raw magit's** — a
  correction made when MG.18e needed `V`. Magit binds revert to `V` and
  reset to `X`, but magit is not modal, so those cost it nothing; here
  `V` is grammar. evil-magit already resolved exactly these collisions
  (revert `V`→`_`, reset `X`→`O`, discard `k`→`x`, apply `A`
  unchanged) and deliberately leaves `V` as `evil-visual-line`;
  vim-fugitive keeps it free for the same reason — its visual-mode
  mappings stage partial hunks. The original commit message claimed
  "Emacs magit's own keys", which was doubly wrong: it copied `V`'s
  binding without its tradeoff, and mis-attributed `O` to magit. A
  Normal-mode chord bound by a mode is consumed *unconditionally* (a
  handler returning `None` counts as handled), so `V` for revert meant
  linewise Visual did not exist in any magit buffer — which is what
  made region staging unreachable. Guarded now by
  `no_magit_mode_binds_a_visual_entry_key`.
- **`revert` passes `--no-edit`.** git would otherwise open `$EDITOR`
  for the message — inside lattice that is a hang on a prompt the user
  cannot answer, not a UI.
- The five new chords were validated automatically by MG.15's
  cross-cutting guard (every chord resolves to a registered action AND
  a handler) without any new bespoke test.

**Not shipped, deferred to MG.21:** tag, merge beyond magit-branch's
`m`, bisect, submodule, remote management. The plan's own rule holds:
a menu row that does nothing is worse than an absent one, so none of
these appear in a transient yet.

> **2026-07-31:** tag and merge landed early, in MG.23c1 / MG.23c2, on
> the prompt shape those slices established. MG.21's remaining scope is
> bisect, submodule and remote management — the genuinely-new
> subsystems, which is the same set MG.23i+ names.
>
> **2026-08-01:** remote management landed as MG.21b/c/d — as a buffer
> (`magit-remote-mode`), not the transient magit uses — and bisect as
> MG.21e/f/g, as headerline state plus a gated `B` menu rather than
> either, and submodules as MG.21h/i. **MG.21 is complete.**

**Also not shipped:** transient entries for the three that DID land.
They are reachable by chord in every commit-showing view, which is the
primary path; a `C-c g` entry needs a commit target and the root
dispatch is context-free, so it wants the same "act on the current
buffer" resolution `C-c f` uses. Worth doing with MG.21 rather than
half-wiring now.

> **2026-07-31, corrected by MG.23h:** "the root dispatch is
> context-free" is no longer true — it now receives a
> `TransientContext`. But the predicate is **not** the answer for these
> three, and magit says so: it puts `A` Apply, `V` Revert and `X` Reset
> in its *ungated* group, because `magit-cherry-pick` / `magit-revert` /
> `magit-reset` are transients that **prompt for a commit** rather than
> reading point. So what these rows want is a commit picker, not
> `:if-derived` — and that needs no substrate work at all. Filed as
> MG.23j below.

- **Tests:** 4 in `lattice-magit` (argv appends the target;
  only the destructive reset asks; revert never opens `$EDITOR`; the
  confirm targets its real execute half), plus the existing chord guard
  covering all five bindings.

### MG.21b/c/d — remote management ✅ (2026-08-01)

Design: [`../../architecture/magit.md`](../../architecture/magit.md)
§4.6b. The first of MG.21's three genuinely-new subsystems, taken first
because it is the one with daily use.

**Shape decision: a buffer, not a transient.** Magit's `M` is a
transient with `remote.<name>.url` as variable rows; our transient
substrate has no variable rows, so the port would have hidden the URLs
and made every operation a blind prompt. Recommended and taken on
**heuristic #1** (genuinely-better long-term fit — remote management is
a list of records with attributes, and rendering it removes the picker
each of `r` / `d` / `u` would otherwise need) and **paramount #3**
(everything-is-a-buffer: `/`, `y`, `gr` come free). The rejected (A)
transient-only shape and the rejected (C) status-section shape are in
§4.6b.

| Slice | Scope | Tests |
|---|---|---|
| MG.21b | `lattice_vcs::Remote` + `RemoteEntry` + `parse_remote_v` | 6 unit (parser, incl. differing pushurl / malformed-line skip / URL-with-spaces) + 8 integration against real `git` |
| MG.21c | `magit-remote-mode`, `remote_styled_spans`, `headerline::remote_fields`, `BufferStates::all` | 10 mode unit + 2 styler + 1 headerline + 1 ex-command/action reachability |
| MG.21d | `action:magit-global-remote` + the `M` dispatch row | 2 (offered in every context and not inert; no mode binds `M` as a chord) |

**Three hand-kept guards were bumped, and each caught something real
before it was:** the registered-mode count (14→15), the
handler-collection list (the new chords reported as dead until
`MagitRemoteMode` joined it), and the root-dispatch inert-row count
(23→24).

**Substrate added: `BufferStates::all()`.** A prompt's `-finish` action
fires with the PROMPT buffer's id, so `state_for` cannot reach the
buffer whose content the work changed. Refreshing through the service
instead is context-free. Generic, on `BufferStates<S>`, not a
remote-specific helper — any prompt-backed mode has this problem.

**No bench.** Nothing here touches the UI thread or a hot path: the git
calls run on `spawn_blocking` behind a detached task, and the render is
O(remotes). Stated rather than left silent so the four-artefact rule is
visibly met rather than quietly skipped.

**Deferred, named:** editing a separate *push* URL. `u` sets the fetch
URL; the list shows both columns so a split is visible, but there is no
chord for the push side. `M`'s remaining magit rows (`C` configure, `P`
prune-refspecs, `z` unshallow, `d u` update-default-branch) are also not
here — none is daily-use, and a row that does nothing is worse than an
absent one.

### MG.21e/f/g — bisect ✅ (2026-08-01)

Design: [`../../architecture/magit.md`](../../architecture/magit.md)
§8.9. The second of MG.21's three new subsystems.

**Shape: state + a gated menu, no buffer.** magit-status is already a
buffer, so a `magit-bisect-mode` wins no paramount-goal ground
(heuristic #1), and bisect state *is* repo state — `SectionIndex`
already carries `branch` / `ahead` / `behind` for the same reason. The
headerline is where lattice answers "what state is this repo in"; it
already carries `REBASE IN PROGRESS`. A `SectionKind` was rejected on
merit rather than cost: every `SectionEntry` variant carries
diff-bearing-file invariants a bisect status cannot satisfy.

| Slice | Scope | Tests |
|---|---|---|
| MG.21e | `lattice_vcs::Bisect` + `BisectState` + `parse_bisect_vars` | 4 unit + 8 integration driving a real bisect |
| MG.21f | `SectionIndex::bisect`, `headerline::bisect_label`, the alert | 4 |
| MG.21g | `B` sub-transient, the gate, `MagitViews::all` | 4 (both gate directions, no inert row, `B`/`M` chord-freedom) |

**Two bugs the tests caught rather than shipped.**

*The count was wrong and self-consistently so.* `revisions_left` was
`count(rev-list bad ^good) - 1`, which reported 6 where `git bisect`
prints 3 — git reports the worst-case half after the midpoint it chose,
not the range size. It passed its first test because that test asserted
my reading against my own hand-computed constant. Fixed by using `git
rev-list --bisect-vars` (the plumbing that exists for this), and the
test now parses the number out of git's own printed message and
compares, so the two can no longer agree with each other and disagree
with git.

*The alert was hidden exactly when it is always true.* It was pushed
after `status_fields`' clean-tree early return — and bisecting a clean
tree is the normal case, since git checks out each candidate for you.
Guarded by `the_bisect_alert_shows_on_a_clean_tree`.

**A purity seam, added because the guard would otherwise have been
flaky:** `dispatch_transient_with(ids, ctx, bisect_in_progress)` is
pure and `dispatch_transient` is the thin impure wrapper. Probing
inside the builder would have made the root menu's inert-row count
depend on whether the developer's own checkout was mid-bisect while the
suite ran.

**Substrate added: `MagitViews::all()`**, peer of `BufferStates::all()`.
A mark moves HEAD, so an open log and diff are as stale as the status
buffer; refreshing only the firing buffer would leave the others
confidently showing the previous HEAD.

**No bench:** the gate is a `stat`, the progress git calls run only
while a bisect is actually in flight, and nothing lands on the UI
thread.

**Deferred, named:** `git bisect run <script>` (magit's `s`), the
bisect log as a buffer, and marking a revision other than the one git
checked out.

### MG.23k — `D`, view arguments ✅ (2026-08-02)

Design: [`../../architecture/magit.md`](../../architecture/magit.md)
§8.10. The daily-use item of MG.23's tail.

**One chord, not magit's two — decided, not defaulted.** Magit uses `D`
for diff args and `L` for log args. `D` is an editing operator, inert
in a read-only buffer, so it carries over; `L` is the bottom-of-screen
motion and stays off chords like `M` and `B`. Options were (a) one
polymorphic `D` through the `MagitView` seam, (b) `D` + a `gl`
deviation, (c) dispatch rows only. **(a)** on heuristic #1 — the seam
already exists for `gr`, and "arguments for the view I am in" is one
question, so one key answering it is the better long-term shape than
two keys and a learned deviation. Cost, stated: a magit user's `L`
muscle memory gets nothing.

**Two things verified against real git rather than assumed.**
`--unified 3` and `-U 3` are both rejected, so a joined form was needed
(`RemoteArgKind::ValueJoined`) — the separated form was tried first and
failed. `--author x` and `-n 200` are accepted separated, which is why
the joined/separated distinction is per argument and not global.

**The positional hazard, and the guards for it.** One action carries
the union of both flag tables and receives a *positional* list, so a
shifted slot means the wrong flag runs — silently. `VIEW_ARG_TABLES` is
the one list the schema builder and the slot lookup both read, guarded
by "schema equals the tables in order" and "no two flags share a name",
plus a test that a diff view handed a fully-populated union still gets
only diff arguments.

- **Tests:** 6.
- **No bench:** the git call was already off-thread; this changes its
  argv, not where it runs.
- **Deferred, named:** MG.23's other tail items — `r` blame removal,
  `M` log-merged (whose magit key is a motion we protect, so it needs
  its own answer) and `e` edit-line-commit.

### MG.21h/i — submodules ✅ (2026-08-02)

Design: [`../../architecture/magit.md`](../../architecture/magit.md)
§4.6d, §12.9c. **MG.21 is complete with this.**

**Shape: a buffer, and here magit agrees** — `magit-list-submodules` is
a buffer there too, so the UX-convention rule and paramount goal #3
point the same way rather than trading off. Implementation is
MG.21b/c/d's, reused wholesale: stored `entries` for the cursor
mapping, `BufferStates::all()` for the prompt-finish refresh,
prompt-name carriers for the two-step add.

| Slice | Scope | Tests |
|---|---|---|
| MG.21h | `lattice_vcs::Submodule` + `SubmoduleEntry` + `parse_submodule_status` | 7 unit + 6 integration against real submodules |
| MG.21i | the mode, styler, headerline fields, `o` row, ex-command | 12 |

**Three guards caught real defects before they shipped:**

*The confirm's execute half declared no argument slot.* `d` carries the
submodule path, but without a `CONFIRM_TARGET_ACTIONS` entry the value
lands nowhere and `carried_target` falls through to re-deriving from
the cursor — so a refresh landing while the dialog is open would point
a working-tree deletion at a different submodule.
`every_destructive_pair_carries_a_target_except_the_one_with_none`
caught it.

*The mode-count and inert-row guards* caught the mode missing from the
handler-collection list (15→16, 26, 29).

*The renderer/test spacing disagreed* — caught by asserting the styler
against `render_submodule_list`'s actual output rather than a
hand-typed line.

**A limitation found rather than invented: magit's workdir is
process-wide.** `<CR>` should open the submodule's own magit-status,
and cannot: `workdir::magit_workdir` discovers from the process CWD, so
every magit buffer is bound to one repository. The chord is absent
rather than lying. Per-buffer workdir is its own slice and is also what
magit's `Z` worktree rows will need — a shared prerequisite, not a
submodule detail. **Worth filing before worktree work starts.**

**No bench:** git runs on `spawn_blocking` behind a detached task; the
render is O(submodules).

**Deferred, named:** magit's `d` unpopulate (`u`'s inverse, and
adjacent-to-destructive), and per-submodule fetch.

### MG.21a — diff line-background tints ✅

Carved out of MG.22 and landed first: it is independent of the parser
question, and it is most of the visible "emacs magit looks richer"
gap. Design:
[`../../architecture/magit-hunk-mode.md`](../../architecture/magit-hunk-mode.md)
§"Why the current output looked flat".

The diagnosis in the design fragment was **wrong on first pass** and
the correction shaped the fix. `diff.add.line` / `diff.remove.line` are
not reachable from a `StyledSpan`; both renderers apply them as a
full-row background keyed on a `DiffSignMap` looked up by buffer id,
and sign maps were built only from live `DiffSession`s — which a
buffer whose *content* is a diff never has.

**Shipped:**

- `Editor::diff_signs_from_spans` — derives `(line, DiffSignKind)`
  from the spans a mode publishes: a row styled `DiffAdd` /
  `DiffRemove` earns the matching tint. Runs in
  `drain_pending_synthetic_highlights` on the actor thread, `O(lines)`
  per refresh, never at paint time.
- `Editor::provider_diff_signs`, merged into the published map by
  `diff_sign_maps_by_buffer` (sessions win on collision).
- `DiffSignMap::from_entries` promoted from `#[cfg(test)]` to `pub`,
  with the ascending-order requirement `sign_at`'s binary search
  depends on documented.
- `lattice-magit`'s two diff stylers collapsed onto one
  `classify_diff_line` ladder — they had carried verbatim copies, so a
  rule fixed in one could miss the other.

**Rejected — a parallel signs channel** (a `AppEffect::DiffLineSigns`
twin of `CompilationGutterSet`, built then discarded). It would have
had to repeat the `splice_insert` / `splice_remove` arithmetic that
keeps magit-status's inline `=` expansion aligned, and could therefore
drift out of sync with the text. Deriving from the post-splice spans
cannot. It also needed no producer changes at any of the 8 span sites.

**Zero renderer changes in either peer** — the cross-renderer audit is
vacuous here by construction, which is the point.

Six tests in `dispatch.rs`, all proven non-vacuous (3 fail with the
merge disabled): the derivation, the no-diff-styling case (magit's
log / blame / branch views share the channel and must stay untinted),
ascending order, end-to-end reach into the published map, clearing on
a refresh that drops the diff, and splice alignment.

**Not shipped:** `magit.hunk.line-backgrounds` to opt out — the option
lands with MG.22, which is where magit's options get an owner.

### MG.22 — `magit-hunk-mode` 🚧

> **Status corrected 2026-08-01** (verified against source, not icons).
> The mode **exists** and owns its chords — MG.24a built it, moving
> `s`/`u`/`x` off magit-status and magit-diff and `a`/`-`/`]c`/`[c` off
> `magit-core-mode`, and amending this fragment's ownership list, which
> had omitted the staging chords. That is items 1–2 of the list below.
>
> | Owns | State |
> |---|---|
> | chords that act on a hunk (`s`/`u`/`x`, `a`/`-`) | ✅ MG.24a |
> | navigation within diff content (`]c`/`[c`) | ✅ MG.24a |
> | structural highlighting (`tree-sitter-diff`) | 📝 — `diff_styled_spans` still hand-rolled |
> | `<CR>` via the `diff_target` seam | ✅ 2026-08-01 |
> | `magit.hunk.*` options | 📝 — no occurrences in source |
>
> The drift this corrects was self-inflicted: half of MG.22 landed
> under a different slice id (MG.24a, from the duplication audit)
> without reconciling the parent, so a slice that was 40% done still
> read 📝. The remaining three items need no chord work and are
> independent of each other; the parser one still carries the open
> question below about a *minor* supplying a parser.


Design fragment:
[`../../architecture/magit-hunk-mode.md`](../../architecture/magit-hunk-mode.md).
Decisions taken 2026-07-29; implementation not started.

Five majors show unified-diff content and each reimplements the same
three behaviours — 8 `diff_styled_spans` call sites, 3 `file_at_cursor`
parsers, 3 `<CR>` visit handlers — while the options that should govern
diff display have nowhere to live at all.

**Decided:**

- A **minor** named `magit-hunk-mode`, in `lattice-magit`. Minor
  because the same content appears under five different majors, each
  keeping its own chords and refresh — the relationship `help-mode`
  has with `markdown-mode`.
- `<CR>` resolves its target through the **`MagitView` seam**
  (`diff_target(path)`), a third use of the MG.13 pattern rather than
  a fourth mechanism. Rejected: re-parsing scope out of the buffer
  name, which would make a name format load-bearing in a second place
  — the drift that left every stash chord dead until MG.15.
- Parse with **`tree-sitter-diff`**, deleting the hand-rolled
  `diff_styled_spans`. It covers renames, binary changes, mode lines
  and `index` lines that the hand-rolled styler silently misses.

**Sequencing note — two independent wins, and the cheap one is not the
parser.** The flat look of magit's diffs is because they map spans to
`diff.add.text` / `diff.remove.text` (**foreground only**) while
`diff.add.line` / `diff.remove.line` already exist carrying
**background tints** and are already used by the gutter path. Applying
the line elements is an element-mapping change that lands independently
of tree-sitter and is worth doing first.

**Unresolved before coding:** lattice attaches parsers through the
*major* (`Lang` → `DocumentSyntax` buffer-local). A magit buffer's
major is `magit-*`, so a minor cannot get a parser the usual way. The
likely answer is registering `"diff"` in the `LangRegistry` and having
the mode write `DocumentSyntax` itself, but that needs checking against
the syntax worker's assumptions about who owns that local.

**Out of scope:** inline gutter/overlay diffs on ordinary file buffers.
That path in `lattice-diff` remains unowned by any mode — tracked in
the help-docs slice plan as a separate design item.

## Cross-references

- [`../../architecture/magit.md`](../../architecture/magit.md) — design fragment (what + why)
- [`../../architecture/diff-system.md`](../../architecture/diff-system.md) — diff subsystem Magit consumes
- [`../../architecture/diff-extraction.md`](../../architecture/diff-extraction.md) — `SubsystemBoot` pattern
- [`../../architecture/host-provider-boundary.md`](../../architecture/host-provider-boundary.md) — boundary that inverts Magit out
- [`../../architecture/mode-architecture.md`](../../architecture/mode-architecture.md) — Mode trait, ModeActivator, ActionHandlerRegistry
- [`../../architecture/compilation-mode.md`](../../architecture/compilation-mode.md) — synthetic-buffer + process spawning pattern
- [`../implementation.md`](../implementation.md) — per-slice status ledger
