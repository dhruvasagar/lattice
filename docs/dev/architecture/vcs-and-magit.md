# VCS and Magit-style Integration

**Status: SUPERSEDED 2026-07-25** — this 2026-05-31 sketch is replaced by the
detailed design fragment [`magit.md`](magit.md), which owns the authoritative
contracts, mode decomposition, keymap surface, performance posture, and slice
plan. The three-layer model survives; everything else (mode decomposition,
deliverable path as native crate through `SubsystemBoot`, WIT-gap analysis,
`lattice-vcs` vs `lattice-magit` crate split rationale, complete view
inventory, and 12-slice plan) is in `magit.md`. This file is retained for
historical reference (the original three-layer concept); cross-references
have been repointed to `magit.md`.

## 1. Why this fragment exists

The original D.7 slice was scoped to a small `lattice-vcs` crate
providing `git_baseline(path, ref) -> Rope` for the `:Gdiff` ex-command.
After D.6 closed, the design conversation revealed that:

1. **The always-on UX** (gutter signs against HEAD for every file in a
   git repo) is what every modern editor — Helix, Zed, VSCode,
   IntelliJ — ships as table-stakes. `:Gdiff` alone doesn't deliver
   that.
2. **Magit-style is a paramount-goal-#2 test case.** "WebAssembly
   Component Model plugin host from day one" means a magit-equivalent
   needs to be buildable as a plugin. If it can't be, the plugin API
   isn't actually first-class.
3. **Magit is v1, not deferred.** v1's plugin priorities include
   magit-style git UX. D.7 must therefore design the VCS surface so a
   magit plugin can consume it without a redesign.

Goal: every layer below the magit-plugin layer is built before D.7
ships. The plugin lands on top once Phase 7 (plugin architecture)
delivers the WASM host.

## 2. Three-layer model

```
┌──────────────────────────────────────────────────────────┐
│  Layer 3 — Magit-style UX                                │
│    Commit / branch / log / blame / stage hunks /         │
│    rebase / cherry-pick / stash / fetch / push           │
│    Lands as a BUILT-IN PLUGIN (compile-time linked)      │
│    through the same WIT contract a third-party plugin    │
│    would use. v1 priority plugin.                        │
└────────────────────────┬─────────────────────────────────┘
                         │ consumes
┌────────────────────────▼─────────────────────────────────┐
│  Layer 2 — `lattice-vcs` subsystem on `lattice-host`     │
│    Repository discovery + filesystem watcher tasks.      │
│    AUTO-INLINE-DIFF: every file in a git repo gets       │
│    `DiffSession(GitBaseline(HEAD, path))` registered on  │
│    `DocumentOpened` — gutter signs visible immediately,  │
│    no command needed.                                    │
│    Watches `.git/HEAD`, `.git/index`, `.git/refs/*`;     │
│    fires `RepositoryEvent` on the event bus when state   │
│    changes externally (commit / checkout / index update).│
│    CORE.                                                 │
└────────────────────────┬─────────────────────────────────┘
                         │ consumes
┌────────────────────────▼─────────────────────────────────┐
│  Layer 1 — `lattice-vcs` crate (data layer)              │
│    Repository / GitBlob / Reference / WorkingTree /      │
│    Index / Object types. `gix` dependency isolated.      │
│    Read-API complete in v1; WRITE-API designed but       │
│    stubbed (`unimplemented!()`) until Layer 3 builds.    │
│    The write-API shape is locked in now so Layer 3       │
│    landing doesn't churn the contract.                   │
│    CORE.                                                 │
└──────────────────────────────────────────────────────────┘
```

## 3. Layer-1 API surface (read-only v1)

The crate exposes a small typed surface that's reproducible from any
in-tree consumer (Layer 2 subsystem) and any WIT-bound plugin (Layer 3
magit, future jujutsu/sapling alternatives, etc.). The shape below is
indicative — bikeshed during D.7.a:

```rust
// Repository discovery + handle.
pub struct Repository { /* gix::Repository wrapped */ }
impl Repository {
    pub fn discover(path: &Path) -> Result<Arc<Self>, RepoError>;
    pub fn workdir(&self) -> &Path;
    pub fn gitdir(&self) -> &Path;
}

// Reading.
pub struct GitBlob;
impl GitBlob {
    pub fn read(repo: &Repository, oid: Oid) -> Result<Bytes, GitError>;
}

pub struct Reference;
impl Reference {
    pub fn resolve(repo: &Repository, name: &str) -> Result<Oid, GitError>;
    // HEAD, branch names, tags, remote-tracking, short SHAs.
}

pub struct WorkingTree;
impl WorkingTree {
    pub fn path_status(repo: &Repository, path: &Path) -> PathStatus;
    pub fn statuses(repo: &Repository) -> Vec<(PathBuf, PathStatus)>;
}

pub enum PathStatus {
    Clean,
    Modified,
    Added,
    Deleted,
    Untracked,
    Ignored,
    Unmerged,
    Conflicted,
}

// Convenience: a BaselineSource impl that consumes this crate.
pub struct GitBaseline {
    repo: Arc<Repository>,
    path: PathBuf,
    reference: String,  // "HEAD", branch name, sha, ...
}
impl BaselineSource for GitBaseline { /* snapshot() reads HEAD:path */ }
```

### Write-API (Phase 7+, but shape locked now)

```rust
// Sketched here so Layer 3 doesn't drive an API break.
impl Index {
    pub fn stage_path(repo: &Repository, path: &Path) -> Result<(), GitError>;
    pub fn unstage_path(repo: &Repository, path: &Path) -> Result<(), GitError>;
    // MG.18a (2026-07-29): the sketched `stage_hunk(path, HunkSpec)` /
    // `unstage_hunk(..)` pair is GONE — the shape did not survive
    // contact with git. There is no index operation that stages "hunk N
    // of path P"; `git add -p` synthesizes a patch and pipes it to
    // `git apply --cached`, and a spec safe enough for that must carry
    // the hunk's whole body — at which point it IS the patch, and
    // `path` becomes a second source of truth that can disagree with
    // it. Region staging (a REWRITTEN hunk) cannot be expressed as a
    // reference to an existing hunk at all.
    // See docs/dev/architecture/magit-hunk-staging.md.
    pub fn apply_patch(repo: &Repository, patch: &str, cached: bool, reverse: bool) -> Result<()>;
}
impl Commit {
    pub fn create(repo: &Repository, message: &str, opts: CommitOpts) -> Result<Oid, GitError>;
    pub fn amend(repo: &Repository, message: Option<&str>) -> Result<Oid, GitError>;
}
impl Branch {
    pub fn checkout(repo: &Repository, name: &str) -> Result<(), GitError>;
    pub fn create(repo: &Repository, name: &str, base: Oid) -> Result<(), GitError>;
    pub fn delete(repo: &Repository, name: &str, force: bool) -> Result<(), GitError>;
}
```

v1 D.7 stubs these (`unimplemented!("D.7.write — Phase 7+")`); Layer 3
implements them. The trait surface IS the contract Layer 3 will consume
via the WIT bridge.

## 4. Layer-2 subsystem (`lattice-host::vcs`)

Same shape as `DiffSubsystem` / `LspSupervisor`:

- Owns `HashMap<RepoRoot, Arc<Repository>>` keyed by discovered repo
  root.
- One `RepositoryWatcher` task per active repo: tokio fs-watcher on
  `.git/HEAD`, `.git/index`, `.git/refs/heads/*`. Debounced; fires
  `RepositoryEvent` on the event bus.
- Subscribes to `DocumentOpened` events; for documents in a git repo,
  auto-registers a `DiffSession` against `GitBaseline(HEAD, path)`.
- Subscribes to `DocumentClosed`; tears down the auto-registered
  session.
- Subscribes to `RepositoryEvent`; for HEAD-change or index-update,
  pokes the debouncers of every auto-registered session in the affected
  repo (so a `git checkout` from outside the editor reflows the gutter
  signs).

Default-on; controlled by a typed option `git.auto-head-diff` (default
`true`). Matches Zed / VSCode behaviour.

## 5. Layer-3 magit plugin

The magit-equivalent ships as a built-in plugin — compile-time linked
into the editor binary, but goes through the SAME WIT contract a
third-party plugin would use. Rationale: this is the strongest
validation of paramount goal #2 (extensibility). If the plugin contract
can't host magit, it can't host serious functionality.

### What magit-the-plugin needs from the host

A working magit-equivalent needs at least these plugin-host primitives:

1. **Buffer creation** — open arbitrary buffers (commit-message editor,
   log view, status view, blame view). Each is a Document with a major
   mode (`magit-status`, `magit-log`, `magit-commit`, ...) and minor
   modes for chord scope.
2. **Event subscriptions** — listen for `RepositoryEvent`,
   `DocumentChanged`, `BufferOpened`, `CursorMoved`. Plugin tasks
   subscribe via the WIT bridge.
3. **Mode + keymap registration** — register chord bindings scoped to
   the plugin's modes (`g r` to refresh status, `s` to stage, `c c` to
   commit-popup, etc.). Goes through `CommandRegistry` like any other
   command.
4. **Prompt orchestration** — multi-step interactive flows (commit
   message → confirm → run). Likely composes existing minibuffer +
   picker primitives; may need a "transient" abstraction (per
   magit-transient).
5. **Buffer mutation API** — write to plugin-owned buffers (refreshing
   the status view, appending log entries). Read-only for the user via
   the mode definition.
6. **VCS API consumption** — call `lattice-vcs` Layer 1 read + write
   methods through the WIT bridge. Some operations (`git push`) need
   subprocess support — see §6.

### Open question: subprocess support in plugins

Some git operations are best run via the `git` CLI (push, fetch,
rebase, complex merges) because gitoxide doesn't yet cover them, or
because users want exact `git` semantics. Magit shells out via
`git` for these. The plugin contract needs to allow plugins to spawn
processes — currently the WIT surface is read-/write-bounded; adding
process-spawn is non-trivial (security, sandboxing). Revisit when
Phase 7 plugin architecture is designed.

### Open question: persistent state

Magit caches log queries, diff renders, repo state across sessions.
The plugin contract needs a persistent-storage primitive (key-value
scoped to the plugin). Might fall out of session-state save/restore.

### Open question: rendering rich buffer content

Magit's status / log / blame views are heavily decorated (text + fold
indicators + clickable regions + diff snippets inline). The cell-grid
renderer (§5.6) handles styled text natively, but the
"clickable regions" and "interactive sections" plumbing may need WIT
extensions (regions, clickability, drilldown).

## 6. Sequencing

Conditional on Phase 7 (Plugin Architecture) timing:

- **D.7 (now-ish, ships gutter signs + `:Gdiff`)** — Layer 1 +
  Layer 2 + the consumption sites in the diff subsystem. The
  always-on UX (gutter signs against HEAD) lands here.
- **Phase 7 — Plugin Architecture** — WASM Component Model host;
  reference plugin (likely something lighter than magit — maybe a
  formatter or linter plugin) proves the contract.
- **Phase 7+ / Phase 8 — Magit plugin** — first heavy plugin; tests
  whether the WIT surface really supports serious functionality.
  If gaps emerge (subprocess, rich rendering, persistent state),
  extend the WIT surface and the magit plugin together.

Magit IS v1. v1's "stable + usable + table-stakes parity" includes
git-as-a-first-class-concept. The order is just: foundations first
(D.7), plugin host (Phase 7), magit (Phase 7+ or 8).

## 7. What NOT to commit to yet

- Whether the magit plugin authors a custom transient abstraction or
  reuses pickers + minibuffer flows.
- Whether `git push` / `git fetch` / `git rebase --interactive` route
  through gitoxide (when supported) or shell out via the `git` CLI.
- Whether blame data lives in `lattice-vcs` Layer 1 or in the plugin.
- Whether the WIT contract exposes the raw `git2`-style object database
  or only the higher-level operations magit wants.

These all wait for the magit design pass that begins around Phase 7.

## 8. Cross-references

- `design.md` §2 Goals / paramount goal #2 — extensibility-from-day-one.
- `design.md` §5.5 Plugin Subsystem — WASM Component Model details.
- `design.md` §11 Project layout — `lattice-vcs` crate slot.
- `diff-system.md` §3.4 — `BaselineSource` trait that `GitBaseline`
  implements.
- `slice-plans/diff-system.md` D.7 — gets re-carved per §6 above.
