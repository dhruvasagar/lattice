# Autoread — design fragment (external-change detection + refresh)

> **Status: implemented (AR.0–AR.6).** Addresses `design.md` §15:21 ("File
> watcher / auto-revert", deferred) for the Document-buffer case. Vim's
> `autoread`: when a file changes on disk, refresh the buffer. Detection is a
> live `notify` watcher (not vim's trigger-based mtime poll); the *reload
> policy* is kept vim-faithful. Conflict resolution reuses the diff subsystem.
>
> Slice sequencing + per-slice landing notes:
> [`../operations/slice-plans/archive/autoread.md`](../operations/slice-plans/archive/autoread.md).
> Deferred enhancements: auto-reload on conflict-verdict, 3-way auto-merge,
> network-FS mtime-poll fallback (§6), option-flip immediate re-sync.

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
| **Dirty (conflict)** | Open a **diff resolver** (`ProgrammaticDiffRequest`, ours ∣ disk). User reconciles per hunk (`do`/`dp` / `:diffget`/`:diffput`), then `:diff-accept` writes the merge to disk and `:e!` reconciles the buffer. Never clobbers local edits. Full flow below. |
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

### Conflict resolution flow — the exact UX

When `apply_pending_autoread_for_active` finds a dirty buffer whose file changed,
`open_autoread_conflict_diff` reuses `open_programmatic_diff`, which lays out
**three panes** (the origin pane is kept, two diff panes open to its right, all
equal-width):

```
┌─────────────────┬──────────────────────┬───────────────────────┐
│ your buffer     │ <file> (baseline)    │ <file> (proposed)     │
│ (origin pane,   │ on-disk / EXTERNAL   │ YOUR content, a copy  │
│  untouched,     │ version, READ-ONLY   │ — EDITABLE, active,   │
│  still dirty)   │ (left, "theirs")     │ cursor at 1st hunk    │
└─────────────────┴──────────────────────┴───────────────────────┘
                   └────── 2-way diff pane group ──────┘
```

The diff *group* is `[baseline, proposed]`; the origin pane is a bystander. The
cursor lands in the **proposed** (editable, right) pane at the first change.

**Per-hunk reconciliation is the operative workflow** — the whole-session
`:diff-accept` / `:diff-reject` only finalize. From the proposed pane the user
walks and transfers hunks with the standard 2-way diff primitives (see
`diff-system.md`; these already exist):

- `]c` / `[c` — jump between hunks.
- `do` (`:diffget`) — **obtain** this hunk from the baseline (pull the external
  change in). Leaving a hunk untouched keeps your version.
- `dp` (`:diffput`) — push your hunk to the baseline; rarely useful here since the
  baseline is discarded, but available.
- Free-text edits in the proposed pane are also fine — it's a real buffer.

**Finalize:**

- `:diff-accept` → the **host** writes the proposed pane's *live* content (with the
  user's per-hunk transfers + edits) to the file (`tear_down_single_diff_session`
  → the `programmatic_diff_accept_paths` save, host-side — NOT via the oneshot),
  closes the two diff panes, drops their buffers, refocuses the origin pane.
- `:diff-reject` → no write (the file keeps its external content), panes close,
  refocus origin.

The completion oneshot is **fire-and-forget** for autoread (the receiver is
dropped at open); the save-on-accept is host-side, so a dropped receiver changes
nothing. This is why the flow works without the producer round-trip the IDE peer
uses.

**The `:e!` reconciliation step (and why the guard needs it).** After
accept/reject the user is back in their *original* buffer, which is **still dirty
with the pre-conflict edits and still out of sync with the now-resolved file**.
`autoread_conflict_open` keeps autoread hands-off for that buffer so a
resolve-then-save can't loop (accept writes disk → watcher fires → the still-dirty
original would re-open a resolver forever). The user reconciles their buffer with
`:e!`, which reloads from disk, mints a **new** buffer id (clearing the guard), and
re-stamps the fingerprint — autoread re-engages. The warning message points at
this: *"…resolve in the diff (`:e!` to discard local)"*.

**Known v1 friction (honest list; all deferred, none data-losing):**

1. The origin pane is redundant for autoread (it duplicates the proposed side's
   initial content). The IDE peer wants it (the `:claude` terminal); autoread does
   not. A future `open_programmatic_diff` variant could suppress it.
2. The user edits a *copy*, then must `:e!` their real buffer. The deeper fix is a
   2-way diff whose editable side **is** the live document buffer, so resolution
   lands directly and no `:e!` is needed.
3. `:diff-accept` doesn't auto-reload the original buffer. The deferred
   verdict-await enhancement (hold the oneshot, reload on `Accept`) removes the
   `:e!` step and clears the guard automatically.
4. Resolving by `:w` (keep-mine, overwrite disk) instead of `:e!` leaves the guard
   set (same buffer id), so autoread stays off for that buffer until it's reloaded
   or closed. `:w` here also overwrites the accepted merge, so the guiding message
   steers to `:e!`; `:w` is the deliberate "keep my version" escape hatch.

## 6. Options + lifecycle

- **`autoread`** — typed `bool`, **default `true`** (Neovim's default; matches the
  feature intent), per-buffer overridable via the resolved-options path. When
  `false` for a buffer, it is not watched.
- **Lifecycle** — buffer open/activate registers a `Watch(dir, basename)` (deduped);
  buffer close registers `Unwatch`; option flip re-syncs. Every command is a
  non-blocking `cmd_tx` send (fingerprint/registration-gated, mirroring
  `refresh_lsp_file_watcher`).

### The wake — why the change channel is an `InboundBus` (fixed 2026-08-04)

`drain_autoread_changes` runs inside `run_tick_pending`, which in
production is reached exactly two ways: the tail of `App::apply` (the
next **keystroke**) and the actor's `async_landed` select arm. The
watcher originally emitted into a plain
`mpsc::UnboundedSender<AutoreadChange>` and fired neither, so an
external change — a `git checkout` driven from magit, a `git pull` in
another terminal — sat in the channel until the user happened to press
a key. Classic "it works, but only after I hit something": it reads as
a rendering bug when it is a missing wake.

`spawn_autoread_watcher_task` now takes the editor's `async_landed`
`Arc<Notify>` as a **required parameter** and builds its channel with
[`lattice_mode::inbound::make_inbound_raw`], so the wake is baked into
every `send` and cannot be forgotten (paramount goal #4 —
async-correct by construction, not by discipline).

`make_inbound_raw` rather than `make_inbound`: the per-item work is
irreducibly `&mut Editor` (the reload rewrites buffer contents,
cursor and scroll), which a `FnMut(T) -> Vec<Effect>` handler cannot
capture. That is precisely the case that constructor exists for, so
the host keeps the receiver and drains it from its own tick.

**Testing it the way it fails.** Asserting on `rx.recv()` does *not*
catch a missing wake — the change is in the channel either way. The
regression test (`watcher_wakes_the_editor_on_external_write`) awaits
the `Notify` and deliberately never touches the receiver, since
draining would mask the very thing under test.

**Still deliberate: background buffers wait.** `drain_autoread_changes`
records a pending change for *every* affected buffer, but
`apply_pending_autoread_for_active` applies only the active one;
others wait for activation. That is vim's checktime-on-`BufEnter`
semantics and is intended. Consequence worth knowing: during a magit
operation the *magit* buffer is active, so a changed file shown in
another split stays stale until it is focused.

### Deferred enhancements

- **Auto-reload on the `Accept` verdict** — hold the completion oneshot (instead of
  dropping it), spawn a tiny actor-side await that reloads the original buffer on
  `Accept`. Removes the manual `:e!` step and clears the conflict guard
  automatically (the reload mints a new id). The single biggest conflict-flow UX
  win; see §5's friction list #3.
- **Resolve against the live buffer** — a 2-way variant whose editable (right) side
  *is* the user's document buffer, not an in-memory copy, so per-hunk transfers land
  directly and no `:e!` is needed (friction #2). Also lets the origin pane be
  dropped (friction #1).
- **3-way conflict auto-merge.** Retain a load/save-time **base** snapshot per
  buffer so the resolver diffs `base / ours / theirs`, auto-merging non-overlapping
  changes and surfacing only true conflicts (marker-free 3-way chords `d3o` / `dB`
  already exist). Costs one retained rope per dirty file-backed buffer; the engine
  already supports N=3.
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
- Slice sequencing + status: [`../operations/slice-plans/archive/autoread.md`](../operations/slice-plans/archive/autoread.md).
