# Magit project diff (the editable cross-file diff view)

Design for `magit-project-diff`: every changed file in the working tree
composed into **one editable [multibuffer](multibuffer-views.md)**, one
excerpt per hunk, folded per file.

Companion to [`magit.md`](magit.md) (the subsystem),
[`magit-hunk-staging.md`](magit-hunk-staging.md) (per-hunk staging) and
[`diff-system.md`](diff-system.md) (hunk computation). Sequencing:
[`../operations/slice-plans/magit-project-diff.md`](../operations/slice-plans/magit-project-diff.md).
Catalogue entry A.1 in
[`../operations/slice-plans/multibuffer-providers.md`](../operations/slice-plans/multibuffer-providers.md).

## 1. The gap

Magit already diffs well, in two shapes, and neither is this one:

- **`magit-status` sections** — Staged / Unstaged with inline hunks.
  Cross-file, but it is a *status* buffer: the hunks are patch text and
  the surface is built for staging, not reading at length.
- **`*magit:diff:staged:<path>*` / `*magit:diff:unstaged:<path>*`** —
  a real scrollable diff buffer with file-level `s` / `u` / `x`. Reads
  well, but it is **per file**, and it is still patch text.

What is missing is the third shape: **all changed files at once, as
real source you can edit.** You are reviewing a 30-file change, you spot
a typo in file 19, and today you must leave the diff, open the file,
find the line, fix it, and come back. The multibuffer collapses that to
typing in the excerpt.

This is a different job from staging, so it is a third surface, not a
replacement for either of the two above.

## 2. Excerpts are hunks anchored in real files

An excerpt is a hunk's **post-image range in the working-tree file**,
with the standard context. Because the excerpt is anchored in the actual
file (not in generated patch text), editing it goes through the ordinary
M.3 edit-propagation pipeline and lands in the file, with no
patch-application step and no write-back path of its own.

Diff colouring layers over the excerpt exactly as it does elsewhere —
syntax highlighting underneath, diff styling on top
([`span-layering.md`](span-layering.md)). Nothing about rendering is
special-cased for this view.

### 2.1 Editability follows the post-image — and only the working tree is a file

This is the load-bearing constraint, and it is a real limit rather than
a simplification:

| Comparison | Post-image | View |
|---|---|---|
| working tree vs `HEAD` | the file on disk | **editable** |
| working tree vs index (unstaged) | the file on disk | **editable** |
| index vs `HEAD` (staged) | an index blob | **read-only** |
| `rev1..rev2` | two blobs | **read-only** |

An index blob is not a file, so there is no anchor an edit could
propagate through. Rather than inventing an index-write-back path, the
non-working-tree comparisons open read-only, and the view's headerline
says so. Staging the *edits you just made* is what the existing magit
surfaces are for, and §5 keeps that door open.

A read-only excerpt is not a degraded mode — it is the correct rendering
of a comparison between two things that are not the file on disk.

### 2.2 An edit invalidates what the scan derived, and the view says so

Editing is the point of this view, and it is also what makes the view
stop describing itself. The classification behind the tints, the gutter
signs and the deletion ghosts is computed once per scan, in **source
line coordinates**. An in-excerpt insert does not resize its excerpt
(`slide_anchors_for_source` slides only excerpts an edit sits *above*),
so the composed rows keep their indices while the text beneath them
moves: every tint below the edit then paints the wrong line and each
deletion ghost anchors a row out. Nothing announced it, and the drift
grew with exactly the work the view exists to support.

**Policy: drop what we can no longer describe, say so, and let `gr`
rebuild.** On the first edit to a file, that file's spans and ghosts are
cleared and the headerline gains `· N edited files — gr to refresh`,
extending the scan's own summary rather than replacing what the view
says it is. Every *other* file keeps its colouring — one edit must not
cost the other twenty-nine their diff.

The substrate publishes the fact and the provider decides what it means:
`MultibufferSourceEdited { view, source }`, the peer of
`MultibufferSourceClosed`, carries the `DocumentId → source` translation
that the multibuffer already owns, and `magit-project-diff-mode`
subscribes. No host involvement in either half.

**Why not recompute instead.** Re-diffing the edited file on an idle
settle would fix the tints and still leave the excerpt *set* stale: an
edit that creates a new hunk gets no excerpt, and one edited back to the
baseline keeps an excerpt showing unchanged code. That is liveness that
looks total and is not — worse than honest staleness, because the user
cannot see where the guarantee stops. Live re-excerpting is the third
option and the UX rule vetoes it outright: re-excerpting while the user
types moves unedited content under the cursor.

The convention here is genuinely split, so it is named rather than
smoothed over. Zed's project diff — the substrate-adjacent peer — does
update live; magit and fugitive refresh explicitly. The differentiator
is that in Zed the diff is the primary review surface with no refresh
key in the user's muscle memory, whereas this view sits inside a magit
surface where `gr` is universal and the headerline is already where a
view reports its own state.

Per-excerpt staging (§5) needs an excerpt ↔ hunk mapping that survives
typing — the same mapping a live re-diff would need to be honest. When
that exists, this policy is the thing to revisit.

## 3. Ownership

The provider lives in **`lattice-magit`**, which takes a dependency on
`lattice-multibuffer` (acyclic — multibuffer depends on neither magit
nor diff). This follows the provider-home reversal in
[`multibuffer-views.md`](multibuffer-views.md) §3.7: a provider that is
a subsystem's user-facing surface belongs in that subsystem's crate, so
the trigger, the keymap, the handler bodies and the view all sit
together.

Magit already owns the two inputs — `lattice-vcs`
(`working_tree::statuses` for the changed set) and `lattice-diff`
(`compute_diff` for the hunks) — so no data source has to move.

### 3.1 It inherits its chords instead of declaring them

The view joins the magit family as a minor and takes the family's
cross-buffer chords rather than restating them. **Which part of the
family** is the correction PD.9 made: it implied `magit-core-mode`,
whose `i`, `C`, `D`, `S`, `U`, `q` and `yr` are safe only on the
read-only lists its `ActivationPolicy::Majors` gate allows — and this
view is editable, so `i` opened the .gitignore prompt instead of
entering Insert. It now implies **`magit-nav-mode`** (the chords that
mean something in a buffer you can type in) plus
**`refreshable-view-mode`** for `gr`.

**Inheriting `gr` is not the same as having a refresh**, which PD.7c
found the hard way. `refreshable-view-mode` binds the chord and
dispatches whatever an active mode *declared* via `refresh_action()`
([`mode-architecture.md`](mode-architecture.md) §5.5); PD.9 moved the
view onto that mode and left no declaration, so `gr` resolved to
nothing and pressing it did nothing at all — silently, since an
unresolved chord produces no error and no log line. The mode now
declares `action:magit-project-diff-refresh` **and** supplies its
handler (declaring the target and leaving the body to the host would be
the half-migration the mode-ownership rule forbids). Its own action
rather than `action:magit-refresh`, which dispatches through the
`MagitView` trait to a per-buffer published view: this is a *provider*
view, so re-running its provider IS the rebuild. The handler reads the
comparison back off the service so `gr` in a staged view refreshes as
staged. A registry-walk guard now fails any mode that implies
`refreshable-view-mode` without declaring an action.

This is the "shared behaviour is a minor mode, never a copied keymap"
rule paying out rather than being restated: a new magit view gets the
family's chords by activating the family's mode, and the gap where
`magit-diff-mode` was hand-given `s` and `u` but not `x` cannot recur
here.

## 4. Scan is async, like every other provider

`working_tree::statuses` then `compute_diff` per changed file is
filesystem and CPU work, so it follows the established async-provider
pattern: the trigger returns immediately with an empty view, batches
stream in over typed events, and the **headerline** carries progress and
completion (the convention for async-populated buffers — not the status
line, not a notification).

`spawn_blocking`, not `spawn` — the editor actor is a `current_thread`
runtime, so a bare `spawn` lands the diff computation on the actor
thread. Paramount goal #1, and the failure mode is the one
`ui_responsive_during_scan`-style tests exist to catch.

File-boundary folds (M.8) mean a 50-file diff collapses to a 50-row
outline at `foldlevel=0` — which is what makes the view usable at the
scale where it earns its keep.

## 5. What is deliberately not in the first cut

- **Per-excerpt staging (`s` / `u` / `x`).** Genuinely wanted — it is
  the thing this view could do that neither Zed's project diff nor
  magit's patch buffers do, since you could fix and stage in one place.
  It needs an excerpt ↔ hunk mapping that survives edits (the hunk moves
  the moment you type), which is its own problem. Deferred to a slice,
  not designed away.
- **`rev1..rev2` and staged comparisons.** Read-only by §2.1; worth
  having, but the working-tree case is the daily driver and proves the
  shape first.
- **Replacing anything.** `magit-status` and the per-file diff buffers
  are unchanged.

## 6. Paramount-goal alignment

- **#1 Performance.** Scan off-thread via `spawn_blocking`, batched
  events, headerline progress; folds keep element fan-out proportional
  to the viewport, not to the diff.
- **#2 Extensibility.** No new host surface: the provider contributes an
  ActionId and a transient row from inside `lattice-magit`, and requires
  zero `Editor::` additions — the acid test for a provider crate.
- **#3 Everything-is-a-buffer.** A plain `BufferKind::Multibuffer`; it
  must pass `multibuffer_is_a_regular_buffer.rs` verbatim. Read-only
  variants use the existing read-only property, **not** a kind-branch.

## 7. UX (higher court)

The view adds a surface and changes none. `gr` in it means refresh, as
in every other magit buffer. The convention it follows is Zed's project
diff (the closest peer, and the substrate-adjacent reference), while the
chords stay magit's — the split the "editor references weighted by
substrate" rule prescribes.

## 8. Rejected alternatives

- **Extend `magit-diff-mode` to write through to the working tree.**
  Rejected: its buffer is patch text, so a write-through would need to
  map edited patch lines back to file offsets — reconstructing, badly,
  the anchoring a multibuffer excerpt gives for free. It would also make
  one buffer mean two things depending on the comparison.
- **A `ProjectDiffProvider` in `lattice-multibuffer`** (the 2026-06-01
  provider-home lock). Rejected: it would put the view in one crate, the
  transient row in another, and leave `lattice-multibuffer` — a
  substrate crate — depending on VCS and diff.
- **Index write-back so staged diffs are editable.** Rejected for the
  first cut: it is a genuine feature with a genuine cost, and shipping
  read-only is honest where shipping a half-working write path is not.
- **Making this the default `d` in the diff transient.** Rejected: `d`
  is the patch view people already have muscle memory for; the editable
  view is a peer, and it earns its own row (§9).

## 9. Trigger

- Ex-command **`:magit-project-diff`** — dashed, namespaced, one alias.
- A fourth row on the **Diff transient** (`d`), which currently carries
  `d` diff / `f` file / `v` side-by-side. **`e` — "edit"** reads
  correctly and is free. (`p` was considered and passed over: real magit
  binds `p` to *diff paths* in the same menu, so reusing it for
  "project" would fight established muscle memory.)
