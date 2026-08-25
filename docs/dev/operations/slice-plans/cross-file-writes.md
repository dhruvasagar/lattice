# Cross-file writes — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts live in
> [`../../architecture/cross-file-writes.md`](../../architecture/cross-file-writes.md)
> — the effect shape, why an anchor rather than a range, why one effect
> rather than two, where the capability gate lives, and the rejected
> alternatives.

**Status:** 📝 planned. Unblocks [`org-mode.md`](org-mode.md) OM.6b
(archive) and OM.11 (refile + capture), which are the only reason this
exists and the only consumer at the end of it.

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
| XF.0 | Gate: effect-list failure semantics (answered; test outstanding) | 📝 |
| XF.1 | `Effect::WriteToFile` + the anchor resolver | 📝 |
| XF.2 | Open-or-reuse a target buffer, without stealing focus | 📝 |
| XF.3 | Insert-then-cut applied as one unit | 📝 |
| XF.4 | The WIT effect + the boundary capability gate | 📝 |
| XF.5 | A fixture guest end to end | 📝 |
| XF.6 | Docs, ledger, site | 📝 |

Every slice ships four artefacts (CLAUDE.md heuristic #5): doc, bench
where a hot path is touched, tests covering the failure mode as well as
the happy path, graceful error handling. One slice, one commit,
committed as it goes green, `scripts/precommit.sh <crate>` before each.

**No bench gate is expected.** Design §9: nothing here is per-keystroke,
per-frame or per-tick, and the guest call that returns the effect is
already under the grammar round-trip ratchet. A slice that finds
otherwise should say so rather than quietly skip the artefact.

---

### XF.0 — the gate 📝 (answered; the slice is the regression test)

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

- *test:* a `Effect::Many` whose first part cannot apply still applies
  the rest — asserting the current behaviour, with a comment naming
  `cross-file-writes.md` §5 as what depends on it.
- *paramount:* #2.

**Keep it first anyway.** It is minutes of work, and it is the slice that
would have caught the premise being wrong before XF.1 through XF.3 were
built on top of it.

### XF.1 — the native effect 📝

`Effect::WriteToFile { path, anchor, text, cut }` in `lattice-grammar`,
plus `FileAnchor { End, Start, Line(u32) }`. No WIT, no plugin — a
native mode could use this, and building it native-first means the
boundary slice has something to convert *to*.

The anchor resolver is the whole of the logic worth testing here: given a
document snapshot and an anchor, produce the byte offset to insert at.

- *tests:* `end` on an empty file, on a file with and without a trailing
  newline (the two produce different offsets and only one is right);
  `start`; `line(n)` in range; `line(n)` past the end clamping to `end`
  rather than erroring, per design §3.
- *doc:* the fragment already carries the contract; this slice adds the
  variant's own doc comments.

### XF.2 — open-or-reuse, in the background 📝

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

- *tests:* an already-open buffer is reused and its unsaved content is
  the base the edit applies to; a background open lands in `:ls` and
  leaves the active buffer alone; a nonexistent file with an existing
  parent opens empty; a nonexistent parent is an error; a directory is
  an error.
- *the failure mode this slice exists to prevent, as its own test:* open
  a file, modify it without saving, then write to it by path — the
  modification must still be there afterwards.

### XF.3 — insert-then-cut, as one unit 📝

The applier. Resolve the anchor, apply the insert through
`Editor::apply_targeted_edit` (which already routes active-document vs
peer-buffer), and only on success apply `cut` to the buffer the action
ran in.

- *exit:* a `WriteToFile` with a `cut` moves text between two buffers and
  neither `u` is a cross-buffer undo — each buffer reverses its own half
  (design §5).
- *tests:* the happy path both ways (`cut` present and absent); **a
  failing insert leaves the source untouched**, which is the data-loss
  case and the reason the effect is one effect; an out-of-bounds `cut`
  after a landed insert skips with a `warn!` rather than losing the
  insert.
- *no bench.*

### XF.4 — the WIT effect + the gate 📝

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

- *tests:* a granted path converts; a path outside every prefix becomes
  an `Echo` and never reaches the dispatcher; `../` cannot escape a
  granted prefix; a plugin with **no** `fs` grant writes nothing; a
  not-yet-existing file under a granted prefix is permitted.
- *doc:* fragment §6; plus the note in `plugin-host.md` that this is the
  enforcement shape `OpenProviderView` will reuse.

### XF.5 — a fixture guest, end to end 📝

A `wasm32-wasip2` fixture that returns a `write-to-file` from an action,
driven through a real `Editor` — the whole path, the way
`agenda_source.rs` drives `agenda-guest`.

- *exit:* the fixture's chord moves a line from its own buffer into a
  second file, and the second file's buffer holds it afterwards.
- *tests:* the move; the denial (the same fixture with no `fs:write`
  grant writes nothing and echoes); reuse of an already-open target.
- **Test it the way it fails:** assert the target buffer's content
  *without* dispatching another action first.

### XF.6 — docs, ledger, site 📝

- `docs/user/` — this is a user-visible authority change. A plugin that
  can write beside itself is worth a paragraph wherever plugin
  capabilities are explained (`core-plugins.md`, `init.md`).
- `implementation.md` — the Phase-7/8 surface row, and the org-mode
  section's "what is blocked" paragraph stops being true.
- `plugin-host.md` §capabilities; `org-mode.md` §9's deferral note.
- `slice-plans/org-mode.md` — OM.6b and OM.11 move ⛔ → 📝, and **that
  is when the org plan can finally be archived**, not before.
- Zola: `nav.toml`, sync, search.

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
