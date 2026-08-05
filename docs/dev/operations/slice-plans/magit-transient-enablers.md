# MG.42 — the four enablers behind MG.41d / MG.41e

**Status:** ✅ all four enablers landed (2026-08-05). Parent:
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

## E4 — sequencer gates for cherry-pick and revert ✅

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

**Landed.** `cherry_pick_in_progress` / `revert_in_progress` read
`CHERRY_PICK_HEAD` / `REVERT_HEAD` through a shared
`sequencer_head_exists`; two `DispatchGates` fields; six `RemoteOp`
consts; `cherry_pick_transient` / `revert_transient`. 4 tests.

**The two sequences deliberately do NOT share their sequencer rows.**
`git revert --continue` errors during a cherry-pick and vice versa, so
one shared "sequencer" table would fire the wrong command in one of
the two menus. `each_sequence_fires_its_own_commands` asserts no action
appears in both — the tempting refactor is the bug.

The file's *presence* is the state, so there is nothing to parse and
nothing to cache: the probe cannot go stale behind git's back the way a
remembered flag would.

## E2 — a multi-step runner ✅

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

**Landed.** `spawn_git_sequence(label, Vec<GitStep>)` plus the step
builders, and five rows: stash `Z`/`I`/`W`, commit `F`/`S`, merge `a`.
Branch `s`/`S` still wait on E3 (they need a name).

Three correctness details the tests pin, each a way the operation
could look right and be wrong:

- **A snapshot `apply`s, never `pop`s.** A pop removes the very stack
  entry the snapshot exists to create — leaving neither a restore
  point nor a changed tree.
- **Instant fixup rebases from `<commit>~1`, not `<commit>`.** The
  fixup has to be replayed *alongside* its target, so the rebase must
  start one before it; rebasing onto the commit itself leaves the
  marker unmerged and the operation silently pointless. `--autostash`
  because an instant fixup is reached mid-edit, which is exactly when
  a dirty-tree failure is least welcome.
- **Absorb deletes with `-d`, never `-D`.** Git refuses `-d` on a
  branch that is not fully merged, so a failed merge leaves the branch
  intact; `-D` would destroy it precisely when the merge did not take.

`run_remote_op` also gained `GIT_SEQUENCE_EDITOR=true`, the todo-list
peer of the `GIT_EDITOR` it already set. `rebase -i --autosquash` opens
the generated todo list, and accepting it unchanged IS autosquash —
git has already ordered the lines. Without it an instant fixup hangs
the same way `--continue` would without `GIT_EDITOR`.

## E3 — a second input ✅

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

**Landed.** A `two_input_op!` macro chaining prompt → prompt → run,
plus reset `f` a file and stash `b` branch.

**The carried value is a single static slot, and that cannot
mis-pair.** The second prompt is only ever opened BY the first's finish
handler, so a read is always preceded by its matching write; the second
finish `take()`s, so a value is consumed once; and a cancelled second
prompt leaves a stale value that the next chain's first step overwrites
before anything reads it.

An empty value at EITHER step runs nothing — the alternative is a
half-applied operation with an empty argument, and `git checkout <c> --
""` is not a no-op.

**Reset-a-file is `checkout <commit> -- <path>`, not `reset`.**
`checkout` replaces the file in index and working tree, which is what
the row promises; `reset <commit> -- <path>` moves index entries only
and leaves the file on disk untouched. Same words, different outcome,
and the failure would look like the command silently doing nothing.
The `--` placement is separately pinned, because a file named like a
branch would otherwise be read as a revision.

Rebase `p`/`u`/`e` remain: they need `RemoteTarget` resolution wired to
the rebase op rather than a second prompt, so they belong with a
rebase-onto slice rather than here.

## E1 — message-composing operations ✅

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

**Unblocks:** commit `w` reword, `A` augment, merge `e` edit — all
three now landed.

**Landed.** `CommitIntent` (`Create` / `Amend` / `Reword` / `Augment` /
`MergeEdit`) replaces the `amend: bool`, `Commit::reword` /
`Commit::augment` / `Commit::merge_with_message` in `lattice-vcs`, and
the commit `w` / `A` and merge `e` rows.

**Reword is `--amend --only`, not `--amend`** — and that single flag is
the whole point of the row. Without `--only`, anything currently staged
is swept into the commit being reworded: a content change the user did
not ask for and would not expect from a row labelled "reword". The two
intents are deliberately not collapsed, and a test asserts they differ.

The name still selects the intent, but once, explicitly, in
`from_buffer_name` — and `reword` is matched before `amend` so a future
rename cannot make one shadow the other. What is gone is the *implicit*
coupling: behaviour no longer depends on a substring test scattered at
the point of use.

**The two targeted intents carry their target IN the buffer name**
(`*magit:augment:<sha>*`, `*magit:merge-edit:<branch>*`). The
alternative — a side-channel map keyed by buffer — is a second thing
that can go out of step with the buffer the user is looking at, and it
would go out of step silently: the compose buffer is opened long before
the commit runs. The name is already the intent selector, so extending
it costs nothing new, and `:ls` shows which commit the buffer is about
to squash into.

**Augment's `--squash` and `-m` compose rather than conflict.** Git
writes `squash! <subject>` as the first line and appends the message
below it — which is exactly augment's semantics, a squash the author
annotated. This is verified against real git rather than assumed:
without it the obvious implementation is two steps (commit, then
rewrite the message), and the note the user typed would be dropped by
the one-step version with no error.

**Neither targeted intent seeds a prior message.** Augment's note is
the user's own addition *below* a generated marker line, and a merge
message is written fresh; seeding either would put text there the user
then has to delete.

**Augment needs a real ex-command, not just an action.** Its picker
fallback opens `COMMIT_PICK_SOURCE`, which invokes `"<arg> <sha>"` as a
command id — so the arg must name a *registered ex-command*
(`:magit-augment`), not the action. That string is unchecked at compile
time and its failure mode is the quiet kind: the picker opens, lists
commits, the user picks one, and nothing happens.
`every_commit_picker_arg_names_a_registered_ex_command` pins every such
arg against the real registry.

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
