# Cross-file writes — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts live in
> [`../../architecture/cross-file-writes.md`](../../architecture/cross-file-writes.md)
> — the effect shape, why an anchor rather than a range, why one effect
> rather than two, where the capability gate lives, and the rejected
> alternatives.

**Status:** ✅ XF.0–XF.6 complete (2026-08-26). The primitive ships,
gated on `fs:write` at the boundary. [`org-mode.md`](org-mode.md) OM.6b
(archive) and OM.11 (refile + capture) are unblocked — they were the only
reason this exists, and they are now ordinary plugin work in the plugin
repo rather than blocked on a missing host mechanism.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Where this sits

Phase 7/8 surface work — it extends the plugin host's effect vocabulary
and its capability model, so `slice-plans/plugin-host.md` is its
neighbour. The org slices it unblocks live in the plugin repo and land
after XF.5.

## Sequencing

```
XF.0  gate: effect-list failure semantics   ← answered at design; pin it
  │
XF.1  the native effect + the anchor resolver        (no WIT, no plugin)
  │
XF.2  path → buffer: open-or-reuse, in the background
  │
XF.3  insert-then-cut, as one applied unit
  │
XF.4  the WIT effect + the boundary capability gate
  │
XF.5  a fixture guest proves the whole path
  │
  ├── OM.6b  archive subtree          (plugin repo)
  └── OM.11  refile + capture         (plugin repo)
  │
XF.6  docs, ledger, site
```

| Slice | Description | Status |
|---|---|---|
| XF.0 | Gate: effect-list failure semantics | ✅ |
| XF.1 | `Effect::WriteToFile` + the anchor resolver | ✅ |
| XF.2 | Open-or-reuse a target buffer, without stealing focus | ✅ |
| XF.3 | Insert-then-cut applied as one unit | ✅ |
| XF.4 | The WIT effect + the boundary capability gate | ✅ |
| XF.5 | A fixture guest end to end | ✅ |
| XF.6 | Docs, ledger, site | ✅ |

Every slice ships four artefacts (CLAUDE.md heuristic #5): doc, bench
where a hot path is touched, tests covering the failure mode as well as
the happy path, graceful error handling. One slice, one commit,
committed as it goes green, `scripts/precommit.sh <crate>` before each.

**No bench gate is expected.** Design §9: nothing here is per-keystroke,
per-frame or per-tick, and the guest call that returns the effect is
already under the grammar round-trip ratchet. A slice that finds
otherwise should say so rather than quietly skip the artefact.

---

### XF.0 — the gate ✅ (2026-08-25)

Design §5's one-effect argument rests on how a returned `list<effect>`
behaves when one of them fails. **Answered by inspection at design time,
and the answer is stronger than assumed:** `apply_effect_host` returns
`()` and `Effect::Many(parts)` walks its parts unconditionally, so an
effect cannot report failure at all — a host that wanted to stop after a
failed insert has nothing to stop on.

So there is no design question left here. What remains is the test, for
OM.0's reason: a contract this design leans on should be pinned so a
dispatcher refactor cannot silently reverse it, and "already correct" is
exactly the result worth pinning.

**Landed as four unit tests in `dispatch.rs`, not an integration test**,
and the reason is the finding: `Editor::handle_effect` — the public entry
— **does not flatten `Effect::Many` at all.** Its own doc says the
recursion "stays on App". The flattening the design cites is
`apply_effect_host`'s, which is private, so an integration test would
have asserted about a different function than the one §5 names and passed
for the wrong reason.

A second thing the first draft got wrong and the tests caught:
`Effect::ApplyEdit` does not mutate inside the applier. It defers through
`out.next_actions`, which the renderer re-dispatches. A test that skipped
that drain sees an unchanged document and reads as "the effect failed" —
which is how the first version of these tests failed against a working
system.

Tests: the headline (a doomed part does not stop a later one); the same
list REVERSED, so it cannot pass merely because ordering favoured it;
consecutive failures not poisoning what follows; and a failing part being
a no-op rather than a partial write — which is what makes "insert first,
cut only if it landed" safe to build on.

### XF.1 — the native effect ✅ (2026-08-25)

`Effect::WriteToFile { path, anchor, text, cut }` in `lattice-grammar`,
plus `FileAnchor { End, Start, Line(u32) }`. No WIT, no plugin — a
native mode could use this, and building it native-first means the
boundary slice has something to convert *to*.

The anchor resolver is the whole of the logic worth testing here: given a
document snapshot and an anchor, produce the byte offset to insert at.

**The compiler enforced the cross-renderer rule**, which is the pleasant
version of that rule working: adding the variant broke six exhaustive
matches — the WIT boundary, the host's two effect classifiers, and BOTH
renderers' (the TUI twice, GPUI once). Nothing had to be remembered.

Both renderers group it with `ApplyEdit` as host-applied: there is
nothing renderer-coupled in path→buffer, the insert, or the cut. Both
classifiers report it as a mutation — for Visual auto-exit and for `.`
eligibility — because it always changes at least the target and, with a
`cut`, the source too.

**The WIT boundary rejects it with a typed error for now**, the
`OpenProviderView` precedent. Crossing it before XF.4 would be an
unchecked cross-file write, since the gate that authorises the path
cannot run at the applier (§6).

Tests: `end` one past the last line; every anchor agreeing on an EMPTY
file, which is capture's first run and where a disagreement would be an
out-of-range insert; `start`; `line(n)` in range; `line(n)` past the end
clamping to exactly `End` rather than erroring — the difference between
"your archive entry went somewhere" and "your archive entry is gone";
and `line(n) == count` being append rather than an off-by-one inside the
last line.

### XF.2 — open-or-reuse, in the background ✅ (2026-08-25)

The one genuinely missing primitive: **path → `BufferId`**.

- Already open? `Editor::find_document_by_path` answers it. **Reuse it.**
  This is the correctness case, not an optimisation: opening a second
  buffer over a file the user has unsaved changes in and then editing the
  copy loses their work silently.
- Not open? Read it (`spawn_blocking` — the actor is a `current_thread`
  runtime and design §9's one hard constraint is that this read does not
  land on it), build a document, and insert it through
  `BufferStore::insert_document_buffer` as an ordinary **listed**
  document buffer.
- **The active pane does not move.** A plugin write must not steal focus.
- Missing file → created (empty document, path set) when its parent
  exists; missing parent → `Err`, per design §8.

`Editor::resolve_path_to_buffer`. Two things the slice found.

**`find_document_by_path` was not enough for the reuse check.** It
compares `PathBuf`s, and `Path` equality is component-wise — so
`dir/./notes.org` matches `dir/notes.org` for free, but
**`dir/sub/../notes.org` does not** (`Path::new("/a/c/../b") ==
Path::new("/a/b")` is `false`; verified rather than assumed). A producer
whose path happens to contain `..` would have opened a SECOND buffer over
an already-open file, and the user's unsaved work in the first would be
invisible to the write. `find_document_by_real_path` canonicalizes both
sides; a path that will not canonicalize (the file does not exist yet)
falls back to raw, which can only fail to match — the safe direction,
since the unsafe one is matching two different files together.

**The first version deadlocked.** It called `buffers.document_handle()`
inside `buffers.for_each()`, which the registry's own doc forbids in
as many words — the callback holds the mutex. It did not fail the test
run, it *hung* it. Ids are collected first and the handles looked up
after.

Tests (10): the headline reuse case, asserting the buffer's UNSAVED
content is what the write sees; both `..` spellings, in both directions
(awkward-then-plain as well as plain-then-awkward, because the
canonicalisation has to happen on both sides); focus not moving; the
target landing in `:ls` and findable by path; a missing file opening
empty with its path set; a missing parent refused *without* creating the
tree on the way out; a directory refused; and resolving twice yielding
one buffer.

### XF.3 — insert-then-cut, as one unit ✅ (2026-08-25)

The applier. Resolve the anchor, apply the insert through
`Editor::apply_targeted_edit` (which already routes active-document vs
peer-buffer), and only on success apply `cut` to the buffer the action
ran in.

**It applies inline, and could not have reused `ApplyEdit`'s deferral.**
`Effect::ApplyEdit` pushes an `Action::ApplyEdit` onto `next_actions` for
the renderer to walk — unconditionally, and an effect cannot report
failure (XF.0). Composing this out of two of those would have run the cut
whether or not the insert landed, which is the "the subtree is gone"
outcome the one-effect design exists to make unrepresentable.
`apply_targeted_edit` returns a `Result`, so applying inline is what
makes "only if it landed" expressible at all.

**Two position bugs the tests caught, both silent.**

`content_line_count` strips the phantom line after a trailing newline
(CV.2), so `Position::new(line_count, 0)` — the obvious append — exists
for `"* Old\n"` and does **not** for `""` or `"a"`. On an empty target
the insert failed, and with a `cut` that took the insert-failed branch on
a perfectly writable file: the archive silently did nothing. The append
position now comes from `rope_line_count` + `line_byte_len`, which is the
true end in every case.

And appending to a file whose last line has no newline spliced onto it:
`"notes"` + `"* Archived\n"` became `"notes* Archived"`, corrupting a
line the user never touched. The producer cannot prevent this — it has
never read the target — so the host supplies the separator. `resolve_line`
stays the pure, tested line-level function; turning a line into a
position is the host's job because only the host has the buffer.

Tests (12): append / start / create; the move; **a failed insert leaving
the source untouched**, in two shapes (missing parent, and a directory
target — different branches); an already-open target written in place
with its unsaved content; the target left unsaved so the disk is
untouched; each buffer undoing its own half; both trailing-newline cases,
including that repeated appends do not accumulate blank lines; and
`Line(n)` past the end appending rather than failing.

### XF.4 — the WIT effect + the gate ✅ (2026-08-26)

`write-to-file(write-to-file-payload)` in `types.wit`, its boundary
conversion, and the capability check.

**The check runs at the boundary** (design §6) — the conversion has the
`Store<PluginState>` and therefore the `CapabilityGrant`; the dispatcher
does not and must not. On denial the effect is replaced with an `Echo`
naming the refusal, so the dispatcher only ever sees authorised effects.

- The check is `host_services::grant_permits_walk`'s twin: canonicalize
  both sides, fail safe when canonicalization fails.
- **Canonicalize the parent when the target does not exist** — capture's
  first run creates its file, and a non-existent path canonicalizes to
  nothing. Missing this turns "create the capture file" into a permanent
  denial.
- `info!` on denial (one-shot, user-actionable).

`file-anchor` + `write-to-file-payload` in `types.wit`, both boundary
directions, and `EffectAuthorizer` — built once per plugin at load from
`store.data().grant`, so the keystroke path pays only the prefix compare
a `WriteToFile` actually needs and nothing at all for any other effect.

**Three things the gate has to get right, each its own test.**

*Read is not write.* `walk_within_grant` accepts `fs:read` OR `fs:write`
because a walk only reads; conflating them here would let any plugin that
can list a tree write into it. Only write prefixes count.

*`..` cannot escape.* Both sides canonicalize, so
`<granted>/../secret/passwords` is refused where a textual prefix match
would have allowed it.

*A not-yet-existing file is judged by its PARENT.* Capture's first run
creates its target, and a non-existent path canonicalizes to nothing —
without this the whole create-a-capture-file case would be permanently
denied, i.e. the feature unreachable by construction. The parent check
does not open a hole: a non-existent file outside the grant is still
refused, and that has its own test.

**`Many` recurses**, or the gate would be the whole gate — bypassed by
wrapping the write in a list. A denial replaces only that part with an
`Echo`; the rest of a compound effect still runs, so one refused write
does not silently cancel everything else the action did.

**One conversion path, deliberately.** `effect_from_guest` does the
`from_wit` and the authorise together, and all three effect-returning
trampolines (operator, action, ex-command) call it. A fourth
contribution kind cannot get the conversion and forget the gate.

A non-UTF-8 path is refused rather than lossily converted: a mangled path
names a *different file*, and this effect writes to it.

Tests: 12 on the authorizer, 4 boundary round-trips (including every
anchor, since a variant collapsing to `End` would file every capture at
the bottom, and the `cut` surviving, since losing it turns a move into a
copy that looks like it worked).

### XF.5 — a fixture guest, end to end ✅ (2026-08-26)

A `wasm32-wasip2` fixture that returns a `write-to-file` from an action,
driven through a real `Editor` — the whole path, the way
`agenda_source.rs` drives `agenda-guest`.

The `grammar-guest` fixture gained an `archive-to` action returning a
`WriteToFile` whose path comes from `ctx.args`. **One guest covers both
the granted and the denied case, and the tests vary only the manifest** —
which is what makes a denial demonstrably about the capability rather
than about the plugin having been written differently.

Four tests through the real boundary: a granted plugin's write crossing
with its payload intact; the same guest, same action, same path with NO
grant being replaced by an `Echo` before it can reach the editor; a
`fs:read` grant not authorising a write (the distinction `host-services`'
walk deliberately does not make); and `..` failing to escape a granted
prefix.

**Verified by mutation, not just by passing.** Unwiring
`authorizer.authorize` in the trampoline fails exactly those three
denial tests and leaves the other eleven green — so they are testing the
*wiring*, which is the half `effect_authorizer`'s unit tests cannot
show, and the half whose absence would be an unchecked cross-file write
reachable from any plugin.

**A second contribution-count assertion existed**, in
`lattice-plugin-loader`'s `unload_reload.rs`, and only
`cargo test --workspace` found it — a scoped precommit over
`lattice-plugin-host` cannot see a test in another crate that counts this
fixture's contributions. Third time this session that the workspace run
caught something scoped runs could not.

### XF.6 — docs, ledger, site ✅ (2026-08-26)

- **`docs/user/plugins.md`** — the user-visible half, and the one worth
  getting right: a plugin can now file text into a file you have not
  opened. What stops that being alarming is stated plainly — it edits a
  *buffer*, leaves it modified and unsaved so nothing reaches disk until
  you `:w`, appears in `:ls`, undoes with `u`, and never steals focus;
  and the path is checked against the grant rather than trusted,
  including against `..`. Also that `fs:read` over a directory is not
  permission to write into it.
- **`implementation.md`** — the org-mode section's "what is blocked"
  paragraph stops being true, and gains the four silent bugs the slices
  caught.
- **`org-mode.md`** slice plan — OM.6b and OM.11 move ⛔ → 📝 with what
  each now has to do, and what XF does *not* supply (capture's template
  flow, refile's target picker).
- Design fragment + slice-plan statuses; Zola sync.

**The org plan is still not archivable**, and the header says why: 📝 is
open work too. Archiving it now would bury the two slices this whole
detour existed to unblock.

---

## What this does NOT unblock

Worth stating so a later reader does not go looking.

- **`OpenProviderView`'s plugin surface.** This lands the enforcement
  *shape* that question reuses (a boundary check against the grant), not
  its policy. Which providers a plugin may trigger is a separate
  decision.
- **Saving.** The target is left modified, per design §7. A plugin that
  writes to disk is a larger authority and a later decision.
- **`org-capture` templates.** Capture's *write* unblocks here; its
  template UI (a prompt flow, a target picker) is org's own work and
  belongs in OM.11.

## The acid test, as an assertion

To be asserted at XF.5 rather than claimed in prose:

- a plugin with `fs:write:<dir>` moves text into a file under `<dir>`
  that the editor had never opened,
- the same plugin **without** that capability moves nothing and says so,
- and the host gains no knowledge of what the text is — no org, no
  filetype, no command name anywhere in the primitive.
