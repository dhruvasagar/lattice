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
| PICK.1 | Picker transient-mode extension | None (picker subsystem) | 📝 |
| MG.1 | `lattice-magit` crate scaffolding | VCS.2, mode-architecture | 📝 |
| MG.2 | magit-status buffer rendering | MG.1 | 📝 |
| MG.3 | magit-status actions (s/u/x, cc/ca, =, p, <CR>) | MG.2 | 📝 |
| MG.4 | magit-commit buffer | MG.2 | 📝 |
| MG.5 | magit-diff buffer | MG.2 | 📝 |
| MG.6 | magit-log buffer | MG.2 | 📝 |
| MG.7 | magit-blame buffer | VCS.2 | 📝 |
| MG.8 | Transient menus (picker transient mode) | MG.3, PICK.1 | 📝 |
| MG.9 | magit-stash, magit-branch, magit-rebase | MG.8 | 📝 |
| MG.10 | Polish + perf + edge cases | MG.1–MG.9 | 📝 |

## Dependency graph

```
VCS.1 → VCS.2 ─┬─→ MG.1 → MG.2 → MG.3 ─┬─→ MG.8 → MG.9
                │       │    ├── MG.4   │
PICK.1 ─────────┘       │    ├── MG.5   │
                         │    ├── MG.6   │
                         │    └── MG.7 ──┘
                         │
                         └── (MG.7 also reads VCS.2 data for blame)
```

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

## Cross-references

- [`../../architecture/magit.md`](../../architecture/magit.md) — design fragment (what + why)
- [`../../architecture/diff-system.md`](../../architecture/diff-system.md) — diff subsystem Magit consumes
- [`../../architecture/diff-extraction.md`](../../architecture/diff-extraction.md) — `SubsystemBoot` pattern
- [`../../architecture/host-provider-boundary.md`](../../architecture/host-provider-boundary.md) — boundary that inverts Magit out
- [`../../architecture/mode-architecture.md`](../../architecture/mode-architecture.md) — Mode trait, ModeActivator, ActionHandlerRegistry
- [`../../architecture/compilation-mode.md`](../../architecture/compilation-mode.md) — synthetic-buffer + process spawning pattern
- [`../implementation.md`](../implementation.md) — per-slice status ledger
