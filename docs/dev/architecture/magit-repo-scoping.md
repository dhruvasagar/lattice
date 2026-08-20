# Magit repo scoping — the repository is the active buffer's, not the shell's

Design for making every magit surface act on **the repository containing
the file you are looking at**, rather than the repository containing the
process's working directory.

Companion to [`magit.md`](magit.md) (the subsystem). Sequencing:
[`../operations/slice-plans/magit-repo-scoping.md`](../operations/slice-plans/magit-repo-scoping.md).

## 1. The gap

`magit_workdir()` is `Repository::discover(".")` — the process cwd. Every
magit view calls it from its own `on_activate`, so every magit buffer
describes whatever repository the editor was *started* in.

The workflow this blocks is ordinary: open a file from another checkout
(`:e ~/work/api/handler.go`), fix something, and stage it. Today `C-x g`
shows the *first* repository's status, and the staging chords act there
too — so the only way to work on the second repo is to quit and restart
the editor somewhere else. An editor that can open a file from anywhere
should be able to commit it from anywhere.

**It cannot be fixed where it currently lives.** By the time a view's
`on_activate` runs, the active buffer *is* the magit buffer — the file
you pressed `C-x g` from is no longer active. Resolution has to happen at
the **trigger** and be carried into the view. That is the whole of the
work; swapping `magit_workdir()` for `workdir_for_file()` is the easy
part and, done alone, would be a no-op.

## 2. Resolution: three questions in order

A trigger resolves its repository by asking, in this order:

1. **Is the active buffer a magit buffer?** Then use *that buffer's*
   repository. Without this rule, `C-x g` inside repo B's log buffer
   would jump you back to the cwd repo — a magit chord pressed inside
   magit would silently change which repository you are working on.
2. **Does the active buffer have a file?** Then discover from that file's
   parent (`workdir::workdir_for_file`, which exists and takes a
   *directory* precisely because `gix::discover` fails silently on a file
   path — see `workdir.rs`'s module note).
3. **Otherwise, the working directory** (`magit_workdir()`, today's
   behaviour). A fresh editor with nothing open still answers `C-x g`,
   which is what keeps this a widening rather than a trade.

`None` from all three means "not in a repository", and the view says so
exactly as it does today.

## 3. Identity: the repository is part of the buffer

Magit buffers are named for what they show (`*magit:status*`), and that
name says nothing about *which repository* — so one buffer had to mean
whichever repo was last asked for.

Per **repository**, then: `*magit:status:lattice*` and
`*magit:status:api*` are different buffers and coexist. `:ls` tells them
apart, `:b` reaches either, and `<C-6>` returns to the one you left. This
is Emacs magit's model (a status buffer per repository) and fugitive's,
and it is the only shape in which "act on another repo without leaving
this one" is expressible at all.

### 3.1 The name is for humans; the workdir is carried beside it

The name carries the repository's **basename** because that is what a
user recognises in `:ls`. A basename cannot round-trip to a path, and two
checkouts can share one (`~/work/api` and `~/oss/api`), so the name is
**not** the source of truth for where git runs.

- **Display** — `*magit:<view>:<basename>*`, produced and parsed by one
  pair of functions, the shape `magit-file-revision-mode` already uses
  for `blob_buffer_name` / `parse_buffer_name`. One producer, one parser,
  every caller through them: MG.15 lost every stash chord to a
  producer/parser split, and this is the same trap with more callers.
- **Truth** — a per-buffer entry recording the resolved workdir, written
  by the trigger and read by the view at activation. The same
  side-channel shape MG.26b uses for blame requests, and for the same
  reason: `on_activate` cannot see what the trigger saw.

**Basename collisions get qualified, not merged.** If the name is already
taken by a buffer whose recorded workdir is a *different* path, the new
buffer qualifies its name with the parent directory
(`*magit:status:work/api*`). Merging them would be the worst outcome
available — two repositories sharing one buffer, with the staging chords
acting on whichever was recorded last.

## 4. Consistency is the requirement, not the entry points

The 41 `magit_workdir()` call sites split into two populations, and
fixing only the first is worse than fixing neither:

- **Entry points** (one per view's `on_activate`, plus the transients):
  these resolve per §2 and record the answer.
- **Action bodies** (14 in `magit_global_mode.rs` alone — stage, commit,
  checkout, the file-dispatch rows): these must read **the buffer's
  recorded workdir**, never re-resolve. A status buffer showing repo B
  whose `s` stages into repo A is a data-loss-shaped bug, and it is what
  a half-migration here produces.

The rule that keeps this from rotting: **after this change, no magit code
outside the resolver calls `magit_workdir()`.** A grep guard enforces it,
the same way `gr_is_declared_once.rs` guards its rule.

## 5. Paramount-goal alignment

- **#1 Performance.** Resolution is one `discover` walk per trigger
  (already paid today, just from a different starting point). Nothing new
  on the actor thread; the git work was and stays off-thread.
- **#3 Everything-is-a-buffer.** The repository becomes part of buffer
  identity, which is what `:ls` / `:b` / `<C-6>` already assume of every
  other buffer. No kind-branching: the name and the recorded workdir are
  per-buffer *properties*.
- **Mode ownership.** Resolver, naming pair and the per-buffer record all
  live in `lattice-magit`. The host gains nothing.

## 6. UX (higher court)

The chord you press does what the buffer in front of you implies. Nothing
moves for a single-repo user: one repo means one status buffer, named for
it, behaving exactly as before — the only visible change is that `:ls`
now says which repository it is, which it should always have said.

## 7. Rejected alternatives

- **One `*magit:status*`, repointed on open.** Smaller (the fixed name
  survives) and the refresh-on-open hook would re-scan it. Rejected: the
  buffer's meaning would change without its name changing, `:ls` could
  not tell you which repo you were looking at, and two repos could never
  be open at once — which is the entire request.
- **Full path in the buffer name.** Unambiguous and round-trippable, so
  no side-channel needed. Rejected on the surface it is read from: `:ls`
  and the tabline would show `*magit:status:/Users/…/src/lattice*` for
  every magit buffer. Names are read far more often than they are parsed.
- **A "current repository" global, set on file open.** Rejected: it makes
  the answer depend on history rather than on what is in front of you,
  and two panes showing two repos would disagree with it.
- **Resolving from cwd but letting `:magit-status <path>` override.**
  Rejected as the *primary* mechanism — it makes the common case
  (working across two checkouts) the one that needs an argument. Worth
  having later as an explicit form.
