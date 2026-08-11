# Multibuffer stale sources

Design for the **stale-source guard**: a multibuffer must not persist a
source file whose on-disk content changed after the view loaded it.

Companion to [`multibuffer-views.md`](multibuffer-views.md) (the view
model) and `autoread.md` (the same problem for ordinary buffers).
Sequencing:
[`../operations/slice-plans/multibuffer-stale-sources.md`](../operations/slice-plans/multibuffer-stale-sources.md).

## 1. The bug

A multibuffer's sources are **snapshots**. Each provider reads a file
into a fresh `RopeDocumentHandle` at view-creation time
(`providers::search`, `providers::problems`,
`lattice-lsp::providers::references` all do this, deliberately sharing
one shape). The handle holds that text for the view's lifetime.

`MultibufferDocumentHandle::save` then writes **every dirty source back
to disk**, and says so in its own comment — *"Generic for ALL
multibuffer views (narrow, project-search, future diff / references)"*.

So:

1. `:copen` (or `:search`, or `:lsp-references`) loads `foo.rs`.
2. `foo.rs` changes underneath — a rebase, a formatter, another editor,
   the user's own `:w` in a different pane.
3. The user edits an excerpt and `:w`s the view.
4. The view writes its **stale** copy plus the edit, silently discarding
   whatever step 2 wrote.

That is data loss, not a misplaced offset. It is also **not**
provider-specific: every provider that loads from disk has it, which is
all of them.

An ordinary file buffer is already protected — `autoread` fingerprints
each file-backed buffer and detects external change. Multibuffer
sources were simply never wired into that protection.

## 2. The guard

Reuse `OnDiskFingerprint`, do not invent a second mechanism. Its design
is already right for this:

- **content hash is authoritative** — a bare `touch` that bumps mtime
  without changing bytes must not trip the guard;
- **`(mtime, size)` is a cheap pre-gate** — avoids hashing a file that
  demonstrably has not moved;
- **`stat` failure degrades** to `mtime = None` rather than erroring —
  a missing stat must never break a save.

### 2.1 Capture at insertion, verify at save

The baseline is captured when a source enters the view. At that instant
the in-memory text *is* what was just read from disk, so
`OnDiskFingerprint::from_path_and_text(path, text)` is exactly the
right baseline with no extra I/O beyond one `stat`.

At save, per dirty source:

1. Cheap pre-gate: `(mtime, size)` unchanged ⇒ not stale, write it.
2. Otherwise re-read and hash. Content equal to the baseline ⇒ the file
   was touched but not changed ⇒ write it.
3. Content differs ⇒ **stale**. Refuse *that source*, keep going with
   the others, and report which files were skipped.

**Refusing one source, not the whole save, is deliberate.** A 30-file
references view where one file moved should still persist the other 29;
failing the lot would punish the user for an unrelated change and
tempt a `:w!` habit that discards the very thing the guard protects.

### 2.2 What the user sees

An echo naming the skipped files, not a silent count. The recovery is
already in the user's hands and worth naming in the message: `gr`
refreshes a references view, `:copen` re-opens problems, `:search`
re-runs a scan — each re-reads from disk and reconciles.

No prompt. A modal "overwrite?" on `:w` would block the save path,
which is exactly where a prompt is most hostile, and the safe answer is
almost always "don't clobber".

## 3. Where the type lives

`OnDiskFingerprint` currently sits in `lattice-host::autoread`.
`lattice-multibuffer` must not depend on `lattice-host`, so the type
moves down to **`lattice-core`** — the crate that already owns
`Document` / `BufferId`, and a dependency of both.

`lattice-host::autoread` re-exports it so its four existing call sites
and the two TUI tests are unchanged. This is a relocation, not a
redesign: the type, its semantics and its tests move intact.

## 4. Rejected alternatives

- **Guard in each provider.** Rejected: three providers with the same
  hole, and every future one inherits it. The save path is shared, so
  the guard belongs with the save path.
- **Re-read sources at save and merge.** Rejected: a three-way merge on
  the `:w` path, with no UI to resolve conflicts. Refusing is honest;
  merging silently is the failure this guard exists to prevent, wearing
  a helpful hat.
- **Fail the whole save on any stale source.** Rejected (§2.1).
- **Watch source files with the autoread watcher.** Rejected for now —
  it would keep views live, but a watcher per source across every open
  multibuffer is real cost for a case the save-time check already makes
  safe. Revisit if users want live-updating views rather than merely
  non-destructive ones.
- **A new fingerprint type local to multibuffer.** Rejected: two
  mechanisms for "did this file change" would drift, and autoread's has
  already had the `touch`-vs-content distinction thought through.

## 5. Paramount-goal alignment

- **UX (higher court).** The guard's whole point: never silently
  destroy the user's other work. The partial save (§2.1) keeps it from
  becoming an obstacle.
- **#1 Performance.** Save-path only, never per-keystroke or per-frame.
  The pre-gate means an unchanged file costs one `stat`.
- **#3 Everything-is-a-buffer.** Fixed at the shared `Document::save`
  impl, so every provider inherits it — no kind-branching, no
  per-provider copy.
