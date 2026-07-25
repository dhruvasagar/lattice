+++
title = "Magit — Git porcelain as a core plugin"
+++


**Status:** design fragment. Contracts, data model, mode decomposition, keymap
surface, performance posture, WIT-gap analysis, slice plan. Supersedes
[`vcs-and-magit.md`](../vcs-and-magit/) (2026-05-31 sketch; archived 2026-07-25
— see header in that file). This fragment owns *what* and *why*. Slice
sequencing + per-slice status lives in
[`../operations/slice-plans/magit.md`](../operations/slice-plans/magit).

Sibling fragments: [`diff-system.md`](../diff-system/) (diff data model +
subsystem), [`diff-extraction.md`](../diff-extraction/) (diff as modes via
`SubsystemBoot`), [`compilation-mode.md`](../compilation-mode/) (the
synthetic-buffer + process-spawning pattern Magit reuses),
[`host-provider-boundary.md`](../host-provider-boundary/) (the boundary drawing
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

**Sections** (top to bottom, each collapsible via fold):

| Section | Content | Data source |
|---|---|---|
| Unpulled commits | Commits on upstream not yet merged | `WorkingTree::unpulled(repo)` via gix |
| Unpushed commits | Local commits not yet on upstream | `WorkingTree::unpushed(repo)` via gix |
| Staged changes | Files with changes in the index — paths + status only. Diffs loaded on demand via `=`. | `WorkingTree::statuses(repo)` filtered to `Modified` (staged) |
| Unstaged changes | Files with working-tree changes — paths + status only. Diffs loaded on demand via `=`. | `WorkingTree::statuses(repo)` filtered to `Modified` (unstaged) |
| Untracked files | Untracked files, listed | `WorkingTree::statuses(repo)` filtered to `Untracked` |
| Stashes | List of stashes with messages | `Stash::list(repo)` via gix |
| Recent commits | Last N commits with abbreviated SHAs and subjects | `git log --oneline -N` |

**Section rendering:** Each section has a header line (bold, with section name
and item count) followed by its body. Sections are fold regions anchored at the
header line. The fold engine handles `TAB` / `za` / `zM` / `zR` without
magit-specific code.

**Inlined diffs:** Staged and unstaged sections start as file lists with
status labels only. Pressing `=` on a file entry loads and inlines the diff
for that file — a local edit to the buffer, not a full refresh. The diff
uses the existing virtual-row provider + inline overlay path from D.3 for
deletion blocks, gutter signs, and background tints. Diffs are cached in the
`DiffCache` (§6.4) and invalidated when the file's status changes.

**Headerline:** The view-header virtual row shows: branch name + repo status
indicator (clean/dirty/+N~M) + upstream tracking (ahead/behind counts).

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

### 4.5 magit-diff (`*magit:diff*`)

Enhanced diff view for reviewing unstaged or staged changes.

- Opens a two-pane side-by-side diff (reuses D.4 pane group machinery).
- Left pane: staged version (or HEAD), right pane: working tree.
- Hunk staging directly from the diff buffer (same `s` / `u` chords).
- Visual mode over hunks + `s` stages the selected region as a partial hunk.

### 4.6 magit-stash, magit-branch, magit-rebase

Each is a synthetic Document with its own major mode:

- **magit-stash** (`*magit:stash*`): stash list with apply/pop/drop/create
  operations.
- **magit-branch** (`*magit:branch*`): branch list with checkout/create/delete/
  merge operations.
- **magit-rebase** (`*magit:rebase*`): interactive rebase todo buffer.
  Editable pick/reword/squash/fixup/drop list. `C-c C-c` runs the rebase;
  `C-c C-k` aborts.

## 5. Mode decomposition

One crate, two layers of modes: a **shared minor mode** (`magit-core`) active
on every magit buffer, and **per-view major modes** for view-specific chords
and lifecycle.

```
lattice-magit crate
├── magit-core          (MinorMode, ActivationPolicy::OnMajorMatch)
│   ├── Keymap:  gr     → refresh
│   │            q      → close (bury, return to previous buffer)
│   │            ]] / [[→ next/prev section
│   │            ]f / [f→ next/prev file/entry
│   │            ]c / [c→ next/prev hunk
│   │            TAB    → toggle section fold
│   │            S-TAB  → cycle section visibility
│   ├── ActionHandlers: refresh, close, navigation, fold
│   │   (dispatch and file-dispatch via global `C-c g`/`C-c f`)
│   └── Shared providers: section fold registration, headerline
│
├── magit-status-mode   (MajorMode)
│   ├── Keymap:  s / u / x → stage / unstage / discard
│   │            cc / ca    → commit / amend
│   │            =          → toggle inline diff
│   │            p          → stage hunk interactively
│   │            <CR>       → open/visit at cursor
│   │            (branch/stash/rebase/fetch/push via C-c g dispatch transient)
│   ├── ActionHandlers: stage/unstage/discard at cursor, open views
│   └── Buffer provisioning: ensure *magit:status*, populate, refresh
│
├── magit-commit-mode   (MajorMode)
│   ├── Keymap:  C-c C-c → commit
│   │            C-c C-k → abort
│   │            C-c C-d → toggle diff view
│   ├── ActionHandlers: commit, abort, toggle-diff
│   └── Buffer provisioning: ensure *magit:commit*, populate staged diff
│
├── magit-log-mode      (MajorMode)
│   ├── Keymap:  <CR>   → show commit detail
│   │            = =    → toggle log arguments
│   ├── ActionHandlers: open commit, refresh with args
│   └── Buffer provisioning: ensure *magit:log*, run git log, render
│
├── magit-blame-mode    (MajorMode)
│   ├── Keymap:  <CR>   → show commit for line
│   │            q      → close blame
│   │            p      → blame parent commit
│   ├── ActionHandlers: open commit at SHA, re-blame at parent
│   └── Buffer provisioning: ensure *magit:blame:<path>*, render annotations
│
├── magit-diff-mode     (MajorMode)
│   ├── Keymap:  s / u / x → stage / unstage / discard
│   ├── ActionHandlers: stage/unstage hunk at cursor
│   └── Buffer provisioning: ensure diff session, render
│
├── magit-stash-mode    (MajorMode)
│   ├── Keymap:  a / p → apply / pop stash
│   │            d     → drop stash
│   │            z     → create new stash
│   │            <CR>  → show stash diff
│   ├── ActionHandlers: apply/pop/drop/create/show stash
│   └── Buffer provisioning: ensure *magit:stash*, list stashes
│
├── magit-branch-mode   (MajorMode)
│   ├── Keymap:  <CR>  → checkout branch
│   │            c     → create branch (minibuffer prompt)
│   │            d     → delete branch
│   │            m     → merge branch
│   ├── ActionHandlers: checkout/create/delete/merge branch
│   └── Buffer provisioning: ensure *magit:branch*, list branches
│
└── magit-rebase-mode   (MajorMode)
    ├── Keymap:  C-c C-c → execute rebase
    │            C-c C-k → abort rebase
    │            editable buffer (pick/reword/squash/fixup/drop)
    ├── ActionHandlers: execute/abort rebase
    └── Buffer provisioning: ensure *magit:rebase*, populate todo list
```

**Standing-rule check (mode ownership):** every chord (`s`, `u`, `c c`, `TAB`,
etc.) is registered by the mode that owns the buffer it fires on. The
action-handler body lives in the same crate as the keymap registration.
`lattice-magit/src/magit_status_mode.rs` contains both the keymap definition
AND the `fn stage_hunk_at_cursor(...)` handler. No `Editor::do_magit_stage_hunk`
shim exists. The acid test holds.

### 5.1 Activation policy

- **`magit-core` minor mode**: activated on any buffer whose major mode matches
  `magit-status-mode | magit-log-mode | magit-commit-mode | ...`.
  Registered as `MinorMode::with_major_matcher(...)`.
- **Major modes**: activated by id when the buffer is created via
  `ModeActivator::ensure_named_document("magit:status", MAGIT_STATUS_MODE_ID, ...)`.

### 5.2 Lifecycle — `ModeActivator` + tick drain pattern

Each magit view follows the synthetic-buffer lifecycle proven by `*compilation*`
and `*messages*`:

1. **Trigger** (action handler or ex-command): calls
   `ctx.activator.ensure_named_document(name, mode_id, flags)`. The activator
   creates the buffer, sets the major mode, returns `BufferId`.
2. **`on_activate`**: pulls `DocumentHandle`, subscribes to `RepositoryEvent`
   and `DocumentChanged`, spawns a refresh task.
3. **Refresh task** (`spawn_blocking`): runs only lightweight git commands
   (`git status --porcelain` via gitoxide, `git stash list`, `git log
   --oneline -N`). Produces formatted buffer content as file lists with
   status labels — no diff content. Pushes through
   `apply_edit_batch_blocking`.
4. **Tick drain** (on actor thread): coalesces output into `apply_edit_batch`
   calls. Folds and decorations recompute via standard provider refresh
   cycle. Diff content is loaded lazily via `=` on demand (§6.3).
5. **Guard `Drop`** (deactivation): drops the event subscription, aborts
   in-flight refresh tasks, removes keymap layer. Buffer survives frozen until
   `:bd`.

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

A `RepositoryEvent` fires (file saved, index changed, HEAD moved). The
status buffer refresh task runs on `spawn_blocking`:

1. `WorkingTree::statuses(repo)` → list of changed files with status codes
   (gitoxide — fast, no process spawn).
2. `Stash::list(repo)` → stash entries (gitoxide).
3. `git log --oneline -20` → recent commits (CLI — fast, formatted output).
4. Format sections as file lists with status labels. **No diffs.**
5. `apply_edit_batch_blocking(buffer_id, edits)` replaces buffer content.
6. Invalidate cached diff data for files whose status changed.

The buffer shows file names and statuses only — a list view, not a diff
view. Rendering is O(files), not O(lines-of-diff), and typically completes
in 10-50ms for repos up to 500 tracked files.

### 6.3 Lazy diff loading — `=` on a file entry

When the user presses `=` on a file entry in the Staged or Unstaged section:

1. Check the `DiffCache`: if the file's diff is already loaded and still
   valid (status hasn't changed since cached), re-render it inline and
   register fold ranges. If the diff was previously expanded and the user is
   toggling it off, remove the inline diff content and fold ranges.
2. If not cached: run `git diff --cached <path>` or `git diff <path>` on
   `spawn_blocking`.
3. Parse hunks, format as styled text with deletion blocks as virtual rows.
4. Insert the diff content into the buffer below the file header line
   (using `apply_edit_batch_blocking` — a targeted edit, not a full buffer
   replacement).
5. Store hunk data in the `DiffCache` keyed by `(path, section_kind)`.
6. Register hunk fold ranges via the fold overlay service.
7. Move cursor to the inserted diff region.

The diff insertion is a **local edit** to the buffer — other sections and
files above/below are untouched. The renderer's virtual-row interleaving
handles deletion blocks; the fold engine handles hunk folding.

`=` on a section header (e.g., "Unstaged changes") toggles diffs for all
files in that section. Each file's diff loads independently on
`spawn_blocking`; results stream in as they complete.

### 6.4 `DiffCache` — per-file diff state

```rust
/// Owned by the mode's Guard. Keyed by (path, section: Staged | Unstaged).
pub struct DiffCache {
    entries: HashMap<DiffKey, DiffEntry>,
}

pub struct DiffKey {
    pub path: PathBuf,
    pub section: SectionKind,  // Staged or Unstaged
}

pub struct DiffEntry {
    pub hunks: Vec<Hunk>,           // parsed hunk data (from git diff output)
    pub expanded: bool,             // whether the diff is currently visible
    pub status_version: PathStatus, // the status when this diff was computed
    pub line_start: usize,          // buffer line where the diff content starts
    pub line_count: usize,          // number of lines the diff occupies
}
```

Invalidation: when a `RepositoryEvent` fires and a file's `PathStatus`
changes, any cached `DiffEntry` for that file is invalidated. The status
buffer re-renders to a clean state (file list only). If the diff was
expanded, the user must press `=` again after the refresh to re-expand it
(the diff data is stale and must be recomputed).

### 6.5 Process spawning

## 7. Section model and fold integration

Sections are the core UX primitive in magit-status. By default, sections show
**file paths and status labels only** — no diffs are pre-computed. Diffs load
on demand when the user presses `=` on a file entry (§6.3).

```
▼ Staged changes (3)                                  ← header (foldable)
  modified   src/main.rs                              ← file entry (status only)
  modified   src/lib.rs
  ...
```

After pressing `=` on `src/main.rs`:

```
▼ Staged changes (3)
  modified   src/main.rs                              ← file entry (state: expanded)
  ──────────────────────────────────────────────────  ← separator
  │ -old line                                         ← inline diff hunk (loaded on demand)
  │ +new line
  ──────────────────────────────────────────────────  ← hunk boundary
  modified   src/lib.rs                               ← file entry (status only, not expanded)
  ...
```

### 7.1 Section folds

- **Section header → fold range.** Start is the header line; end is the next
  section's header (or buffer end). Toggling a section fold collapses the
  entire section body.
- **Hunk fold → nested fold** (optional). Within the "Unstaged changes" section,
  each hunk is individually foldable. Repeated `TAB` walks innermost→outermost
  (vim convention).
- **Register through the fold overlay service.** `magit-core`'s `on_activate`
  registers a `SectionFoldProvider` that computes fold ranges from the section
  index.

### 7.2 Section index (mode-private data)

The section index is an in-memory data structure built during buffer refresh.
It stores file paths and status labels — **not diffs**. Diff data lives in
the separate `DiffCache` (§6.4) and is populated lazily when the user presses
`=`.

```rust
struct SectionIndex {
    sections: Vec<Section>,
}

struct Section {
    pub kind: SectionKind,       // Staged, Unstaged, Untracked, Stashes, ...
    pub header_line: usize,
    pub body_start: usize,
    pub body_end: usize,
    pub entries: Vec<SectionEntry>,
}

enum SectionEntry {
    File { path: PathBuf, status: PathStatus },
    Stash { index: usize, message: String },
    Commit { sha: String, subject: String },
    UntrackedFile { path: PathBuf },
}
```

The `DiffCache` (§6.4) holds per-file diff data, populated lazily:

```rust
pub struct DiffCache {
    entries: HashMap<DiffKey, DiffEntry>,
}

pub struct DiffKey {
    pub path: PathBuf,
    pub section: SectionKind,  // Staged or Unstaged
}

pub struct DiffEntry {
    pub hunks: Vec<ParsedHunk>,      // parsed from git diff output
    pub expanded: bool,              // currently visible in the buffer
    pub status_version: PathStatus,  // status when computed; stale if changed
    pub line_start: usize,           // buffer line of first diff content row
    pub line_count: usize,           // number of diff content rows
}
```

Invalidation: when `RepositoryEvent` fires and a file's `PathStatus`
changes, any cached `DiffEntry` for that file is dropped. The file returns
to its unexpanded state; the user must press `=` again to re-expand it with
fresh diff data.

Both stored in `RefCell`s on the mode's Guard — NOT on `Document` or
`Editor`. Consumed only by magit action handlers. This is the
**substrate-helper** pattern (CLAUDE.md substrate-vs-helper rule): consumed
only by the mode's own handlers → lives as helper data in `lattice-magit`,
not as a `Document` trait method.

### 7.3 Hunk staging from sections

Hunk-level operations (`s`/`u`/`x`) require the file's diff to be expanded
(via `=`) — the `DiffCache` must have a valid `DiffEntry` for the file at
cursor. If the diff is not expanded, `s`/`u`/`x` on the file header operate
on the entire file instead (stage/unstage/discard the whole file).

When the cursor is on a line inside an expanded diff:

- `s` (stage): resolves the file path and hunk boundaries from the
  `DiffEntry`, runs `git add -p <file>` with the hunk spec. Index change
  fires `RepositoryEvent` → status buffer auto-refreshes, diff cache
  invalidated.
- `u` (unstage): symmetric via `git reset -p <file>`.
- `x` (discard): `git checkout -- <file>` or `git restore <file>` for the
  hunk.

When the cursor is on a file header (diff not expanded, or outside any
hunk): `s` stages the entire file; `u` unstages; `x` discards all
working-tree changes.

### 7.4 Stale hunk boundary detection

Hunk boundaries from `git diff` output may become stale if the file has been
edited since the status buffer was refreshed. Before applying a partial stage:
re-read the file's current hunks, check that the cursor's hunk still matches
its recorded boundaries, and reject with "file changed — refresh and retry" if
boundaries have shifted. This is important because `git add -p` semantics tied
to stale diff output produce confusing results.

### 7.5 Navigation model — three levels, three chord families

The magit-status buffer has a three-level structure:
**sections → files/entries → hunks**. Navigation follows vim-unimpaired
conventions adapted to this hierarchy.

| Level | What you navigate | Chord family | Walks | Registered by |
|---|---|---|---|---|
| **Sections** | Top-level section headers (Staged, Unstaged, Untracked, Stashes, Recent commits) | `]]` / `[[` | `SectionIndex::sections`, stopping at section header lines | `magit-core` |
| **Files/entries** | File headers within the current section, stash entries, commit lines | `]f` / `[f` | `SectionIndex::entries` within the current section, stopping at entry boundary lines | `magit-core` |
| **Hunks** | Individual diff hunks within a file's displayed diff | `]c` / `[c` | `DiffCache` hunk entries for the current file (magit-status, only after `=` has expanded the diff); `HunkIndex` from the `DiffSession` (magit-diff) | `magit-core` (shadows diff system) + diff system (magit-diff) |

**Hunk navigation after lazy loading:** `]c`/`[c` in magit-status only skip
between hunks for files whose diff is currently expanded (present in
`DiffCache` with `expanded = true`). If the cursor is on a file header line
(diff not expanded), `]c`/`[c` moves to the next/previous file header
instead — same as `]f`/`[f`.

**Why `]c`/`[c` are registered in both the diff system and `magit-core`:**

- The diff system registers `]c`/`[c` at `KeymapLayer::MinorMode(diff-mode)`.
  `diff-mode` activates only on buffers with an active `DiffSession`
  (magit-diff, files with auto-inline-diff against HEAD).
- `magit-core` registers `]c`/`[c` at `KeymapLayer::MinorMode(magit-core)`.
  `magit-core` activates on every magit buffer (magit-status, magit-log, etc.).
- In `magit-diff` buffers, BOTH `diff-mode` and `magit-core` carry `]c`/`[c`.
  Minor-mode keymap priority is activation order; the first-activated mode's
  binding wins. Both point at the same `HunkIndex` (magit-diff has a real
  `DiffSession`), so the behaviour is identical.
- In `magit-status` buffers, only `magit-core`'s `]c`/`[c` fire — they walk the
  `DiffCache`'s hunk entries for expanded files. No `DiffSession` exists here.
- In non-magit buffers with `diff-mode` (e.g., a file with auto-gutter-diff),
  only the diff system's `]c`/`[c` fire. No magit mode is active.

**Multi-file navigation** (`]E`/`[E`, `]e`/`[e`) from the multibuffer system
is inherited when a project-wide-diff multibuffer includes magit excerpts —
zero magit-specific code needed for cross-file jumping.

**Section visibility cycling** (`S-TAB`) walks through:
1. All sections expanded (default on open).
2. Only changed sections visible (Staged, Unstaged) — untracked/stashes/commits
   collapsed.
3. Only staged + unstaged file headers visible — hunks collapsed.
4. All collapsed (only section headers visible).
`TAB` toggles the fold at the cursor (section ↔ file ↔ hunk, innermost first).

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

### 8.4 Magit transients — concrete layouts

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
- `C-c f` → opens the file-dispatch transient for the current buffer's file

These follow emacs convention; all are unused in default vim normal mode.

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

## 9. Commit buffer design

The commit buffer (`*magit:commit*`) is a synthetic Document in
`magit-commit-mode` with two regions:

```
┌─────────────────────────────────────────────┐
│ [read-only] diff of staged changes          │
│ (scrollable, background-tinted)             │
│ ─────────── message area ────────────────   │
│ │ Add user authentication endpoint          │  ← subject
│ │                                           │
│ │ Implements OAuth2 flow with...            │  ← body
│ ──────────────────────────────────────────  │
│ C-c C-c commit   C-c C-k abort   C-c C-d toggle diff │
└─────────────────────────────────────────────┘
```

- **Diff region** (top, read-only): populated by `git diff --cached` —
  the exception to the lazy rule, since reviewing staged changes IS the
  purpose of this buffer. Loaded async on open with a headerline progress
  indicator. Rendered as inline overlay (reuses D.3 machinery).
- **Message region** (bottom, editable): standard text content.
- `C-c C-c`: validate subject non-empty → `Commit::create(repo, message)` →
  close buffer → publish `RepositoryEvent`.
- `C-c C-k`: close without committing.
- Amend: `c a` pre-populates the previous message, uses `Commit::amend`.

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

Each commit line stores its SHA, refs, and subject in the section index for
`<CR>` resolution. Graph characters styled with git's default coloring.

### 10.2 Blame view

Blame data is loaded per-file from `git blame --line-porcelain`, rendered as
**gutter decorations** in a dedicated blame gutter column (right of line
numbers). Cached per-file + per-revision; invalidated on file changes. Follows
the same pattern as the diff gutter column (`DiffSignKind`).

## 11. Integration with the diff subsystem

Three integration points, all consuming existing diff primitives:

1. **Inline diffs in magit-status** — loaded lazily via `=` on demand (§6.3).
   Parsed from `git diff --cached <path>` / `git diff <path>` for the file
   at cursor. Rendered as styled text with deletion blocks as virtual rows.

2. **magit-diff** — a full `DiffSession` (HEAD vs working tree via
   `GitBaseline`, side-by-side presentation). Reuses D.4 pane groups and
   `HunkRowMapper`. Diff loaded on open (the view IS the diff — this is the
   reason the user opened it). Hunk navigation (`]c`/`[c`) works through
   the diff system's registered motions (via `diff-mode`). Hunk transfer
   operators (`do`/`dp`) also work natively.

3. **Commit buffer diff preview** — loaded on open (the exception — the
   user opened the commit buffer to review staged changes). Content from
   `git diff --cached` at buffer-open time, rendered as inline overlay.

**`magit-diff-mode` keymap** adds `s`/`u` (stage/unstage hunk) on top of the
diff system's native `]c`/`[c` + `do`/`dp` surface, since `diff-mode` and
`magit-core` are both active on the diff buffer.

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
| `s` | Stage hunk or file at cursor | `magit-stage` | `s` is vim's substitute operator. Fugitive convention; no-op on read-only buffer. |
| `u` | Unstage hunk or file at cursor | `magit-unstage` | `u` is vim's undo. Fugitive convention; no-op on read-only buffer. |
| `x` | Discard hunk or file at cursor | `magit-discard` | `x` is vim's delete character. No-op on read-only. `x` is easier to type than `X` (no shift); the read-only buffer makes the override safe. |
| `=` | Toggle inline diff at cursor | `magit-toggle-diff` | `=` is vim's format/indent operator. Fugitive convention. |
| `cc` | Commit — open `*magit:commit*` | `magit-commit` | `cc` = change-line (suppressed in read-only). Fugitive convention. |
| `ca` | Amend previous commit | `magit-commit-amend` | `ca` = change-around (suppressed in read-only). |
| `p` | Stage hunk interactively (`git add -p`) | `magit-stage-patch` | `p` is vim's paste. No-op on read-only. Fugitive convention. |
| `<CR>` | Context-aware open/visit at cursor | `magit-visit` | See §12.3. |

**Operations accessed through the `C-c g` dispatch transient or ex-commands**
(removed from direct keybindings — the first key of each two-key chord clashes
with a fundamental vim navigation or operator prefix):

| Operation | Removed binding | Clash | Access via |
|---|---|---|---|
| Open log for file | `ll`/`lo` | `l` = right motion | `:magit-log` / `:magit-log-buffer-file` / `C-c g` dispatch |
| Open detail diff | `dd`/`dr` | `d` = delete operator prefix | `:magit-diff` / `:magit-diff-range` / `C-c g` dispatch |
| Branch checkout/create/delete | `bb`/`bc`/`bd`/`bm` | `b` = back-word motion | `:magit-branch` / `C-c g` → Branch submenu |
| Merge | `mm`/`ma` | — | `C-c g` → Merge submenu |
| Stash operations | `zz`/`za`/`zp`/`zd`/`zl` | `z` = fold/scroll prefix — worst clash since magit also uses folds | `:magit-stash-list` / `C-c g` → Stash submenu |
| Fetch / Push | `F` / `P` | `F{char}` = find-backwards, `P` = paste-before | `:magit-fetch` / `:magit-push` / `C-c g` dispatch |
| Rebase | `rr`/`rc`/`ra` | `r{char}` = replace char | `:magit-rebase` / `C-c g` → Rebase submenu |

The `C-c g` dispatch transient (§8.4.1) is the **discoverability surface** — it shows
every available operation with descriptions and submenus. Users learn the
direct chords over time; new users press `C-c g`.

### 12.3 `<CR>` — context-aware open/visit

`<CR>` is a general "visit/drill-into" action, not file-dispatch. Its behavior
depends on what's under the cursor:

| Cursor on | `<CR>` action |
|---|---|
| File entry (staged/unstaged/untracked section) | Open the file for editing (working-tree version) |
| Commit line (log, recent commits section) | Open `*magit:commit:<sha>*` showing full diff |
| Blame line | Open `*magit:commit:<sha>*` for the line's commit |
| Branch name | Check out that branch |
| Stash entry | Open stash detail diff |
| Hunk (staged/unstaged diff) | Open the file with cursor at the hunk location |

This is registered as a generic `magit-visit` action in `magit-core` that
dispatches on the `SectionIndex` entry kind at the cursor position. Per-view
major modes can shadow it with view-specific behavior (e.g., `magit-log-mode`
binds `<CR>` to `magit-log-show-commit` which opens the commit detail buffer).

### 12.4 `magit-commit-mode`

The commit buffer is partially editable (the message region). `C-c` chords
are the emacs/magit convention for commit operations; `C-c` is not a normal-
mode prefix in vim (it's `C-c` in insert mode for escape-like behavior),
so there is no clash in the editable commit-message region.

| Chord | Action | Command |
|---|---|---|
| `C-c C-c` | Commit with message | `magit-commit-confirm` |
| `C-c C-k` | Abort commit | `magit-commit-abort` |
| `C-c C-d` | Toggle diff preview | `magit-commit-toggle-diff` |

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
| `p` | Re-blame at the parent commit | `magit-blame-parent` |
| `q` | Close blame buffer | `magit-close` |

Blame chunk navigation (next/previous blame chunk, recentering) uses the
`]c`/`[c` hunk motions inherited from `magit-core`. `p` re-runs blame
against the parent of the commit at the cursor line.

### 12.7 `magit-diff-mode`

`magit-diff-mode` inherits all `diff-mode` chords (`]c`/`[c`, `do`/`dp`)
plus `magit-core` chords. It adds:

| Chord | Action | Command |
|---|---|---|
| `s` | Stage hunk at cursor | `magit-stage` |
| `u` | Unstage hunk at cursor | `magit-unstage` |
| `x` | Discard hunk at cursor | `magit-discard` |

### 12.8 `magit-stash-mode`

| Chord | Action | Command |
|---|---|---|
| `<CR>` | Show stash diff at cursor | `magit-stash-show` |
| `a` | Apply stash at cursor (keep in list) | `magit-stash-apply` |
| `p` | Pop stash at cursor (apply + drop) | `magit-stash-pop` |
| `d` | Drop stash at cursor | `magit-stash-drop` |
| `z` | Create new stash (`git stash`) | `magit-stash-create` |

### 12.9 `magit-branch-mode`

| Chord | Action | Command |
|---|---|---|
| `<CR>` | Check out branch at cursor | `magit-branch-checkout` |
| `c` | Create new branch (minibuffer prompt) | `magit-branch-create` |
| `d` | Delete branch at cursor | `magit-branch-delete` |
| `m` | Merge branch at cursor into current | `magit-branch-merge` |

### 12.10 `magit-rebase-mode`

| Chord | Action | Command |
|---|---|---|
| `C-c C-c` | Execute rebase | `magit-rebase-confirm` |
| `C-c C-k` | Abort rebase | `magit-rebase-abort` |

The buffer is an editable todo list (`pick`/`reword`/`squash`/`fixup`/`drop`).
The user edits the list using normal vim editing commands, then `C-c C-c` to
execute.

### 12.11 Ex-commands (dashed + namespaced)

| Command | Action |
|---|---|
| `:magit-status` | Open magit-status for the current repo |
| `:magit-log [ref]` | Open magit-log buffer |
| `:magit-blame [path]` | Open blame for current file or path |
| `:magit-commit` | Open commit buffer |
| `:magit-diff [ref]` | Open detail diff against ref |
| `:magit-stash-list` | Open stash list buffer |
| `:magit-branch` | Open branch list buffer |
| `:magit-file-dispatch` | Open file-dispatch popup for the current code buffer's file |
| `:magit-rebase` | Start interactive rebase |
| `:magit-fetch [remote]` | Fetch from remote |
| `:magit-push [remote] [branch]` | Push to remote |
| `:magit-merge [branch]` | Merge a branch into current |

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

## 13. Performance posture

The design follows a "lazy by default" strategy (§6). Every operation is
deferred until explicitly invoked by the user. The status buffer opens as a
fast file list — no pre-computed diffs, no full-repo git operations beyond
`git status --porcelain`.

- **Status buffer open (initial):** `git status --porcelain` (via gitoxide)
  + `git stash list` + `git log --oneline -20`. For a repo with 500 tracked
  files, completes in **10-50ms** on `spawn_blocking`. The buffer is a file
  list with status labels — no diff content. Headerline shows progress
  during initial load; subsequent loads are near-instant.

- **Status buffer auto-refresh (after commit/stage/unstage):** same fast path.
  Only file statuses are re-fetched. Cached diffs are invalidated for files
  whose status changed; files with unchanged status keep their cached diff
  (if expanded). Buffer update is a full replace via `apply_edit_batch`;
  file list renders instantly.

- **Diff loading (`=` on a file):** `git diff --cached <path>` or
  `git diff <path>` on `spawn_blocking`. For typical files (100-2000 lines),
  completes in **5-50ms**. Content inserted as a local edit — other sections
  and files are untouched. Renders as virtual rows on next frame.

- **Diff loading (`=` on a section):** N files load independently on
  `spawn_blocking`. Results stream in as each file completes; the buffer
  updates incrementally. For 20 changed files, all diffs visible within
  **100-400ms** total.

- **Per-keystroke (in magit buffer):** chord dispatch → mode filter → action
  handler. Identical overhead to any other buffer (<500ns p99). No WASM
  boundary.

- **Section folding:** fold engine is viewport-bounded. Collapsing a 200-line
  section (or a group of expanded diffs) is O(fold-elision in cells worker).
  Proven by D-fix.5.

- **Inline diff rendering:** deletion blocks as virtual rows — renderer treats
  them identically to document rows. O(viewport-rows). Proven by D.3.

- **Blame annotations:** loaded async on demand, cached per-file. Gutter
  column rendering O(viewport-lines). No per-frame recompute.

- **Memory:** section index ~5-20KB for typical repo, `DiffCache` ~1-5KB
  per expanded file, blame line map ~5-50KB per file. Git process output
  buffered and discarded after parse. Total memory overhead for a fully-
  expanded status buffer with 20 changed files: < 200KB.

- **Comparison with Emacs magit:** Emacs magit runs `git diff --cached` and
  `git diff` on every status refresh — O(all-changed-lines). In large repos
  with 50+ changed files and large diffs (refactors touching hundreds of
  lines), this can take 2-10 seconds. Lattice's lazy approach is O(files) on
  open, O(lines-in-expanded-files) on demand — typically 50-100× faster for
  the initial status render.

- **No UI-thread work.** Zero I/O, parsing, git operations, or formatting on
  the render thread. Renderer sees ordinary Documents. `match buffer_kind` is
  untouched — magit buffers are ordinary Documents with major modes, folds, and
  decorations. The kind-agnostic-buffer invariant holds.

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

See [`../operations/slice-plans/magit.md`](../operations/slice-plans/magit)
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

- [`vcs-and-magit.md`](../vcs-and-magit/) — superseded 2026-05-31 sketch (see
  header in that file).
- [`diff-system.md`](../diff-system/) — diff subsystem Magit consumes.
- [`diff-extraction.md`](../diff-extraction/) — `SubsystemBoot` pattern Magit's
  `install(boot)` follows.
- [`host-provider-boundary.md`](../host-provider-boundary/) — boundary keeping
  Magit inverted out of `lattice-host`.
- [`mode-architecture.md`](../mode-architecture/) — `Mode` trait, `ModeActivator`,
  `ActionHandlerRegistry`, Drop-based cleanup.
- [`compilation-mode.md`](../compilation-mode/) — synthetic-buffer + process
  spawning + tick drain pattern Magit reuses.
- [`lighthouse.md`](../lighthouse/) — bundled-plugin pattern; host-services
  Magit's WASM migration path depends on.
- [`fold-architecture.md`](../fold-architecture/) — section/hunk fold registration.
- [`virtual-rows.md`](../virtual-rows/) — deletion-block virtual rows.
- [`design.md`](../design/) §5.2.1 — unified command/grammar dispatch.
- [`design.md`](../design/) §5.9 — everything-is-a-buffer; synthetic Documents.
- [`design.md`](../design/) §5.10 — event system; `RepositoryEvent`.
- [`design.md`](../design/) §5.12 — typed options.
- [`../operations/implementation.md`](../operations/implementation) —
  VCS.1–VCS.2 and MG.1–MG.10 slice status tracked here.
- [`../operations/slice-plans/magit.md`](../operations/slice-plans/magit) —
  per-slice status, test counts, commit references.
