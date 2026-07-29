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
| MG.12 | Destructive-action parity — confirm before branch delete / stash drop | MG.9 | ✅ |
| MG.13 | Action handlers at boot, not activation (was: "binding testability") | MG.8 | ✅ |
| MG.14 | Headerline across every magit buffer | MG.4–MG.11 | ✅ |
| MG.15 | Stash detail view (`<CR>` in magit-stash) | MG.9, MG.14 | ✅ |
| MG.16 | Remote/stash ex-command parity (`:magit-push` etc.) | MG.8 | ✅ |
| MG.17a | Transient flags (`--force-with-lease`, `--prune`, …) + preview | MG.8 | ✅ |
| MG.17b | Transient `Argument` items (prompt → back to the menu) | MG.17a | ✅ |
| MG.18 | Hunk-level staging | MG.5, MG.13 | 📝 |
| MG.19 | magit-diff side-by-side + `do`/`dp` | MG.18, D.4 | 📝 |
| MG.20 | Operation coverage — reset / revert / cherry-pick | MG.17a | ✅ |
| MG.21 | Remaining operations — tag, merge beyond `m`, bisect, submodule, remotes | MG.17b | 📝 |
| MG.22 | `magit-hunk-mode` — the mode owning diff *content* | MG.20 | 📝 |

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

### MG.20 — reset / revert / cherry-pick ✅ (2026-07-29)

`A` (cherry-pick), `V` (revert), `Os` / `Om` / `Oh` (reset
soft/mixed/hard) — Emacs magit's own keys — working in **every view
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
`V` on a staged file or a `--graph` connector does nothing rather than
acting on a neighbour.

- **`Oh` is the only one that asks.** `reset --hard` discards
  uncommitted work irrecoverably; `--soft` and `--mixed` keep it.
  Prompting on the safe two would train the user to dismiss the prompt
  that matters. Registered in `confirm::DESTRUCTIVE_ACTIONS`, so the
  ask half performs no git call at all.
- **`revert` passes `--no-edit`.** git would otherwise open `$EDITOR`
  for the message — inside lattice that is a hang on a prompt the user
  cannot answer, not a UI.
- The five new chords were validated automatically by MG.15's
  cross-cutting guard (every chord resolves to a registered action AND
  a handler) without any new bespoke test.

**Not shipped, deferred to MG.21:** tag (needs a name prompt — the
MG.17b `Argument` machinery now exists for it), merge beyond
magit-branch's `m`, bisect, submodule, remote management. The plan's
own rule holds: a menu row that does nothing is worse than an absent
one, so none of these appear in a transient yet.

**Also not shipped:** transient entries for the three that DID land.
They are reachable by chord in every commit-showing view, which is the
primary path; a `C-c g` entry needs a commit target and the root
dispatch is context-free, so it wants the same "act on the current
buffer" resolution `C-c f` uses. Worth doing with MG.21 rather than
half-wiring now.

- **Tests:** 4 in `lattice-magit` (argv appends the target;
  only the destructive reset asks; revert never opens `$EDITOR`; the
  confirm targets its real execute half), plus the existing chord guard
  covering all five bindings.

### MG.22 — `magit-hunk-mode` 📝

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
