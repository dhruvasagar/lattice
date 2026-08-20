# Magit repo scoping — slice plan (MR)

> **Status: ✅ Complete, archived 2026-08-20.** Opened and finished the
> same day, MR.1 … MR.6. Implements
> [`magit-repo-scoping.md`](../../architecture/magit-repo-scoping.md):
> every magit surface acts on the repository containing the active
> buffer's file, not the process's working directory.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Status

| Slice | Title | Status |
|---|---|---|
| MR.1 | The resolver + the naming pair — **landed inside MR.2** | ✅ |
| MR.2 | Per-buffer workdir record + the first trigger (absorbs MR.1) | ✅ |
| MR.3a | The name grammar + the parameterless views | ✅ |
| MR.3b | The views that encode parameters (diff, log, file, …) | ✅ |
| MR.4 | Action bodies read the buffer's repo (`magit_global_mode`, transients) | ✅ |
| MR.5 | The grep guard + docs | ✅ |
| MR.6 | The pickers + the project-diff opener | ✅ |

MR.1→MR.2→MR.3 is the spine. MR.4 is the half that makes it *correct*
rather than merely differently-wrong, so it was not optional polish —
and MR.5's guard is what proved MR.3 and MR.4 had both stopped short.
MR.6 cleared the exceptions that guard was left holding.

**The lesson the series keeps repeating:** every slice here shipped
believing it was complete, and three of them were not. A behavioural
test cannot tell a cwd-bound site from a correct one, because a test
process has one checkout — so the enumeration has to come from a source
grep, and the grep has to match the *question* rather than whichever
spelling the last author used. Four spellings turned up in the end:
`magit_workdir()`, `Repository::discover(".")`,
`std::env::current_dir()`, and a `git` subprocess with no
`current_dir`.

---

## MR.1 — the resolver + the naming pair ✅

- `workdir::repo_for_trigger(…) -> Option<PathBuf>`, implementing design
  §2's three questions in order (magit buffer's own repo → active file's
  repo → cwd).
- `magit_buffer_name(view, workdir)` / `repo_display_from_name(name)` —
  one producer, one parser, per §3.1. Every caller through them; MG.15
  is the precedent for what a producer/parser split costs.
- Basename-collision qualification (`*magit:status:work/api*`).

**Tests.** Each resolution branch in isolation; a file outside any repo
falls through to cwd; two repos sharing a basename produce two distinct
names, and the same repo asked twice produces the same one (idempotent —
re-triggering must find the buffer you already have, not stack a second).

> **Corrected 2026-08-20, from attempting it: MR.1 cannot land alone.**
> Written and green (resolver + `repo_label` + `qualified_repo_label`, 10
> tests), it still could not be committed: with no caller yet, all three
> functions raise `dead_code`, and the standing rule treats a rustc
> warning in touched code as always-real rather than as noise to allow
> through. "No behaviour change, nothing calls it yet" is precisely the
> shape that rule refuses.
>
> So the boundary was wrong, not the code. **MR.1 lands as part of MR.2**,
> with the status trigger as its first caller. The work is preserved as a
> patch — `scratchpad/mr1-resolver.patch` in the session that wrote it —
> and reproducible from the test names above, which are the spec.
>
> Worth carrying forward as a slicing lesson: a slice whose whole content
> is "add a helper" has no warning-clean landing, so it is not a slice.
> Pair it with its first consumer.

## MR.2 — the per-buffer record + the first trigger ✅

Absorbs MR.1 (see the note above). Scope: **status only** — the other
views keep their fixed names and cwd resolution until MR.3, so this slice
stays landable.

- A workdir entry keyed by **buffer name**, written by the trigger and
  read by the view at `on_activate` (the shape
  `magit_diff_mode::ViewArgsRequests` and `magit_blame_mode::BlameRequests`
  already use — the opener leaves values under the buffer's name, the
  mode takes them when it activates). Keyed by name rather than id
  because the buffer does not exist yet when the trigger runs, and
  because `BufferStore::name_for` makes id → name → workdir a lookup
  rather than a second map to keep in sync.
- **Not** one-shot like `ViewArgsRequests`: MR.4's action bodies read it
  for the buffer's whole life. Cleared on `DocumentClosed`.

### The constraint that decides the mechanism

> **Corrected 2026-08-20, from executing it.** The paragraph below
> replaces an earlier one that named the wrong type and drew the wrong
> conclusion from it. What it said: `ExCommandSpec::apply` receives
> `lattice_grammar::ActionContext`, which "carries `buffer_id` and a
> `Buffer`, but no path and no services", so magit should cache the
> buffer-store handle in a service of its own and look the path up from
> there. Both halves were wrong, and the plan built on them could not
> have worked.

**An ex-command could not name the buffer it fired in at all.**
`ExCommandSpec::apply` receives `ExCommandContext` (`registry.rs:368`) —
`bang / args / range / register / count / cancel`. Not `ActionContext`,
and *no buffer id*. So the question was never "how does the ex-command
reach the store"; it was "what would it look up in it". Caching the
store handle answers a question nobody was asking.

The store itself was never the obstacle: `SubsystemBoot::buffer_store()`
hands magit the handle at install time (`BootContext::new` receives it in
Phase A, long before the *service-registry* entry at
`editor_boot.rs:1624`). Capturing it in `register_ex_commands` is the
`blame_requests` pattern, one line.

**Chosen: `ExCommandContext` carries `buffer_id`.** Filled in
`dispatcher::execute_ex_command` from the same parameter the Action arm
beside it already passes on — the fact exists at the call site and was
simply not forwarded. Magit then resolves through one function
(`repo_scope::open_repo_view`) that both surfaces call: `C-x g` from an
action handler (services + buffer id), `:magit-status` from an
ex-command closure (captured store + buffer id).

Anchored on **paramount #3**: the `:` line is a parser front-end onto the
one dispatcher, and a command reached that way was seeing strictly less
than the same command reached by a chord. Vim's ex-commands are
buffer-scoped by definition (`:w`, `:%s`, `:bd`); the absence read as an
asymmetry, not a decision. Cost: one public field on a `lattice-grammar`
type and 16 literal construction sites (14 of them tests; the plugin
boundary only *projects* the context, so the WIT record is unchanged).

Rejected alternatives, with reasons:

- **Let the two surfaces diverge** (`C-x g` resolves from the active
  buffer; `:magit-status` stays cwd-based unless given an argument).
  Rejected: it makes the common case the one that needs an argument, and
  the same command would mean two things depending on how it was reached.
- **Route both through the provider-view seam** (`OpenProviderView`, the
  way `:magit-project-diff` opens). The opener runs host-side with a
  `&mut dyn ModeActivator`, so resolution would happen in one place at
  apply time and `lattice-grammar` would not change. Rejected on
  heuristic #1: `ModeActivator` exposes no active buffer either, so it
  needs a new generic host method regardless — the same
  host-API-for-one-consumer cost this plan already rejected
  `ModeContext::activated_from` for — and status is a synthetic
  `Document`, not a multibuffer, so MR.3 would then face converting 15
  more views to a seam built for a different shape, or leaving magit with
  two opening mechanisms.
- **A host-side `ModeContext::activated_from`** carrying the buffer the
  activation came from (the host already stashes it in
  `prev_pane_for_popup`). Genuinely elegant — no trigger plumbing at all,
  every view resolves at activation, both surfaces work for free. But it
  adds a generic host API for one consumer, makes resolution implicit,
  and is ambiguous for activations that are not triggers (`:b`, `<C-6>`).
  Worth revisiting only if a second subsystem wants the same fact.

### Then

- `:magit-status` resolves → computes the name → records the workdir →
  emits `OpenSyntheticBuffer` with the computed name.
- `magit-status-mode::on_activate` reads the workdir by its buffer name,
  falling back to `magit_workdir()` (a buffer reopened by `:b` after a
  restart has no record).
- The ~11 production sites naming `"*magit:status*"` (2 in
  `lattice-magit/src/lib.rs`, 3 in `magit_global_mode.rs`, 1 each in
  `magit_remote_mode.rs` / `magit_submodule_mode.rs`, 2 in
  `lattice-ui-tui/src/app/magit_bindings.rs`, 2 in
  `lattice-ui-tui/src/render.rs`) compute it instead, plus 4 in
  `lattice-host/tests/synthetic_buffer_survives_command_line.rs`.

  > **There were two, not eleven.** The count was a grep over the whole
  > string, and everything except `lib.rs`'s ex-command registration and
  > `magit_global_mode.rs`'s `C-x g` handler turned out to be a test
  > asserting a *parser* declines the name, a doc comment, or a test
  > opening the buffer by name through `open_synthetic_buffer`. Those
  > last still pass unchanged and still mean what they meant: a buffer
  > named `*magit:status*` with no record falls back to the working
  > directory, which is exactly the `:b`-after-restart path.

**Tests.** The record survives the trigger→activation gap; closing the
buffer drops it; a second trigger for the same repo overwrites rather
than accumulating.

### Landed 2026-08-20

`crates/lattice-magit/src/repo_scope.rs` (new) holds the record, the
`DocumentClosed` index and the shared trigger body; the resolver and the
name producer live in `workdir.rs`; `lattice-grammar` gained
`ExCommandContext::buffer_id`. 16 tests across the two magit modules,
2572 green in the touched crates.

Two things came out different from the plan and are worth carrying into
MR.3:

- **The parser half of the naming pair did not land.** Design §3.1 asks
  for one producer and one parser; in MR.2 nothing reads a repository
  back *out* of a name — the record is keyed by the whole name, and `:ls`
  prints the name verbatim. Written and then deleted rather than
  committed unused, per MR.1's own lesson. It lands in MR.3 or MR.4 with
  whatever first reads it (a headerline, most likely).
- **The trigger body is view-agnostic already.**
  `repo_scope::open_repo_view(view, mode_id, …)` takes the view name, so
  MR.3 is mostly switching the remaining `mk(...)` registrations and
  `open!(...)` contributions over to it — not writing new resolution per
  view.

## MR.3 — views read the record

Each view's `on_activate` takes its workdir from the record instead of
`magit_workdir()`, falling back to the resolver when there is no record
(a buffer opened by `:b` after a restart).

**Tests.** `C-x g` from a file in repo B opens repo B's status while the
editor's cwd is repo A — the acid test for the whole change, and the one
that fails today.

### Carved into MR.3a + MR.3b, 2026-08-20 — the reason is the names

Reading the record is uniform and mechanical (one line per view), but a
view can only *find* its record by name, and a view whose name is fixed
has one buffer for every repository. So the read cannot land without the
rename, and the rename is where the work is.

The rule MR.3 adopts, and the reason it cannot be adopted one view at a
time:

```text
*magit:<view>[:<repo>[:<rest>]]*        segment 2 is the repository, ALWAYS
```

`*magit:log:src*` is unreadable without a fixed position — the `src`
repository, or the log of the path `src`? Both are names magit already
produces. Fixing segment 2 answers it, but only if *every* producer and
parser of a given view moves together: a half-converted view is exactly
the ambiguity, spelled out. That splits the views into two populations,
which are the two slices.

## MR.3a — the name grammar + the views with no parameters ✅

`workdir::{magit_buffer_name, magit_buffer_name_with, parse_magit_name}`
— one composer, one parser, `MagitName { view, repo, rest }` — plus the
views whose names carry no parameters of their own, so adding the
repository segment cannot collide with anything they already encode:
**commit, amend, reword, branch, remote, submodule, refs**, and the
commit family's targeted forms (`*magit:augment:<repo>:<sha>*`,
`merge-edit`, `reword-commit`) which move with it because one parser
reads all six.

Also here, because they are the same edit:

- `repo_scope::view_workdir(ctx, buffer, handle)` — the activation-side
  read every view now shares, including status (MR.2 had inlined it).
  Reading the record and indexing the document for `DocumentClosed`
  cleanup live in one helper, so a new view cannot take one and forget
  the other.
- `CommitIntent::from_buffer_name` stops matching by substring
  (`name.contains("amend")`). With a repository in the name, a checkout
  called `amend` would have selected the wrong operation on every commit
  buffer in it — a structured parse makes that unrepresentable rather
  than unlikely, and the test pins it with a repo named `amend`.
- `magit_refs_mode::REFS_BUFFER` (`"*magit:refs*"`) becomes `REFS_VIEW`
  (`"refs"`). A constant naming one buffer for all repositories is the
  assumption this slice removes.

**Tests.** A table over all eight converted views — a conversion that
does seven of eight looks right from inside whichever repository you are
in, and is invisible until someone works across two. Plus the targeted
commit form carrying repo *and* target, since those fail differently
(wrong checkout vs. squash into nothing).

## MR.3b — the views that encode parameters ✅

**diff, log, stash (list + show), rebase (+ rebase-edit), file, note,
merged, cherry** — each moved whole: producer, parser, tests, and its
call sites. Every view's `on_activate` now reads the record through
`repo_scope::view_workdir`; no magit view resolves from the working
directory any more. What is left calling `magit_workdir()` is exactly
MR.4's list: action bodies and the transients.

The parsers shifted by one segment rather than being rewritten —
`parse_magit_name` hands back `rest`, the exact string each parser
already read.

### Three things this slice found

- **A collision MR.3a had just introduced.** `magit-revision-mode`
  showed a commit as `*magit:commit:<sha>*` while the compose buffer had
  become `*magit:commit:<repo>*` — the same shape, so in a checkout named
  like a sha they were one buffer with whichever mode reached it first.
  The revision view took its own word: `*magit:show:<repo>:<sha>*`. This
  is the ambiguity argument from MR.3a's carve, arriving as an actual
  bug rather than a hypothetical, and it is why the stash list and
  stash-show had to move together too.
- **`magit_workdir()` was returning a RELATIVE path.** `gix` reports the
  workdir relative to where discovery started, so an editor launched in
  `crates/lattice-ui-tui` got `../..` — which has no final component, so
  every buffer was named `*magit:<view>:../..*`, and the collision check
  compared `../..` against absolute paths for the same checkout. Harmless
  while the path only ever reached `git -C`; not harmless once it is
  labelled and compared. `magit_workdir` canonicalises now. Found by a
  TUI test, which is the only place the cwd is not the repo root.
- **In-buffer producers cannot record.** `<CR>` on a commit, a hunk's
  side-by-side split, `p`/`n` through a file's revisions — these build a
  buffer name from inside a helper holding only its own view's state, so
  they have a label but no way to write a record. Rather than thread
  services through every one, `RepoScopes::workdir_for_label` recovers
  the path from whichever sibling buffer IS recorded against that label,
  and `view_workdir` records it on arrival. Sound because labels are
  unique among open magit buffers by construction — that is what the
  collision qualification at the trigger is for.

## MR.4 — action bodies ✅

The `magit_workdir()` sites in `magit_global_mode.rs` and `transients.rs`
read **the buffer's** repo. Per design §4 this is what separates "fixed"
from "worse": a status buffer showing repo B whose `s` stages into repo A
is data-loss-shaped.

**Tests.** Stage / commit / checkout invoked in repo B's buffer touch
repo B, with the cwd pointed at repo A throughout.

### Landed 2026-08-20

`repo_scope::active_workdir` is the one answer to "which repository does
this act on", with `action_workdir(ctx)` for handlers and
`workdir_or_cwd(store, scopes, buffer)` for ex-commands. Its three
questions are design §2's, with the first one *read* from the record
rather than re-resolved — re-resolving is exactly the bug.

Every operation helper (`spawn_git`, `spawn_git_sequence`,
`spawn_computed`, `spawn_remote_op`, `spawn_remote_op_to`,
`spawn_subtree_op`, `spawn_note_merge`, `spawn_note_prune`,
`spawn_note_remove`, `spawn_rebase_verb`, `spawn_gitignore`,
`current_branch`, `git_config::set`) now **takes** its workdir instead of
finding one. That is the part with teeth: a wrong-repo push is not
recoverable the way a wrong-repo buffer is, and a helper that can still
ask the process where it is will eventually be called from somewhere that
should not have.

Also here:

- **The dispatch menus' ROWS.** `DispatchGates::probe()` became
  `probe_in(workdir)`, and `TransientContext` gained `buffer` — the
  buffer the menu was opened over. Without it a menu built over repo B
  offered the way out of repo A's stopped rebase, and hid it when repo B
  was the one stopped. The seven cwd-based gate readers in
  `transients.rs` are deleted rather than rewired: with the workdir
  passed in there is nowhere left for a row to read the process's
  repository from.
- **`active_target`'s argument branch** — the picker-routed `C-c f` rows,
  where the path arrives as an argument. The no-argument branch already
  resolved from the file; these two disagreed.
- **`spawn_gitignore`** wrote `.gitignore` into `Repository::discover(".")`
  — a cwd site the `magit_workdir()` sweep never saw, because it spelled
  the discovery out rather than calling the helper. Worth remembering
  when MR.5's grep guard is written: the guard must look for the
  *discovery*, not only for the helper's name.

### Not converted, and why

Three `magit_workdir()` callers remain outside the resolver, none of them
an action body:

- `git_config::refresh` — a process-wide config cache, keyed by nothing.
  Making it per-repository is a cache-shape change, not a scoping one.
- `providers::project_diff`'s opener — reached through
  `ProviderViewOpener`, whose `ModeActivator` exposes no active buffer.
  Same gap `TransientContext` had; the fix is the same shape and belongs
  with whoever needs it next.
- `picker_sources`' revision preview — a preview buffer, built where no
  buffer id is in scope.

MR.5's grep guard states them as exceptions with the reason attached;
MR.6 then removed all three, and the exception list with them.

## MR.5 — the guard + docs ✅

- Grep guard: no `magit_workdir()` outside the resolver (design §4's
  anti-rot rule), same shape as `gr_is_declared_once.rs`.
- User docs: `magit.md` on which repository a magit buffer acts on and
  what `:ls` now shows; the per-view pages where they name the buffer.
- `sync-docs.sh` + `zola build`.

### The guard greps the QUESTION, not the helper

`crates/lattice-magit/tests/one_repo_resolver.rs`. It matches
`magit_workdir()`, `discover(".")` **and** `std::env::current_dir()`,
because the first spelling alone would have certified a magit that was
still substantially cwd-bound: MR.3 and MR.4 both built their work lists
by grepping the helper's name, and both lists were short.

What it found on first run, all of it real:

- **`magit-rebase-mode`'s `on_activate`** — an entire view, resolving
  from the process's repository, through MR.3, the slice whose whole
  subject was views. It spelled `Repository::discover(".")` out.
- **`:magit-branch-create`** and **five branch-family finish handlers**
  (create-from, checkout-at, create-no-checkout, rename, delete) — every
  one creating or deleting branches in the process's repository.
- **`spawn_gitignore`**, found during MR.4 by the same reasoning before
  the guard existed.

Eight production sites, none of which any behavioural test could catch:
a test process has one checkout, so a cwd-bound site and a correct one
are indistinguishable inside it. That is the argument for the test being
a source grep, and it is written into the file's header.

Exceptions are listed one per entry with a reason, and a second test
asserts each reason is more than a token — an exception without an
argument is a suppression wearing the guard's clothes. Two are genuine
(`:magit-init` and `:magit-clone` seed prompts from the working
directory, because there is no repository yet); three are MR.6's debt,
recorded rather than hidden.

## MR.6 — the pickers + the project-diff opener ✅

The three the guard excepted by name, now none of them:

- **The pickers.** `RepoLens` (store + scopes, taken at registration)
  answers "which repository is this picker about" from
  `PickerContext::active_buffer`, resolved at `init` before the listing
  goes off-thread — a task that asked afterwards would be asking about
  whatever buffer is active by the time it runs. All nine sources plus
  the revision preview.
- **The project-diff opener.** `ModeActivator` gained `active_buffer()`,
  the peer of `TransientContext::buffer` and
  `ExCommandContext::buffer_id` and generic for the same reason: a
  provider view is opened *over* something, and what it shows usually
  depends on which. Default `None`, so test activators stay valid.
- **The git-config cache**, now keyed by repository. It held one map for
  the process, which was invisible while every magit surface acted on one
  repository and wrong the moment they stopped: the `Configure` rows in a
  menu opened over repo B reported repo A's `pull.rebase`, and `C` then
  *wrote* what the row had read — the read and the write disagreeing
  about which repository they were about. `DispatchGates` carries the
  workdir now, so both kinds of question a menu asks ("is a rebase
  stopped", "what is `pull.rebase`") are answered about one repository.

### A fourth spelling, and a guard that could not fail

The commit picker ran `Command::new("git").args(["log", …]).output()`
with no `current_dir` — inheriting the editor's directory, the same bug
as `discover(".")` in a shape neither the `magit_workdir` sweep nor the
discovery grep could see. The guard grew a third test for it: every git
subprocess must say where it runs.

That test then **passed on the broken code**. Its proximity check asked
whether `current_dir` appeared in the builder chain, and the comment
explaining the rule contains the words `current_dir` — so the fix hid
itself. Found by mutating the fix back out and watching the guard stay
green; the chain strips comments now, and the mutation was re-run to
confirm it fails and then passes. Worth writing down: a guard nobody has
watched fail is a guard nobody has tested.

---

## Cross-references

- [`../../architecture/magit-repo-scoping.md`](../../architecture/magit-repo-scoping.md) — design (what + why)
- [`magit.md`](magit.md) — the subsystem's own plan
