# MG.43 — closing MG.41 / MG.42

> **ARCHIVED 2026-08-15.** MG.43a–MG.43h complete. Verified against source, not
> status icons, before filing. The design fragment (if any) stays in
> `docs/dev/architecture/` — only the slice plan moved.

**Status:** ✅ complete (2026-08-05), except the v1 exclusions below. Closes the open items in
[`magit-transient-completeness.md`](archive/magit-transient-completeness.md)
(MG.41d / MG.41e / MG.41f) and the deferred list in
[`magit-transient-enablers.md`](archive/magit-transient-enablers.md) (MG.42).
Design fragment: [`../../../architecture/magit.md`](../../../architecture/magit.md).

MG.41 built the menu structure and MG.42 built the shared enablers.
What is left is the tail: ~30 rows across 11 menus, plus the two items
each plan recorded as blocked.

The inventory below is verified against `transients.rs`'s row tables,
not against either plan's status prose — MG.41's "still open" lists had
already drifted (several rows it names as missing landed in MG.42).

| Menu | Missing vs magit |
|---|---|
| Commit `c` | `e` extend |
| Reset `O` | `w` worktree |
| Branch `b` | `x` reset, `s` spin-off, `S` spin-out |
| Merge `m` | `p` preview, `i` dissolve |
| Tag `t` | `r` release, `p` prune |
| Rebase `r` | `p`/`u`/`e` onto, `s` subset, `m` edit, `w` reword, `k` remove, `f` autosquash |
| Cherry-pick `A` | `a` apply, `h` harvest, `m` squash, `d` donate, `n` spinout, `s` spinoff |
| Revert `_` | `v` changes |
| Fetch `f` | `m` submodules |
| Dispatch `d`/`l` | argument transients (MG.41f) |
| **All menus** | `C` configure |

---

## MG.41f was never blocked

MG.41 recorded diff/log argument menus as ⛔ "needs the diff/log actions
to accept arguments", inferring an operation change.

**The mechanism was already there.** MG.17a projects a transient's
toggled state onto the action's declared `args_schema` positionally —
that is what makes one handler body serve both the `:` line and a
transient. `magit_diff_mode` separately already has `refresh_with_args`
writing `extra_args` onto its mode state.

The actual gap is narrow: the *open* actions declare an empty
`args_schema`, so the projection has nothing to land in. That is a
schema plus a hand-off, not a new capability.

The blocked note was right about the symptom it caught — the flags
would have rendered and been discarded — and
`every_root_dispatch_item_resolves_to_a_real_action_not_a_flag_fallback`
was right to reject the first attempt. It was wrong about the cause.

---

## Slices

| Slice | Status | What |
|---|---|---|
| MG.43a | ✅ | Single-argv rows: commit `e`, revert `v`, cherry-pick `a`, branch `x` |
| MG.43b | ✅ | Rebase's onto-a-target rows (`p`/`u`/`e`/`s`/`f`) |
| MG.43c | ✅ | Rebase todo rows (`m`/`w`/`k`) |
| MG.43d | ✅ | Cherry-pick `h`/`d`/`n`/`s`, branch `s`/`S` |
| MG.43e | ✅ | Merge `p`/`i`, tag `r`/`p` |
| MG.43f | ✅ | Reset `w`, fetch `m` (stash `w` dropped from v1) |
| MG.43h | ✅ | MG.41f — diff / log argument transients |
| MG.43g | ✅ | `C` configure — variable rows with async prefetch |

Each lands green on its own; the row-heavy slices are table edits plus
one op each, because MG.41a and MG.42 already paid for the machinery.

### MG.43a — landed

Commit `e` extend (`commit --amend --no-edit`), revert `v` and
cherry-pick `a` as `CommitOp` variants, branch `x` reset.

**`x` was being held free FOR reset**, and landing it turned a
reservation into a binding: `the_branch_submenu_keeps_the_list_it_replaced`
now asserts `x` = reset and `k` = delete rather than "`x` is absent".
That pair is why the keys moved at all — a user reaching for magit's
reset must never hit delete.

**Keys are overloaded across the gated states.** `A` is *pick* idle and
*continue* stopped; `a` is *apply* idle and *abort* stopped. The gate
is the only thing making that safe, so the test asserts the property —
any key in both states resolves to a different row — rather than
hard-coding keys, and carries a vacuity guard so a refactor that
stopped sharing keys cannot make it assert nothing.

**Branch reset carries its ref to the execute half** (IX.1) rather than
re-deriving: a background refresh while the confirm is open must not
change what gets reset.

### MG.43b — landed

Rebase `p` onto pushRemote, `u` onto upstream, `e` elsewhere, `s` a
subset, `f` autosquash.

**These do NOT reuse push/pull's upstream resolution, and that is the
slice's one real decision.** `resolve_upstream` produces a two-token
`"<remote> <branch>"` pair because `git push` wants them separate.
Git's own synopsis is `git rebase [<upstream> [<branch>]]`, so
`git rebase origin main` is not an error — it reads `origin` as the
upstream and `main` as the branch, silently replaying a different range
than the row promised. `@{upstream}` and `@{push}` are revisions git
resolves natively, so the rows pass them straight through.

`--autosquash` requires `-i`: it only affects the generated todo list.
Without it git accepts the flag and folds in nothing, so the row would
look like it worked.

Rebase `m` / `w` / `k` are NOT here — they rewrite the todo list rather
than name a base, so they belong with MG.43c's commit-moving rows.

### MG.43c — landed, and it lifted a documented limitation

Rebase `m` edit a commit, `w` reword a commit, `k` remove a commit.
One builder, three verbs — the verb IS the operation.

**`w` was the interesting one.** `run_rebase` set `GIT_EDITOR=true`,
which accepts a reword's message unchanged: the rebase exits 0 and the
message does not change. That is the limitation this module's header
recorded, and it is invisible from outside — success either way.

The fix is the trick the module already used one level up. The message
is collected in a compose buffer FIRST, then `GIT_EDITOR` points at
`cp <message-file>`, exactly as `GIT_SEQUENCE_EDITOR` already points at
`cp <todo-file>`. `edit` and `drop` keep `true`, because neither opens
an editor.

`rewording_a_commit_applies_the_new_message` asserts on the resulting
message rather than the exit status, which is the only thing that
distinguishes the fixed version from the broken one.

**A real race surfaced, and it was not a test artefact.** The rebase
scratch file was named `<pid>-<upstream>`, which is not unique: two
rebases in flight sharing an upstream share the path, and one
overwrites the other's todo. Two tests collided because identical
fixture repos built in the same second produce identical shas. Fixed
with a monotonic counter — the same shape
`tempdir-helpers-need-a-counter` records.

**`k` conflicts when a later commit builds on the dropped one**, and
should: that is a real conflict git stops on, not something the row
papers over. The test uses independent files precisely so it asserts
the drop, not the conflict.

### MG.43g — variable rows, and why the prefetch is the design

Magit renders a git-config value *inside* the menu (`pull.rebase
= true`) and edits it in place. Lattice's transients have `Flag`,
`Argument` and `Submenu` but no variable kind.

> **UX (higher court):** the inline value is the row's whole point —
> `C` that opens something else breaks the muscle memory the row
> exists to serve. Convention governs here (the UX-convention rule):
> magit users carry this across editors.
> **Paramount goals:** protects #2 — a variable row is a generic
> transient capability, not a magit one, so the next subsystem with
> config-backed toggles gets it free. The risk is to #1, and it is
> the entire design question: a synchronous `git config --get` per
> row would put I/O in a path opened by a keystroke.
> **Heuristic #1 (long-term fit, on merit):** the row kind is the
> better long-term design; the *synchronous read* is the part that is
> wrong. Relocating the I/O is what makes the good design admissible,
> rather than a reason to reject it.
> **Heuristic #3 (third option):** the rejected pair are (b) read
> synchronously — matches magit and violates #1 outright — and (c) a
> config buffer, which protects #1 and #3 but loses the inline value.
> The prefetch is the option that keeps both.
> **Mode ownership:** `TransientItemKind::Variable` is generic and
> lands in `lattice-picker`; the git-config reader, the key tables and
> every `C` row stay in `lattice-magit`.

Values are fetched off-thread when the **parent** menu opens and
rendered from cache, so building the menu reads a map and never the
filesystem.

**The cache must be able to say "unknown".** A variable row whose
value has not arrived yet renders as pending rather than as unset —
showing `pull.rebase = false` for "we have not looked" would be a
confident lie about the user's config, and the row exists precisely to
report the current value.

**Landed.** `TransientItemKind::Variable { key, value, action }` in
`lattice-picker`, rendered by both peers; `git_config.rs` in
`lattice-magit`; `C` rows on branch, push, pull, fetch, tag and notes.

Details worth keeping, each a way the row could look right and be
wrong:

- **The prefetch fires where the dispatch source is built, not inside
  a row.** Every submenu is constructed eagerly when `C-c g` opens, so
  that is the single point running before all of them. It is
  fire-and-forget; a refresh landing later shows up next open, which
  is exactly why `…` exists as a state.
- **One `git config --list -z`, not one `--get` per row.** Per-key
  reads would be a process per row. `-z` rather than the default
  line-oriented output because a value may contain a newline (any
  multi-line alias), which the default format cannot represent
  unambiguously.
- **Three display states, not two.** `…` unread, `unset` read-and-absent,
  and the value. Collapsing the first two makes the menu assert
  something about the user's config it never checked.
- **An empty prompt UNSETS.** `git config key ""` leaves the key
  present-and-empty, which reads back as set — so clearing a row would
  silently fail to clear it.
- **The prompt is seeded with the current value.** A configure row
  edits an existing setting; starting blank would mean retyping it to
  change one character, and would make a stray `<CR>` clear it.
- **`Variable` shares the host's `Action` dispatch arm** via an
  or-pattern rather than getting a copy, so argument projection,
  region carrying and effect application cannot drift between them.
- **Notes' `C` is gated off during a merge**, like every other row
  there: changing the notes ref mid-merge is precisely what the gate
  exists to prevent.

`C` and `X` both left the branch menu's "deliberately free" list this
arc — each landed in the slot that was being held for it, and the
tests that asserted absence now assert what occupies them.

---

### MG.43h — landed, and MG.41f's diagnosis corrected

The blocked note said the fix was "teach the diff/log open actions to
accept arguments", i.e. an operation change. It was narrower:
MG.17a's projection was already generic, and only the empty
`args_schema` was missing.

Two things it did need, neither of them an operation change:

- **A place to leave the values for a buffer that does not exist yet.**
  The toggles are answered before the view opens, so `ViewArgsRequests`
  holds them under the buffer's name and the mode takes them on
  activation — the shape `BlameRequests` already established. Taken,
  not read, so a later plain `:magit-diff` does not inherit them.
- **The UNION schema, not each view's own table.** `view_argv` resolves
  a flag by its position in `VIEW_ARG_TABLES`. An own-table schema
  works for diff by coincidence (it is first) and breaks log silently:
  `slot_of` returns an index past the end of log's own argument list,
  so every log toggle is collected and then read as unset. I wrote the
  own-table version first; `a_log_toggle_survives_the_round_trip_to_argv`
  is the test that exists because of it.

`declared_flag_names` now whitelists the view tables. That is not a
loosening of the guard that caught MG.41f — the open actions genuinely
declare those names — and
`the_view_open_actions_declare_the_union_their_menus_project_onto`
pins the premise so the whitelist cannot go stale into vacuity.

### MG.43d — ported from magit's source, not its docstrings

Read out of `lisp/magit-sequence.el` (`magit--cherry-move`) and
`lisp/magit-branch.el` (`magit--branch-spinoff`). These move and delete
commits, and each pair differs only in where you end up — exactly what
a paraphrase loses.

| Row | src | dst | ends on |
|---|---|---|---|
| `h` harvest | other branch | current | current |
| `d` donate | current | existing branch | current |
| `n` spinout | current | new branch | current |
| `s` spinoff | current | new branch | the new branch |

**Reading the source caught an error the docstrings would not have.**
The spin rows' start point is the UPSTREAM, not the current branch
(`magit--cherry-spinoff-read-args` passes
`magit-get-upstream-branch`). I wrote `current` first; the new branch
then already contained the commit, so the cherry-pick onto it was
empty and git stopped. With no upstream it falls back to the commit's
own parent — the nearest point guaranteed not to contain it.

Other details that are load-bearing:

- **Removing the commit from `src` takes one of two paths.** At the
  tip, `update-ref` moves the branch back one — in its THREE-argument
  compare-and-swap form, naming the value `src` must still hold, so a
  concurrent change fails the write instead of being silently
  discarded. Below the tip it is rebased out instead; `update-ref`
  there would drop every later commit with it.
- **Spin-OUT promotes itself to spin-off when the tree is dirty**,
  which is magit's own behaviour: staying on a branch about to be
  `reset --hard` would destroy the uncommitted work.
- **No upstream means the old branch is left alone.** Rewinding it
  would discard commits that exist nowhere else.
- These run through `spawn_computed`, not `spawn_git_sequence`: later
  steps depend on state discoverable only part-way through (does the
  branch exist, is the commit at the tip), and computing that up front
  would be git I/O on a keystroke.

Nine real-repo tests, asserting the commit is on the destination AND
gone from the source — a "move" that forgot to remove is just the
copy `A` already does.

---

## Dropped from scope

**Stash `w` worktree-only — not in v1.** Git has no flag for it; magit
implements it with `git stash create` plus tree plumbing, and every
composable approximation is wrong in a different way (`--keep-index`
stashes the staged changes too, so popping re-applies them). Rather
than ship a row that matches magit's label and stashes different
content, the row is dropped from scope. Decided 2026-08-05.

---

## Still deliberately out

**`:customize` as the long-term home for per-repo git config.** MG.43g
adds the row kind and magit's own `C` rows; it does not make every git
config key reachable. That remains its own arc.

**No new benchmarks.** Every row here is `LatencyClass::Display` menu
work or a spawned git call already off the actor thread; MG.43g's
prefetch is explicitly designed to keep it that way. Recorded as a
deliberate omission, as MG.41 and MG.42 both did.
