# Magit project diff — slice plan

> **Status: Active.** Opened 2026-08-10. Implements
> [`magit-project-diff.md`](../../architecture/magit-project-diff.md):
> the editable cross-file working-tree diff as a multibuffer.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Depends on [`refreshable-views.md`](refreshable-views.md) RV.2 (the view
inherits `gr` from `magit-core-mode` rather than declaring one).
Catalogue entry: A.1 in
[`multibuffer-providers.md`](multibuffer-providers.md).

## Status

| Slice | Title | Status |
|---|---|---|
| PD.1 | `lattice-magit` → `lattice-multibuffer` dep + provider skeleton | ✅ |
| PD.2 | Scan — changed set → baselines → excerpts | ✅ |
| PD.3 | Trigger — `:magit-project-diff` + the Diff transient's `e` row | ✅ |
| PD.4 | Edit propagation + the read-only rule | ✅ |
| PD.5 | File-boundary folds | ✅ |
| PD.6 | Rename + dispatch promotion | ✅ |
| PD.7a | Added/changed lines coloured | ✅ |
| PD.7b | Removed lines as virtual rows | ✅ (searchability a known ceiling) |
| PD.7c | Staleness policy after an edit | ✅ |

PD.1→PD.2→PD.4 is the spine. PD.3 can land any time after PD.1 (an
empty view is a legitimate intermediate). PD.5 is independent.

---

## PD.1 — Provider skeleton ✅

- `lattice-magit/Cargo.toml` gains `lattice-multibuffer`. **Assert the
  direction holds** — `lattice-multibuffer` must not gain a magit or
  diff dep.
- `lattice-magit/src/providers/project_diff.rs`.
- `ProjectDiffService` + `…Handle` keyed by `BufferId`: the comparison
  (working-tree / staged / range), the changed set, per-file hunks.
  **Register and look up under the same `T`** — the handle alias, not
  the inner type (the `TypeId` pitfall).
- `MagitProjectDiffMode` minor, identity marker. It activates
  `magit-core-mode` alongside and declares **no `gr` and no `q`** of its
  own (design §3.1).
- `DocumentClosed` cleanup drops the service entry.

**Tests.** The view is a plain `BufferKind::Multibuffer` — must pass
`multibuffer_is_a_regular_buffer.rs` verbatim; closing drops the service
entry; `magit-core-mode` is active on it (so `gr` / `q` resolve without
this crate binding them — the duplication regression).

## PD.2 — The scan ✅

> **Landed as the blocking half only.** `scan_changed_files` is pure and
> synchronous, documented as `spawn_blocking`-only; the batched-events +
> headerline-progress machinery moves to PD.3, where a trigger actually
> exists to drive it. Splitting it that way keeps PD.2 testable against
> real repos without a runtime, and stops PD.3 from being a bare
> ex-command with nothing behind it.
>
> **The bench is still owed** (throughput + actor-latency-during-scan).
> Recorded here rather than implied by the ✅.



- `working_tree::statuses(repo)` → changed paths; `compute_diff` per
  file → hunks; hunk post-image range → excerpt.
- **`spawn_blocking`, not `spawn`.** The actor runtime is
  `current_thread`; a bare `spawn` puts diff computation on the actor
  thread. If a `yield_now().await` loop appears here, the shape is
  wrong.
- Batched typed events; the view opens empty and fills.
- Headerline carries progress and completion — not the status line, not
  a notification.

**Tests.** 10 changed files → expected excerpt count grouped under 10
file headers; a file changed *during* the scan does not corrupt the
view; **excerpts appear without a keypress** (the wake test — a test
that presses a key first passes on the broken version too).

**Bench + runtime-responsiveness.** Throughput on a large working tree
*and* actor-latency-during-scan, the pair the provider tests require —
`multibuffer/benches/actor_latency_during_scan.rs` is the model.

## PD.3 — Trigger ✅

> **Landed with the PV.1 seam, in one commit.** The acid test in this
> slice — "zero `Editor::` additions" — could not be met by the shape
> the three existing providers use (a per-provider `AppEffect` variant
> plus a host dispatch arm plus a plugin-boundary arm). Rather than
> spend a fourth variant and then argue the acid test had been met on a
> technicality, PD.3 built the **generic provider-view seam** first and
> became its first consumer. Seam + first user land together because a
> seam with no consumer is dead code and a consumer with no seam does
> not compile — the documented "cannot compile without its neighbour"
> exception to one-slice-one-commit.
>
> The seam's design is `multibuffer-views.md` §3.7a; its follow-on
> migrations are PV.2 / PV.3 in
> [`multibuffer-providers.md`](multibuffer-providers.md).
>
> **PD.2's deferred async half landed here too**, as specified — the
> trigger is what gave the batched scan something to drive.

- Ex-command `:magit-project-diff`, one dashed namespaced alias, with
  an optional `staged` argument. No collapsed spelling; no new 1–2
  letter short.
- A fourth `TransientRow` on `DIFF_SHOW_ROWS`: key `e`, label `edit`,
  "Edit the working-tree diff across files". `d` / `f` / `v` unchanged.
- `action:magit-project-diff` + its handler in `magit_global_mode`'s
  contribution list. Both front-ends return the *same*
  `AppEffect::OpenProviderView`, so they cannot drift.
- `open_project_diff` — the registered opener. Opens the view **empty
  and immediately**, then streams. Re-triggering re-drives the scan
  into the existing `*magit:project-diff*` buffer rather than stacking
  a second buffer under the same name.
- The scan: `spawn_blocking` for the git status + baselines, then
  `spawn_blocking` per batch for the reads + diffs; `attach_batch` on
  the async side spawns source documents and appends. Headerline
  carries progress and completion. Each batch publishes
  `MultibufferExcerptsReady` — the registered off-keystroke wake.
- **Zero `Editor::` additions, zero host `Action` variants, zero
  per-provider host arms** — the acid test, met rather than argued.

**Also fixed here (a latent bug, not scope creep).**
`MultibufferExcerptsReady` and its `wake_on_event` registration lived
inside `lattice-multibuffer`'s feature-gated `providers::search`
module. A `--no-default-features` build — and any provider outside that
crate, which is exactly what this one is — therefore had **no
off-keystroke wake at all**: excerpts would have appeared only on the
next keypress. Moved to the crate root, wake un-gated.

**Tests.** The transient's `e` opens the view; `d`, `f`, `v` still do
what they did (the regression that matters — the trie checks a node's
own binding before its children, which is what pushed `f` and `v` into
this menu in the first place); the ex-command resolves; the comparison
argument parses (including the case/whitespace and unknown-value
paths); a file that vanished mid-scan is skipped, not fatal.

**GPUI parity.** No new `Effect` variant — `AppEffect` extension only,
which both renderers route through the existing `Effect::AppAction`
path. Confirmed with `grep -rn "project.diff\|OpenProviderView"
crates/lattice-ui-gpui/` → empty, recorded here rather than assumed.

## PD.4 — Edit propagation + the read-only rule ✅

- Working-tree comparisons: excerpt edits propagate through the standard
  M.3 pipeline into the file. No patch application anywhere.
- Staged / `rev..rev` comparisons: the view opens **read-only**, via the
  existing per-buffer read-only property — **never** a renderer or
  motion kind-branch.
- The headerline states which comparison is shown and whether it is
  editable, so read-only is explained rather than merely enforced.

**The read-only half landed first** (2026-08-19), through the generic
`read-only-mode` minor rather than a magit-local gate, with
`ModeActivator::deactivate_minor_by_id` added so a re-triggered view can
become writable again. It absorbed a fix it could not be honest without:
the mode contributed the `ReadOnly` option and nothing else, and that
option is only consulted by `read_only_edit_rejected` — the insert-mode
char path. Operators bypassed it entirely, so the mode stopped typing
and left `x` / `dd` / `cw` working. `ReadOnlyMode` now declares an
invocation runner.

**Edit propagation was already working; what was missing was the
coverage.** Three tests now pin the anchoring the design's §2 claim rests
on, which is the part that would have broken silently: an excerpt
anchored in generated patch text would still render, still accept
keystrokes, and propagate an edit to nowhere. So they assert that an
attached source holds the *working-tree* text (not the HEAD baseline —
the hunk ranges are post-image either way, so the two look identical in
the view), byte-identical to the file on disk; and that an edit made in
a live view arrives in the source document carrying that file's *path*.
Propagation into some document proves nothing.

`MultibufferDocumentHandle::source_text` was added for that last
assertion — the peer of `source_path`, and needed for the same reason: a
provider that spawns its own sources keeps no handle to them.

**"Closing a source file removes its excerpts" does not apply to this
provider, and the test is not written.** Recorded rather than silently
dropped. A project-diff source is created by `attach_batch` and handed
to `view.add_source`, which touches only the view's own state — these
documents are never in the host's `BufferRegistry`, so the user cannot
close one and `Event::DocumentClosed` never names them. The generic
mechanism exists and is tested where it belongs
(`lattice-multibuffer`: the source is pruned from the map and
`MultibufferSourceClosed` is published so a provider can choose policy);
it is reachable by a provider whose sources *are* user-visible buffers.
Note the generic default is to keep the excerpts and render empty rows,
which is right for search — closing one buffer should not delete your
results — and no provider consumes the event yet.

"...and the hunk recomputes" is likewise narrower than it reads: an edit
slides the excerpt's anchors (`slide_anchors_for_source`), so the view
stays coherent within a session. A genuine re-diff is `gr`.

**Also required by the tests above:** operator edits were being dropped
in every multibuffer view (grammar dispatch ran against a scratch rope
and nothing wrote back). Fixed separately; without it PD.4's
`x_does_not_delete_in_a_read_only_multibuffer` would have passed against
a mode that contributed nothing at all.

## PD.5 — File-boundary folds ✅

M.8 folds so `:set foldlevel=0` gives one row per file.

Most of this turned out to be built already and none of it where the
slice expected. M.8's `FileBoundaryFoldProvider` and `ExcerptFoldProvider`
are registered by `MultibufferMode::on_activate` for *every* multibuffer,
so the project diff inherited both; `zM` collapsed a diff to one row per
file before this slice started. Two things were actually missing.

**PD.5a — fold identity outlived nothing.** `recompute_folds` carries
closed/open state across a rebuild by matching `Fold::identity`, and the
file-boundary provider used the source's `BufferId`. `attach_batch` calls
`BufferId::next()` for every file it re-reads, so a `gr` renumbered all of
them: identity missed on every fold and the positional fallback keyed on
rows a changed diff had already moved. Every collapsed file sprang open,
looking like the fold command not sticking. Identity is now hashed from
the source path via the generic `source_path` accessor. Not
project-diff-specific — the search provider re-mints sources the same way.

**FL.1 — `foldlevel` did not exist.** §5 of the fold design fragment
specified `:set foldlevel=N` honouring nesting depth; nothing implemented
it, and `folds.rs` said so in a comment ("v1 doesn't model the level
option yet"). Built as `folds::fold_levels` / `apply_fold_level` /
`apply_fold_level_to_new` plus the typed option, with three decisions
recorded in the design fragment: equal-range folds are siblings not
parent/child (or a one-excerpt file would sit at level 2 and `foldlevel=1`
would collapse a view with one level of structure); the option is a bulk
action rather than a standing invariant (or every rebuild would undo the
user's `za`); and the default is `99` rather than vim's `0`, because
overlay fold sources are registered regardless of `foldmethod` and a `0`
default would open every search result, project diff and agent transcript
collapsed to nothing.

**Tests.** `project_diff_folds.rs` drives the real `:set` cascade over a
project-diff-shaped view: 50-file diff at `foldlevel=0` → 50 rows;
`foldlevel=1` → one row per hunk; raising it reopens; toggling one file
reveals that file's hunks and no other's; fold state survives a rebuild;
a manual toggle survives a rebuild. Plus the level arithmetic in
`folds.rs` — nesting, equal ranges, order-independence, and that the
`level >= folds.len()` early-out agrees with the full pass.

**Regression caught during the slice:** diff-mode emits unchanged-region
folds already `closed: true`. Seeding new folds with
`closed = depth > level` reopened all of them at the default level, since
a level-1 fold is not deeper than 99. `apply_fold_level_to_new` now ORs —
`foldlevel` may add a close, never remove one a provider asked for.

## PD.6 — the name and the way in ✅

Two corrections from first use (2026-08-19).

**`:magit-diff-project` → `:magit-project-diff`.** Everything else about
this feature spells it the other way round: the mode is
`magit-project-diff-mode`, the buffer `*magit:project-diff*`, the design
fragment `magit-project-diff.md`. The command was the single surface
disagreeing, which is the split HD.1's rule exists to remove — a thing
answers to one name everywhere. Renamed outright with no alias, per the
one-alias ex-command rule; it is unreleased.

**Promoted to the dispatch's top level as `e`.** It shipped reachable
only as `C-c g` → `d` → `e`, one level inside the Diff menu beside three
rows that all open patch text. That put the view people reach for most
often behind the one they reach for least, and framed it as a variant of
the patch views rather than the different surface it is. The Diff-menu
row stays: the promotion adds a route rather than moving one, and a test
asserts both, because losing the old one would break whatever muscle
memory had formed.

## PD.7 — diff colouring on the excerpts ✅ (a, b, c all landed)

**Reported from first use, and it is the thing standing between this
view and being useful:** the excerpts render, but nothing shows what
changed, so a reader cannot tell a changed line from its context.

Design §2 already claims this works — *"diff colouring layers over the
excerpt exactly as it does elsewhere — syntax highlighting underneath,
diff styling on top. Nothing about rendering is special-cased for this
view."* That is aspiration written in the present tense. Nothing attaches
a `DiffSession` to the composed multibuffer, and `project_diff.rs`
contains no reference to diff signs, sessions or decorations at all.

What makes it more than a wiring job: the existing diff subsystem is
per-buffer, keyed on a `BufferId` with a baseline and a current rope. A
project-diff view is **one** buffer whose rows come from *many* sources,
each with its own baseline — so either the session model grows a
composed-view variant, or the provider computes the styling itself at
excerpt-build time (it already has both texts; `read_and_diff` holds the
baseline and the working-tree content when it computes the hunk ranges)
and publishes it as decorations the renderer layers.

The second is likely right and cheaper, but it is a design choice about
where diff styling for composed views comes from, so it wants deciding
rather than assuming. Both renderers must land together per the
cross-renderer rule.

## PD.7c — staleness policy after an edit ✅

Design: [`magit-project-diff.md`](../../architecture/magit-project-diff.md)
§2.2, which owns the *why* (including why a live re-diff was rejected
and where the Zed/magit convention split falls).

**What shipped.**

- `MultibufferSourceEdited { view, source }` in `lattice-multibuffer` —
  the peer of `MultibufferSourceClosed`, published from the same
  `DocumentChanged` arm that already slides anchors and recomposes. The
  substrate publishes the fact with its `DocumentId → source`
  translation done; providers choose policy.
- `magit-project-diff-mode` subscribes in `on_activate` (RAII guard
  unsubscribes) and applies the policy: first edit to a file clears that
  file's spans + deletion ghosts, headerline gains
  `· N edited files — gr to refresh`. Later edits to the same file are
  ignored — the styling is already gone, and re-publishing per keystroke
  would be work for no change on the path the user is typing on.
- The scan's per-source classification moved onto `ProjectDiffService`,
  because the edit handler has to rewrite it and the scan's task locals
  do not outlive the scan. A batch still landing for a file the user has
  already edited is skipped, so a slow scan cannot un-clear it.
- `begin_styling` runs at open *and* at every `gr`, which is what makes
  a refresh clear the edited marks.

**Prerequisite found while building it, fixed first** (commit
`f5ac42e2`): `gr` in this view resolved to nothing. PD.9 moved the view
onto `refreshable-view-mode` and left no `refresh_action()` declaration,
so the chord was bound with no target — silent, since an unresolved
chord produces no error. PD.7c's headerline points at `gr`, so the key
had to work before the message could be honest.
`refreshable_views_declare_their_refresh.rs` now walks the booted mode
registry and fails any mode that inherits the chord without declaring
what it refreshes.

**Tests.** `project_diff_staleness.rs` drives the real chain — host
`DocumentChanged` → substrate translation → the mode's subscriber →
policy → headerline — and asserts it lands **without a keypress**; plus
"a file you did not touch keeps its colouring" and "the second edit to
the same file is not reprocessed". The first version of the test failed
with `"Idle"`, which is what proves the chain is what makes it pass.

---

## Deferred

- **Per-excerpt staging (`s` / `u` / `x`).** The thing this view could
  do that neither Zed's project diff nor magit's patch buffers do — fix
  and stage in one place. Needs an excerpt ↔ hunk mapping that survives
  the user typing (the hunk moves on the first keystroke). Its own
  design question, not a follow-on chore.
- **Index write-back**, which would make staged comparisons editable.
- **`:magit-project-diff <rev1>..<rev2>`** — read-only by design §2.1;
  cheap once PD.2 parameterises the comparison, but not the daily
  driver.
