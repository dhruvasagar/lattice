# Magit concluding commits — slice plan

> **Status: Active.** Opened 2026-10-25. Implements
> [`magit-concluding-commits.md`](../../architecture/magit-concluding-commits.md):
> every operation git would stop to ask a message for is concluded in the
> commit buffer.

Design owns *what* and *why*; this file owns *when* and *in what order*.

## Status

| Slice | Title | Status |
|---|---|---|
| CC.1 | The commit buffer concludes what git stopped | ✅ |
| CC.2 | Pull follows git config; a stopped merge opens the buffer | ✅ |
| CC.3 | The `--continue` rows compose when a commit is pending | 📝 |
| CC.4 | Revert `V V` composes | 📝 |
| CC.5 | Audit pin: every message-writing op is accounted for | 📝 |

## CC.1 — The commit buffer concludes what git stopped

`magit-commit-mode`'s `Create` intent, on activation, asks
`InFlightOp::detect`. In flight with a pending commit:

- seed the message area from `.git/MERGE_MSG` (a rebase conflict stop
  writes one too; an `edit` stop does not, and is not pending);
- confirm with `git commit --cleanup=strip -F <msg>`, then `<op>
  --continue` while `.git/sequencer/` remains; rebase with `rebase
  --continue` and `GIT_EDITOR="cp <msg>"`;
- the headerline says what is being concluded.

Landed as `lattice_vcs::PendingCommit` (which commit git is waiting on, and
its message — beside `InFlightOp`) and `lattice-magit`'s `conclude` (the two
confirm paths, beside rebase `w`'s `cp <msg>` hand-off, whose Windows-safe
quoting it reuses). Fixes `c c` opening empty mid-merge on its own, before
any trigger changes.

Tests: seeded from `MERGE_MSG`, `# Conflicts:` stripped from the commit,
a merge concludes with two parents, a two-commit cherry-pick concludes the
stopped one with its original author and carries on, a rebase concludes
with the original author, abort leaves the state in place, and nothing in
flight still commits as `Create` did.

## CC.2 — Pull follows git config; a stopped merge opens the buffer

`RemoteOp::PULL` drops `--ff-only` for `--no-commit`; `--ff-only` becomes
a flag. After the run, still `Merge` in flight with no unmerged paths ⇒
send a compose request through an inbound bus, whose handler opens the
repository's commit buffer. Conflicted ⇒ the existing stopped report.

Tests: a diverged pull opens the buffer **without a keystroke** (assert on
the wake, as `async_landed` tests do); fast-forward opens nothing;
`pull.ff=only` and `pull.rebase=true` behave as git does; the flag
restores fast-forward-only.

✅ Landed as two commits. The first is a host fix CC.2 surfaced:
`Effect::OpenSyntheticBuffer` from an off-keystroke drain (tick callback,
inbound handler, idle gate) was routed to the renderer tail and dropped,
because a peer drains that tail only after a dispatch. The host now applies
it in `absorb_off_keystroke_effects`, beside WK.10's popup pair
(`lattice-host/tests/off_keystroke_opens_synthetic_buffer.rs`, red without
the fix). The second is CC.2 itself. The bus is a `ComposeBusHandle`
service, not a static: a `OnceLock` keeps the first editor's bus, which
in a test binary is the wrong editor. Tests:
`a_diverged_pull_opens_the_commit_buffer_on_gits_merge_message` (waits via
`settle_woken`, which ticks only on `async_landed`),
`pull_does_what_the_config_says` (merge / rebase / ff-only / fast-forward
against real clones, each asserting whether a compose would be asked
for), and `compose_waits_for_a_resolved_stop`. "Fast-forward opens
nothing" is pinned at that gate rather than end to end: the only
end-to-end signal that the pull finished is magit's report, which goes
through a process-wide sink and in a test binary can land in another
test's editor.

## CC.3 — The `--continue` rows compose when a commit is pending

Merge, cherry-pick, revert and rebase `--continue`: when git would commit
(a `*_HEAD` marker; a rebase stop with staged changes), open the commit
buffer through the CC.2 bus instead of running `--continue` with
`GIT_EDITOR=true`. Otherwise run as now (a sequence with nothing pending,
an `edit` stop with nothing staged).

## CC.4 — Revert `V V` composes

`revert --no-commit <commit>`, then the commit buffer through the CC.2
bus, seeded with git's `Revert "…"` message. `V v` (no commit) unchanged.

## CC.5 — Audit pin

A test over every `RemoteOp` / `RevOp` argv and every `spawn_git` caller
that can create a commit: it passes its message, stops and composes, or is
listed with a reason (`m m`'s `--no-edit`, fixup / squash, instant fixup).
Includes verifying `subtree pull` / `subtree merge`, which the 2026-10-25
audit did not reach. This is what the design gives up generality for
(design §6), so it ships with the work, not after it.
