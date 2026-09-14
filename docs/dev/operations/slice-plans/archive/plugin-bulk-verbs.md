# Plugin bulk verbs — scope for the `:plugins` view (PM.9)

> **Design contract:** [`../../../architecture/plugin-manager.md`](../../../architecture/plugin-manager.md)
> §4 (the refresh policy), §8.1–§8.3 (scope, bulk runs, clean).
>
> **Status: ✅ complete** (2026-09-14). Archived on landing — every slice
> settled in one sitting.

## Why

`:plugins` could only ever act on the row under the cursor, so keeping a set of
plugins current meant pressing `b` once per row and knowing which rows needed
it. There was also no `update` verb at **any** scope: nothing in the editor
went and fetched a newer source.

Investigating that surfaced a defect underneath it. `resolve_git` ran
`git fetch --tags origin` on every re-resolve of an existing unpinned checkout
and then stopped — the `checkout` after it was reachable only under a pin — so
the objects arrived and local `HEAD` never moved. An unpinned plugin was frozen
at the commit it was first cloned at, for the life of the checkout, while
paying a network round trip on every boot to stay that way. Any "update all"
built on top would have reported success and changed nothing, which is why the
fix is PM.9a and not a follow-up.

## Slices

| Slice | What | Status |
|---|---|---|
| PM.9a | `RefreshPolicy` — fetch AND move, or neither | ✅ |
| PM.9b | `PluginLoader::update` + `:plugin-update <name>` | ✅ |
| PM.9c | The bulk engine (`run_bulk`) + `clean` | ✅ |
| PM.9d | The view's scope chords + per-leg progress | ✅ |
| PM.9e | Docs — design fragment, user docs, `:help` | ✅ |
| PM.9f | This plan + full-workspace verification | ✅ |

### PM.9a — `RefreshPolicy` ✅

Two callers want opposite things from a resolve, so the resolver is told which
is asking: `UseCache` (boot, rebuild) uses the checkout as it stands and now
touches **no** git at all for a warm unpinned one; `Update` fetches, then
`reset --hard FETCH_HEAD`. Strictly less network than before, not more —
nothing advances behind the user at startup.

`reset --hard` rather than merge or pull: the checkout is a cache the editor
owns, never a tree the user edits, so the tracked head is simply what it should
contain and a merge could conflict with nobody there to resolve it.

A pin ignores the policy — it is already the answer to "which commit" — while a
CHANGED pin still fetches and moves under either.

- The replaced test (`an_existing_checkout_without_a_pin_fetches`) asserted the
  broken behaviour: true, and useless, because the fetch led nowhere. Three
  tests now pin the contract instead.

*Tests:* 3.

### PM.9b — `update` + `:plugin-update` ✅

`update` is `rebuild` with one argument changed, so `rebuild_with(name, policy,
verb)` is the shared body. What "newer" means belongs to the source: unpinned
git fetches and moves, `Local` is always current (the directory IS the source),
`Prebuilt` re-downloads, and a **pinned** git source declines — updating past a
pin discards what the user wrote in `init.rs`.

- `update_refusal` is a free function over the source alone, so the arm table
  is testable without constructing a private `LoadedRecord` to assert a string.

*Tests:* 3.

### PM.9c — the bulk engine + `clean` ✅

**Sequential, by design.** Concurrency reads as the obvious win and is wrong
three times over: `cargo` already saturates the machine (and a full build tree
is tens of gigabytes — this was hit for real mid-slice); every leg ends in a
reload, which mutates the registries by copy-on-write RCU; and the user wants
to read which plugin is building now, not six rows all claiming to be.

`BulkLeg` is an enum, not a `Result`, because **skipped is not failed** — `4
updated, 2 pinned` reads as success where `4 updated, 2 failed` sends someone
hunting for a problem that does not exist.

`clean` is conservative four ways (§8.3) and bang-gated: `:plugin-clean` lists,
`:plugin-clean!` removes. `removable_under` and `clean_listed` are free
functions so the rules are tested against a real directory tree — a test must
never read the user's actual config root, and certainly never delete from it.

*Tests:* 12.

### PM.9d — the view's chords ✅

Lowercase acts on the row, uppercase on every row — the idiom `t` / `T` already
taught. The three `*_all` loops collapsed into `run_bulk(op, on_leg)`; the
manager passes a callback that repaints **between** legs, because a bulk
rebuild is minutes of `cargo` and a view that only updated at the end would sit
still for exactly the time it mattered.

The progress note rides the **title line**: the interactivity layer maps
`cursor.line - HEADER_LINES` into the plugin list, so an extra header row would
put every chord on the wrong plugin, and only while a run was in flight.

`X` confirms, carrying the **names** rather than a cursor position — between
the prompt and `y` a plugin can finish loading.

- Also added `keymap_cmds_have_registered_handlers`, which `actions.rs` had
  claimed pinned this wiring since PL8.H.3 and which **did not exist anywhere
  in the workspace**. A chord could name an unregistered command, or one with
  no handler, and the only symptom would be a key that does nothing while
  `:describe-key` agreed it was bound.

*Tests:* 6.

### PM.9e — docs ✅

Design fragment §4 + §8.1–8.3; `docs/user/plugins.md` and
`docs/user/plugins-mode.md` (whose chord table had been missing `b` since
PM.8b). `site/content/` regenerated via `site/scripts/sync-docs.sh`.

### PM.9f — this plan + verification ✅

## No bench, deliberately

The four-artefact rule asks for benchmark coverage where perf impact would be
visible. There is none to make visible here: every verb is off-thread build
orchestration whose cost is `cargo`'s, the dispatch path only spawns, and the
one measurable change is **negative** work — PM.9a removes a per-boot network
round trip per unpinned git plugin. A benchmark would measure `cargo`.

## Known pre-existing failure, not from this work

`unload_reverses_picker_and_grammar_contributions` (`lattice-plugin-loader`,
`tests/unload_reload.rs:184`) fails on clean `HEAD` — a fixture guest registers
9 grammar contributions and the assertion still says 8. Verified by stashing
before slice 1 and re-run at each gate; it stayed the only failure throughout.
