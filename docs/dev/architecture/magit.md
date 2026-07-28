# Magit — Git porcelain as a core plugin

**Status:** design fragment. Contracts, data model, mode decomposition, keymap
surface, performance posture, WIT-gap analysis, slice plan. Supersedes
[`vcs-and-magit.md`](vcs-and-magit.md) (2026-05-31 sketch; archived 2026-07-25
— see header in that file). This fragment owns *what* and *why*. Slice
sequencing + per-slice status lives in
[`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md).

Sibling fragments: [`diff-system.md`](diff-system.md) (diff data model +
subsystem), [`diff-extraction.md`](diff-extraction.md) (diff as modes via
`SubsystemBoot`), [`compilation-mode.md`](compilation-mode.md) (the
synthetic-buffer + process-spawning pattern Magit reuses),
[`host-provider-boundary.md`](host-provider-boundary.md) (the boundary drawing
that keeps magit inverted out of `lattice-host`).

## 1. Why this fragment exists

The diff system (D.1–D.8) is complete: two-way/three-way/side-by-side/inline
overlay, gutter signs, hunk motions and operators, virtual rows, scroll-binding,
fold providers. The VCS data layer (`lattice-vcs` crate) and subsystem
(auto-inline-diff against HEAD, git watcher, `RepositoryEvent`) are designed but
**not implemented** — there is no `lattice-vcs` crate, no `RepositoryWatcher`,
no `GitBaseline`.

This fragment locks in:

1. **The complete Magit view surface** — every buffer view, its mode, its keymap,
   its data dependencies, so the VCS Layer 1+2 implementation can be sized to
   feed Magit's needs from day one (not retrofitted later).
2. **The deliverable path** — whether Magit ships as a WASM component plugin
   (the `vcs-and-magit.md` original intent) or as a native Rust crate through the
   `SubsystemBoot` seam (the pattern proven by diff-extraction, compilation-mode,
   and claude-code). §15 evaluates both against the paramount goals and
   heuristics.
3. **The mode decomposition** — one major mode per view type, `magit-core` as a
   shared minor mode, every chord and action handler owned by the crate that
   defines it. No `Editor::do_magit_*` methods. The acid test.
4. **The `lattice-vcs` vs `lattice-magit` crate split** — §3.1 explains why they
   are separate crates and why merging them would violate the host-provider
   boundary.

When the design is locked and VCS Layers 1+2 are built, Magit lands as the
**first heavy core plugin** — the feature that proves the extension architecture
can host serious, production-grade functionality.

> **UX (higher court):** the UX is Magit's — section-collapsible status buffer,
> transient prefix menus, hunk-at-a-time staging, inline diffs of staged changes
> in the commit buffer, blame annotations alongside code, log with graph. Every
> view is a buffer-backed Document. The UX convention is muscle memory from magit,
> fugitive, and gitsigns; the implementation carries zero new render-path risk
> because the renderer already handles the primitives (folds, virtual rows, gutter
> signs, inline diffs, styled text).

## 2. Paramount-goal alignment

- **#1 Performance.** Every git operation runs on `spawn_blocking`; buffer
  content is generated off-thread and published via `apply_edit_batch_blocking`.
  The renderer sees ordinary Documents with ordinary row streams — zero new
  hot-path branches. Per-keystroke overhead in a magit buffer is identical to
  any other buffer (chord dispatch + mode filter + action handler). No
  per-frame WASM calls.

- **#2 Extensibility.** Magit installs through the same `SubsystemBoot` seam as
  every other core feature (claude-code, terminal, compilation, diff modes).
  A third-party magit-equivalent — built on a different VCS backend (jujutsu,
  sapling, fossil) — can install through the same seam with zero host changes.
  The WASM migration path (§14) is a documented upgrade from native crate to
  component plugin; the mode architecture is identical, only the dispatch
  boundary moves.

- **#3 Extensible vim modal editing.** Every magit chord is registered through
  the `CommandRegistry` and lives at `KeymapLayer::MinorMode(magit-core)` or a
  per-view major-mode layer. Chords compose with vim grammar where applicable
  (e.g. `v` + hunk motions in a diff view = visual selection over hunks).

- **#4 Asynchronicity.** Git process I/O streams through the event bus → tick
  drain → `apply_edit_batch`. The editor actor never blocks on git. Async
  buffer progress (headerline spinner + stage) follows the `*messages*` and
  `*compilation*` precedent.

## 3. Architecture: the three-layer model

```
┌──────────────────────────────────────────────────────────────┐
│  Layer 3 — lattice-magit crate (feature-buffer modes)        │
│    Owns: magit-status-mode, magit-log-mode, magit-commit,    │
│    magit-blame, magit-diff, magit-stash, magit-branch,       │
│    magit-rebase, magit-core (shared minor mode).             │
│    Keymaps at MinorMode(magit-core) + MajorMode(per-view).   │
│    Installs through SubsystemBoot seam.                      │
│    INVERTED OUT of lattice-host (same tier as oil/file-tree).│
│    Deps: lattice-vcs, lattice-diff, lattice-mode,            │
│          lattice-runtime, lattice-core, lattice-protocol,    │
│          lattice-grammar, lattice-keymap.                    │
└────────────────────────┬─────────────────────────────────────┘
                         │ consumes
┌────────────────────────▼─────────────────────────────────────┐
│  Layer 2 — lattice-host::vcs subsystem                       │
│    RepositoryWatcher (tokio fs-watcher on .git/HEAD, index,  │
│    refs/heads/*). Fires RepositoryEvent on event bus.        │
│    Auto-registers DiffSession(GitBaseline(HEAD, path)) on    │
│    DocumentOpened — gutter signs appear immediately.         │
│    Option git.auto-head-diff (default true).                 │
│    CORE — stays in host (same category as LSP/diff).         │
└────────────────────────┬─────────────────────────────────────┘
                         │ consumes
┌────────────────────────▼─────────────────────────────────────┐
│  Layer 1 — lattice-vcs crate (data layer)                    │
│    Repository / GitBlob / Reference / WorkingTree / Index /  │
│    Commit / Branch / Stash. gix dependency isolated.         │
│    Read-API + Write-API — both complete (not stubbed).       │
│    GitBaseline (implements DiffParticipantSource).           │
│    Zero lattice-* dependencies. Leaf crate.                  │
└──────────────────────────────────────────────────────────────┘
```

### 3.1 Why `lattice-vcs` and `lattice-magit` are separate crates

Two crates, not one, because they sit on opposite sides of the host-provider
boundary and serve different consumers with different dependency profiles.

|                                   | `lattice-vcs`                                                                                                      | `lattice-magit`                                                                                                                                |
|-----------------------------------|--------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------|
| **What it is**                    | Pure data layer — git object model + read/write API                                                                | Feature-buffer UI plugin — modes, keymaps, action handlers, synthetic buffers                                                                  |
| **`lattice-*` deps**              | Zero (only `gix`, `ropey`, `smallvec`)                                                                             | ~8 (`lattice-vcs`, `lattice-diff`, `lattice-mode`, `lattice-runtime`, `lattice-core`, `lattice-protocol`, `lattice-grammar`, `lattice-keymap`) |
| **Consumed by**                   | (a) `lattice-host::vcs` subsystem (Layer 2 auto-inline-diff — CORE), (b) `lattice-magit` (Layer 3 views — FEATURE) | The CLI binary (wired through `SubsystemBoot::install`)                                                                                        |
| **Host-provider boundary**        | CORE — host depends on it for auto-inline-diff (same tier as `lattice-diff`)                                       | INVERTED OUT — host must NOT name it (same tier as oil/file-tree)                                                                              |
| **Can host function without it?** | No — auto-gutter-diff-against-HEAD is table-stakes                                                                 | Yes — the editor is fully itself without magit views                                                                                           |
| **Platform deps**                 | Pure data — builds on any target                                                                                   | Process spawning, file watching — needs `std::process`, tokio, event bus                                                                       |

If the two were merged into one `lattice-magit` crate:

- The **host** (`lattice-host::vcs`) would depend on a crate that also pulls
  in mode infrastructure, keymaps, action handlers, and synthetic-buffer
  machinery — all of which are feature-buffer concerns the host should not
  see.
- The **host-provider boundary** would blur: the same crate provides both core
  data (consumed by the host's auto-inline-diff) and feature-buffer UI
  (consumed by the CLI binary's `SubsystemBoot` install). This is exactly the
  kind of "half-migration" the standing rule forbids.
- A **third-party VCS backend** (jujutsu, sapling) would need to reimplement
  the magit UI just to get the data layer — or the data layer would need to
  be extracted later anyway.

The split matches the existing precedent: `lattice-diff` (pure algorithm crate,
zero `lattice-*` deps) vs the diff subsystem mode + overlay providers
(extracted into `lattice-diff` via `SubsystemBoot`). The data layer stays
lean; the UI layer pulls everything it needs.

### 3.2 What changed from the vcs-and-magit.md sketch

- **Write-API is no longer stubbed.** The original sketch proposed stubbing
  `Index::stage_hunk`, `Commit::create`, `Branch::checkout` as
  `unimplemented!("D.7.write — Phase 7+")`. Magit needs stage/unstage/commit/
  branch/checkout/rebase/stash from day one — the write-API ships complete in
  Layer 1.
- **`spawn-process` is NOT gated on a WIT host-service.** Magit calls the git
  CLI on `spawn_blocking` — the same native `std::process::Command` pattern
  compilation-mode uses. No WIT host-service dependency. The lighthouse-designed
  `spawn-process` WIT seam is for WASM plugins; Magit v1 doesn't cross that
  boundary.
- **Rich buffer rendering does not require new WIT primitives.** Magit buffers
  are styled text with folds, virtual rows, and gutter decorations — all of
  which the renderer already supports through the mode-architecture primitives
  (decoration providers, fold overlay service, virtual-row provider registry).
- **Deliverable path is native crate, not WASM.** The original sketch committed
  to WASM-as-first-path. §15 evaluates both against the paramount goals +
  heuristics and recommends native crate for v1, with WASM migration documented
  as a post-v1 upgrade when the host-services surface matures.

## 4. View inventory

Every magit view is a synthetic Document with a major mode. Buffer names follow
the `*magit:<kind>*` convention (consistent with `*messages*`, `*lsp*`,
`*compilation*`).

### 4.1 magit-status (`*magit:status*`)

The primary workhorse. One buffer that shows the state of the current
repository.

**Sections** (top to bottom, each collapsible via fold). "Unpulled
commits" / "Unpushed commits" sections are target design, **not built** —
`SectionKind` has no such variants; the only ahead/behind signal today is
the two counts in the branch status line (below), not a per-commit list:

| Section | Content | Data source |
|---|---|---|
| Staged changes | Files with changes in the index — paths + status only. Diffs loaded on demand via `=`. | `WorkingTree::statuses(repo)` filtered to `Modified`/`Added`/`Conflicted` (staged) |
| Unstaged changes | Files with working-tree changes — paths + status only. Diffs loaded on demand via `=`. | `WorkingTree::statuses(repo)` filtered to `Modified`/`Deleted`/`Unmerged`/`Conflicted` (unstaged) |
| Untracked files | Untracked files, listed | `WorkingTree::statuses(repo)` filtered to `Untracked` |
| Stashes | List of stashes with messages | `Stash::list(repo)` via gix |
| Recent commits | Last N commits with abbreviated SHAs and subjects | `git log --oneline -N` (CLI, not gix) |

A `Conflicted` path appears in BOTH the Staged and Unstaged sections
simultaneously — real double-listing, not a bug (see §7.2's `entry_key`
note on why this matters for expansion state).

**Section rendering:** Each section has a header line (bold, with section name
and item count) followed by its body. Sections are fold regions anchored at the
header line. The fold engine handles `TAB` / `za` / `zM` / `zR` without
magit-specific code.

**Inlined diffs:** Staged and unstaged sections start as file lists with
status labels only. Pressing `=` on a file entry loads and inlines the diff
for that file — a local edit to the buffer, not a full refresh (§6.3 has
the real mechanism: a full-text `git diff` insert with syntax-highlight
spans, not virtual-row deletion blocks or a per-file cache).

**Headerline:** target design — a view-header virtual row showing branch
name + repo status indicator + upstream tracking (ahead/behind counts).
**Not wired up.** `SectionIndex::branch_status_line()` computes the string
(`branch [N↑] [N↓]`) but nothing calls it yet; branch/ahead/behind data is
computed during refresh but not currently surfaced to the user anywhere in
the buffer.

### 4.2 magit-log (`*magit:log*`)

Commit history with graph, branch and tag decorations.

- Arguments configurable via buffer-local options: `--all`, `--graph`,
  `--decorate`, `-N` (count), path filter.
- Output of `git log --oneline --graph --decorate -50` rendered as styled text.
- Each commit line: abbreviated SHA (dim), graph (styled), refs (colored tags),
  subject (normal).
- `<CR>` on a commit line opens `*magit:commit:<sha>*` showing the full diff.

### 4.3 magit-commit (`*magit:commit*`)

The commit-message editor. Opens from magit-status `c c` or directly via
`:magit-commit`.

- **Top region** (read-only, scrollable): inline diff of staged changes, the
  same render as the "Staged changes" section in magit-status.
- **Bottom region** (editable): the commit message — subject line, blank
  separator, body.
- `C-c C-c` (confirm): calls `Commit::create(repo, message)` → closes buffer →
  refreshes magit-status. `C-c C-k` (abort): closes buffer without committing.
- Amend support: `c a` pre-populates the previous commit message and uses
  `Commit::amend`.

### 4.4 magit-blame (`*magit:blame:<path>*`)

Per-line git blame annotations alongside the current file.

- Data from `git blame --line-porcelain <path>` via `spawn_blocking`.
- Annotation per line: abbreviated SHA (8 chars, colored by author), author
  name (truncated to N chars), relative date — rendered as gutter decorations.
- `<CR>` on a blame line opens `*magit:commit:<sha>*`.
- Blame data cached in a per-file `BlameLineMap`; invalidated on file changes.

### 4.5 magit-diff (`*magit:diff*`, or path-scoped `*magit:diff:<path>*`)

Target design: a two-pane side-by-side diff (reuses D.4 pane group
machinery), left pane staged/HEAD, right pane working tree, hunk-level
staging including partial-hunk staging from a Visual-mode selection.
**Not built.** The current `MagitDiffMode` is a real, scoped middle
ground, not the target: it populates the buffer with `git diff HEAD`
(staged + unstaged changes combined against HEAD, matching this
section's original "against HEAD" framing) as plain styled text —
one buffer, no panes, no `DiffSession`/`GitBaseline` integration. It
registers its OWN `s`/`u` handlers, scoped to this buffer's own
`DiffState`, resolving the file at cursor by scanning upward for the
nearest `diff --git a/<path> b/<path>` header — file-level only, not
hunk-level (no `x`/discard chord either). This closes a real bug: the
buffer used to open empty with `s`/`u` declared in the keymap but no
handler of their own, so pressing them silently fired whatever
`magit-status` handler happened to be registered, against
magit-status's captured state rather than this buffer's cursor. The
full side-by-side `DiffSession` + hunk-level staging design above
remains a real follow-up, larger than this pass.

The mode now also backs a path-scoped variant, opened by file-dispatch's
`d` item (§8.8): buffer name `*magit:diff:<path>*` instead of the bare
`*magit:diff*`. `on_activate` parses the optional path back out of the
buffer name (`store.name_for(buffer_id)` stripped of the
`*magit:diff:`/`*` wrapper) and runs `git diff HEAD -- <path>` instead of
the whole-tree `git diff HEAD` — same mode, same `s`/`u` handlers, same
`DiffState`, just a narrower git invocation. The unscoped `:magit-diff`
ex-command is unaffected; it still opens the bare `*magit:diff*` buffer.

### 4.6 magit-stash, magit-branch, magit-rebase

Each is a synthetic Document with its own major mode:

- **magit-stash** (`*magit:stash*`): stash list with apply/pop/drop/create
  operations.
- **magit-branch** (`*magit:branch*`): branch list with checkout/delete/merge
  operations. `c` (create) opens a two-step picker-driven wizard (pick base
  branch, then type the new name) — see §12.9.
- **magit-rebase** (`*magit:rebase[:<upstream>]*`): interactive rebase todo
  buffer. The upstream is encoded in the buffer name, mirroring magit-blame's
  file-in-buffer-name pattern (`*magit:rebase*` with no arg falls back to
  resolving `@{upstream}`). The todo is real — built from `git log --reverse
  --format="pick %h %s" <upstream>..HEAD`, not a placeholder. Editable
  pick/reword/squash/fixup/drop list. `C-c C-c` writes the buffer's
  (possibly user-edited) non-comment lines to a temp file and actually
  STARTS the rebase via `git rebase -i <upstream>` with
  `GIT_SEQUENCE_EDITOR="cp <tempfile>"` and `GIT_EDITOR=true` (accepts a
  `reword` step's original message unchanged — no message-edit UI, a known
  limitation). `C-c C-k` runs `git rebase --abort` only if
  `.git/rebase-merge` or `.git/rebase-apply` actually exists, so it can't
  fail against a rebase that was never started.

### 4.7 magit-revision (`*magit:commit:<sha>*`)

A read-only `git show --stat -p <sha>` view of a single commit. Opened by
magit-log's `<CR>` and magit-blame's `<CR>`, both of which resolve a sha and
open this buffer rather than duplicating a "show one commit" view per
caller. No mode-specific chords — `q`/`gr`/nav come from `magit-core` (this
mode is in its `ActivationPolicy::Majors` list); `gr` is a harmless no-op
here since a commit's content doesn't change under a fixed sha.

## 5. Mode decomposition

One crate, two layers of modes: a **shared minor mode** (`magit-core`) active
on every magit buffer, and **per-view major modes** for view-specific chords
and lifecycle.

```
lattice-magit crate
├── magit-global-mode   (MinorMode, ActivationPolicy::Universal)
│   ├── Keymap:  C-x g  → :magit-status
│   │            C-c g  → :magit-dispatch (Effect::OpenTransient)
│   │            C-c f  → :magit-file-dispatch (Effect::OpenTransient)
│   └── ActionHandlers: action:magit-global-* (status/commit/log/branch/
│       stash/rebase/pull/push/file-stage/file-diff/branch-create-finish) —
│       the root dispatch + file-dispatch transients' items (and the
│       branch-create wizard's prompt) fire these, registered ONCE
│       process-wide via a static OnceLock (see §8.8)
│
├── magit-core          (MinorMode, ActivationPolicy::Majors([...9 major
│   │                    modes incl. magit-revision-mode]))
│   ├── Keymap:  gr     → refresh
│   │            q      → close (bury, return to previous buffer)
│   │            ]] / [[→ next/prev section
│   │            ]f / [f→ next/prev file/entry
│   │            ]c / [c→ next/prev hunk
│   │            TAB    → toggle section fold
│   │            S-TAB  → cycle section visibility
│   ├── ActionHandlers: refresh, close, navigation, fold
│   └── Shared: `ActionRegsGuard` — RAII guard every non-status mode below
│       uses to hold its `ActionHandlerRegistration`s (see §5.2)
│
├── magit-status-mode   (MajorMode)
│   ├── Keymap:  s / u / x → stage / unstage / discard file at cursor
│   │            cc / ca    → commit / amend
│   │            =          → toggle inline diff (file-level)
│   │            p          → stage hunk interactively (not supported — see §12.2)
│   │            <CR>       → open/visit at cursor
│   ├── ActionHandlers: stage/unstage/discard at cursor, open views
│   └── Buffer provisioning: ensure *magit:status*, populate, refresh;
│       registers its own `MagitStatusFoldSource` (§7.1)
│
├── magit-commit-mode   (MajorMode)
│   ├── Keymap:  C-c C-c → commit
│   │            C-c C-k → abort
│   ├── ActionHandlers: commit, abort
│   └── Buffer provisioning: ensure *magit:commit*/*magit:amend*, populate
│       staged diff + (amend) prior message
│
├── magit-log-mode      (MajorMode)
│   ├── Keymap:  <CR>   → open *magit:commit:<sha>* (magit-revision-mode)
│   ├── ActionHandlers: open commit, refresh
│   └── Buffer provisioning: ensure *magit:log*, run git log, render
│
├── magit-revision-mode (MajorMode) — new: `git show --stat -p <sha>` for
│   │                    one commit; buffer name carries the sha
│   ├── Keymap:  (none — q/gr/nav inherited from magit-core)
│   └── Buffer provisioning: ensure *magit:commit:<sha>*, render
│
├── magit-blame-mode    (MajorMode)
│   ├── Keymap:  <CR>   → open *magit:commit:<sha>* for blamed line
│   │            p      → re-blame at the parent of the revision shown
│   ├── ActionHandlers: open commit at SHA, re-blame at parent
│   └── Buffer provisioning: ensure *magit:blame:<path>*, render annotations
│       inline (not a gutter column — see §10.2)
│
├── magit-diff-mode     (MajorMode)
│   ├── Keymap:  s / u → stage / unstage file at cursor (no `x`)
│   ├── ActionHandlers: stage/unstage file at cursor, refresh
│   └── Buffer provisioning: `git diff HEAD` (staged+unstaged combined) as
│       plain text — no `DiffSession`, no panes (§4.5)
│
├── magit-stash-mode    (MajorMode)
│   ├── Keymap:  a → apply stash    p → pop stash
│   │            d → drop stash     z → create new stash
│   ├── ActionHandlers: apply/pop/drop/create stash
│   └── Buffer provisioning: ensure *magit:stash*, list stashes
│
├── magit-branch-mode   (MajorMode)
│   ├── Keymap:  <CR>  → checkout branch
│   │            c     → open branch-create wizard (Effect::OpenPicker
│   │                    { source: "magit-branch-pick-base" })
│   │            d     → delete branch
│   │            m     → merge branch
│   ├── ActionHandlers: checkout/delete/merge branch; create's second step
│   │       (`action:magit-branch-create-finish`) is a GLOBAL handler in
│   │       magit-global-mode, not here — it fires against the PROMPT
│   │       buffer opened by the wizard's picker step, not this buffer
│   └── Buffer provisioning: ensure *magit:branch*, list branches
│
└── magit-rebase-mode   (MajorMode)
    ├── Keymap:  C-c C-c → execute rebase
    │            C-c C-k → abort rebase (only if a rebase is in progress)
    │            editable buffer (pick/reword/squash/fixup/drop), a REAL
    │            todo from `git log --reverse` against the resolved upstream
    ├── ActionHandlers: execute/abort rebase
    └── Buffer provisioning: ensure *magit:rebase[:<upstream>]*, populate
        real todo list
```

**Standing-rule check (mode ownership):** every chord (`s`, `u`, `c c`, `TAB`,
etc.) is registered by the mode that owns the buffer it fires on. The
action-handler body lives in the same crate as the keymap registration.
`lattice-magit/src/magit_status_mode.rs` contains both the keymap definition
AND the handler bodies (`crate::actions::register_action_handlers`). No
`Editor::do_magit_stage_hunk` shim exists. The acid test holds. One real
gotcha this surfaced: every non-status mode used to `mem::forget` its
`Vec<ActionHandlerRegistration>` rather than holding it in the mode's
`Guard` — harmless for a single open buffer, but with two buffers of the
same major mode open simultaneously the second's registration silently
replaced the first's (registry is last-write-wins per `CommandId`), so
firing a chord in buffer A could execute buffer B's captured state against
A's cursor. Fixed by a shared `ActionRegsGuard` (`magit_core_mode.rs`) that
all modes but magit-status now hold — magit-status already did this
correctly via its own `MagitStatusGuard`.

### 5.1 Activation policy

- **`magit-core` minor mode**: `ActivationPolicy::Majors(vec![...])` naming
  every magit major mode explicitly (status, commit, diff, log, blame,
  stash, branch, rebase, revision) — not a major-id pattern matcher.
- **`magit-global-mode` minor mode**: `ActivationPolicy::Universal` — active
  on every buffer, of any kind, so `C-x g`/`C-c g`/`C-c f` work from
  anywhere. `on_activate` runs on EVERY buffer activation under this
  policy, not once (see §8.8 for the idempotency gotcha this creates for
  the global action handlers it also registers).
- **Major modes**: activated by id when the buffer is created via
  `ctx.activator.ensure_named_document(name, mode_id, flags)` (an ex-command's
  `apply` closure returning `Effect::OpenSyntheticBuffer { name, mode_id }`,
  resolved by the host the same way every other synthetic buffer is).

### 5.2 Lifecycle — `ModeActivator` + tick drain pattern

Each magit view follows the synthetic-buffer lifecycle proven by `*compilation*`
and `*messages*`, with one simplification from what was originally planned
here: there is no `RepositoryEvent` to subscribe to yet (VCS Layer 2 — the
`RepositoryWatcher` — is still design-only, see §1). Refresh is
trigger-driven, not filesystem-watch-driven:

1. **Trigger** (ex-command): calls `ctx.activator.ensure_named_document(name,
   mode_id, flags)`. The activator creates the buffer, sets the major mode,
   returns `BufferId`.
2. **`on_activate`**: pulls the `DocumentHandle`, runs an initial
   `spawn_blocking` refresh, populates the buffer, and registers this
   buffer's action handlers into a `Vec<ActionHandlerRegistration>` held by
   the mode's `Guard` (see §5's standing-rule check for why this matters).
3. **Refresh task** (`spawn_blocking`): runs the view's git command(s) —
   lightweight for magit-status (`git status --porcelain`-equivalent via
   gitoxide, `git stash list`, `git log --oneline -N`), heavier where the
   view IS the content (magit-diff's `git diff HEAD`, magit-blame's
   `git blame --line-porcelain`). Produces formatted text, applied via
   `apply_edit_batch`.
4. **Refresh trigger, in practice**: `gr` (explicit, every view) and
   post-mutation auto-refresh (stage/unstage/discard/checkout/etc. — see
   §13's async-architecture note) both re-run step 3. There is no
   background watch: an external change to the repo (another terminal's
   `git commit`) is not picked up until the user presses `gr` or triggers a
   mutation from within the buffer.
5. **Guard `Drop`** (deactivation): drops the action-handler registrations
   (unregistering them — see §5's standing-rule check), removes any
   registered fold source (magit-status only, §7.1). Buffer survives frozen
   until `:bd`.

## 6. Data flow — lazy by default

Magit buffers are **lazy**: only the data needed to paint the viewport is
fetched. Expensive operations (diffs, blame, commit details) are deferred
until explicitly invoked by the user. This is the single most important
performance decision in the magit design — Emacs magit slows down on large
repos because it pre-computes every diff on status open; Lattice never does.

### 6.1 Lazy strategy per view

| View | On open / refresh | On demand |
|---|---|---|
| **magit-status** | `git status --porcelain` (file paths + status letters), `git stash list`, `git log --oneline -N` | `=` loads `git diff --cached <path>` / `git diff <path>` per-file |
| **magit-diff** | Nothing (explicitly invoked by user) | Diff loaded on open (the view IS the diff) |
| **magit-log** | `git log --oneline --graph --decorate -N` | `<CR>` loads `git show <sha>` for the commit at cursor |
| **magit-blame** | Nothing (explicitly invoked by user) | Blame loaded on open (the view IS the blame) |
| **magit-commit** | `git diff --cached` (the only exception — reviewing staged changes IS the purpose) | — |

**Why magit-commit is the exception:** The commit buffer exists solely to
review staged changes and write a message. The staged diff IS the content
the user came to see — deferring it would leave an empty buffer. However,
it's scoped to staged changes only (not the full working tree), and loading
is async with a headerline progress indicator.

### 6.2 Status buffer refresh — fast path

Triggered by `gr` or by an auto-refresh after a mutation (stage/unstage/
discard) — NOT by a filesystem watch (§5.2: there is no `RepositoryEvent`
yet). The refresh task runs on `spawn_blocking` (`refresh::build_and_format`):

1. `WorkingTree::statuses(repo)` → list of changed files with status codes
   (gitoxide — fast, no process spawn).
2. `Stash::list(repo)` → stash entries (gitoxide).
3. `git log --oneline -20 --format="%h %s"` → recent commits (CLI, not gix).
4. Format sections as file lists with status labels via `SectionIndex::
   format_buffer_styled`. **No diffs.**
5. `apply_edit_batch` (async, on the current task, after the blocking phase
   completes) replaces buffer content wholesale.
6. Clear `StatusBufferState::expanded` — a full-buffer replace collapses any
   inline expansion, so stale entries there would desync the next `=`/`<CR>`
   press's "already expanded" check from what's actually on screen (§6.3).

The buffer shows file names and statuses only — a list view, not a diff
view. Rendering is O(files), not O(lines-of-diff).

### 6.3 Lazy diff/patch loading — `=` on a file, `<CR>` on a stash, `d` for a dedicated buffer

One shared mechanism (`toggle_expand` in `actions.rs`) backs `=` on a file
entry and `<CR>` on a stash entry — there is no per-kind duplication. The
cursor line is classified first (`classify_line` → `StatusLine::{File,
Stash, Commit}`, §7.2), then:

1. Look up the entry's `entry_key` in `StatusBufferState::expanded:
   HashMap<String, usize>` (key → inserted line count, not a hunk-aware
   cache — there is no `DiffCache`, no hunk parsing, no per-file `git diff`
   caching keyed by status version).
2. If already expanded: delete exactly the recorded line count starting the
   row after the entry (`collapse_range` — the end must land at column 0 of
   the following row, not that row's text length, or the collapse eats the
   next entry's content; see the fix's regression tests in `actions.rs`).
3. If not expanded: run `git diff [--cached] -- <path>` (file) / `git stash
   show -p` (stash) on `spawn_blocking`, apply line-level syntax-highlight
   spans (add/remove/hunk-header/diff-header — `highlight::diff_styled_spans`,
   NOT the diff system's virtual-row deletion-block machinery), insert the
   whole text as one edit below the entry line, and record the inserted
   line count in `expanded`.

Both branches record/clear `expanded` only AFTER the edit has actually
landed (not eagerly before spawning the task) — recording eagerly let a
rapid second toggle race ahead of the edit and compute a delete/insert
range against rows that didn't reflect it yet.

`=` on a section header does NOT currently expand every file in the
section — only single-entry toggle is implemented.

`<CR>` on a **commit** entry does NOT go through `toggle_expand` — it opens
the dedicated `*magit:commit:<sha>*` buffer (`magit-revision-mode`),
matching every other magit view with a per-row SHA (log, blame, rebase).
This changed from an earlier inline-toggle behaviour identical to `=`/stash
— kept for stashes (which have no dedicated "stash detail" mode to open)
but inconsistent for commits, which do.

`d` on a **file** entry (staged or unstaged) is a third path, independent
of `toggle_expand`: it opens a dedicated `magit-diff-mode` buffer scoped to
that file AND that section's baseline (`*magit:diff:staged:<path>*` /
`*magit:diff:unstaged:<path>*` — see `magit_diff_mode::DiffScope`), rather
than inserting the diff inline. Exists alongside `=`, not instead of it —
`=` is the quick in-place look, `d` is for a diff too large to read
comfortably without its own scrollable buffer (and without growing the
status buffer's line count or re-triggering its splice-based inline-
highlight bookkeeping for every subsequent entry).

### 6.4 Expansion tracking — `StatusBufferState::expanded`

The target design's per-file `DiffCache` (parsed hunks, per-file staging,
staleness detection) is **not built**. What exists is much simpler — a
map from entry identity to how many buffer lines its expansion occupies:

```rust
pub struct StatusBufferState {
    // ...
    pub expanded: HashMap<String, usize>,
}
```

Keyed by `entry_key()`, which includes `staged` for `File` entries — a
`Conflicted` path appears in BOTH the Staged and Unstaged sections at once
(§4.1), as two distinct buffer rows that can be independently expanded;
collapsing that distinction into a path-only key would let expanding one
row's diff make `toggle_expand` treat the *other* row as already-expanded
too, corrupting the collapse.

No hunk data is parsed or stored — `s`/`u`/`x` always act at file
granularity (§7.3), regardless of whether the diff is expanded. No
invalidation-on-repo-change exists either, since there's no
`RepositoryEvent`: a full refresh (§6.2) simply clears the whole map,
collapsing every expansion unconditionally.

### 6.5 Process spawning

## 7. Section model and fold integration

Sections are the core UX primitive in magit-status. By default, sections show
**file paths and status labels only** — no diffs are pre-computed. Diffs load
on demand when the user presses `=` on a file entry (§6.3).

```
Staged changes (3)                                    ← header (not foldable — §7.1)
  modified     src/main.rs                             ← file entry (status only)
  modified     src/lib.rs
  ...
```

After pressing `=` on `src/main.rs` (actual current rendering — no
separator lines, no fold triangle; the raw `git diff` text is inserted
directly below the entry with syntax-highlight spans, and the whole
inserted span is what `MagitStatusFoldSource` folds as one unit):

```
Staged changes (3)
  modified     src/main.rs                             ← file entry (state: expanded)
diff --git a/src/main.rs b/src/main.rs
@@ -1,2 +1,2 @@
-old line
+new line
  modified     src/lib.rs                              ← file entry (status only, not expanded)
  ...
```

### 7.1 Section folds

- **Section header → fold range.** Target design; **not built** — there is
  no fold registered for the section headers themselves today, only for
  *expanded entries* (below). `TAB`/`S-TAB` at a section header currently
  have nothing to fold there.
- **Expanded-entry fold → nested fold.** What IS built, and differs from
  the original design: `MagitStatusFoldSource` (`fold_source.rs`) is an
  overlay `FoldSource` — mirroring `lattice_diff::HunkFoldSource`'s shape —
  that recomputes fold ranges live from `StatusBufferState::expanded` plus
  a buffer scan on every `compute_folds()` call, rather than caching stale
  line numbers. For each expanded entry it emits one outer fold (header
  line through the end of the inserted patch) containing one inner fold
  per `@@ ...@@` hunk within it — nesting expressed the same way the
  generic fold engine expresses it everywhere else: range containment.
- **Registered by `MagitStatusMode`, not `magit-core`.** `MagitStatusMode::
  on_activate` registers the source via the generic `FoldOverlayService`
  extension point (`FoldOverlayServiceHandle::add_source`); `MagitStatusGuard::
  drop` removes it — same Drop-based lifecycle `DiffModeGuard` and
  `MultibufferModeGuard` use. Other magit views have no folds at all today.
- **`magit-core`'s `TAB`/`S-TAB` dispatch to the generic fold engine.**
  `Effect::AppAction(ToggleFoldAtCursor)` / `CycleFoldsGlobal` — previously
  these unconditionally returned `None` (dead keys).

### 7.2 Section index (mode-private data)

The section index is an in-memory data structure built during buffer refresh.
It stores file paths and status labels — **not diffs**:

```rust
pub struct SectionIndex {
    pub sections: Vec<Section>,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
}

pub struct Section {
    pub kind: SectionKind,       // Staged, Unstaged, Untracked, Stashes, RecentCommits
    pub header_line: usize,
    pub body_start: usize,
    pub body_end: usize,
    pub entries: Vec<SectionEntry>,
}

pub enum SectionEntry {
    File { path: PathBuf, status: PathStatus },
    Stash { index: usize, message: String },
    Commit { sha: String, subject: String },
    UntrackedFile { path: PathBuf },
}
```

This matches the real `sections.rs` types (the target design's separate
`DiffCache` keyed by `(path, section)` with parsed `Hunk`/`ParsedHunk` data
does not exist — see §6.4 for what actually tracks expansion state).

A second, independent data structure resolves *which* line the cursor is
on back to a `StatusLine` — `classify_line`/`classify_line_text` in
`actions.rs`. It matches the line against `FILE_LABELS` (a fixed list —
`"clean"`, `"modified"`, `"new file"`, `"deleted"`, `"untracked"`,
`"ignored"`, `"unmerged"` — checked as whole-word prefixes) rather than
guessing at word boundaries. This exists because a naive "split the line on
the first space" parse mis-splits the two-word `"new file"` label, taking
`"file"` as the label and the wrong half of the path — a real bug this
replaced. `status_label()` (`sections.rs`) and `FILE_LABELS` (`actions.rs`)
must stay in sync; a unit test in `sections.rs` checks that every rendered
label is a member of `FILE_LABELS` mechanically rather than by inspection.

Both live as helper data in `lattice-magit` (`StatusBufferState` on the
mode's `Guard`-reachable state, not `RefCell`s specifically) — NOT on
`Document` or `Editor`. Consumed only by magit action handlers. This is the
**substrate-helper** pattern (CLAUDE.md substrate-vs-helper rule): consumed
only by the mode's own handlers → lives as helper data in `lattice-magit`,
not as a `Document` trait method.

### 7.3 Staging from sections — file-level only

Target design: hunk-level `s`/`u`/`x` when the diff is expanded, file-level
otherwise. **Not built.** The real `s`/`u`/`x` (and `<CR>`'s stash/commit
toggle) always resolve `classify_line` at the cursor and act on the whole
`StatusLine::File`/`Stash`/`Commit` — whether or not its diff/patch happens
to be expanded makes no difference to what the action does. There is no
code path that inspects cursor position *inside* an expanded diff to
resolve a hunk. `git add -p`-equivalent hunk staging is explicitly
unsupported today (§12.2's `p` chord explains why: it's genuinely
interactive over stdin, which the TUI's raw-mode input loop already owns —
running it via `Command::output()` would hang the actor waiting on a child
that's also waiting on stdin neither process routes to the other).

### 7.4 Stale hunk boundary detection

Not applicable yet — there is no hunk-level staging (§7.3) to have stale
boundaries in the first place. This section describes a target-design
safeguard for when hunk-level staging is eventually built, not a current
mechanism.

### 7.5 Navigation model — three levels, three chord families

The magit-status buffer has a three-level structure:
**sections → files/entries → hunks**. Navigation follows vim-unimpaired
conventions adapted to this hierarchy.

| Level | What you navigate | Chord family | Walks (actual) | Registered by |
|---|---|---|---|---|
| **Sections** | Top-level section headers (Staged, Unstaged, Untracked, Stashes, Recent commits) | `]]` / `[[` | A raw buffer scan for lines matching `sections::is_section_header` (untrimmed-line prefix check against `SECTION_HEADER_PREFIXES`) — NOT `SectionIndex::sections` (that structure isn't retained past the refresh that built it) | `magit-core` |
| **Files/entries** | Any indented, non-blank line | `]f` / `[f` | A raw buffer scan for lines whose UNTRIMMED text starts with `"  "` — every entry line, not "current section" scoped; see below for a real bug this fixed | `magit-core` |
| **Hunks** | `@@ ...@@` / `diff --git` lines anywhere in the buffer | `]c` / `[c` | A raw buffer scan for lines starting with `@@` or `diff --git` (trimmed) — same scan in every magit buffer, magit-status and magit-diff alike | `magit-core` |

None of these three walkers consult `SectionIndex`, `StatusBufferState::
expanded`, or any per-file cache — they are plain buffer-text scans
(`section_headers`/`entry_lines`/`hunk_lines` in `magit_core_mode.rs`),
identical in every magit buffer kind. The target design's
`DiffCache`-aware "only walk expanded files' hunks" behavior for
magit-status, and the `HunkIndex`-backed `DiffSession` navigation for
magit-diff, are **not built** — there is no `DiffSession` or `diff-mode`
minor mode active on any magit buffer; `]c`/`[c` are entirely `magit-core`'s
in every case.

**A real bug this uncovered:** `entry_lines` used to check
`starts_with("  ")` on the line AFTER trimming it — `trim()` strips all
leading whitespace, so a trimmed string can never start with two spaces.
The check was unsatisfiable; `]f`/`[f` never navigated anywhere, on any
magit buffer, from the moment they were written. Fixed by checking the raw
(untrimmed) line and trimming only for the prefix comparisons that follow.

**Multi-file navigation** (`]E`/`[E`, `]e`/`[e`) from the multibuffer system
is inherited when a project-wide-diff multibuffer includes magit excerpts —
zero magit-specific code needed for cross-file jumping. (Aspirational: no
magit view currently participates in a project-wide-diff multibuffer.)

**Section visibility cycling** (`S-TAB`) — target design: a 4-step cycle
from all-expanded down to only-section-headers-visible. **Not built as
described**, since there are no section-level folds to cycle through
(§7.1) — `TAB`/`S-TAB` dispatch to the generic fold engine
(`Effect::AppAction(ToggleFoldAtCursor)` / `CycleFoldsGlobal`) and only
ever have something to act on where `MagitStatusFoldSource` has registered
a fold: an expanded entry's header (outer) or one of its `@@` hunks
(inner). Folding an unexpanded file/section header is currently a no-op —
there's nothing registered there to fold.

## 8. Transient menu design

Magit's "transient" presents groups of options with single-key selection,
toggleable flags, argument inputs, and a live command preview. Rather than
building a separate `PopupMenu` host primitive, the transient uses the
**existing picker** as its rendering and interaction surface — extended
with a transient interaction mode.

### 8.1 Why the picker

The picker already provides everything a transient menu needs as rendering
and interaction substrate:

- **Floating overlay rendering** — TUI + GPUI parity, styled text, scroll
- **Keyboard capture** — `j`/`k` navigation, `q`/`Esc` dismiss, single-key
  chords via the existing key capture path
- **Display preferences** — `PickerDisplay::BottomPopup`, `Floating`,
  `Inline`; the transient can configure its position
- **Preview pane** — already renders above/below the candidate list;
  repurposed as the transient's live command preview
- **Header/footer** — picker title becomes the transient title; footer
  becomes the dismiss/back hint

What the picker needs for transient mode:

1. **Grouped, non-filterable entries** — the transient is a fixed action
   menu, not a searchable candidate list. A `PickerMode::Transient` variant
   tells the picker to render grouped entries with section headers instead
   of a flat filterable list.
2. **Single-key triggers** — each entry carries a key binding; pressing that
   key fires the entry's action without cursor navigation.
3. **Toggleable flags** — entries that represent boolean flags; pressing the
   key toggles the value in-place and updates the preview line.
4. **Argument value entry** — entries that open a minibuffer prompt for a
   value; on confirm, the value is set and control returns to the transient.
5. **Nested transients** — entries that open a new transient, replacing the
   current one. `DEL` or `BS` returns to the parent.

### 8.2 `TransientSpec` — the data model

A transient is a data structure the mode defines and passes to the picker.
The picker renders it and drives the interaction.

```rust
/// Lived in `lattice-picker`, extended for transient support.
pub struct TransientSpec {
    pub title: String,
    pub groups: Vec<TransientGroup>,
    pub preview: Option<Box<PreviewFn>>,
    pub footer: Option<String>,
}

pub struct TransientGroup {
    pub label: String,          // "Actions", "Arguments", "Configure"
    pub items: Vec<TransientItem>,
}

pub struct TransientItem {
    pub key: Vec<KeyChord>,     // ["b"], ["c"], ["C-c", "C-c"]
    pub label: String,          // "checkout"
    pub description: String,    // "Check out a branch"
    pub kind: TransientItemKind,
}

pub enum TransientItemKind {
    Action(ActionId),
    Submenu(TransientSpec),
    Flag { name: String, value: bool },
    Argument { name: String, value: Option<String>, prompt: String },
}

/// Builds the live command preview from current transient state.
/// Called every time a flag toggles or an argument changes.
pub type PreviewFn = dyn Fn(&TransientState) -> String + Send;

/// Accumulated state: flag values and argument values.
/// Keyed by the `name` field from Flag and Argument items.
pub type TransientState = HashMap<String, TransientValue>;

pub enum TransientValue {
    Bool(bool),
    String(String),
}
```

### 8.3 Picker integration

The picker's API gains an `open_transient` method alongside the existing
`open`:

```rust
impl PickerRegistry {
    /// Open a transient menu. Reuses the picker's rendering pipeline but
    /// switches to grouped, non-filterable display with single-key triggers.
    pub fn open_transient(
        &self,
        spec: TransientSpec,
        display: PickerDisplay,  // BottomPopup, Floating, etc.
    ) -> TransientHandle;
}
```

When opened:
1. The picker renders `title` as the picker header.
2. Each `TransientGroup` renders as a section: the group label in bold,
   followed by its items.
3. Each item renders as: `key` (highlighted), `label` (normal), `description`
   (dimmed marginalia).
4. Flag items show their current value (e.g., `[x]` or `[ ]`).
5. Argument items show their current value or a placeholder.
6. The preview line (from `preview` fn) renders in the picker's preview pane.
7. The footer renders at the bottom.

Keyboard handling:
- Single-key press on an Action item: fires `ActionId` via the handler
  registry, closes the transient.
- Single-key press on a Flag item: toggles `TransientValue::Bool`,
  re-renders in-place, updates preview.
- Single-key press on an Argument item: opens minibuffer prompt; on confirm,
  sets the value and returns to the transient.
- Single-key press on a Submenu item: closes current transient, opens the
  nested `TransientSpec`.
- `j`/`k` / `C-n`/`C-p`: scroll groups if they overflow the viewport.
- `q` / `Esc` / `C-g`: dismiss.
- `BS` / `DEL`: back to parent transient (if nested).

**What actually got built differs from §8.2/8.3 in a few ways — see §8.8
for the real mechanism.** In short: there is no `PickerRegistry::
open_transient()` call site (the real entry point is
`Effect::OpenTransient { source: String }`, resolved by a
`TransientSourceRegistry`, since `TransientSpec` can't cross into
`lattice-grammar`'s `Effect` enum); `TransientItemKind` really is `Action
(CommandId)` / `Submenu(Arc<TransientSpec>)` / `Flag { name, default }` /
`Argument { name, default, prompt }` / `Dismiss` (used for confirmation
dialogs' `n`/`q`); and `Argument` is currently a no-op when pressed (§8.8).

### 8.4 Magit transients — concrete layouts

The layouts below (marginalia, nested Branch/Merge/Rebase menus,
diffstat, ahead/behind counts, warning glyphs, `Flag` toggles) are the
**target design**. **Partially built** — see §8.8 for what actually
renders today. In short: the group structure and most action items are
real (including `commit` and `stash` sub-transients), but there is no
marginalia column, no `Flag`/`Argument` item anywhere, no preview line,
and no Branch/Merge/Rebase sub-transient. File-dispatch's six items are
real but still act on the active buffer's own file, not a
cursor-resolved entry.

Every entry in the examples below carries **marginalia** — live git data
rendered as dimmed right-aligned text alongside the entry. Marginalia is
resolved from `lattice-vcs` or the `SectionIndex` when the `TransientSpec`
is built, not per-frame. It gives the user decision-making context without
requiring separate git commands.

#### Dispatch menu (`C-c g` globally)

```
┌─ Magit ───────────────────────────────────────────────────────┐
│                                                                │
│  Magit dispatch                                on main 3↑ 2↓   │
│                                                                │
│  ▸ Working tree                                                │
│    [s]  stage               Stage changes              3       │
│    [u]  unstage             Unstage changes            2       │
│    [x]  discard             Discard changes                    │
│    [c]  commit              Commit changes                     │
│                                                                │
│  ▸ History                                                     │
│    [l]  log                 Show commit history        HEAD    │
│    [L]  log-ref             Show log for ref                   │
│                                                                │
│  ▸ Branches, merging, rebasing                                │
│    [b]  branch              Branch operations          ▸ main  │
│    [m]  merge               Merge operations           ▸      │
│    [r]  rebase              Rebase operations          ▸      │
│                                                                │
│  ▸ Stashing                                                    │
│    [z]  stash               Stash operations           ▸ 2    │
│                                                                │
│  ▸ Remotes                                                     │
│    [F]  fetch               Fetch from remote          origin │
│    [P]  push                Push to remote             origin │
│                                                                │
│  ▸ Misc                                                        │
│    [gr] refr[h]             Refresh status buffer              │
│                                                                │
│  q dismiss                                                     │
└────────────────────────────────────────────────────────────────┘
```

Entries marked `▸` are Submenu items — pressing their key opens a nested
transient. The marginalia column shows: number of staged/unstaged files
(`3`/`2`), current branch (`main`), stash count (`2`), remote name
(`origin`), ahead/behind in the title row. All resolved from
`WorkingTree::statuses(repo)` and `Stash::list(repo)` when the transient is
built.

#### Branch menu (from dispatch `b`)

```
┌─ Branch ───────────────────────────────────────────────────────┐
│                                                                │
│  Branch                                          on main 3↑ 2↓ │
│                                                                │
│  ▸ Actions                                                     │
│    [b]  checkout           checkout existing branch    ▸ main   │
│    [c]  create             create a new branch                 │
│    [d]  delete             delete a branch              ⚠      │
│    [m]  merge              merge into current branch    main   │
│    [r]  rename             rename a branch                     │
│    [w]  worktree           create a worktree                   │
│                                                                │
│  ▸ Configure                                                   │
│    [-]  [ ] force          force-create or force-delete        │
│    [-]  [x] track          set upstream tracking        main   │
│                                                                │
│  ────────────────────────────────────────────────────────────  │
│  git checkout -b --track origin/main main                      │
│                                                                │
│  q dismiss   DEL back   -f toggle   -t toggle                  │
└────────────────────────────────────────────────────────────────┘
```

`[-f]` and `[-t]` are Flag items. Pressing `-` followed by `f` toggles the
force flag; the preview line updates from `git checkout main` to
`git checkout -f main`. Marginalia shows: current branch on `[b]` and
`[m]`, warning glyph `⚠` on `[d]` (destructive), upstream tracking branch
on `[-t]`.

#### File-dispatch menu (`C-c f` globally)

```
┌─ File ────────────────────────────────────────────────────────┐
│                                                                │
│  src/auth/login.rs                         modified  +12 -3   │
│                                                                │
│  ▸ Actions                                                     │
│    [s]  stage               stage this file          modified  │
│    [u]  unstage             unstage this file        staged    │
│    [x]  discard             discard changes           ⚠ +12 -3 │
│    [d]  diff                show diff for this file   +12 -3   │
│                                                                │
│  ▸ History                                                     │
│    [l]  log                 show log for this file    23 cmts  │
│    [b]  blame               blame this file           a1b2c3d  │
│                                                                │
│  ▸ Filesystem                                                  │
│    [r]  rename              rename / move this file           │
│    [D]  delete              delete this file            ⚠     │
│    [c]  checkout            checkout from HEAD          clean   │
│                                                                │
│  ────────────────────────────────────────────────────────────  │
│  file: src/auth/login.rs (+12 -3)                              │
│                                                                │
│  q dismiss   s stage   d diff   l log                          │
└────────────────────────────────────────────────────────────────┘
```

Marginalia shows: file status (`modified`/`staged`/`clean`), diffstat
(`+12 -3`), what `[x] discard` would destroy (`⚠ +12 -3`), commit count
for the file (`23 cmts`), last commit SHA (`a1b2c3d`). The `⚠` glyph
uses the existing severity icon system to flag destructive actions.

**Marginalia sources:**

| Context | Data | Source |
|---|---|---|
| File status | `modified` / `staged` / `clean` / `untracked` | `WorkingTree::path_status(repo, path)` |
| Diffstat | `+12 -3` | `git diff --stat <path>` (cached per transient-open) |
| Commit count | `23 cmts` | `git log --oneline <path>` line count (cached per transient-open) |
| Last SHA | `a1b2c3d` | `git log -1 --format=%h <path>` |
| Staged/unstaged counts | `3` / `2` | `WorkingTree::statuses(repo)` → count by `PathStatus` |
| Stash count | `2` | `Stash::list(repo).len()` |
| Ahead/behind | `3↑ 2↓` | `WorkingTree::ahead_behind(repo)` |
| Remote name | `origin` | `Reference::resolve(repo, "HEAD@{upstream}")` → parse remote |
| Warning glyph | `⚠` | Static — severity icon system glyph |

### 8.5 Transient state management

The transient's state (flag values, argument values) is held in a
`RefCell<TransientState>` on the mode's Guard. When a transient opens:

1. Magit's action handler builds a `TransientSpec` from the current
   `TransientState` (reading flag defaults, argument defaults, and building
   the `PreviewFn` closure).
2. Calls `ctx.picker.open_transient(spec, PickerDisplay::BottomPopup)`.
3. The picker renders the transient. On each flag toggle or argument change,
   the picker calls `PreviewFn` to update the preview line.
4. When the user presses an Action key, the picker calls
   `ctx.action_handlers.fire(action_id)` and closes.
5. On dismiss (`q` / `Esc`), the picker closes without action. The
   `TransientState` is preserved in the Guard for the next transient open.

### 8.6 Discovery — `C-c g` / `C-c f`, and `g?` for mode help

From any buffer (not just magit buffers), the global bindings provide direct
access:

- `C-x g` → opens `*magit:status*` (the primary entry point)
- `C-c g` → opens the repo-level dispatch transient
- `C-c f` → opens the file-dispatch transient — **its items are real**
  (stage / diff the active buffer's own file, §8.8); it still cannot
  resolve "the entry at cursor in a magit-status buffer", only "the
  active buffer's own file"

These follow emacs convention; all are unused in default vim normal mode.
All three are registered by `magit-global-mode` (`ActivationPolicy::
Universal`), not `magit-core` — `magit-core` only activates on magit
buffers, which would make these unreachable from anywhere else.

Within magit buffers, the dispatch and file-dispatch transients are accessed
through the same global bindings — there is no buffer-local dispatch key
(single-key candidates like `?` clash with reverse-search, `h` clashes with
left motion).

**`g?` for mode help.** A future Lattice-wide convention: `g?` opens a help
buffer for the current buffer's major mode. In a magit buffer, `g?` would
show the magit keybindings and ex-commands — the discoverability surface for
learning the mode's chords. This is a general mechanism, not magit-specific;
it applies to every major mode.

### 8.7 Sequencing — picker transient mode lands before Magit transients

The picker's transient-mode extension is NOT a magit-specific slice — it's
a **picker enhancement** that Magit consumes. It lands as a prerequisite
slice before MG.8 builds the magit transient definitions.

**PICK.1 deliverables:**
- `TransientSpec`, `TransientGroup`, `TransientItem`, `TransientItemKind`,
  `PreviewFn`, `TransientState` / `TransientValue` types in `lattice-picker`
- Picker rendering: grouped entry layout with section headers, key+label+
  description rendering, flag toggle indicators, preview pane repurposing
- Keyboard: single-key trigger dispatch, flag toggle, argument → minibuffer,
  submenu navigation, `BS`/`DEL` back, `q`/`Esc` dismiss
- `PickerRegistry::open_transient(spec, display)` API
- Tests: single-group transient, multi-group with scroll, flag toggle updates
  preview, nested submenu open + back, argument value entry → return,
  dismiss with q/Esc, TUI + GPUI render parity

MG.8 then builds magit-specific transient definitions (dispatch, branch,
merge, rebase, stash, file-dispatch) on top of this picker mode.

### 8.8 Current implementation — dispatch, discovery, and display

This is the largest gap between this fragment's original design and what
shipped. The mechanism differs in ways that matter beyond naming, so this
section documents the *actual* current wiring rather than annotating every
paragraph above in place.

**`Effect::OpenTransient { source: String }` carries only a name, not a
spec.** `magit-dispatch`/`magit-file-dispatch` (`C-c g`/`C-c f`) return this
new effect instead of aliasing `:magit-status` as they originally did.
`TransientSpec` lives in `lattice-picker`, which sits downstream of
`lattice-grammar` (where `Effect` is defined) — `lattice-grammar` cannot
depend on `lattice-picker`, so the effect can only carry a name. A
`lattice_picker::TransientSourceRegistry` (mirroring `PickerRegistry`'s
named-source shape, simplified — no candidate generator or arg-schema
concept, just a name and a zero-arg `Fn() -> TransientSpec` builder)
resolves the name to a spec at the RENDERER's effect-handling site. Magit's
`install(boot)` registers `"magit-dispatch"` and `"magit-file-dispatch"`
builders via `boot.register_service::<TransientSourceRegistryHandle>(...)`.

**Root dispatch's items fire real actions, but only through a mechanism
built specifically for this.** `TransientItemKind::Action(CommandId)`
dispatch (`do_transient_trigger` in `lattice-host/src/dispatch.rs`)
resolves ONLY via `ActionHandlerRegistry::lookup` — never through the
ex-command path. But every other `action:magit-*` handler in this crate is
registered PER-BUFFER (only live while its owning buffer is open) —
unusable for a menu reachable from any buffer. So `magit_global_mode.rs`
contributes a SEPARATE set of `action:magit-global-*` handlers
(status/commit/amend/log/diff/branch/stash/rebase — each just builds the
same `Effect::OpenSyntheticBuffer` its ex-command equivalent returns —
plus real fetch/pull/push/stash-push handlers, the six file-dispatch
handlers, and the branch-create wizard's finish handler; see below).

These are contributed via **`Mode::action_handlers()`**, NOT registered
from `on_activate`. An earlier version did the latter, gated by a
`static OnceLock` (because `ActivationPolicy::Universal` re-runs
`on_activate` on every buffer activation) — that was fundamentally racy:
`on_activate`'s future runs through `ModeRegistry::spawn_cascade`'s
try-sync-then-spawn path shared with every other mode in the same
activation batch, so if any of those has real async work, this mode's
own synchronous registration defers to a background task with no
guarantee it lands before the next keystroke. Symptom: the transient
opened fine but every item just dismissed it (`ActionHandlerRegistry::
lookup` returned `None`). `Mode::action_handlers()` sidesteps it
entirely — the host walks every mode's contributed list in a plain
synchronous loop at boot, strictly after the command registry is
frozen. See `magit_global_mode.rs`'s module doc for the full history.

The `CommandId`s the dispatch items need are resolved once at
`install()` time (`boot.commands_mut().id_by_name(...)`, since every
`action:magit-*` name is registered earlier in the same `install()`
call) and captured by value into the builder closures
(`transients::DispatchActionIds` / `FileDispatchActionIds`, built by
`resolve_dispatch_ids` / `resolve_file_dispatch_ids`) — the registry's
builders are zero-arg, so this is how a boot-time-resolved id reaches a
spec built long after boot, possibly once per `C-c g` press. Those two
resolver functions are shared with the regression tests in `lib.rs`
precisely so a field added later can't silently escape coverage.

**Root dispatch: 6 groups, 12 leaf items across 2 submenu levels.**
Working tree (`s` status, `d` diff, `c` ▸ commit) / History (`l` log) /
Branches (`b` branch) / Stashing (`z` ▸ stash) / Remotes (`f` fetch,
`F` pull, `P` push) / Misc (`r` rebase). Two sub-transients exist:
`commit_transient` (`c c` commit, `c a` amend) and `stash_transient`
(`z z` push, `z l` list). The richer marginalia layout in §8.4
(diffstat, ahead/behind, warning glyphs, per-item counts) does not
exist, and neither do the branch/merge/rebase sub-transients with
`Flag` toggles — no item in either menu is a `Flag`.

**Key assignments track Emacs magit's own `magit-dispatch` /
`magit-file-dispatch` wherever lattice has the capability** (`s` `c`
`d` `l` `b` `z` `r` `f` `F` `P`; file: `s` `u` `x` `d` `l` `b`) — this
is the UX-convention rule, not heuristic #2: muscle memory is the
dominant cost for a menu this central, so matching magit beats a
locally-reasoned alternative. Magit entries lattice has no
implementation behind (bisect, merge, tag, revert, reset, cherry-pick,
submodule, remote, patch; file-level stage-all/unstage-all, edit-blob,
trace-definition) are **absent, not present-and-inert** — the whole
point of the `Flag`-fallback regression test is that a menu row which
does nothing when pressed is indistinguishable from a bug.

**Remote/stash operations (`f`/`F`/`P`, `z z`) are real, run off the
actor thread, never block on credentials.** `magit_global_mode`'s
`action:magit-global-{fetch,pull,push,stash-create}` handlers each
spawn a detached `tokio::task` running `git fetch` / `git pull
--ff-only` / `git push` / `git stash push` on `spawn_blocking`, all
with `GIT_TERMINAL_PROMPT=0` so a missing/expired credential fails the
git subprocess immediately instead of hanging the task on interactive
input that can never arrive. Pull is deliberately `--ff-only` — it
never opens a merge-commit editor or leaves a conflict mid-merge for a
background task to get stuck in. Each handler returns an optimistic
`Effect::Echo` synchronously; the real outcome (success or the git
error text) lands via `tracing::info!`/`tracing::error!` only — same
documented limitation as every other detached background mutation in
this crate (commit, rebase execute): no synchronous path exists back to
the echo area from a task that outlives the handler call.

**File-dispatch (`C-c f`) has six real items.** All resolve the file
through the shared `active_file(ctx)` helper —
`BufferStore::path_for(id)` on whatever buffer was active when the
transient was opened, then `Repository::discover(path.parent())` (a
directory, per `gix`'s requirement) to get the workdir and a
repo-relative path. `s`/`u` call `Index::stage_path`/`unstage_path` on
`spawn_blocking`; `x` returns `Effect::Confirm` targeting
`action:magit-global-file-discard-execute`, which runs `git checkout
--` — the same two-step destructive-action shape magit-status's own
`x` uses. `d`/`l`/`b` open path-scoped `*magit:diff:<path>*` /
`*magit:log:<path>*` / `*magit:blame:<path>*` buffers (each mode parses
its own optional path out of the buffer name, §4.5/§11).

This resolves ONLY "the active buffer's own file" — it still cannot
resolve "the entry at cursor in a magit-status buffer" (there's no
plumbing carrying a `SectionIndex` cursor into the file-dispatch
transient's build), so invoking `C-c f` from inside `*magit:status*`
itself acts on whatever file `*magit:status*`'s own buffer resolves to
(i.e. nothing — it's synthetic, so `path_for` returns `None` and the
item no-ops), not the entry under the cursor. Use magit-status's own
`s`/`u`/`x`/`d` chords there. A real fix needs the same kind of
context-capture the branch-create wizard uses (stash the target in the
transient or a captured buffer id), not a structural change to
`file_dispatch_transient` itself.

**Three regression tests guard the menus** (`lattice-magit/src/lib.rs`):
every leaf resolves to a real `Action` and not the inert `Flag`
fallback (recursing through both submenus); no two items share a key
within one menu level (a duplicate makes the second unreachable, which
also presents as "this entry does nothing"); and an inverse vacuity
check asserting an all-unresolved spec DOES report every leaf inert —
without it, a walker that silently visited nothing would pass the
first two tests trivially.

**`TransientItemKind::Argument` is still a no-op** — `do_transient_trigger`
explicitly defers it; pressing an Argument item's key does nothing. This
is a *different* mechanism from the one below: `Argument` is meant to be
an in-place, in-transient-menu value edit (type a value, stay in the
transient, item shows the new value). It is unrelated to and NOT solved
by the new prompt mechanism just described — that one is a full-buffer
minibuffer swap (transient closes, prompt buffer opens, transient does
not resume). No item in this crate uses `Argument` today.

**A generic single-line prompt-with-callback mechanism now exists** —
`Effect::OpenPrompt` / `PickerAcceptOutcome::OpenPrompt` /
`PromptLineMode`, see §12.9 and `docs/dev/architecture/picker.md` §4.4
and `docs/dev/architecture/rich-minibuffer.md` §6. It backs the
branch-create wizard's second step; it does not back `Argument` (above).

**Display-preference bug, now fixed.** Transients used to ALWAYS render as
a floating popup regardless of `picker.display` — a real user-reported bug
("picker display should not have anything to do with transient's own
logic, but transient should respect the picker's display preference").
Now: `picker_use_minibuffer`/`picker_is_minibuffer` — computed once,
generically, by the picker's own render dispatch (TUI's `render.rs`,
GPUI's `window.rs`) — governs transient placement exactly like it already
governed regular candidate-list pickers. Transient code only contributes
ROW CONTENT: `transient_rows_gpui` (GPUI) and `transient_group_item_lines`
(TUI) are each a SINGLE shared windowing function used by BOTH the popup
and minibuffer-strip placement variants, so the actual group/item/scroll
computation cannot drift between the two. **The separation of concerns:
the picker owns placement, the transient owns content + interaction** —
worth stating explicitly since it's easy to reintroduce the bug by having
transient code make its own placement decision again.

## 9. Commit buffer design

The commit buffer (`*magit:commit*`/`*magit:amend*`) is a synthetic
Document in `magit-commit-mode`. Target design has two regions with the
diff region read-only and rendered as an inline overlay; **not built as
such** — the real buffer is one flat, fully-editable text buffer (the
mode's options set `NoFile`/`Number` only, no `ReadOnly`) with a fixed
marker line splitting it conceptually in two:

```
┌─────────────────────────────────────────────┐
│ --- Staged diff (review before committing) ---│  ← plain text, not
│ diff --git a/... (git diff --cached output)  │    read-only-enforced,
│ ...                                           │    not an overlay
│ --- Commit message (edit below) ---           │  ← MESSAGE_MARKER
│ Add user authentication endpoint              │  ← subject
│                                                │
│ Implements OAuth2 flow with...                │  ← body
│                                                │
└─────────────────────────────────────────────┘
```

`C-c C-c` confirm / `C-c C-k` abort are the only chords — there is no
`C-c C-d` toggle-diff (it was never implemented).

- **Diff region** (top): populated by `git diff --cached`, synchronously
  awaited during `on_activate` before the buffer is shown to the user —
  no headerline progress indicator exists. Amend (`ca`, buffer name
  `*magit:amend*`) additionally pre-populates the message region from
  `git log -1 --format=%B` (the current HEAD commit's message) instead of
  leaving it blank.
- **Message region** (below `MESSAGE_MARKER`): plain editable text; the
  confirm handler collects every non-blank line after the marker as the
  message.
- `C-c C-c`: if the message is empty, fails loud with an
  `Effect::Echo { level: Error, .. }` instead of silently no-op'ing (a real
  fix — it used to just do nothing). Otherwise runs `Commit::create`/
  `Commit::amend` on `spawn_blocking` in a detached background task and
  closes the buffer OPTIMISTICALLY (`Effect::QuitEditor`) before the git
  write completes — there is no `RepositoryEvent` to publish (§5.2); a
  failure surfaces only via `tracing::error!`, not back to the echo area,
  since there's no synchronous path from a detached task. A known,
  documented limitation, not a silently swallowed error.
- `C-c C-k`: close without committing.

## 10. Log and blame views

### 10.1 Log view

Generated from `git log --oneline --graph --decorate -N`:

```
* a1b2c3d (HEAD -> main, origin/main) Add user authentication
* e4f5g6h Fix login redirect loop
* i7j8k9l Merge branch 'feature/payments'
|\
| * m0n1o2p Implement Stripe checkout
|/
* u6v7w8x Initial commit
```

`<CR>` does NOT resolve from a stored section index — magit-log-mode
builds no `SectionIndex` at all (that structure is magit-status-only).
Instead `extract_sha` re-parses the buffer line at the cursor directly:
the first whitespace-delimited token that's ≥4 hex-digit characters,
wherever the graph-drawing characters happen to end (returns `None` for
graph-only connector lines with no commit). `<CR>` opens
`*magit:commit:<sha>*` (`magit-revision-mode`, §4.7) — a real synthetic
buffer; it used to write the same `git show` content to an uncleaned temp
file in the repo workdir and open that via a plain file buffer, which this
replaced. The buffer text itself carries no styled spans — plain
`git log --oneline --graph --decorate` CLI output, unstyled (no
`--color`, no post-processing into spans).

### 10.2 Blame view

Target design: gutter decorations in a dedicated blame column alongside
the original file, following `DiffSignKind`'s pattern. **Not built this
way.** The real `magit-blame-mode` is a separate whole-buffer view
(`*magit:blame:<path>*`, path extracted from the buffer name) that
REPLACES its own content with `git blame --line-porcelain <path>`
reformatted as one line per source line: `<sha-8> <author right-padded to
12>  <source line text>` — the blame annotation is inlined into the text
itself, not a gutter beside a separately-open file. `<CR>` on a line opens
`*magit:commit:<sha>*` for that line's blamed commit. `p` re-blames at the
PARENT of whatever revision is currently shown (`BlameState::rev`,
starting at `"HEAD"`, walked back via `git rev-parse <rev>^` — resolution
failure, e.g. at the root commit, logs via `tracing::debug!` and leaves
the buffer showing the current revision rather than erroring) — not
always "the commit at the cursor line" and not always "the parent of
HEAD"; repeated `p` presses keep walking further back. No per-revision
cache exists; each `p` press re-runs `git blame` on `spawn_blocking`.

## 11. Integration with the diff subsystem

Target design: three integration points consuming existing diff
primitives (`DiffSession`, `GitBaseline`, virtual-row deletion blocks,
`do`/`dp` hunk transfer operators). **None of the three currently
integrate with the diff subsystem at all** — no magit buffer registers a
`DiffSession`, and `diff-mode` is never active on any magit buffer.
What each view actually does:

1. **Inline diffs in magit-status** — loaded lazily via `=`/`<CR>` on
   demand (§6.3). Plain `git diff [--cached] -- <path>` / `git stash show
   -p` / `git show <sha>` output, inserted as text with line-level
   syntax-highlight spans (add/remove/hunk-header/diff-header) — not
   virtual-row deletion blocks, not sourced from a `DiffSession`.

2. **magit-diff** (§4.5) — `git diff HEAD` (staged + unstaged combined),
   one buffer, no panes, no `GitBaseline`. `]c`/`[c` here are exactly
   `magit-core`'s generic buffer-text scan for `@@`/`diff --git` lines
   (§7.5) — the same mechanism every other magit buffer uses, not a
   diff-system-registered motion. `do`/`dp` hunk-transfer operators do not
   apply — there is no diff session for them to operate against.

3. **Commit buffer diff preview** — loaded on open via `git diff --cached`
   as plain text (§9), not an inline overlay.

**`magit-diff-mode` keymap** adds `s`/`u` (stage/unstage file, not hunk —
§4.5) on top of `magit-core`'s shared chords. There is no `diff-mode`
active on this buffer to add `]c`/`[c`/`do`/`dp` "on top of" — those come
from `magit-core` alone, same as everywhere else.

## 12. Keybindings — complete grammar surface

### 12.0 Global bindings — emacs convention

Magit registers three global keybindings during `install(boot)`, following
emacs convention. `C-x g` is unused in default vim normal mode. `C-c g` and
`C-c f` use `C-c` as a multi-key chord prefix — a Lattice-specific departure
from vim (where `C-c` is a single-key interrupt terminator in normal mode).
In Lattice, `C-c` is a first-class chord prefix usable by any mode; magit
is the first heavy consumer of this convention.

| Chord | Action | Command |
|---|---|---|
| `C-x g` | Open magit-status for the current repo | `:magit-status` |
| `C-c g` | Open magit-dispatch (repo-level transient popup) | `magit-dispatch` |
| `C-c f` | Open magit-file-dispatch (file-level transient popup) | `magit-file-dispatch` |

`C-x g` opens the status buffer — the primary entry point. `C-c g` opens
the repo-level dispatch popup directly from any buffer, bypassing the status
buffer. `C-c f` opens the file-dispatch popup for the current buffer's file.

The user can remap any of these in `init.rs`:

```rust
keymap::global_bind(boot, "C-x g", "magit-status")?;       // the default
keymap::global_bind(boot, "C-c g", "magit-dispatch")?;     // the default
keymap::global_bind(boot, "C-c f", "magit-file-dispatch")?; // the default
```

### 12.1 `magit-core` (shared minor mode)

Every magit buffer is a read-only synthetic Document. In this context, many
native vim operators (`d`, `c`, `s`, `x`, `p`, `y`, `q` for macro recording)
have no meaningful effect. Bindings that would clash with fundamental
**navigation** keys (`h`, `j`, `k`, `l`, `w`, `b`, `e`) and **fold/scroll**
keys (`z`, which is vim's fold and scroll prefix and would be the worst clash
since magit also uses folds) are never overridden.

| Chord | Action | Command | Vim conflict resolved |
|---|---|---|---|
| `gr` | Refresh current magit buffer | `magit-refresh` | `gr` is unused in default vim; consistent with `gr` in `compilation-mode` |
| `q` | Close magit buffer (bury — return to previous, buffer stays open as cache) | `magit-close` | `q` starts macro recording, but macro recording is meaningless in a read-only synthetic buffer. Fugitive/magit convention. |
| `]]` | Next top-level section | `magit-next-section` | Standard vim-unimpaired |
| `[[` | Previous top-level section | `magit-prev-section` | Standard vim-unimpaired |
| `]f` | Next file/entry within current section | `magit-next-file` | `]f` in vim is "next file in directory" — file-adjacent, consistent |
| `[f` | Previous file/entry within current section | `magit-prev-file` | |
| `]c` | Next hunk | `magit-next-hunk` | Shared with diff system; shadows in magit buffers where `diff-mode` is not active (§7.5) |
| `[c` | Previous hunk | `magit-prev-hunk` | |
| `TAB` | Toggle section/hunk fold at cursor | `magit-toggle-fold` | Not a normal-mode key |
| `S-TAB` | Cycle section visibility (global) | `magit-cycle-sections` | Not a normal-mode key |

**Notes on removed bindings:**
- `g` → removed (clashes with vim prefix: `gg`, `gt`, `gf`, `gd`, etc.).
  Replaced by two-key `gr` — matching compilation-mode's `gr` for recompile.
- `h` → removed (clashes with left motion — fundamental navigation key).
  The global binding `C-c g` (§12.0) opens the dispatch transient from
  any buffer — no buffer-local dispatch key needed in magit buffers.
- `q` semantics changed: was `magit-kill-buffer`, now `magit-close`. The
  buffer is buried, not deleted — it stays in the buffer list and retains
  its content as a cache. `:bd` still kills it explicitly.

### 12.2 `magit-status-mode`

The status buffer is read-only everywhere except inside the commit-message
region (which is a separate `magit-commit-mode` buffer). All single-letter
operator overrides below are acceptable because the operators (`s`, `u`, `X`,
`c`, `p`) have no write effect on a read-only buffer. Navigation keys are
preserved.

| Chord | Action | Command | Vim conflict resolved |
|---|---|---|---|
| `s` | Stage file at cursor (file-level, not hunk — §7.3) | `magit-stage` | `s` is vim's substitute operator. Fugitive convention; no-op on read-only buffer. |
| `u` | Unstage file at cursor (file-level, not hunk) | `magit-unstage` | `u` is vim's undo. Fugitive convention; no-op on read-only buffer. |
| `x` | Discard file at cursor (file-level, not hunk) | `magit-discard` | `x` is vim's delete character. No-op on read-only. `x` is easier to type than `X` (no shift); the read-only buffer makes the override safe. |
| `=` | Toggle inline diff at cursor (file entries only) | `magit-toggle-diff` | `=` is vim's format/indent operator. Fugitive convention. |
| `d` | Open the file at cursor's diff in a dedicated `magit-diff-mode` buffer, scoped to the section's baseline (§6.3) | `magit-diff-file` | `d` is vim's delete-operator prefix (`dd`, `dw`, ...) but has no meaning standalone; no-op on read-only, so safe to override. |
| `cc` | Commit — open `*magit:commit*` | `magit-commit` | `cc` = change-line (suppressed in read-only). Fugitive convention. |
| `ca` | Amend previous commit | `magit-commit-amend` | `ca` = change-around (suppressed in read-only). |
| `p` | Attempt `git add -p` — shows an error explaining it's unsupported (genuinely interactive over stdin; running it would hang the actor waiting on a child also waiting on stdin) | `magit-stage-patch` | `p` is vim's paste. No-op on read-only. Fugitive convention. |
| `<CR>` | Context-aware open/visit at cursor | `magit-visit` | See §12.3. |

**Operations reachable through the `C-c g` dispatch transient or
ex-commands** (removed from direct keybindings — the first key of each
two-key chord clashes with a fundamental vim navigation or operator
prefix). The transient column below is aspirational for most rows — only
a flat, one-item-per-group root dispatch exists today (§8.8); there are no
Branch/Merge/Stash/Rebase SUBMENUS yet, only the direct ex-commands:

| Operation | Removed binding | Clash | Access via |
|---|---|---|---|
| Open log | `ll`/`lo` | `l` = right motion | `:magit-log` |
| Open detail diff | `dd`/`dr` | `d` = delete operator prefix | `:magit-diff` |
| Branch checkout/delete/merge | `bb`/`bd`/`bm` | `b` = back-word motion | `:magit-branch` |
| Branch create | `bc` | `b` = back-word motion | `:magit-branch-create <name>` (from HEAD), or `c` inside `*magit:branch*` for the pick-base-then-name wizard (§12.9) |
| Stash operations | `zz`/`za`/`zp`/`zd` | `z` = fold/scroll prefix — worst clash since magit also uses folds | `:magit-stash-list` |
| Fetch / Push | `F` / `P` | `F{char}` = find-backwards, `P` = paste-before | No standalone ex-command — only via the root dispatch's real `F`/`P` items (`git pull --ff-only` / `git push`, §8.8) |
| Rebase | `rr`/`rc`/`ra` | `r{char}` = replace char | `:magit-rebase [upstream]` |

The `C-c g` dispatch transient is the **discoverability surface** in
target design; today it shows one item per group (status/commit/log/
branch/stash/rebase/pull/push — the last two now real, §8.8) rather
than every operation above — see §8.8 for exactly what it renders.

### 12.3 `<CR>` — context-aware open/visit

`<CR>` is a general "visit/drill-into" action, not file-dispatch. Its
behavior depends on what's under the cursor and which mode's buffer it's
pressed in — there is no single generic dispatcher; each major mode
registers its own `<CR>` handler:

| Mode | Cursor on | `<CR>` action |
|---|---|---|
| magit-status | File entry | Open the file for editing (working-tree version) |
| magit-status | Stash / Commit entry | Toggle its patch inline (same mechanism as `=` on a file — §6.3), NOT a separate buffer |
| magit-log | Commit line | Open `*magit:commit:<sha>*` (magit-revision-mode) |
| magit-blame | Blame line | Open `*magit:commit:<sha>*` for the line's commit |
| magit-branch | Branch name | Check out that branch |

"Hunk (staged/unstaged diff) → open the file at the hunk location" is
target design, **not built** — there is no hunk-level resolution anywhere
(§7.3).

`magit-status`'s `<CR>` is `action:magit-visit`, registered PER-BUFFER by
`magit_status_mode`'s `on_activate` (via `actions::register_action_handlers`)
— NOT a generic handler in `magit-core`, and NOT dispatched from a
`SectionIndex` entry kind. It calls the same `classify_line` cursor
classification §7.2 describes, then branches on `StatusLine::File` (open)
vs. `Stash`/`Commit` (toggle inline). Every other mode listed above
registers its own separate `<CR>` handler under its own `action:magit-*`
command name — "shadowing" is really "each major mode owns its own
binding," not one action dispatching differently per mode.

### 12.4 `magit-commit-mode`

The commit buffer is partially editable (the message region). `C-c` chords
are the emacs/magit convention for commit operations; `C-c` is not a normal-
mode prefix in vim (it's `C-c` in insert mode for escape-like behavior),
so there is no clash in the editable commit-message region.

| Chord | Action | Command |
|---|---|---|
| `C-c C-c` | Commit with message | `magit-commit-confirm` |
| `C-c C-k` | Abort commit | `magit-commit-abort` |

There is no `C-c C-d` toggle-diff-preview chord — never implemented (§9).

### 12.5 `magit-log-mode`

| Chord | Action | Command |
|---|---|---|
| `<CR>` | Show commit detail for the commit at cursor | `magit-log-show-commit` |

Log argument toggling (count, `--all`, `--graph`, path filter) is accessed
through the `C-c g` dispatch transient's Log submenu or via `:magit-log`
with arguments.

### 12.6 `magit-blame-mode`

| Chord | Action | Command |
|---|---|---|
| `<CR>` | Show commit for the blamed line | `magit-blame-show-commit` |
| `p` | Re-blame at the parent of the revision currently shown | `magit-blame-parent` |

`q`/`gr`/nav come from `magit-core` (not a blame-mode-specific `q`). Both
`<CR>` and `p` used to be dead — keymapped but never registered at all
(always fell through to the registry's dead-marker `Effect::None`); both
are real now (§10.2). Blame chunk navigation via `]c`/`[c` inherits
`magit-core`'s generic scan (§7.5) — since blame output contains no `@@`/
`diff --git` lines, `]c`/`[c` are effectively inert in a blame buffer
today, not "hunk motions" in the diff-system sense.

### 12.7 `magit-diff-mode`

There is no `diff-mode` active on this buffer to inherit chords from (§11)
— `]c`/`[c` come from `magit-core`'s generic scan, same as every other
magit buffer. `magit-diff-mode` itself adds only:

| Chord | Action | Command |
|---|---|---|
| `s` | Stage file at cursor (file-level, not hunk — §4.5) | `magit-stage` |
| `u` | Unstage file at cursor (file-level, not hunk) | `magit-unstage` |

No `x`/discard chord exists in `magit-diff-mode`.

### 12.8 `magit-stash-mode`

| Chord | Action | Command |
|---|---|---|
| `a` | Apply stash at cursor (keep in list) | `magit-stash-apply` |
| `p` | Pop stash at cursor (apply + drop) | `magit-stash-pop` |
| `d` | Drop stash at cursor (asks first — §12.13) | `magit-stash-drop` |
| `z` | Create new stash (`git stash`) | `magit-stash-create` |

There is no `<CR>` chord in `magit-stash-mode` — stash-detail-on-`<CR>` was
never implemented (nor is there an `action:magit-stash-show` command).

### 12.9 `magit-branch-mode`

| Chord | Action | Command |
|---|---|---|
| `<CR>` | Check out branch at cursor | `magit-branch-checkout` |
| `c` | Open the branch-create wizard (pick base, then type name) | `magit-branch-create` |
| `d` | Delete branch at cursor (force `-D`, asks first — §12.13) | `magit-branch-delete` |
| `m` | Merge branch at cursor into current | `magit-branch-merge` |

**`c` now opens a real two-step wizard, modeled on Emacs magit's own
branch-create flow, instead of pointing at an ex-command.** This closes
the gap this section used to document ("no generic single-line
interactive text-prompt-with-callback mechanism anywhere") — that
mechanism now exists (`Effect::OpenPrompt` / `PromptLineMode`, a new
host-level rich-minibuffer primitive documented in
`docs/dev/architecture/rich-minibuffer.md` §6 and
`docs/dev/architecture/picker.md` §4.4/§4bis.7) and the branch-create
wizard is its first real consumer:

1. `c`'s handler in `magit_branch_mode.rs` returns
   `Effect::OpenPicker { source: "magit-branch-pick-base" }` — no
   prompt yet, just a normal picker.
2. `BranchPickBaseSource` (`crates/lattice-magit/src/picker_sources.rs`,
   registered as `:picker magit-branch-pick-base`) lists `Branch::list`
   as candidates, routed through `RoutingPayload::BranchBase { name }`
   (picker.md §6.1 — branch names get MRU identity like theme names).
3. Accepting a candidate returns `PickerAcceptOutcome::OpenPrompt` — the
   picker-accept peer of `Effect::OpenPrompt`, same fields — asking
   "New branch name (from `<base>`):" and stashing `base` in the
   follow-up prompt buffer's synthetic name
   (`*magit:branch-create-from:<base>*`), exactly like magit's blame/
   rebase/revision modes already encode their target in a buffer name.
4. `<CR>` on the prompt fires `action:magit-branch-create-finish` — a
   GLOBAL handler in `magit_global_mode.rs` (not `magit_branch_mode`,
   since the prompt can outlive the branch buffer) — which reads the
   typed name via `ActionContext::prompt_value`, recovers `base` by
   parsing the prompt buffer's own name back out, and runs
   `Branch::create(repo, name, true, Some(&base))` on `spawn_blocking`
   in a detached task (result surfaces via `tracing`, not synchronously
   — same limitation as every other detached mutation in this crate).

The direct ex-command, `:magit-branch-create <name>` (creates from HEAD,
no base choice — `Branch::create(repo, name, true, None)`), is still
registered unchanged for the scriptable/quick path; the wizard is the
interactive path when a non-HEAD base is wanted.

### 12.10 `magit-rebase-mode`

| Chord | Action | Command |
|---|---|---|
| `C-c C-c` | Execute rebase | `magit-rebase-confirm` |
| `C-c C-k` | Abort rebase (only if a rebase is actually in progress — and then it asks first, §12.13) | `magit-rebase-abort` |

The buffer is a REAL editable todo list (`pick`/`reword`/`squash`/`fixup`/
`drop`), built from `git log --reverse --format="pick %h %s"
<upstream>..HEAD` — not a hardcoded placeholder. `C-c C-c` collects the
buffer's non-comment lines and actually starts the rebase (§4.6); `C-c
C-k` checks `.git/rebase-merge`/`.git/rebase-apply` before running
`--abort`, so it cannot fail against a rebase that was never started.
That same check decides whether it asks: nothing in progress means
`C-c C-k` is only closing a buffer nobody ran, so it closes outright
(§12.13).

### 12.11 Ex-commands (dashed + namespaced)

| Command | Action |
|---|---|
| `:magit-status` | Open magit-status for the current repo |
| `:magit-log` | Open magit-log buffer (no ref/count argument yet — always `-50`) |
| `:magit-blame [path]` | Open blame for current file or path |
| `:magit-commit` | Open commit buffer |
| `:magit-diff` | Open `*magit:diff*` (`git diff HEAD`) — no ref argument yet |
| `:magit-stash-list` | Open stash list buffer |
| `:magit-branch` | Open branch list buffer |
| `:magit-branch-create <name>` | Create a branch from HEAD and check it out (no base choice — the interactive `c` wizard in `*magit:branch*` lets you pick a base, §12.9) |
| `:magit-rebase [upstream]` | Start interactive rebase; no arg resolves `@{upstream}` |
| `:magit-dispatch` | Open the repo-level dispatch transient (`Effect::OpenTransient`) |
| `:magit-file-dispatch` | Open the file-dispatch transient — its items are real (stage/diff the active buffer's file, §8.8) |

`:magit-fetch`, `:magit-push`, and `:magit-merge` are still **not
registered as standalone ex-commands** — pull/push are real operations
now (`git pull --ff-only` / `git push`, §8.8) but reachable only through
the root dispatch transient's `F`/`P` items (`action:magit-global-pull`/
`-push`), not through a `:` command. Branch merge exists only as the
branch-list buffer's `m` chord (`git merge <branch>`), not as a
standalone ex-command either.

### 12.12 Registrations

All commands register through `CommandRegistry` on crate install:

```rust
pub fn install(boot: &mut SubsystemBoot) {
    let magit_core = MagitCoreMode::new();
    let magit_status = MagitStatusMode::new();

    boot.modes_mut().register_minor(magit_core);
    boot.modes_mut().register_major(magit_status);
    // ... register all modes

    boot.commands_mut().register(CommandSpec::ex("magit-status", /* ... */));
    boot.commands_mut().register(CommandSpec::ex("magit-log", /* ... */));
    // ... register all ex-commands

    // Keymaps pushed through Mode::keymap()
    // Action handlers registered through Mode::action_handlers()
    // Guard Drop unregisters on deactivation
}
```

### 12.12b Action-handler registration — boot, not activation

**The defect.** A mode's `on_activate` runs inside the cascade future
`ModeRegistry::spawn_cascade` spawns. Registering action handlers there
— which every per-buffer magit mode did, because the handler closed
over an `Arc<Mutex<…State>>` built during activation — leaves a window
after the buffer opens where the chord resolves through the keymap, the
mode reads as active, and **no handler exists**. The keypress does
nothing. A user hitting `d` quickly after `:magit-branch` sees it; so
did MG.8's dead transients, whose fix was the same move.

**The contract.** Handlers live in `Mode::action_handlers()`, resolved
once at boot (`mode_action_handlers::register_mode_action_handlers`,
after every subsystem's `install()`). Per-buffer state is published into
a `BufferStates<S>` service keyed by `BufferId` and read from
`ActionContext` at call time.

Two rules make this correct:

1. **Publish above the first `.await`.** `spawn_cascade` polls the
   cascade future once, synchronously, on the App thread before spawning
   it. Everything before the first pending await has therefore run by
   the time `activate_major` returns. Moving an `.await` above the
   `publish` call silently reopens the window — and silently is the
   operative word: nothing fails, the chord just does nothing for a
   while. `magit-status-mode` was written that way at first and left
   `x` / `=` / `<CR>` dead for the width of a `git status`, the longest
   window of any view, in the most-used one. When touching an
   `on_activate`, check the ordering; the mechanical version is "first
   non-comment `.await` line number must exceed the `publish` line
   number", modulo early-return branches that never publish (the
   not-a-git-repo path). State that genuinely
   cannot be known until after an await (magit-commit's `diff_end_line`,
   magit-rebase's resolved `upstream`) publishes an inert initial value
   and is filled in through the `Arc<Mutex<_>>` afterwards — the
   handlers already refuse to act on the inert value. Only the cascade's
   *root* step (the major) gets this guarantee; implied minors run
   later, so `magit-core-mode` keeps a state-free handler shape.
2. **One boot registration per action id.**
   `ActionHandlerRegistry::register` inserts — last writer wins — and
   dropping a registration unregisters *by action id*. So an action
   bound by more than one mode must be registered exactly once, and must
   not be registered per-activation by anyone. `action:magit-refresh`
   (`gr`) is the case in point: five modes bound it, which only worked
   because per-activation registration installed one at a time. It is
   now owned by `magit-core-mode` (which binds the chord) and dispatched
   per buffer through the `MagitView` trait, so each view still owns its
   own refresh body and nothing branches on buffer kind. A test fails if
   any two modes contribute the same boot `action_name`.

Rule 2 is not merely a constraint of the new shape — **per-activation
registration was never safe once two buffers can hold handlers at
once**, because the registry is keyed by `CommandId` alone, with no
buffer dimension. Three real instances, all fixed by the migration:

- **Two modes, one action.** `action:magit-stage` / `action:magit-unstage`
  were registered from `on_activate` by both `magit-status-mode` and
  `magit-diff-mode`.
- **One mode, two buffers.** `magit-core-mode` is a minor that activates
  on *every* magit buffer and registered `q` and the six navigation
  chords per activation. Two magit buffers ⇒ two registrations.
- **Two modes, one action, different bodies.** `action:magit-close` was
  registered by `magit-status-mode` (`Effect::BufferDelete`) *and*
  `magit-core-mode` (`Effect::DismissPopup`), so `q` in the status
  buffer did one or the other depending on cascade ordering. Note this
  was not an open design question: `DismissPopup` is the documented and
  **already tested** intent —
  `q_on_magit_status_buries_it_and_never_quits_the_editor` asserts that
  `q` restores the buffer active before magit-status opened, and core's
  handler records the live bug (`q` quitting the editor) it fixed. The
  status-side registration contradicted that test and silently voided
  the guarantee whenever it won the race. Removing it makes the tested
  behaviour deterministic rather than changing it.

In every case the second registration won and the first deactivation
unregistered the action for **both**. It only looked safe because one
magit buffer tended to be open at a time. The old `ActionRegsGuard` doc
comment already named the hazard; holding the tokens bounded the damage
but could not prevent it. Boot registration removes it at the source —
**no `handlers.register(...)` call remains anywhere in `lattice-magit`**,
which is the cheapest regression check available.

Contract and helpers: `crates/lattice-magit/src/buffer_state.rs`.
Sequencing and per-mode migration status: the MG.13 entry in the slice
plan.

### 12.13 Destructive actions — the ask/execute contract

magit mutates a repository from a lot of single keystrokes. Most of
those are reversible and asking about them would be noise; a few throw
away work that git itself cannot hand back. Those go through one shape,
and only that shape:

- The chord's own action (`action:magit-<x>`) performs **no git call at
  all**. It reads the target off the buffer and returns
  `Effect::Confirm { prompt, yes_action }`.
- The git call lives in a separate `action:magit-<x>-execute`, named as
  that confirm's `yes_action`.

Answering `n` therefore cannot mutate anything — not by a guard that
could be forgotten, but because the code that mutates was never
reached. The execute half re-reads its target at the cursor rather than
carrying it through the prompt: the confirm transient owns every
keystroke while it is open, and `do_transient_trigger` hands the
yes-action the *document* cursor, so the cursor is provably where the
prompt was built from.

The prompt always names its target (`Delete branch feature/foo?`,
`Drop stash@{2}?`) — the transient covers the buffer the target was
read from, so a question that says only "Delete branch?" is
unanswerable without dismissing it.

The set (`crates/lattice-magit/src/confirm.rs`,
`DESTRUCTIVE_ACTIONS`):

| Chord | Ask | Execute | Why it is irreversible |
|---|---|---|---|
| `x` in magit-status | `magit-discard` | `magit-discard-execute` | `git checkout --` overwrites the worktree copy |
| `x` in `C-c f` | `magit-global-file-discard` | `…-execute` | same, on the active buffer's file |
| `d` in magit-branch | `magit-branch-delete` | `…-execute` | `Branch::delete` is a force delete (`-D`) — drops unmerged commits |
| `d` in magit-stash | `magit-stash-drop` | `…-execute` | a dropped stash is gone (`apply`/`pop` leave their content visible; only `drop` doesn't) |
| `C-c C-k` in magit-rebase | `magit-rebase-abort` | `…-execute` | `--abort` discards everything the rebase replayed — but **only asks when a rebase is in progress**; otherwise `C-c C-k` is just closing a todo buffer nobody ran, and closes outright |

That last gate is the one place the shape is conditional, and it is
conditional on the same `rebase-merge`/`rebase-apply` check that decides
whether `--abort` runs at all. It is a `stat` against a gitdir resolved
once at activation — cheap enough for the actor thread in response to an
explicit chord, and it *has* to run there because the confirm is the
effect the handler returns.

`DESTRUCTIVE_ACTIONS` is the single list, asserted two ways: a test
proves both halves of every row resolve in the command registry (an
unregistered execute half turns the whole action into
`confirm: unknown action`, a failure that would otherwise only surface
when a user presses the key), and `confirm::ask` debug-asserts its
`yes_action` appears in the table, so a new destructive action that
skips the list fails for its author rather than quietly for the user.

**What deliberately does *not* ask:** stage / unstage (index-only),
checkout, merge, branch-create, stash apply / pop / create, commit and
amend, rebase execute. Each is either reversible or is itself the
explicit confirm step of a dedicated buffer. This matches Emacs magit's
own `magit-no-confirm` default set; the standing rule is UX convention
over local rationale for surfaces carrying muscle memory.

## 13. Performance posture

The design follows a "lazy by default" strategy (§6). Every operation is
deferred until explicitly invoked by the user. The status buffer opens as a
fast file list — no pre-computed diffs, no full-repo git operations beyond
`git status --porcelain`.

- **Status buffer open (initial):** `git status --porcelain`-equivalent (via
  gitoxide) + `git stash list` + `git log --oneline -20`. For a repo with
  500 tracked files, completes in **10-50ms** on `spawn_blocking`. The
  buffer is a file list with status labels — no diff content. There is no
  headerline progress indicator (§4.1) — the buffer simply appears once the
  initial refresh completes.

- **Status buffer auto-refresh (after commit/stage/unstage):** same fast
  path. `StatusBufferState::expanded` is cleared UNCONDITIONALLY on every
  refresh (§6.2/§6.4) — there is no selective per-file invalidation by
  status change, since there is no per-file diff cache to invalidate. Any
  previously-expanded entry collapses; the user re-expands with `=`/`<CR>`
  if still wanted. Buffer update is a full replace via `apply_edit_batch`.

- **Diff/patch loading (`=` on a file, `<CR>` on a stash/commit):** a single
  `git diff`/`git stash show -p`/`git show` invocation on `spawn_blocking`.
  Content inserted as a local edit with syntax-highlight spans — NOT
  virtual-row deletion blocks (§6.3); no diff-system integration here at
  all. Section-level "`=` expands every file in the section" is target
  design, not built — only single-entry toggle exists today.

- **Per-keystroke (in magit buffer):** chord dispatch → mode filter → action
  handler. Identical overhead to any other buffer (<500ns p99). No WASM
  boundary.

- **Fold recompute (`MagitStatusFoldSource`):** recomputes live from
  `expanded` + a buffer scan on every `compute_folds()` call (§7.1) rather
  than caching stale ranges — O(buffer lines), not O(expanded-entries²);
  bounded by how much of the buffer is currently expanded, not repo size.

- **Blame:** each `<CR>`-triggered re-blame (or the initial load) is a full
  `git blame --line-porcelain` invocation on `spawn_blocking`, replacing the
  whole buffer. No gutter column, no per-file/per-revision cache (§10.2) —
  every `p` press re-runs blame from scratch against the new parent.

- **Comparison with Emacs magit:** Emacs magit runs `git diff --cached` and
  `git diff` on every status refresh — O(all-changed-lines). In large repos
  with 50+ changed files and large diffs (refactors touching hundreds of
  lines), this can take 2-10 seconds. Lattice's lazy approach never runs a
  diff command during status refresh at all — diffs load only on explicit
  `=`/`<CR>`, one file/stash/commit at a time.

- **No UI-thread work.** Zero I/O, parsing, git operations, or formatting on
  the render thread. Renderer sees ordinary Documents. `match buffer_kind` is
  untouched — magit buffers are ordinary Documents with major modes, folds, and
  decorations. The kind-agnostic-buffer invariant holds.

- **Async architecture — every mutation off the actor thread.** Every
  mutating git operation across the whole crate (stage/unstage/discard/
  checkout/delete/merge/stash-ops/rebase/commit/branch-create/pull/push/
  file-stage) runs via `tokio::task::spawn_blocking`, never synchronously
  on the actor thread. Three shapes, all avoiding a synchronous git call
  inline in an action handler:
  - **Refresh-in-place** (`spawn_mutation_and_refresh` in `actions.rs`,
    `magit_branch_mode.rs`, `magit_stash_mode.rs`): the handler returns
    `None` immediately; a spawned task runs the mutation, then re-runs the
    view's own refresh and applies it. Used by everything that only needs
    to update its own buffer's content (stage/unstage/discard, checkout/
    delete/merge branch, apply/pop/drop/create stash).
  - **Optimistic close** (commit-confirm, rebase-confirm/-abort): the
    handler returns the close effect (`Effect::QuitEditor`) SYNCHRONOUSLY,
    then runs the git write in a fire-and-forget background task, logging
    failure via `tracing::error!`. There is no synchronous path back to the
    echo area from a detached task — a real, documented limitation (§9,
    §4.6), not a silently swallowed error.
  - **Optimistic echo, no buffer to refresh** (pull/push in
    `magit_global_mode`'s `remote_op!` macro; file-stage; the
    branch-create wizard's finish handler): there is no per-buffer state
    to refresh — these fire from the global dispatch/prompt, not a magit
    buffer's own action. The handler returns `Effect::Echo` ("pulling…" /
    "staged `<path>`" / "creating branch…") synchronously, then a
    detached task runs the git write and logs success/failure via
    `tracing::info!`/`tracing::error!` only. Same "no synchronous path
    back" limitation as optimistic-close, just with no buffer to close
    either.

## 14. WIT surface gaps and WASM migration path

The `vcs-and-magit.md` committed to Magit as a WASM component plugin. The
current WIT surface has gaps that make this infeasible without substantial
host-services extensions:

| Magit need | WIT status | Gap |
|---|---|---|
| Read file/buffer content | ✅ `buffer.wit` (read-only `document` resource) | None |
| Write to buffer | ❌ | Need `document-write` or `apply-edit` effect mirror |
| Create buffers | ❌ | Need `ensure-named-document` equivalent in WIT |
| Spawn git process | ❌ | `spawn-process` designed for lighthouse, not built |
| Subscribe to events | ✅ `events.wit` (observation-only, async) | None |
| Call VCS data API | ❌ | Need WIT `vcs` interface mirroring Layer 1 API |
| Call diff subsystem | ❌ | Need WIT `diff` interface for session creation |
| HTTP fetch (push/fetch) | ❌ | `http-fetch` designed for lighthouse, not built |
| Persistent plugin state | ❌ | Need `kv` host-service |
| Fold registration | ❌ | Need WIT `folds` interface |
| Decoration production | ✅ `decorations.wit` (gutter decos, async) | Partial |

**Conclusion:** Magit v1 ships as a native Rust crate through `SubsystemBoot`.
The WIT surfaces it needs (buffer-write, spawn-process, VCS binding, diff
binding) represent 4–6 host-services extensions that are collectively larger
than the magit plugin itself. Building them as a prerequisite would add 3–4
slices of host-runtime work before a single magit view renders.

**WASM migration path (post-v1):** Once lighthouse's host-services ship and the
buffer-write + persistent-storage WIT interfaces land, the mode architecture
supports a clean migration — the `Mode` trait, `ActionHandlerRegistry`, and
`ModeActivator` patterns have identical semantics for native and WASM
implementations. The native crate is not temporary: the WASM boundary adds
per-operation overhead on every git process line and buffer mutation that may
never be competitive with native `spawn_blocking`. The WASM path exists for
third-party VCS backends (jujutsu, sapling) that want to plug into the same
seam.

## 15. Deliverable path: native crate vs WASM plugin

> **UX (higher court):** identical — the user sees magit-status, magit-log,
> magit-commit, etc. WIT vs native is invisible. UX is neutral.

**Option A: Native Rust crate (`lattice-magit`) through `SubsystemBoot` seam.**

> **Paramount goals:** protects #1 (no WASM boundary for git operations);
> protects #3 (modes own full surface — acid test holds); protects #4 (git ops
> on `spawn_blocking`, event-bus-driven).
> **#2 Extensibility:** the `SubsystemBoot` seam IS the extension model. Magit
> validates it with a production-grade feature-buffer. A third-party VCS tool
> installs through the same seam.
> **Heuristic #1 (long-term fit, on merit):** a self-contained subsystem with
> its own modes, git process integration, and performance-sensitive buffer
> generation is genuinely better as a native crate — the same reasoning that
> kept diff, terminal, and compilation as native subsystems. The mode
> architecture is identical either way.
> **Heuristic #2 (paramount, not other editors):** not "VSCode has a git
> extension." The justification is: 5+ WIT gaps (§14); `SubsystemBoot` is the
> proven extension mechanism for core feature-buffers; building the WIT
> host-services first adds 3–4 prerequisite slices that don't render a single
> magit view.
> **Standing-rule check:** ✅ every chord and action-handler body lives in
> `lattice-magit`. No `Editor::do_magit_*` shims. No new `Action` enum variants.

**Option B: WASM component plugin through WIT contract.**

> **Paramount goals:** protects #2 (strongest WIT surface validation —
> feature-buffer through WIT seam is the ultimate extensibility proof);
> sacrifices #1 (every git process line crosses WASM boundary); sacrifices
> delivery velocity (4–6 WIT host-services must ship first).
> **Heuristic #1:** building the host-services now is better long-term fit for
> the ecosystem. But the cost is 3–4 prerequisite slices — those could land in
> parallel with a native Magit proving the UX.

**Recommendation: Option A for v1, with Option C (parallel-track WASM) as the
sequencing strategy.** Magit ships v1 as a native `lattice-magit` crate. The
WIT host-services needed for WASM migration are documented in §14 and land
through lighthouse + ecosystem needs. When the WIT surface is complete, Magit
can be migrated — the mode surface is identical.

## 16. Options

Registered through the typed-options system (§5.12), owned by `lattice-magit`:

| Option | Type | Default | Description |
|---|---|---|---|
| `magit.auto-refresh` | `bool` | `true` | Auto-refresh on `RepositoryEvent` |
| `magit.refresh-debounce-ms` | `u32` | `100` | Debounce window for auto-refresh |
| `magit.status.show-untracked` | `bool` | `true` | Show untracked files section |
| `magit.status.show-stashes` | `bool` | `true` | Show stash list section |
| `magit.status.recent-commits-count` | `u32` | `20` | Recent commits to show |
| `magit.log.count` | `u32` | `50` | Default log entry count |
| `magit.log.graph` | `bool` | `true` | Show commit graph |
| `magit.log.decorate` | `bool` | `true` | Show branch/tag decorations |
| `magit.blame.author-width` | `u8` | `12` | Max author name width in blame gutter |
| `magit.blame.date-format` | `string` | `"relative"` | `relative`, `short`, or `iso` |
| `magit.commit.show-diff` | `bool` | `true` | Show staged diff in commit buffer |
| `magit.commit.confirm-before-push` | `bool` | `false` | Prompt before push after commit |
| `magit.diff.context-lines` | `u32` | `3` | Context lines in inline diffs |

## 17. Slice plan

Slices ordered by dependency. VCS.1 + VCS.2 are prerequisite infrastructure;
Magit views build incrementally on top.

| Slice | Description | Depends on | Status |
|---|---|---|---|---|
| **VCS.1** | `lattice-vcs` crate — Layer 1 data API. `Repository`, `GitBlob`, `Reference`, `WorkingTree`, `Index`, `Commit`, `Branch`, `Stash`, `PathStatus`. Read + Write API complete. `GitBaseline` as `DiffParticipantSource`. `gix` isolated. Zero `lattice-*` deps. | None | 📝 |
| **VCS.2** | Layer 2 subsystem — `RepositoryWatcher`, `RepositoryEvent` on event bus, auto-register `DiffSession(GitBaseline(HEAD))` on `DocumentOpened`, `git.auto-head-diff` option. Formerly D.7. | VCS.1 | 📝 |
| **PICK.1** | Picker transient-mode extension — `TransientSpec` / `TransientGroup` / `TransientItem` / `TransientItemKind` / `PreviewFn` / `TransientState` types in `lattice-picker`. Grouped entry rendering, single-key triggers, flag toggle + live preview update, argument → minibuffer, submenu navigation, `PickerRegistry::open_transient()`. General picker feature; NOT magit-specific. Serves which-key hints, command palette drilldown, future plugin transients. | None (picker subsystem) | 📝 |
| **MG.1** | `lattice-magit` crate scaffolding — crate layout, `MagitCoreMode` (minor), `MagitStatusMode` (major shell), `install(boot)`, mode+command registrations, `SubsystemBoot` wiring. No view content yet. | VCS.2, mode-architecture | 📝 |
| **MG.2** | magit-status buffer — section index (file paths + status labels, no diffs), `DiffCache` (lazy per-file diff cache), lightweight refresh (no diff commands), `=` toggle for on-demand diff loading, section fold registration, inline diff via D.3 virtual rows, headerline with branch/repo status, auto-refresh on `RepositoryEvent`. | MG.1 | 📝 |
| **MG.3** | magit-status actions — `s`/`u`/`x` for hunks and files, `cc`/`ca` commit/amend, `=` toggle inline diff, `p` stage hunk interactively, `<CR>` context-aware open/visit. | MG.2 | 📝 |
| **MG.4** | magit-commit — message editor, staged diff preview, `C-c C-c`/`C-c C-k`, amend. | MG.2 | 📝 |
| **MG.5** | magit-diff — dedicated diff view, reuse D.4 side-by-side, hunk staging from diff buffer (`s`/`u`/`x`). | MG.2 | 📝 |
| **MG.6** | magit-log — commit history with graph, ref decorations, `<CR>` show commit. | MG.2 | 📝 |
| **MG.7** | magit-blame — blame data loading, `BlameLineMap` cache, blame gutter column, `<CR>` show commit, `p` re-blame at parent. | MG.1 | 📝 |
| **MG.8** | Transient menus — `C-c g` dispatch, branch/merge/rebase/stash file-dispatch transient definitions, `TransientSpec` definitions, `TransientState` management, command preview generators, global bindings (`C-c g`, `C-c f`). Consumes PICK.1 picker transient mode. | MG.3, PICK.1 | 📝 |
| **MG.9** | magit-stash, magit-branch, magit-rebase — remaining operation buffers and actions. | MG.8 | 📝 |
| **MG.10** | Polish — persistent state cache, perf optimization, error handling (detached HEAD, bare repo, no-repo), `:help magit`, manual QA pass. | MG.1–MG.9 | 📝 |

**Dependency graph:**

```
VCS.1 → VCS.2 ─┬─→ MG.1 → MG.2 → MG.3 ─┬─→ MG.8 → MG.9
                │       │    ├── MG.4   │
PICK.1 ─────────┘       │    ├── MG.5   │
                         │    ├── MG.6   │
                         │    └── MG.7 ──┘

MG.1…MG.9 → MG.10 (can run in parallel with earlier slices)
```

**Parallelism notes:**
- VCS.1, VCS.2, and PICK.1 are independent and can be built concurrently.
- MG.1 depends on VCS.2 (needs `RepositoryEvent` and git data types).
- MG.4/MG.5/MG.6/MG.7 can run in parallel after MG.1 lands (each is an
  independent major mode definition + buffer provisioning).
- MG.8 waits for both MG.3 (status actions that feed transient dispatch) and
  PICK.1 (the picker's transient rendering mode).
- MG.10 can begin before MG.8/MG.9 complete — polish tasks like error handling
  and `:help magit` touch all views.

See [`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md)
for per-slice status, test counts, and commit references as slices land.

## 18. Testing strategy

- **Unit tests in `lattice-vcs`:** Repository discovery in temp git repos, blob
  read, ref resolution, working-tree status classification, `GitBaseline`
  snapshot round-trip, write-API (stage/unstage/commit/branch) in temp repos.
- **Unit tests in `lattice-magit`:** Section index construction from parsed git
  output, section fold range computation, hunk boundary detection from diff
  output, blame line map construction.
- **Integration tests** (headless host, git repos in `lattice-tests/fixtures/
  magit/`): `:magit-status` opens with expected sections, `s`/`u`/`x` produce
  correct git state changes, `:magit-commit` → confirm → HEAD advances,
  `:magit-log` renders expected commits, `:magit-blame` renders correct
  annotations.
- **Renderer tests (TUI + GPUI):** Section fold toggle renders correctly,
  inline diff hunks paint with deletion blocks, blame gutter column renders
  with correct width and content, headerline shows branch/status.
- **Stress tests:** Large repo (10k commits, 500 tracked files) — status opens
  within 500ms, refresh within 500ms, no corruption under rapid git state
  changes.
- **Edge cases:** Opening magit-status outside a git repo shows "Not a git
  repository"; detached HEAD shows "(HEAD detached at <sha>)"; bare repo denies
  write operations; stale hunk boundaries reject with "refresh and retry".

## 19. Risks

- **Git output parsing fragility.** Magit parses `git diff`, `git log`, `git
  blame`, `git status --porcelain`. The `--porcelain` format is explicitly
  versioned, but edge cases (binary files, submodules, renames, merge conflicts)
  can produce unexpected output. Mitigation: comprehensive fixture corpus;
  defensive parsing with graceful fallback (show raw output on parse failure).

- **Process spawning overhead.** The lazy strategy (§6) avoids the worst
  case: `=` loads diffs per-file on demand, not all files at once. Typical
  users expand 1-3 files per session. Even a full-section expand loads files
  independently on `spawn_blocking` — they stream in as each completes,
  and the UI remains responsive throughout. The per-command overhead
  (~5-50ms spawn + parse) is paid only for files the user explicitly
  expands, not for every changed file in the repo.

- **Muscle-memory fidelity.** Users with magit muscle memory expect exact
  keybinding fidelity. Mitigation: bind canonical magit keymap exactly; use
  magit's own help as the reference; user-facing docs mirror magit's help.

- **Vim keybinding conflicts.** `s` is vim's substitute operator. In magit
  buffers (normal-mode, read-only), `s` must mean "stage." The conflict is
  harmless because the buffer is read-only outside the commit-message region.
  Mitigation: test every magit chord against the default keymap; magit's
  keymap layer has higher priority per layered resolution order.

- **Layer 1 write-API correctness.** Partial hunk staging requires exact
  boundary matching. Mitigation: re-read file state before applying; reject
  stale hunks with a user-visible message.

## 20. Standing-rule verification

- **Mode ownership:** ✅ every chord, action handler, keymap, and buffer
  creation lives in `lattice-magit`. The acid test: landing this crate requires
  zero new `Editor::` methods.
- **No kind-specific logic:** ✅ magit buffers are ordinary Documents with a
  major mode. Renderer's `match buffer_kind` is untouched.
- **Substrate vs helper:** ✅ section index is mode-private helper data, not a
  `Document` trait method. Section fold provider is a `FoldSource` registered
  through the fold overlay service.
- **No UI-thread work:** ✅ git ops on `spawn_blocking`, buffer mutation via
  `apply_edit_batch_blocking`.
- **Dashed namespaced ex-commands:** ✅ `magit-status`, `magit-log`, etc.
- **Separate design from slice plan:** ✅ this fragment owns what + why;
  `slice-plans/magit.md` owns when + how.
- **Four artefacts per slice:** each slice ships design doc update, bench
  coverage, test coverage (unit + integration + renderer), and graceful error
  handling.

## 21. Cross-references

- [`vcs-and-magit.md`](vcs-and-magit.md) — superseded 2026-05-31 sketch (see
  header in that file).
- [`diff-system.md`](diff-system.md) — diff subsystem Magit consumes.
- [`diff-extraction.md`](diff-extraction.md) — `SubsystemBoot` pattern Magit's
  `install(boot)` follows.
- [`host-provider-boundary.md`](host-provider-boundary.md) — boundary keeping
  Magit inverted out of `lattice-host`.
- [`mode-architecture.md`](mode-architecture.md) — `Mode` trait, `ModeActivator`,
  `ActionHandlerRegistry`, Drop-based cleanup.
- [`compilation-mode.md`](compilation-mode.md) — synthetic-buffer + process
  spawning + tick drain pattern Magit reuses.
- [`lighthouse.md`](lighthouse.md) — bundled-plugin pattern; host-services
  Magit's WASM migration path depends on.
- [`fold-architecture.md`](fold-architecture.md) — section/hunk fold registration.
- [`virtual-rows.md`](virtual-rows.md) — deletion-block virtual rows.
- [`design.md`](design.md) §5.2.1 — unified command/grammar dispatch.
- [`design.md`](design.md) §5.9 — everything-is-a-buffer; synthetic Documents.
- [`design.md`](design.md) §5.10 — event system; `RepositoryEvent`.
- [`design.md`](design.md) §5.12 — typed options.
- [`../operations/implementation.md`](../operations/implementation.md) —
  VCS.1–VCS.2 and MG.1–MG.10 slice status tracked here.
- [`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md) —
  per-slice status, test counts, commit references.
