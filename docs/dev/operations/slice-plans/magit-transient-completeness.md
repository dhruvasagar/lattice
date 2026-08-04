# MG.41 — magit transient completeness

**Status:** 📝 planned. Parent plan:
[`magit.md`](magit.md) (MG.1–MG.40). Design fragment:
[`../../architecture/magit.md`](../../architecture/magit.md).

Close the gap between lattice's magit transients and magit's own. The
audit behind this plan is §"Audit" below; the short version is that
**10 of 25 dispatch rows are submenus**, several menus that exist have
2–3 of magit's 7–14 rows, and push / pull / fetch — the ones a user
reaches for daily — expose a single unlabelled action where magit
offers three destinations.

---

## Scoping decision: transient keys are magit's, verbatim

Inside a transient the menu owns every keystroke — it is a modal
surface with visible hints, not a buffer where vim grammar is live.
So **transient row keys follow raw magit exactly**, including keys that
would be unthinkable as buffer chords.

This is a deliberate carve-out from `magit-keys-follow-evil-magit`,
which says lattice takes evil-collection-magit's remaps rather than raw
magit's. That rule is about **buffer keymaps**, where `x` / `v` / `V`
collide with vim grammar. It does not apply inside a transient, and
reading it as though it did is what would produce a menu that matches
neither magit nor evil-magit.

Concretely, this slice **changes** one existing key:

| Menu | Today | After | Why |
|---|---|---|---|
| `branch` | `x` = delete | `x` = **reset**, `k` = delete | magit's own. Today's `x` puts delete where magit users expect reset — the more destructive surprise of the two. |

Dispatch-level keys (`O` reset, `_` revert, …) are **not** touched:
those are the entry chords pressed from a buffer, where the
evil-collection convention correctly applies.

---

## Design: the four-place enumeration has to go first

Adding a transient row today means editing **four** places:

1. `register_action_commands` — `reg("action:magit-global-x", doc)`
2. `DispatchActionIds` — a new `pub x: Option<CommandId>` field
3. `resolve_dispatch_ids` — `x: registry.id_by_name("action:magit-global-x")`
4. the transient builder — a hand-written `action_or_placeholder(…)`

`DispatchActionIds` is already ~45 fields. This plan adds roughly 60
rows; done the current way that is ~240 edits across four
enumerations that must stay in sync, with nothing failing to compile
when they drift — the row silently renders as a placeholder.

That is the same stale-enumeration failure mode this codebase has been
bitten by repeatedly (three separate recording-chokepoint bypasses in
the pane-history arc alone). **MG.41a replaces it before any rows are
added**, so the content slices are one-line-per-row data changes.

> **Heuristic #1 (long-term fit, on merit):** the struct-of-`Option`s is
> genuinely fine at 45 fields and genuinely wrong at 105. The trigger is
> a concrete scaling problem, not novelty — and the alternative is
> paying a 4× edit tax on every row for the rest of the feature's life.
> **Paramount goals:** protects #2 (extensibility — a plugin-contributed
> magit row becomes possible when rows are data) and #3 (the grammar /
> command surface stays the public API; rows reference commands by
> registered name). Sacrifices nothing on #1: resolution happens once
> at install, not per menu-open.
> **Heuristic #2:** anchored on the enumeration failure mode this repo
> has evidence for, not on "table-driven is tidier".
> **Heuristic #3 (third option):** keep the struct and accept the tax —
> viable, and cheaper for the first 10 rows. Rejected because the tax is
> permanent and the drift is silent. A fourth option, generating the
> struct with a macro, keeps compile-time checking but makes the row
> tables unreadable, which is the thing being optimised for.
> **Mode ownership:** every action, table, and handler stays in
> `lattice-magit`. The acid test holds — this plan adds **zero**
> `Editor::` methods and **zero** host `Action` variants.

### MG.41a — rows become data ✅

Replace `DispatchActionIds` / `FileDispatchActionIds` with a
name-keyed resolver plus static row tables.

	/// Resolved once at install; rows name commands, not fields.
	pub struct MagitActionIds(HashMap<&'static str, CommandId>);

	pub struct TransientRow {
		pub key: &'static str,
		pub label: &'static str,
		pub doc: &'static str,
		pub action: &'static str,   // "action:magit-global-push-upstream"
	}

	static PUSH_ROWS: &[TransientRow] = &[ … ];

A row's action name is the single source of truth: `register_action_commands`
iterates the same tables to register, and the builder iterates them to
render. Adding a row is one table entry.

**Losing compile-time field checking is the real cost.** Mitigated the
way this repo already mitigates it for the `<C-h>` help prefix
(`help_prefix_chord_table_resolves_all_commands`): a test asserts every
name in every table resolves against a populated `CommandRegistry`.
Drift fails loudly at test time instead of rendering a placeholder.

**Tests.** Every table row resolves; no duplicate key within one
transient; every registered `action:magit-global-*` is referenced by
some row (catches orphans in the other direction); the existing
transient snapshot tests still pass unchanged.

### MG.41b — transient height is configurable ✅

`render.rs:551` caps every popup at a hard-coded 10 rows:

	fn popup_height(candidate_count: usize) -> usize {
	    const MAX_ROWS: usize = 10;
	    candidate_count.min(MAX_ROWS).max(1)
	}

The dispatch transient has 25 rows plus group headers, so under half
of it is visible and the rest needs scrolling — before this plan makes
it larger still.

`popup_height` is shared by the picker, the completion popup, and
transients. They want different defaults: a picker is *filtered* (you
type to narrow, 10 is plenty), a transient is *browsed* (you read the
menu to find the key). So this adds a separate option rather than
raising the shared constant.

- `ui.transient.max-rows` — typed `i64`, default **20**, in the
  **`Display`** group, not `Magit`: transients are a general UI surface
  (`view_arguments_transient` is not magit-specific), and a
  magit-scoped option would be in the wrong place the first time
  another subsystem grows a transient.
- `popup_height` gains a caller-supplied cap; the picker and completion
  paths keep 10 via a named constant so the change is visibly scoped.

**Tests.** Default gives 20; `:set ui.transient.max-rows=40` widens the
band; a transient shorter than the cap still claims only its own rows
(the cap is a maximum, not a minimum); the picker band is unchanged at
10.

**Landed.** `ui.transient.max-rows` (Display group, default 20),
`popup_height_capped` + `transient_max_rows`, and the same option read
by the **GPUI peer**: it had its own private `TRANSIENT_MAX_VISIBLE_ROWS
= 24` against the TUI's 10, which is exactly the silent divergence the
cross-renderer rule exists to prevent. Both now read one option.

Threading it through GPUI needed the `--features window` build — a
plain `cargo build -p lattice-ui-gpui` does not compile `window.rs` at
all, so the first "clean" build was a no-op. That build is **broken on
clean HEAD** for unrelated reasons (`GpuiTheme` has no
`diff_change_line_bg` / `diff_remove_line_bg`, used by notification
theming); verified by stashing. Not fixed here — out of scope for this
slice, but worth knowing the gpui feature build is currently red.

---

## Audit — what is missing

Row counts are lattice-today vs magit. Keys are magit's.

### Remote operations — the worst case

`remote_op_transient` is built to hold exactly one action row:

	items: vec![action_or_placeholder(run_id, run_key, …)]

| Menu | Today | Magit |
|---|---|---|
| **Push** `P` | flags `-f -u`; `P` push | flags `-f -F -h -n -u`; `p` pushRemote, `u` @{upstream}, `e` elsewhere, `o` another branch, `r` refspecs, `T` a tag, `t` all tags |
| **Pull** `F` | **not a submenu** | flags `-r -a`; `p` pushRemote, `u` upstream, `e` elsewhere |
| **Fetch** `f` | flags `-a -p`; `f` fetch | flags `-p -t`; `p`, `u`, `e`, `o` another, `r` refspecs, `a` all remotes, `m` submodules |

The destination rows are **one operation with different target
resolution**, not seven handlers. Model it as data:

	enum RemoteTarget { PushRemote, Upstream, Elsewhere, OtherBranch, Refspecs, Tag, AllTags }

One action per op taking the target as an argument, so push gains six
rows and one new handler rather than six.

### MG.41c — push / pull / fetch destinations

Push `p u e o r T t` + the three missing flags; Pull promoted to a
submenu with `p u e` + `-r -a`; Fetch `p u e o r a m` + `-t`.

**Tests.** Each destination resolves to the right argv (the preview and
the run share `RemoteOp::preview`, so the preview assertion covers
both); `-f` + `-F` are mutually exclusive; pull is reachable as a
submenu from the dispatch.

### MG.41d — thin existing submenus

| Menu | Today | Adds |
|---|---|---|
| **Commit** `c` | `c a` | `e` extend, `w` reword, `f` fixup, `F` instant fixup, `s` squash, `S` instant squash, `A` augment |
| **Stash** `z` | `z l` | `i` index, `w` worktree, `x` keeping index, `Z`/`I`/`W` snapshots, `a` apply, `p` pop, `k` drop, `b` branch, `v` show |
| **Reset** `O` | `s m h` | `k` keep, `i` index, `w` worktree, `f` a file |
| **Branch** `b` | `b l c n m x L` | `s` spin-off, `S` spin-out; **`x` becomes reset, `k` becomes delete** |

### MG.41e — dispatch rows that should be submenus

Each is a direct action today where magit has a full transient:

| Row | Magit rows |
|---|---|
| `m` merge | `m` merge, `e` merge+edit, `n` no-commit, `a` absorb, `p` preview, `s` squash, `i` dissolve (+ commit / abort while merging) |
| `r` rebase | `p` onto pushRemote, `u` onto upstream, `e` elsewhere, `s` subset, `i` interactive, `m` edit commit, `w` reword, `k` remove, `f` autosquash (+ continue / skip / abort while rebasing) |
| `A` cherry-pick | `A` pick, `a` apply, `h` harvest, `m` squash, `d` donate, `n` spinout, `s` spinoff (+ sequence controls) |
| `t` tag | `t` create, `r` release, `k` delete, `p` prune |
| `_` revert | `V` commit, `v` changes (+ sequence controls) |

Merge and rebase carry **in-progress gating**, the pattern MG.21g /
MG.37 / MG.39 already established for bisect, notes and am: while a
rebase is stopped, the menu shows only the ways out. The `gates` struct
those slices grew is the right home — extend it, do not add positional
bools (the MG.38–MG.40 retro already recorded that).

Suggested order: `r` rebase and `m` merge first (most used, and they
exercise the gating), then `A`, `t`, `_`.

### MG.41f — diff / log argument menus

`d` and `l` are direct actions; magit gives each a transient of
arguments (`-D` decorate, `-g` graph, `-n` limit, `--stat`, `-p` patch,
…) plus target rows. `view_arguments_transient` (MG.23k) already does
exactly this for a *rendered* view, so this is largely wiring existing
machinery to the dispatch rather than new mechanism.

### MG.41g — completion notifications, via the event bus

Every long-running action should tell the user when it finishes. Half
of magit's already do; the other half silently do not:

| Spawner | Notifies |
|---|---|
| `spawn_remote_op` (push / pull / fetch / stash-create) | ✅ Info on success, Error on failure |
| `spawn_subtree_op`, `spawn_note_prune`, `spawn_note_merge`, `spawn_clone` | ✅ |
| **`spawn_git`** (the generic one) | ❌ |
| `spawn_bisect`, `spawn_commit_op`, `spawn_note_remove`, `spawn_gitignore` | ❌ |

The reason is structural: notification is **opt-in per spawner** — each
takes an `Option<NotificationStoreHandle>` and remembers, or doesn't, to
fire it in both arms. `spawn_git` not notifying means anything built on
it inherits the silence.

**The fix is the event bus, not a required parameter.** Threading the
notification handle into every spawner would make it unforgettable but
would also hard-wire *magit* to the *notification* subsystem, and repeat
that wiring for the next subsystem with async work. Instead the op
publishes a typed event and the notification layer subscribes:

	/// A long-running background operation finished.
	BackgroundTaskFinished {
		/// Subsystem that ran it — "magit", "lsp", a plugin id.
		source: Arc<str>,
		/// What finished, in the user's words: "push", "clone …".
		label: Arc<str>,
		outcome: TaskOutcome,   // Succeeded { summary } | Failed { message }
	}

- **magit publishes; it never mentions notifications.** The
  `NotificationStoreHandle` plumbing currently threaded through
  `spawn_*` comes out entirely.
- **One generic subscriber** maps the event to a notification, so the
  policy (which levels, whether to notify at all, rate limiting) lives
  in one place and is configurable later without touching any producer.
- **Any subsystem gets it free** — LSP, compilation, a plugin's async
  work — by publishing the same event. That is the point of the
  decoupling, and it is what a per-spawner handle cannot give.

> **Paramount goals:** #2 (a plugin's async op gets completion
> notifications with no host change), #4 (async work reports back
> without a keypress — the event bus already wakes the editor), and UX
> (a push that silently succeeds or fails is the worst case of the
> async-invisibility class).
> **Heuristic #1:** the per-caller `Option` is not merely untidy — it is
> already wrong in five places, which is the evidence the opt-in shape
> does not hold. The event bus is the genuinely-better long-term fit,
> not the smaller change.
> **Heuristic #2:** anchored on design.md §5.10 (hooks ≡ autocmds ≡
> typed event subscriptions — one bus, typed payloads), not on "events
> are nicer".
> **Mode ownership:** the producer side stays in `lattice-magit`; the
> subscriber is generic host/notify wiring that no subsystem owns.

Publishing still has to be unforgettable on magit's side, so it moves
into the shared spawn helper's completion arms rather than each
caller's — the same "bake it into the primitive" move as the autoread
wake, one layer down.

**Lands before MG.41c and MG.41e**, so the ~15 new async actions those
slices add inherit completion reporting rather than each remembering.

**Tests.** Each spawner publishes on success and on failure; the
subscriber turns a `Failed` outcome into an Error-level notification and
a `Succeeded` into Info; a subsystem that publishes the event with no
magit involvement still notifies (proves the decoupling); the existing
`spawn_remote_op` notifications keep working end to end.

---

## Out of scope, and what stays partial because of it

**Transient variable rows (magit's `C` configure entries).** Magit
renders a git-config value *inside* the menu and edits it in place —
`branch.<name>.pushRemote`, `core.notesRef`, `pull.rebase`. Lattice's
transients have `Flag`, `Argument` and `Submenu` item kinds but no
variable kind, so this needs a new `TransientItemKind` plus git-config
read/write plumbing.

Named rather than silently omitted, because it is why these stay
incomplete after MG.41: **push, pull, fetch, branch, tag, and notes all
keep their `C` row missing.** MG.37 already flagged the same gap for
notes' four configure rows and left magit's keys free for them; MG.41
extends that convention to the rest rather than inventing substitutes.

`:customize` is the likelier long-term home for per-repo git config
than a hand-rolled menu, so the design question is not just "add a row
kind" — worth its own arc.

**No new benchmarks.** Transients are `LatencyClass::Display`, built on
menu-open, O(rows) ≈ 25. The four-artefact rule is met by docs + tests
+ graceful degradation (`action_or_placeholder` already renders a
disabled row for an unresolved action). Recorded as a deliberate
omission.

---

## Sequencing

| Slice | Depends on | Why this order |
|---|---|---|
| MG.41a rows-as-data | — | Every later slice is a table edit only if this lands first. |
| MG.41b height option | — | Independent; lands early so the larger menus are reviewable. |
| MG.41g notifications | — | Independent, and must precede 41c/41e so their new async ops inherit it. |
| MG.41c push/pull/fetch | 41a, 41g | The reported gap; also proves the `RemoteTarget` shape. |
| MG.41d thin submenus | 41a | Pure table growth once 41a lands. |
| MG.41e new submenus | 41a, 41d, 41g | Needs handlers + gating; the biggest slice, carve per menu. |
| MG.41f diff/log args | 41a | Reuses MG.23k machinery; least urgent. |

Each slice lands green on its own; MG.41e sub-slices per menu
(`41e-rebase`, `41e-merge`, …) so no single commit carries five new
transients.
