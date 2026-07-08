# Autoread — design fragment (external-change detection + refresh)

> Addresses `design.md` §15:21 ("File watcher / auto-revert", deferred) for the
> Document-buffer case. Vim's `autoread`: when a file changes on disk, refresh
> the buffer. Detection is a live `notify` watcher (not vim's trigger-based
> mtime poll); the *reload policy* is kept vim-faithful. Conflict resolution
> reuses the diff subsystem.
>
> Slice sequencing: [`../operations/slice-plans/autoread.md`](../operations/slice-plans/autoread.md).

## 1. Goal

A file-backed Document buffer whose on-disk content changes out from under the
editor refreshes to the new content. If the buffer has no unsaved edits the
refresh is silent (cursor + scroll preserved). If it has local edits that
conflict with the on-disk change, the user resolves through a diff view rather
than losing either side.

Non-goals (deferred): emacs `auto-revert-tail-mode` (log following); the mtime
**poll fallback** for filesystems where `notify` is unreliable (network mounts);
3-way auto-merge (see §6).

## 2. Detection — live `notify` watcher, not vim's trigger model — DECIDED

Vim checks mtime only at trigger points (`:checktime`, `BufEnter`, `FocusGained`)
because it predates cheap filesystem watchers. That is *data, not justification*
(heuristic #2). We choose a live watcher on merit:

- **Paramount #1** — the watcher, the `stat`, and the read all run on the LSP
  runtime; the UI thread does no I/O regardless of what changes on disk.
- **Paramount #4** — async by construction, the *same* actor + `cmd_tx` channel
  shape already proven by `LspFileWatcherHandle`.
- **Heuristic #1** — reuses that proven substrate rather than inventing a
  mechanism, and is exactly what §15:21 calls for. The trigger model would
  additionally require `FocusGained` plumbing that exists in neither peer.
- **UX** — near-instant refresh, matching Helix/Zed/VSCode muscle memory; no
  stale-buffer window.

`autoread` names the **reload policy** (silent-if-clean, resolve-if-conflict),
which we keep faithfully. The watch *mechanism* is an orthogonal axis chosen on
merit.

### Rejected alternatives

- **(B) Trigger-based mtime check (literal vim).** Faithful but worse general UX
  (no refresh until a trigger fires) and needs new `FocusGained` events in both
  the TUI (crossterm `EnableFocusChange`) and GPUI peers. Only argument for it is
  "vim does it this way" — demoted by heuristic #2.
- **(C) Idle-tick mtime poll.** Pays `stat` cost forever and adds latency;
  strictly inferior to an event-driven watcher given the watcher infra already
  exists (heuristic #1 forbids the lesser design chosen for convenience). Kept in
  reserve *only* as the documented network-FS fallback (§6).

## 3. Scale invariance — the load-bearing property — DECIDED

**The watch set scales with the number of open file-backed buffers (deduped by
parent directory), never with project file count.** A 500k-file repo with 20 open
buffers across 8 directories is 8 watches and zero idle CPU, because `notify` is
event-driven — idle files cost nothing.

This is categorically different from the LSP watcher's *recursive workspace*
watch. **Autoread must never watch recursively.** Three safeguards bound even the
pathological "thousands of open buffers" case:

1. **Dedupe by parent dir.** Many buffers in one dir → one watch + a basename
   `HashSet`. Event filter is O(1) per event; a busy shared parent dir costs only
   cheap off-thread discards, never a reload (basename miss).
2. **Bounded live-watch set (LRU by focus recency).** If distinct open-buffer dirs
   exceed a cap, evict the coldest dir's watch. Its buffers fall back to a single
   off-thread `stat` on `activate_document` — correctness kept, `inotify`
   descriptor limits never blown. The *active* buffer is always in the hot set, so
   the buffer you're looking at is always watched.
3. **`stat`-gate before read.** A no-op `touch` costs one `stat`, not a read or a
   reload.

Watching a parent dir **non-recursively** (not per-file inode) is deliberate:
atomic saves (write-temp + `rename` — how `:w` and many external tools save) break
per-file inode watches; parent-dir watches survive them and see the new file.

**Path canonicalization (cross-platform correctness).** The watcher canonicalizes
every watched dir key. macOS FSEvents resolves symlinks in the paths it reports
(`/var` → `/private/var`, and the temp dir lives under one), while Linux inotify
echoes the path passed to `watch()`. Watching the *canonical* dir — and keying the
dir→basenames map by it — makes the event-parent lookup match on both. Downstream:
AR.3 may pass raw parent dirs (the watcher canonicalizes), but AR.4 must
canonicalize when mapping a change's (canonical) path back to a `BufferId`, since
stored buffer paths may not be canonical.

## 4. Self-write suppression — the correctness keystone

Each file-backed buffer carries an on-disk **fingerprint** `(mtime, size)`,
stamped when the buffer loads and again after the editor's own `:w`. A watcher
event whose post-read fingerprint equals the stored fingerprint is *our own
write* and is ignored. Without this every `:w` self-triggers a reload attempt.

Beyond the `(mtime, size)` gate, a **content hash** compares the freshly-read
bytes against the last-known content before any reload — killing false reloads
from writes that don't change content.

## 5. Reload / conflict policy — DECIDED

Applied host-side after the runtime drains a validated change event:

| Buffer state | Action |
|---|---|
| **Not dirty** | Silent reload via `open_fresh_into_active_slot(_, Reload)` (cursor + scroll preserved, already clamps to new content). One-line echo `"<file>" reloaded`. |
| **Dirty (conflict)** | Open a **diff resolver**: emit a `ProgrammaticDiffRequest` (ours ∣ disk). The user resolves per-hunk in diff-mode; the verdict drives reload / keep-local / merged-write. Never clobber local edits. |
| **Deleted on disk** | Keep the buffer as-is, warn. Reload only on modify; never wipe on delete. |

### Conflict resolution reuses the diff subsystem — DECIDED

`lattice-diff` already exposes `programmatic::ProgrammaticDiffRequest` — *"open a
diff and await the user's verdict"* (host-drained, side-by-side diff bound to a
completion oneshot; already used by the IDE peer's openDiff and LSP WorkspaceEdit
preview). Autoread's conflict path is a new **producer** of that existing
primitive, not a new mechanism.

- **Heuristic #1** — there is **no** generic y/n confirm primitive in the codebase;
  a binary "reload? y/n" prompt would have to build one. Diff-based reuse avoids
  that and gives strictly richer UX (see the change, resolve per hunk).
- **Everything-is-a-buffer** — the resolver is a diff buffer; no kind-branching.
- **Paramount #1** — diff recompute is async off-thread (ArcSwap publish), so the
  conflict path also does no UI-thread work.
- **Heuristic #2** — anchored on reuse + everything-is-a-buffer; that it matches
  vim's own `:h diff` merge tool is convergent, not cargo-culted.

v1 is **2-way** (ours ∣ disk); the engine's N=3 support enables 3-way auto-merge
later (§6).

## 6. Options + lifecycle

- **`autoread`** — typed `bool`, **default `true`** (Neovim's default; matches the
  feature intent), per-buffer overridable via the resolved-options path. When
  `false` for a buffer, it is not watched.
- **Lifecycle** — buffer open/activate registers a `Watch(dir, basename)` (deduped);
  buffer close registers `Unwatch`; option flip re-syncs. Every command is a
  non-blocking `cmd_tx` send (fingerprint/registration-gated, mirroring
  `refresh_lsp_file_watcher`).

### Deferred enhancements

- **3-way conflict auto-merge.** Retain a load/save-time **base** snapshot per
  buffer so the resolver diffs `base / ours / theirs`, auto-merging non-overlapping
  changes and surfacing only true conflicts. Costs one retained rope per dirty
  file-backed buffer; the engine already supports N=3.
- **Network-FS mtime-poll fallback** (§15:21's "mtime poll fallback") for mounts
  where `notify` doesn't fire.

## 7. Error handling

Recoverable failures (notify watch add/remove, `stat`, read, non-UTF-8, races
where the file vanishes between event and read) `log` at `debug!` and skip —
never panic on the runtime, never swallow silently at a user-actionable level. A
watch that can't be established downgrades that buffer to the on-activate `stat`
path (same as LRU eviction). Diagnostic/probe spans are `debug!`, not `info!`
(per the diagnostic-logging rule — watcher events can burst).

## 8. Cross-references

- Reuses the `LspFileWatcherHandle` runtime-task pattern (`design.md` §15 LSP
  file watcher; `implementation.md` Phase 4.4.l / the "LSP file watcher off the UI
  thread" slice).
- Conflict resolver: [`diff-system.md`](./diff-system.md) §3 (type model), the
  `lattice_diff::programmatic` module.
- Reload primitive: `Editor::do_edit(_, force)` → `open_fresh_into_active_slot(_,
  Reload)` in `lattice-host::dispatch`.
- Slice sequencing + status: [`../operations/slice-plans/autoread.md`](../operations/slice-plans/autoread.md).
