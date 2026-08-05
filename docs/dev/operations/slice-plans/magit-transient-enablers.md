# MG.42 — the four enablers behind MG.41d / MG.41e

**Status:** 📝 planned. Parent:
[`magit-transient-completeness.md`](magit-transient-completeness.md)
(MG.41). Design fragment:
[`../../architecture/magit.md`](../../architecture/magit.md).

MG.41 filled in every transient row that was **only** a row. What
remains in MG.41d and MG.41e is not menu structure — each outstanding
row needs a capability magit-in-lattice does not have yet. Those
capabilities are four, they are shared, and building them as their own
slice is what stops each remaining row inventing its own half-version.

---

## Why a separate plan

MG.41's audit assumed the remaining work was rows. That held for
push / pull / fetch — genuinely one operation with seven destinations,
and MG.41c landed all of them cheaply. It stopped holding at MG.41d:
`stash Z` is *stash-then-reapply*, `commit F` is
*fixup-then-autosquash*, `reset f` needs a commit **and** a path. Those
are operations wearing a row's clothing.

Rather than let MG.41d/e grow a bespoke path per row, MG.42 builds the
four shared pieces and MG.41's leftovers become rows again.

> **Heuristic #1 (long-term fit, on merit):** the alternative — one
> ad-hoc implementation per remaining row — is cheaper for the first
> two rows and worse for all fifteen, and it guarantees that (say)
> "run two git commands and report once" gets written four times with
> four different failure behaviours.
> **Paramount goals:** protects #4 (every composite op still reports
> through one `BackgroundTaskFinished`, so completion stays visible)
> and #2 (E1's intent enum and E2's runner are the seams a plugin-
> contributed magit operation would need too).
> **Mode ownership:** everything lands in `lattice-magit`. The acid
> test from MG.41 still holds — zero `Editor::` methods, zero host
> `Action` variants.

---

## A correction MG.41 shipped

MG.41d's commit message said reword / augment "need an editor" and
that `GIT_EDITOR=true` blocks them. **That is wrong**, and the record
should say so.

`GIT_EDITOR=true` constrains the `spawn_git` path only.
`magit-commit-mode` already composes a message in a buffer and calls
`Commit::amend(&repo, message)` / `Commit::create` directly — no
`$EDITOR`, no hang, no blocked pool thread. The capability has been
there since MG.14; MG.41d simply did not use it.

What is actually missing is smaller and is E1.

---

## E4 — sequencer gates for cherry-pick and revert

**Cheapest, and lands first.** Exactly the shape MG.41e's rebase gate
already established.

`git cherry-pick` and `git revert` both stop on conflict and leave a
sequencer state. Outside one, `--continue` / `--skip` / `--abort`
error; inside one, starting another is what the user must not do. So
each menu shows one set or the other, never both.

- `cherry_pick_in_progress()` / `revert_in_progress()` — read
  `CHERRY_PICK_HEAD` / `REVERT_HEAD` from the gitdir, peers of
  `rebase_in_progress`.
- Two new `DispatchGates` fields.
- `A` cherry-pick and `_` revert become gated submenus.

**Rows.** Cherry-pick: `A` pick (existing action) when idle; `A`
continue / `s` skip / `a` abort when stopped. Revert: `V` commit /
`v` changes when idle; the same three when stopped.

**Tests.** Idle offers only the way in; stopped offers only the ways
out and never the way in; the probe defaults to false so a developer's
own half-finished cherry-pick cannot flake the suite (the reason
`DispatchGates::probe` is separate from the pure builder).

**Unblocks:** MG.41e's `A` and `_`.

## E2 — a multi-step runner

Several operations are *compositions*:

| Row | Composition |
|---|---|
| commit `F` instant fixup | `commit --fixup` → `rebase --autosquash` |
| commit `S` instant squash | `commit --squash` → `rebase --autosquash` |
| stash `Z`/`I`/`W` snapshots | `stash push` → `stash apply` |
| branch `s` spin-off | `branch` → `reset` (→ `checkout`) |
| merge `a` absorb | `merge` → `branch -d` |

`run_remote_op` runs one argv. The runner runs N in order, **aborts on
the first failure**, and publishes exactly **one**
`BackgroundTaskFinished` naming the step that failed.

Both properties are the point. Continuing past a failed step is how a
snapshot silently becomes a plain stash; reporting per-step is how one
logical operation turns into four notifications.

**Tests.** A failing first step runs no second; the published label
names the failed step; a clean run reports once, not N times.

**Unblocks:** commit `F` / `S`, stash `Z` / `I` / `W`, branch `s` /
`S`, merge `a`.

## E3 — a second input

`mk_prompted` carries one value. These need two:

| Row | Inputs |
|---|---|
| reset `f` a file | commit + path |
| stash `b` branch | branch name + stash |
| rebase `p`/`u`/`e` onto | target (+ the `RemoteTarget` resolution MG.41c already has) |

Two options, and the first is preferred: **chain the prompts** — the
finish action of the first opens the second, which is ~10 lines per op
and reuses the whole existing machinery. The alternative, a dedicated
two-field prompt, is a new UI surface for three rows.

`CommitPickSource` already exists for the commit half, so where one
input is a commit the chain can be picker-then-prompt.

**Tests.** Cancelling the second prompt runs nothing (not a
half-applied operation); both values reach the argv in the right
order.

**Unblocks:** reset `f`, stash `b`, rebase `p`/`u`/`e`/`s`.

## E1 — message-composing operations

The only piece that changes existing behaviour, so it lands last.

`magit-commit-mode` decides amend-vs-create by **sniffing the buffer
name** (`name.contains("amend")`). That works for two intents and will
not extend to six.

- Replace it with an explicit `CommitIntent` (`Create`, `Amend`,
  `Reword`, `Augment`, …) on the published commit state.
- Seed the compose buffer with existing text where the intent wants it
  — reword starts from HEAD's message, augment from the target's.

Name-sniffing is worth removing on its own merits: it is implicit
coupling between a buffer's *name* and its *behaviour*, and it fails
silently if a name ever changes.

**Tests.** Each intent produces the right git operation; reword seeds
the existing message; a renamed buffer does not change behaviour (the
regression the enum prevents).

**Unblocks:** commit `w` reword, `A` augment, merge `e` edit.

---

## Sequencing

| Slice | Depends on | Why here |
|---|---|---|
| MG.42-E4 | — | Cheapest; completes two submenus; the pattern is fresh from MG.41e |
| MG.42-E2 | — | Unlocks the most rows |
| MG.42-E3 | — | Independent of E2 |
| MG.42-E1 | — | Only piece touching existing behaviour |

Then MG.41d / MG.41e close with their remaining rows as **rows**.

---

## Deliberately not doing

- **cherry-pick `h` harvest, `d` donate, `n` spinout** — multi-step
  *and* multi-input, and the least-used rows in the menu. E2 + E3
  make them possible later; they do not justify their own scoping now.
- **tag `p` prune** — needs local/remote tag comparison, which is a
  fetch-and-diff, not a git call.
- **merge `p` preview, `i` dissolve** — preview wants a diff view of a
  merge that has not happened; dissolve is a rarely-used inverse.
- **Every menu's `C` configure row** — still blocked on transient
  variable rows, exactly as MG.41 recorded. Unchanged here.

No benchmarks: every one of these is `LatencyClass::Display` menu work
or a spawned git call already off the actor thread. Recorded as a
deliberate omission, as MG.41 did.
