# MG.53 — every selection is a picker

**Status:** 🚧 in progress — MG.53.a ✅ landed. Parent:
[`magit-transient-completeness.md`](magit-transient-completeness.md).
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

**Still open, with reasons:**

- reverse-blame revision — reached through the generic `op.what /
  op.usage()` prompt macro shared with non-revision operations.
  Converting it means splitting that macro, which is a bigger edit than
  the rest of this slice put together.
- bisect known-good — a TWO-step chain (bad, then good) where the first
  value is carried in the prompt buffer's name. A picker chain needs
  the bad rev to survive into the second picker's args; `picked_line`'s
  placeholder makes that expressible (`magit-bisect-start {bad} {}`)
  but the first step has to become a picker too, and bisect's good rev
  is characteristically OLDER than the 200-commit window — the case
  where the ex-command escape is the common path, not the fallback.

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

### MG.53.e — ref and file selections 🚧 ref done, file open

**Ref: landed.** `Merge notes ref` uses `RefPickSource::AllRefs` — and
the "does one source subsume both" question answered itself, since
MG.53.d built exactly that. `core.notesRef` is a config row and moves
to MG.53.f with its peers.

`File (repo-relative):` wants a file picker. **Check first whether the
host's existing `:files` picker is reusable** — a magit-local copy of
"list the repo's files" would be a second implementation of something
the editor already does, and the fuzzy-finder is shared
infrastructure.

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

- [`magit-transient-completeness.md`](magit-transient-completeness.md) — parent (MG.41)
- [`magit-transient-enablers.md`](magit-transient-enablers.md) — MG.42, the same "build the shared capability once" shape
- [`../../architecture/magit.md`](../../architecture/magit.md) — design fragment
- [`../implementation.md`](../implementation.md) — per-slice status ledger
