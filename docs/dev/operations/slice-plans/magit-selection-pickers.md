# MG.53 — every selection is a picker

**Status:** 🚧 MG.53.a/b/d/g/h ✅; c — three of four ✅, bisect known-good
⛔ deferred (2026-08-19, the list would not contain the thing being
named); e — ref ✅, file 🚧 building as (b), the generic host accept
(called 2026-08-19); f ⛔ deferred to `:customize`.

**This plan does not archive when c and e close.** MG.53.f is deferred,
not done, and a deferred slice is open work — archiving here is what
buries it. Parent:
[`magit-transient-completeness.md`](archive/magit-transient-completeness.md).
Design fragment: [`../../architecture/magit.md`](../../architecture/magit.md).

MG.52 turned branch checkout, merge and reset from free-text prompts
into a picker. This plan finishes the sweep: every magit operation that
asks the user to **name something that already exists** offers a list.

---

## The rule this plan applies

> Naming a thing that must already exist → **picker**.
> Naming a thing you are creating → **prompt**.

Both halves are load-bearing. A prompt for an existing name is a typo
waiting to happen: git reports it long after the keystroke that caused
it, and the thing the user wanted was on a list the editor could have
shown. A picker for a *new* name is worse than useless — there is
nothing to pick.

The distinction cuts through cases that look alike. `Rename {remote}
to:` stays a prompt even though a remote is involved, because the
remote was already picked and the input is the new name.

## What the audit found

25 prompt sites. **16 name an existing thing**; the rest are genuinely
free text and stay as they are:

> tag NAME, remote NAME, URLs (remote / submodule / clone), clone
> destination, `Path for {url}`, rename targets, ignore pattern, new
> branch name, stash message, author, context lines.

The 16 group by **what they select**, and that grouping is the slice
boundary, because a group shares one picker source:

| Group | Sites | Source |
|---|---|---|
| Branch | 7 | `BranchPickSource` — exists (MG.52) |
| Revision | 4 | `CommitPickSource` — exists (MG.23j) |
| Tag | 1 | new |
| Remote | 2 | new |
| Ref | 2 | new |
| File | 1 | reuse the host's file picker if it fits |
| Config enum | 3 | see MG.53.f |

## The constraint every slice inherits

A picked candidate reaches an operation **only** through
`RoutingPayload::InvokeCommand`, whose host arm runs `id` as an ex
line. So the target of every picker-backed row has to be a registered
**ex-command**, and the picked value travels inside that line.

That is why MG.52 registered `magit-merge` and `magit-branch-reset`
rather than pointing the picker at the existing `-finish` actions: the
actions take their input from `ctx.prompt_value`, which a picker never
sets. Each slice below therefore carries an ex-command per operation,
and gets the scriptable form for free.

---

## Slices

### MG.53.a — the four single-`spawn_git` branch operations ✅

`merge-no-commit`, `merge-squash`, `rebase-onto-elsewhere`,
`rebase-autosquash`.

**Four, not six.** The audit grouped these with `merge-edit` and
`merge-into` because all six take a branch. Checking what each finish
handler actually *does* separates them: these four are one
`spawn_git($argv(branch), $what)` — the `prompted_op!` macro's whole
body — while `merge-edit` opens a synthetic commit buffer and
`merge-into` runs a multi-step sequence that needs the current branch.
Those two move to MG.53.b with `merge-absorb`, which they resemble far
more than they resemble these.

Pure repetition of MG.52: one ex-command each wrapping the existing
`*_argv` builder, then re-point the row at `BranchPickSource` with that
command as its arg. No new picker source, no new mechanism.

Add each to `PICKED_BRANCH_OPS` so
`each_branch_row_opens_the_picker_with_a_registered_command` covers it
— that guard asserts both failure modes (a row that quietly reverts to
a prompt, and one naming a command nobody registered).

**Depends on:** MG.52 (landed).

**Landed** (522 magit tests). The four ex-commands joined
`magit-merge` / `magit-branch-reset` in one table — they differ only in
the argv they build, which is what a table column is for — and a
`picked_op!` macro replaced `prompted_op!` at the four call sites. The
macro drops the finish half entirely: the ex-command IS the finish
half, so each operation's git arguments still live in exactly one
place. Guard verified non-vacuous by mis-naming one ex-command.

### MG.53.b — the branch operations that are not one git call ✅

`merge-absorb`, `merge-edit`, `merge-into`.

Separated from MG.53.a deliberately, and the boundary is *what the
finish handler does*, not what it takes:

- `merge-absorb` — merge then delete the branch (a `GitStep` sequence)
- `merge-edit` — opens a synthetic commit buffer, no git spawn at all
- `merge-into` — needs the current branch, and declines on detached HEAD

Each needs an ex-command of a different shape. Folding them into a
slice of four identical ones is how an odd case gets a half-version.

**Landed.** Three blocks, not three table rows — a column cannot
express "and also decline on detached HEAD". `PICKED_BRANCH_OPS` now
covers all ten branch rows.

### MG.53.c — the four revision selections 🚧 two landed, two open

`Bisect … known GOOD revision`, `Checkout {path} from revision`,
`Show {path} at revision`, reverse-blame revision.

`CommitPickSource` already takes an ex-command argument, so this is the
same re-point as MG.53.a against a different source.

**Resolved.** `COMMIT_PICK_LIMIT` is 200, and the source's own doc
already settles the policy: *"anything older is reachable by typing the
sha into the ex-command directly."* Picker for the recent window,
ex-command for the rest.

**Landed:** `Show {path} at revision` and `Checkout {path} from
revision` → `CommitPickSource`. The second reuses the SAME
confirm/execute pair the chord path used rather than spawning git
directly — checking out a file discards its uncommitted changes, and
skipping that guard because the caller happened to be a picker would
make safety depend on how the operation was reached.

`Show {path} at revision` needed no new ex-command at all. It needed
no new ex-command: `picked_line` now honours a `{}` placeholder, so the
pick lands in `magit-find-file {} <path>` rather than on the end. The
alternative was an order-adapter ex-command duplicating an operation
that already exists purely to move an argument.

The picker's first row is HEAD (it is `git log`), so the prompt's
`HEAD` default survives as "press Enter" — and now shows the subject.

### MG.53.g — a revision is not only a commit ✅

`Show / checkout {path} at revision` first shipped against
`CommitPickSource`, which is `git log -n200` — the current branch's
history. That cannot reach *view this file as it is on `origin/main`*:
a file living on another branch is not in this branch's history at all,
so no number of commits would surface it. Emacs's `magit-find-file`
completes over branches, tags and commits together.

`RefScope::Revisions` lists refs then recent commits, registered as
`magit-revision`. Refs first — `origin/main` is what someone reaching
for another branch recognises, where a sha is something they would have
to read to identify.

Display and value split here for the first time: a commit row reads
`abbrev subject` while git receives the full sha, because an
abbreviation is ambiguous in principle and git resolves the ambiguity
by refusing.

`CommitPickSource` stays a separate source. Cherry-pick, revert and
reset genuinely want a commit; offering them a branch would be offering
the wrong noun.

**Still open, with reasons:**

- reverse-blame revision — reached through the generic `op.what /
  op.usage()` prompt macro shared with non-revision operations.
  Converting it means splitting that macro, which is a bigger edit than
  the rest of this slice put together.
- bisect known-good — ⛔ **deferred (2026-08-19), on the reason that was
  already written here.** A TWO-step chain (bad, then good) where the
  first value is carried in the prompt buffer's name. A picker chain
  needs the bad rev to survive into the second picker's args;
  `picked_line`'s placeholder makes that expressible
  (`magit-bisect-start {bad} {}`) but the first step has to become a
  picker too, and bisect's good rev is characteristically OLDER than the
  200-commit window. That inverts the rule this plan applies: the list
  would usually *not contain* the thing being named, so the picker is
  the escape and the ex-command is the common path. Building it would
  ship a picker that is wrong about its own premise.

### MG.53.d — tag and remote pickers ✅

**Landed as ONE source, not two.** `Reference::list` already returns
branches, remote-tracking refs and tags in a single `for-each-ref`
walk tagged with `RefKind`, so a tag picker is that walk filtered.
`RefPickSource` takes a `RefScope` (`Tags` / `Remotes` / `AllRefs`) and
registers under three ids — writing a separate `git tag` call beside
`for-each-ref` would be a second way to ask the same question.

`Remotes` reads `git remote`, deliberately NOT `refs/remotes/*`: the
operations that take a remote want `origin`, not `origin/main`.

- tags → `Delete tag`
- remotes → `Prune tags gone from remote`
- (`remote.pushDefault` config row stays with MG.53.f)

Note `tag_prune_argv` takes a **remote**, not a tag — the row's label
(`Prune tags gone from remote:`) reads like a tag operation and is not
one. Verified against the argv builder, not the label.

Extend `magit_registers_exactly_the_sources_its_rows_open` in the same
edit; that test exists precisely to force this.

### MG.53.e — ref and file selections 🚧 ref ✅, file ⛔ deferred (host decision)

**Ref: landed.** `Merge notes ref` uses `RefPickSource::AllRefs` — and
the "does one source subsume both" question answered itself, since
MG.53.d built exactly that. `core.notesRef` is a config row and moves
to MG.53.f with its peers.

`File (repo-relative):` wants a file picker. **Checked: the host's
`:files` picker is half-reusable, and the half that is missing is the
half that matters.** `PickerSource::Files` (the listing) is independent
and could be reused as-is; `PickerAction::OpenFile` (the accept) is
not — it hands the path to `do_edit`, i.e. it OPENS the file, where
magit needs the path to become the transient's `file` argument.

So this is not a magit-local edit. Two routes, and the choice is a host
decision rather than a magit one:

- **(a)** a magit source pairing `PickerSource::Files` with an
  `InvokeCommand` accept — smallest, but a second place that knows how
  to list a repo's files if the listing is copied rather than reused.
- **(b)** a generic "pick a file, run a command" accept in the host,
  which is what `CommitPickSource` / `BranchPickSource` /
  `RefPickSource` all are, one layer down. Every future provider
  wanting "choose a file, then act" gets it.

(b) is the better long-term fit and the reason this slice stops here
rather than taking (a) for expedience.

**Called 2026-08-19: (b).** The deciding argument is paramount goal #2,
not size — a registered "pick a file, then run this ex-command" source
is reachable by every future provider, including WASM ones, through the
same `PickerSourceSpec` surface `magit-branch` / `magit-commit` /
`magit-ref` already use. (a) would put a second consumer of the repo
file listing inside a feature crate and buy nothing beyond a smaller
diff, which heuristic #1 explicitly refuses as a tiebreaker.

The lift is smaller than the deferral implies: `takes_ex_command`
(`lattice-magit/src/picker_sources.rs`) is the shape, and the host
already renders `RoutingPayload::InvokeCommand { id, args }` as an ex
line. What moves to the host is the *listing paired with that accept*,
so magit registers a row rather than a file walk. Tracked as MG.53.e.

### MG.53.h — the row this sweep deleted ✅

Found by the doc audit, not by a test: **`b` and `l` had become the
same row.**

The branch submenu has both because `l` lists local branches and `b`
takes anything `git checkout` accepts — a tag, `origin/main`, a SHA.
MG.52 converted every free-text branch prompt to a picker and swept
`b` up with them, pointing it at `magit-branch` (i.e. `git branch`).
Nothing failed, because `git checkout <local branch>` is a perfectly
good command. The loss was visible only as *two menu rows that do the
same thing*, and as "check out `origin/main` from the menu" quietly
becoming impossible.

MG.53.g had already built the fix without knowing it:
`RefScope::Revisions` is refs + recent commits, which is "anything git
can take" minus the typo. `b` re-points at `magit-revision`; `l` stays
on the local-branch source.

**What the guard has to assert.** No assertion about `b` alone can see
this: `b` opened a picker, that picker listed real branches, and the
ex-command it named was registered — every existing check passed.
`the_branch_revision_row_is_not_the_local_branch_row` asserts the
**pair**, including `assert_ne!` on the two sources, because the defect
is a relationship rather than a property of either row.

The same slice fixed four specs that declared `no_args` while their
`init` required the ex-command (`magit-branch`, `magit-tag`,
`magit-remote`, `magit-ref` — `magit-commit` was already honest).
`args_schema` drives `:picker <id> <Tab>`, so the mismatch advertised
"nothing to type here" and then refused to open. `takes_ex_command`
gives all five one spec, and
`a_source_that_needs_an_ex_command_declares_it` asks every registered
source to `init` with no arguments and requires the refusals to be
exactly the declarations — a behavioural check, so a new source cannot
reintroduce it by copying a stale literal.

### MG.53.f — config enums ⛔ deferred

`pull.rebase`, `fetch.prune`, `tag.gpgSign` are booleans / small enums
typed into a prompt.

Deferred rather than dropped, and not to a magit slice: these values are
already described by the typed-option system
(`docs/dev/architecture/design.md` §5.12 — every option is a typed
registered value, and `:set foo=<Tab>` enumerates). A magit-local
picker over `true` / `false` would hard-code knowledge the option
registry already has. The generic win is "a config row offers its
type's values", which belongs with `:customize`, not here.

---

## Sequencing

```
MG.53.a ──► MG.53.b        (absorb reuses a's ex-command shape)
   │
   └──────► MG.53.c        (independent; different source)

MG.53.d ──► MG.53.e        (ref picker may subsume the tag one)

MG.53.g ──► MG.53.h        (h re-points `b` at the source g built)

MG.53.f                    deferred — belongs with :customize
```

MG.53.a first: it is the largest group, needs no new mechanism, and its
guard already exists.

## What this plan does NOT change

- **Prompts that name new things.** Listed above; they are correct.
- **`:magit-checkout <rev>` and its peers.** MG.52 narrowed the CHORD
  to branches, not the capability — the ex-commands still take any
  revision, and every slice here keeps that split.
- **The picker UI.** Filtering, MRU and preview are
  `lattice-picker`'s; nothing here touches them.

## Cross-references

- [`magit-transient-completeness.md`](archive/magit-transient-completeness.md) — parent (MG.41)
- [`magit-transient-enablers.md`](archive/magit-transient-enablers.md) — MG.42, the same "build the shared capability once" shape
- [`../../architecture/magit.md`](../../architecture/magit.md) — design fragment
- [`../implementation.md`](../implementation.md) — per-slice status ledger
