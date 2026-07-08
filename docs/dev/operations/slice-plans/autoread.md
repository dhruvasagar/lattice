# Autoread — slice plan (AR)

Sequencing for [`../../architecture/autoread.md`](../../architecture/autoread.md).
Goal: a file-backed Document buffer refreshes when its on-disk content changes —
silent when clean, diff-resolved when conflicting — via a live `notify` watcher
that scales with open buffers, not project size, and does zero UI-thread work.

**Status legend:** 📝 planned · 🚧 in progress · ✅ landed

**Locked decisions (2026-07-08, Dhruva):**
- Detection = **live `notify` watcher** (option A), not vim's trigger model.
- `autoread` = **`bool`, default `true`** (Neovim default), per-buffer overridable.
- Conflict (dirty buffer) = **diff resolver** via `ProgrammaticDiffRequest`,
  **2-way** (ours ∣ disk) for v1; 3-way auto-merge deferred.
- Scale-invariance is a **hard requirement**: watch deduped parent dirs of open
  buffers, non-recursive, LRU-bounded with on-activate `stat` fallback.

| Slice | Status | Summary |
|---|---|---|
| AR.0 | ✅ | On-disk **fingerprint** `(mtime, size)` per file buffer + content-hash; stamped on load and on `:w`. Self-write suppression seam. Pure host, no watcher yet. |
| AR.1 | ✅ | **`autoread`** `bool` option (default `true`), per-buffer resolved-options wiring. |
| AR.2 | 📝 | **Watcher runtime task** (`AutoreadWatcherHandle { cmd_tx }`), `notify` over deduped **parent dirs**, basename filter, debounce/coalesce. `notify` calls in `block_in_place`. Non-blocking `Watch`/`Unwatch`/evict sends. Mirrors `LspFileWatcherHandle`. |
| AR.3 | 📝 | **Lifecycle wiring**: register `Watch` on buffer open/activate, `Unwatch` on close, re-sync on option flip. LRU-bounded live set + on-`activate_document` `stat` fallback for the cold tail. |
| AR.4 | 📝 | **Drain + clean-reload policy**: host drains validated change events; `!dirty` ⇒ silent `open_fresh_into_active_slot(_, Reload)` + echo; deleted ⇒ keep + warn. |
| AR.5 | 📝 | **Conflict resolver**: `dirty` ⇒ emit `ProgrammaticDiffRequest` (2-way ours ∣ disk); verdict → reload / keep / merged-write. |
| AR.6 | 📝 | **Bench + docs + hardening**: event→reload latency bench, watch-set setup-cost bench, scale test (watch count tracks open-buffer dirs not project size), runtime-responsiveness test, graceful-degradation sweep; flip design fragment to ✅. |

## AR.0 — Fingerprint + self-write suppression ✅

`(mtime, size)` + content hash stored per file-backed buffer (host-side, alongside
the existing dirty tracking). Stamp on load and after `:w`. Unit tests: a save
updates the fingerprint; an identical-content rewrite is a no-op by content hash;
a foreign write differs. No watcher yet — pure, fast to land green.

**Landed:** new `lattice-host::autoread` module — `OnDiskFingerprint { mtime,
size, content_hash }` with `from_path_and_text` (stat-failure-tolerant),
`same_content` (content-hash authoritative — a `touch` is not a change), and
`stat_unchanged` (the cheap `(mtime,size)` pre-gate AR.2 reads before deciding to
read+hash). `Editor.on_disk_fingerprints: HashMap<BufferId, OnDiskFingerprint>`
(keyed by "has on-disk backing", never `BufferKind`). Stamped via
`Editor::stamp_on_disk_fingerprint` at the load site (`open_fresh_into_active_slot`)
and the plain-save site (`save_blocking`, after a successful write to the buffer's
own path); removed at every per-buffer teardown site incl. `:bd`. **5 module unit
tests + 3 integration tests** (load stamps, save re-stamps to match disk =
self-write suppressible, `:bd` drops the entry); 637 host + full ui-tui suites green.

**Deferred nuance (noted here, not a bug):** `:w <other-path>` (save-as / write a
copy) goes through `save_as_blocking` and deliberately does **not** re-stamp — the
buffer's backing file is unchanged. If a future `:saveas` re-points the buffer's
path, stamping there against the new path is the correct follow-up.

## AR.1 — `autoread` option ✅

**Landed:** `Autoread: bool = true` declared in `lattice-config::core_options`
(auto-registered via the `options!` linkme slice; `:set autoread` /
`:set noautoread` parse-front-end works for free). Host reader
`Editor::autoread_enabled_for(buffer_id)` resolves the per-buffer value via the
existing `resolved_option::<Autoread>` path (default `true`). 2 host tests
(registered + default true); per-buffer-off-suppresses-watching is asserted in
AR.3 where the watcher reads this flag.

## AR.2 — Watcher runtime task 📝

New `AutoreadWatcherHandle { cmd_tx }` on `Editor`; a tokio task on the LSP runtime
owns `notify::RecommendedWatcher`, the event rx, and the dir→basenames map.
Commands: `Watch(dir, basename)`, `Unwatch(dir, basename)`, `EvictDir(dir)`.
Coalesce/debounce a burst into one `stat` per basename; `stat`-gate before read;
content-hash before emitting a host-drainable change event. `notify` API calls in
`tokio::task::block_in_place` so a watch add never stalls sibling runtime tasks.
**Non-recursive watches only.** Tests: throughput + `current_thread`
runtime-responsiveness (standing rule).

## AR.3 — Lifecycle + LRU bound 📝

`Watch` on buffer open/activate (deduped by dir), `Unwatch` on close, re-sync on
option flip — all fingerprint/registration-gated non-blocking sends (mirror
`refresh_lsp_file_watcher`). Live-watch set LRU-bounded by focus recency; on
eviction the buffer falls back to an off-thread `stat` at `activate_document`.
Tests: watch registered on open; unwatched on close; `autoread=false` buffer never
watched; **scale test** — N buffers across K dirs ⇒ ≤ min(K, cap) watches,
independent of project file count.

## AR.4 — Clean reload policy 📝

Host drains validated change events (new `Action`/event, like `drain_lsp_fs_events`).
`!dirty` ⇒ `open_fresh_into_active_slot(_, Reload)` (cursor+scroll preserved) +
`info!`/echo `"<file>" reloaded`. Deleted-on-disk ⇒ keep buffer, warn, no wipe.
Integration test: external write → active buffer reloads to new content, cursor
preserved.

## AR.5 — Diff conflict resolver 📝

`dirty` + external change ⇒ emit `ProgrammaticDiffRequest` (ours ∣ disk, 2-way).
Wire the verdict: take-disk ⇒ reload; keep-local ⇒ ignore (buffer wins until next
`:w`); merged ⇒ apply resolved rope. Integration test: modify buffer + write disk
⇒ diff resolver opens; verdict reload vs keep produces the right buffer content.

## AR.6 — Bench + docs + hardening 📝

- **Bench** — event→reload latency; watch-set setup cost at 10 / 100 / 1000 open
  buffers (asserts flat steady-state).
- **Docs** — flip `autoread.md` to landed; tick this plan per slice; add an
  `autoread` row to `docs/user/` options coverage.
- **Hardening** — notify/stat/read errors `debug!`+skip; file-vanishes-mid-read
  race; non-UTF-8; watch-add failure downgrades to on-activate `stat`.

## Risk / sequencing notes

- **Self-write suppression (AR.0) is the correctness keystone** — land and test it
  before the watcher so AR.2 has a stable fingerprint to gate on. A missing/stale
  fingerprint makes every `:w` self-trigger.
- **Cross-renderer parity** — the diff resolver (AR.5) surfaces through the diff
  subsystem, already a peer-neutral host primitive; no TUI/GPUI-specific work
  expected. The clean-reload echo path is host-side. Audit at AR.6:
  `grep -rn "Autoread\|autoread" crates/lattice-ui-gpui/` should show only whatever
  the diff subsystem already routes.
- **`inotify` limits** — the LRU bound (AR.3) is what keeps descriptor use bounded;
  do not skip it even though the common case (a handful of open buffers) never
  approaches the limit.
