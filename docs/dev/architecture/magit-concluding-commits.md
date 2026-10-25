# Magit concluding commits — git stops, the commit buffer finishes

Design for every magit operation where git, run from a terminal, would
open `$EDITOR` for a commit message. Lattice cannot hand git an editor, so
each of these either failed or committed a message nobody saw.

Companion to [`magit.md`](magit.md) (the subsystem). Sequencing:
[`../operations/slice-plans/magit-concluding-commits.md`](../operations/slice-plans/magit-concluding-commits.md).

## 1. The gap

Every git invocation in `lattice-magit` runs with `GIT_EDITOR=true`, which
accepts whatever message git proposes. That stops an editor we cannot drive
from hanging the task. It also means that wherever git would have *asked*,
lattice answers for the user, unseen.

Found by audit, 2026-10-25:

| Operation | Today |
|---|---|
| Pull, branches diverged, `pull.rebase=false` | `--ff-only` is hard-coded, so it **fails**: `Not possible to fast-forward` |
| `merge --continue` after resolving conflicts | `MERGE_MSG` committed unseen |
| `cherry-pick --continue`, `revert --continue` | the stopped commit's message committed unseen |
| `rebase --continue` after a conflicted pick | the pick's message committed unseen |
| Revert (`V V`) | `--no-edit`, generated message committed unseen |
| `c c` during a stopped merge / pick / revert | the buffer opens **empty**; git's message, and its `# Conflicts:` list, are lost |

Not gaps: commit, amend, reword, augment, merge `e` and rebase `w` already
compose in `magit-commit-mode`. Merge `m m` passes `--no-edit`, as Emacs
magit's does, with `m e` the composing row. Tag `r` takes its message from
a prompt. Fixup, squash, instant fixup, `am`, notes merge and stash write
no message the user is meant to edit.

## 2. Git's own stopped state is the protocol

Each of these operations has a form in which git stops *before* the commit
and records everything needed to finish it: `MERGE_HEAD`, `CHERRY_PICK_HEAD`
or `REVERT_HEAD` names what is in flight, and `.git/MERGE_MSG` holds the
message git would have proposed. `pull --no-commit`, `revert --no-commit`
and a conflict stop all leave exactly that state.

So the design is: **let git stop, then open the ordinary commit buffer on
it.** The buffer reads git's state when it opens:

- **Seed.** In flight, the message area starts from `.git/MERGE_MSG`
  instead of being empty. A rebase's conflict stop writes one too; its
  `edit` stop does not, and a commit there is a new one, not a conclusion.
- **Confirm** concludes what git stopped:
  - merge, cherry-pick, revert: `git commit --cleanup=strip -F <msg>`,
    then `git <op> --continue` if `.git/sequencer/` says more commits
    follow.
  - rebase: `git rebase --continue` with `GIT_EDITOR` pointed at
    `cp <msg>`, the hand-off rebase `w` already uses.
- **Abort** (`C-c C-k`) closes the buffer and leaves git's state alone.
  The status buffer already shows `MERGING` / `CHERRY-PICKING` and offers
  the ways out, so closing the buffer loses nothing.

`c c` is no longer special: committing mid-merge *is* concluding the
merge, which is what `git commit` itself means there.

### 2.1 Why two confirm paths

Verified against git, not assumed (2026-10-25):

- `cherry-pick --continue` and `revert --continue` **do not run
  `GIT_EDITOR` when not on a terminal**; they commit `MERGE_MSG` as it
  stands. So the `cp <msg>` hand-off cannot carry a message for them.
  `git commit -F` does, and keeps the original author (git reads it from
  `CHERRY_PICK_HEAD`). A following `--continue` then carries on the rest
  of a sequence.
- A rebase's conflict stop leaves no `CHERRY_PICK_HEAD`, so a plain
  `git commit` there records **the user** as author. `rebase --continue`
  applies the stop's `author-script` and does run `GIT_EDITOR`, so the
  message goes through `cp <msg>`.
- `--cleanup=strip` is required. A conflicted merge's `MERGE_MSG` carries
  `# Conflicts:` comment lines, and `commit -F` keeps `#` lines unless told
  otherwise. In a terminal the editor session would have stripped them.

### 2.2 Pull follows git's configuration

Pull drops the hard-coded `--ff-only` and runs `git pull --no-commit`, so
git's own `pull.rebase` / `pull.ff` decide what happens:

- fast-forward possible: git fast-forwards; nothing to compose.
- `pull.rebase=true`: git rebases; `--no-commit` has no effect.
- `pull.ff=only`: git refuses a diverged pull, as it would anywhere.
- diverged, merging: git stops with `MERGE_HEAD`, and the commit buffer
  opens seeded with git's merge message.
- unconfigured and diverged: git's own `Need to specify how to reconcile
  divergent branches` is the report.

`--ff-only` stays available as a transient flag.

## 3. Ownership

The buffer, its seeding and its confirm all belong to `magit-commit-mode`.
No new mode and no new crate. The in-flight question is `lattice-vcs`'s
`InFlightOp::detect`, already shared by the transients and the status
headerline; this adds a third reader, not a third detector.

An operation that ends stopped asks for the buffer through an inbound bus
(`SubsystemBoot::inbound`), not a tick callback, so the buffer opens the
moment git stops, without a keystroke (`boot-composition.md` §3).

The bus is a **service** the trigger looks up and hands its spawner, not a
static captured at install as magit's `EVENT_BUS` is. A `OnceLock` keeps
the first editor's bus, so with two editors in one process — every test
binary — a pull in the second would open its buffer in the first. The
handler's `Effect::OpenSyntheticBuffer` is applied host-side by the
off-keystroke drain (`absorb_off_keystroke_effects`): a peer applies that
effect only after a dispatch, and there is none behind a wake.

## 4. Paramount-goal alignment

- **#4 asynchronicity.** No git process waits on a person. Every git run
  finishes and reports; the human step happens in a buffer, between runs.
- **#3 grammar as API.** The commit buffer is one composing surface with
  one set of chords, whatever git stopped in.
- **#1 performance.** Nothing here is on a hot path: each git run is off
  the actor thread on the blocking pool, as every magit mutation is. No
  bench.

## 5. UX (higher court)

Closing the buffer, crashing, or switching to a terminal mid-message all
leave the repository in git's standard stopped state, which `git status`
and any other tool understand. Nothing holds `index.lock` while the user
types.

## 6. Rejected alternatives

- **A waiting editor (Emacs magit's with-editor).** Point `GIT_EDITOR` at
  a lattice helper that blocks until the buffer is confirmed. It catches
  every editor prompt, including ones this audit missed, which is a real
  advantage. Rejected because git holds `index.lock` and a blocking-pool
  thread for as long as the user types, a crash leaves the lock behind,
  and it needs an IPC path that works from git's `sh` on Windows. That
  trades #4 for generality. The audit test (CC.5) is how this design pays
  for the generality it gives up.
- **Write the message first** (`GIT_EDITOR="cp <msg>"` for every op).
  Works only when the message is known before git runs. A pull's merge
  message is not known until after the fetch, and a conflict stop has no
  message until git writes one.
- **Keep `--ff-only` as pull's default.** A safe default, but it overrides
  the user's own git configuration, and `pull.ff=only` already expresses
  that preference for anyone who wants it.
