# Multibuffer stale sources — slice plan

> **Status: Active.** Opened 2026-08-11. Implements
> [`multibuffer-stale-sources.md`](../../architecture/multibuffer-stale-sources.md):
> a multibuffer must not persist a source whose on-disk content changed
> after the view loaded it.

Design owns *what* and *why*; this file owns *when* and *in what order*.

## Where this came from

LR.4 was specified as a references-only "version-skew guard" against
applying edits into a stale offset. Building it surfaced two things
that made the slice wrong as written:

- The failure is **data loss, not a bad offset**. `Document::save` on a
  multibuffer writes every dirty source to disk, so a stale source
  silently discards whatever changed the file externally.
- It is **not references-specific**. `search` and `problems` load from
  disk identically; the save path is shared and says so.

So LR.4 is superseded by this plan. The references view gets the fix by
inheriting it, not by carrying its own copy.

## Status

| Slice | Title | Status |
|---|---|---|
| SS.1 | Lower `OnDiskFingerprint` to `lattice-core` | ✅ |
| SS.2 | Capture a baseline per source at insertion | ✅ |
| SS.3 | Verify at save; refuse stale sources, keep the rest | 📝 |

Strict order: SS.1 unblocks SS.2 (the type must be reachable from
`lattice-multibuffer`), SS.2 before SS.3 (nothing to verify against
without a baseline).

---

## SS.1 — Lower the fingerprint ✅

A relocation, not a redesign. The type, its semantics and its tests
move intact.

- Move `OnDiskFingerprint` (+ its `hash_text` helper) from
  `lattice-host/src/autoread.rs` to `lattice-core`.
- `lattice-host::autoread` re-exports it, so its four call sites
  (`dispatch.rs` ×2, `editor.rs`, plus two `lattice-ui-tui` tests) are
  untouched.

**Tests.** The existing fingerprint tests move with it and still pass;
`lattice-host` still compiles without changes at the call sites.

**Not in this slice:** no behaviour change whatsoever.

## SS.2 — Baseline at insertion ✅

- `MultibufferDocumentHandle` records
  `HashMap<BufferId, OnDiskFingerprint>` alongside its source map.
- Captured when a source is added (construction and `add_source`), from
  the source's path plus the text it holds — which at that moment IS
  what was read from disk, so the baseline is exact and costs one
  `stat`.
- A pathless source (a synthetic document, no file) records nothing and
  is never considered stale.

**Tests.** A view over two files records two baselines; a pathless
source records none; `replace_excerpts` re-baselines the new source set
rather than carrying stale entries forward.

## SS.3 — Verify at save 📝

- In `Document::save`, per dirty source: cheap `(mtime, size)`
  pre-gate → re-read + content-hash on mismatch → refuse only if the
  content actually differs from the baseline.
- **Refuse the source, not the save.** The other sources still persist
  (design §2.1); a 30-file view must not fail wholesale because one
  file moved.
- Echo names the skipped files and the recovery (`gr` / `:copen` /
  `:search` re-read from disk).

**Tests.** Baseline case — edit an excerpt, `:w`, source written. Stale
case — change the file on disk behind the view, `:w`, that source is
NOT written and its external content survives. Partial case — two
sources, one stale, the other still persists. Touch case — bump mtime
without changing bytes, `:w` still writes (content hash authoritative,
the reason the pre-gate is not the decision).

The stale case is the regression this whole plan exists for; it must
assert the external content is intact on disk, not merely that a
warning fired.

---

## Deferred

- **Watching source files** so views update live rather than merely
  refusing to clobber. Real cost (a watcher per source across every
  open multibuffer) for a case the save-time check already makes safe.
- **Three-way merge on conflict.** Needs conflict UI; refusing is the
  honest behaviour until that exists.
