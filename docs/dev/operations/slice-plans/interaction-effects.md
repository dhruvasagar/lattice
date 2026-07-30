# Interaction effects — confirm, prompt, transient

Slice plan for two related pieces of work that turned out to be the same
problem seen from two sides:

1. **A confirmation must act on what it confirmed.** `Effect::Confirm`
   carries only a prompt and an action *name*, so the yes-half has to
   re-derive its target — and the context it re-derives from can change
   between the question and the answer.
2. **Plugins cannot ask the user anything.** `Confirm`, `OpenPrompt` and
   `OpenTransient` have no WIT variant, so a guest cannot name them —
   and the host→guest mapping turns them into `WitEffect::None`, which
   is a silent drop waiting for the first forwarding path.

Design context lives here rather than in a separate fragment: the
confirm contract itself is `magit.md` §12.13, which this work amends.

## 1. The bug: confirmed ≠ executed

§12.13 says the execute half "re-reads its target at the cursor rather
than carrying it through the prompt", justified by "the confirm
transient owns every keystroke while open, so the cursor cannot have
moved".

**It owns keystrokes, not the buffer.** Reachable today, with no
auto-refresh needed:

1. `s` on a hunk — the mutation and its refresh run async.
2. Before it lands, `x` on a file row. The ask half does no git call and
   returns `Confirm`.
3. The refresh lands *while the confirm is open*: MG.18d rebuilds the
   buffer and moves the cursor (`Effect::CursorMoveIn` applies — a
   transient does not change `document_buffer_id`).
4. Answer `y`. The execute half re-reads the cursor row, which now names
   a different file.

You confirmed one thing and discarded another.

§12.13 conflated two things:

- the **safety property** — answering `n` must not mutate, which is why
  the ask half performs no git call; and
- a **mechanism** — re-read the target at execute time,

and the mechanism is the weaker of the two available, because the
context it re-reads is not stable across the wait. The invariant to hold
is **the confirmed target and the executed target are identical**, which
carrying guarantees and re-reading only approximates.

### Carry the payload, not a pointer to it

Coordinates (a cursor row, a row span) are invalidated by a rebuild;
content is not. So:

| Action | Carries |
|---|---|
| file discard | the path |
| hunk / region discard | the **synthesized patch** |
| reset / revert / cherry-pick | the commit SHA |
| branch delete, stash drop | the ref / index |

Carrying the patch has a second benefit: `git apply`'s exact-context
check turns a stale payload into a loud refusal rather than a wrong
apply — the safety property MG.18 already rests on.

## 2. The shape, and why not `CommandInvocation`

```rust
Effect::Confirm { prompt: String, yes_action: String, args: Args }
```

`CommandInvocation` is the canonical "thing to execute" (design
§5.2.1) and was the first choice on design-alignment grounds. It loses
on the requirement that **`Confirm` must cross the plugin seam**:

- `CommandInvocation` has **no WIT mirror** — precisely why
  `Effect::Global` fails at the boundary ("it crosses with the command
  mirror"). Carrying one would make `Confirm` uncrossable until that
  lands.
- `args` **is** mirrored (`wit/types.wit` `variant args`), and a name is
  a string, so the name+args payload crosses today.
- A **name** is also the plugin-native form: plugins register actions by
  name, and a `CommandId` is a host-internal handle they cannot hold.

Requirement selected the design over the aesthetic. Revisit only if the
command mirror lands.

## 3. Why the seam matters

Asking the user a question is not an advanced capability — it is table
stakes for a plugin of any size. Today the whole family is dropped:

```rust
NativeEffect::ChangeDir(_) | NativeEffect::PrintWorkingDir | NativeEffect::ListErrors
| NativeEffect::Confirm { .. } | NativeEffect::OpenTransient { .. }
| NativeEffect::OpenPrompt { .. } => WitEffect::None,
```

So a plugin cannot confirm, prompt, or open a menu — paramount goal #2
with a hole in it.

**Precisely, because the first framing of this was sloppy:** the block
above is the *host→guest* direction, and nothing in production converts
a native effect to WIT today (only the round-trip tests do). The
user-facing gap was simpler — the WIT `effect` variant had no `confirm`
/ `open-prompt` / `open-transient` arms at all, so a guest could not
express them in the first place. IX.3 fixes that by adding the arm.

The `=> WitEffect::None` mapping is still wrong, just not yet load
bearing: it is a lie with the same shape as success, waiting for the
first host→guest forwarding path to become a real silent drop.
`Effect::Global` already fails loudly instead, and IX.4 makes the rest
follow it.

## Slices

| Slice | Scope | Depends | Status |
|---|---|---|---|
| IX.1 | `Effect::Confirm` carries `args`; the confirm transient seeds its state from them so the yes-action receives them | — | ✅ |
| IX.2 | Amend §12.13 to the confirmed-equals-executed invariant; migrate magit's confirm pairs to carry their payload | IX.1 | ✅ |
| IX.3 | WIT mirror for `confirm` — `.wit` variant, both conversion directions, round-trip test | IX.1 | ✅ |
| IX.4 | Unmirrored effects fail with a typed error instead of `WitEffect::None` | — | ✅ |
| IX.5 | `open-prompt` across the seam, following IX.3's pattern | IX.3, IX.4 | ✅ |
| IX.6 | `open-transient` across the seam | IX.3, IX.4 | ✅ |
| IX.7 | magit-other-file-dispatch gains its destructive rows | IX.1, IX.2 | 📝 |

IX.1 is backward compatible by construction: every existing confirm
passes `Args::None` and keeps re-resolving exactly as it does now, so
the contract change and the migration are separable and each lands
green.

## Mechanism note for IX.1

The carried `Args` reach the yes-action through the confirm transient's
own state rather than through a new `TransientItemKind`. The host
already projects state onto an action's `args_schema` **by name**
(`project_transient_state`); IX.1 adds the inverse at confirm-open time
— walk the yes-action's schema, take each positional `ArgValue` from the
carried `Args`, insert it under that spec's name. `transient_args_for`
then reconstructs identical `Args` when the item fires. No new item
kind, and the two projections are inverses of each other by
construction.

## IX.1 — landed 2026-07-30

`Effect::Confirm` gained `args`. The dialog is itself a transient, and a
transient item resolves its arguments from transient *state* when it
fires, so `seed_transient_state` spreads the carried `Args` across the
yes-action's schema at open time and `project_transient_state`
reconstructs them identically at fire time. The two are inverses,
pinned by a round-trip test.

Backward compatible by construction: `confirm::ask` passes `Args::None`,
which seeds nothing, so every unmigrated confirm re-derives exactly as
before. `confirm::ask_with` is the carrying form.

**One pair migrated as the proof:** `magit-global-file-discard` carries
the path it names, read back through MG.23a's `active_target`, which
already prefers the `file` argument over the visited file. That shared
seam is what makes IX.7 a menu-row change rather than a mechanism one.

**Correction (IX.2):** this slice originally claimed the execute half
"needed no change at all". It needed one — it never declared a `file`
slot, and the projection is by name, so the carried path landed nowhere
and the handler silently fell back to the visited file. The migration
did not actually work. IX.2's
`every_destructive_pair_carries_a_target_except_the_one_with_none`
found it.

Both renderer peers wired in the same patch per the parity rule.

- **Tests:** the round trip; `Args::None` seeds nothing (the
  compatibility guarantee); values beyond the schema are dropped rather
  than bound to the wrong name; and `ask` / `ask_with` carrying nothing
  vs. carrying a target.

## IX.3 — landed 2026-07-30

`confirm` has a WIT mirror: a `confirm-payload` record (prompt,
yes-action, args) and an `effect.confirm` arm in `wit/types.wit`.
Bindings regenerate from the `.wit`, so adding the variant made both
boundary matches fail to compile until handled — the exhaustiveness
guarantee doing its job.

`Confirm` is out of the silently-dropped group. A plugin can now ask the
user a yes/no question and dispatch its own registered action by name,
carrying that action's arguments, exactly as a native mode does.

- **Tests:** round trip in both directions with a carried target, and
  with no args — the two shapes a plugin will actually produce.

The remaining silent drops (`change-dir`, `print-working-dir`,
`list-errors`, `open-transient`, `open-prompt`) are IX.4/5/6.

## IX.2 — landed 2026-07-30

§12.13 amended: the contract is now **the confirmed target and the
executed target are the same thing**, with the old "re-reads its target
at the cursor" recorded as the mistake it was and the reachable sequence
that disproves it written out.

Migrated, each carrying its payload rather than a pointer to it:

| Pair | Carries |
|---|---|
| magit-status `x` | the synthesized **patch** (hunk / region) or the path (file) |
| `C-c f` `x` | the path |
| branch delete | the branch name |
| stash drop | the stash index — these **renumber**, so a re-read after a refresh drops a different stash |
| `reset --hard` | the commit SHA |
| rebase abort | nothing, deliberately — one in-progress rebase, no target to name |

Each execute half prefers what it was given and re-derives only when
given nothing, so paths that carry nothing behave exactly as before.

**A bug this slice found in IX.1.** An execute half must *declare* the
slots its ask half fills, because the projection is by name; an
undeclared slot means the value lands nowhere and the handler silently
re-derives. `magit-global-file-discard-execute` had no slot, so IX.1's
proof migration never actually worked. Two guards now pin both halves —
one on the slot names and order, one requiring every destructive execute
to declare somewhere for its target to land.

## IX.4 — landed 2026-07-30

The five remaining unmirrored effects (`open-prompt`, `open-transient`,
`change-dir`, `print-working-dir`, `list-errors`) return a typed error
naming themselves instead of `WitEffect::None`. The error text names the
culprit because its reader is a plugin author with no view of host
internals.

Two groups, and the error says which: `open-prompt` / `open-transient`
are *blocked on a mirror* (IX.5 / IX.6), while `:cd` / `:pwd` /
`:clist` are host-only by intent — they act on the editor process or a
host-owned view.

Safe to do now precisely because nothing in production converts a native
effect to WIT; this is the guard rail going in before the road, not a
behaviour change.

- **Tests:** each unmirrored variant errors, and the message names it.

## IX.5 / IX.6 — landed 2026-07-30

`open-prompt` and `open-transient` have mirrors, completing the
"a plugin can ask the user something" family:

- **`open-prompt`** carries prompt / initial / on-submit-action /
  buffer-name. The submitted text reaches the action through its
  context's `prompt-value` rather than the payload, because the value
  is what the *user* typed, not what the caller chose. `buffer-name` is
  how a multi-step flow smuggles state between prompts (magit's
  branch-create wizard is the native precedent), so the round-trip test
  covers `Some` as well as `None`.
- **`open-transient`** carries the *source name*, not a menu structure.
  The menu is built host-side from the owning crate's
  `TransientSourceRegistry` registration, so a guest names its menu
  rather than shipping a spec across on every press.

With these, a plugin can ask a yes/no question, collect a line of text,
or open its own menu — the three interaction shapes anything
non-trivial needs. IX.4's unmirrored list is down to the three that are
host-only by intent (`:cd`, `:pwd`, `:clist`).

## Cross-references

- `magit.md` §12.13 — the confirm contract this amends
- `magit-hunk-staging.md` — MG.18e's region discard, whose row span is
  the coordinate IX.2 replaces with a patch
- `slice-plans/magit.md` §MG.23a — the other-file dispatch whose
  destructive rows IX.7 unblocks
