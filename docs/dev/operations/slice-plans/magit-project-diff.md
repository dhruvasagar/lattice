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
| PD.1 | `lattice-magit` → `lattice-multibuffer` dep + provider skeleton | 📝 |
| PD.2 | Async scan — changed set → hunks → excerpts, headerline progress | 📝 |
| PD.3 | Trigger — `:magit-diff-project` + the Diff transient's `e` row | 📝 |
| PD.4 | Edit propagation + the read-only rule | 📝 |
| PD.5 | File-boundary folds | 📝 |

PD.1→PD.2→PD.4 is the spine. PD.3 can land any time after PD.1 (an
empty view is a legitimate intermediate). PD.5 is independent.

---

## PD.1 — Provider skeleton 📝

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

## PD.2 — Async scan 📝

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

## PD.3 — Trigger 📝

- Ex-command `:magit-diff-project`, one dashed namespaced alias. No
  collapsed spelling; no new 1–2 letter short.
- A fourth `TransientRow` on `DIFF_SHOW_ROWS`: key `e`, label `edit`,
  "Edit the working-tree diff across files". `d` / `f` / `v` unchanged.
- Handler body in `lattice-magit`, registered against its own ActionId.
  **Zero `Editor::` additions** — the acid test.

**Tests.** The transient's `e` opens the view; `d`, `f`, `v` still do
what they did (the regression that matters — the trie checks a node's
own binding before its children, which is what pushed `f` and `v` into
this menu in the first place); the ex-command resolves.

**GPUI parity.** No new `Effect` variant expected — confirm with
`grep -rn "project.diff" crates/lattice-ui-gpui/` and record the empty
result rather than assuming it.

## PD.4 — Edit propagation + the read-only rule 📝

- Working-tree comparisons: excerpt edits propagate through the standard
  M.3 pipeline into the file. No patch application anywhere.
- Staged / `rev..rev` comparisons: the view opens **read-only**, via the
  existing per-buffer read-only property — **never** a renderer or
  motion kind-branch.
- The headerline states which comparison is shown and whether it is
  editable, so read-only is explained rather than merely enforced.

**Tests.** Edit an excerpt → the working-tree file changes and the hunk
recomputes; a staged-comparison view refuses edits *through the modal
Insert / operator path* (the read-only gate), with the refusal echoed;
closing a source file removes its excerpts.

## PD.5 — File-boundary folds 📝

M.8 folds so `:set foldlevel=0` gives one row per file.

**Tests.** 50-file diff at `foldlevel=0` → 50 rows; toggling a file's
fold reveals its hunks; fold state survives a `gr` refresh.

---

## Deferred

- **Per-excerpt staging (`s` / `u` / `x`).** The thing this view could
  do that neither Zed's project diff nor magit's patch buffers do — fix
  and stage in one place. Needs an excerpt ↔ hunk mapping that survives
  the user typing (the hunk moves on the first keystroke). Its own
  design question, not a follow-on chore.
- **Index write-back**, which would make staged comparisons editable.
- **`:magit-diff-project <rev1>..<rev2>`** — read-only by design §2.1;
  cheap once PD.2 parameterises the comparison, but not the daily
  driver.
