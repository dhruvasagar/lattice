# Slice plan — org capture: concurrent, savable, stackable

Design: [`../../architecture/org-capture-drafts.md`](../../architecture/org-capture-drafts.md).
Supersedes parts of [`archive/org-capture.md`](archive/org-capture.md) (OC.7d) and
[`archive/org-roam.md`](archive/org-roam.md) (OR.7c's create-and-insert).

CD.1–CD.3, CD.6a and CD.6b land in **this** tree. CD.4–CD.8 land in
[`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin) and
depend on the host slices being released first. CD.9 is a trailing host slice
that unblocks one deferred row of the design's §8 table.

| Slice | Tree                 | What                                                                                                                    | Status      |
|-------|----------------------|-------------------------------------------------------------------------------------------------------------------------|-------------|
| CD.1  | lattice              | `Effect::FocusBuffer(u32)`                                                                                              | ✅          |
| CD.2  | lattice              | `open-buffer-at-payload` += `content`, `activate-minor`                                                                 | ✅          |
| CD.3  | lattice              | `host-services.delete-file`                                                                                             | ✅          |
| CD.3b | lattice              | `host-services.can-write-file` (design H5)                                                                              | ✅          |
| CD.3c | lattice              | A failed or denied `WriteToFile` stops the rest of its action (H6)                                                      | ✅          |
| CD.3d | lattice              | `Effect::InvokeCommand(command-ref)` (H7)                                                                               | ✅          |
| CD.4  | org-plugin           | File-backed captures; state in the store; simultaneity; **the caller**; target checked at open, cleanup after the write | ✅          |
| CD.5  | org-plugin           | `:org-capture-drafts` picker + `<leader>oC`                                                                             | ✅          |
| CD.6a | lattice              | `open-picker-payload` += `query` (the picker's initial input)                                                           | ✅          |
| CD.6b | lattice              | `host-services.clamp-position` (design H8)                                                                              | ✅          |
| CD.6  | org-plugin           | `create-and-insert` opens a child capture; write-back; regions                                                          | ✅          |
| CD.7  | org-plugin           | `${origin}` back-reference for the no-write-back verbs                                                                  | ✅          |
| CD.8  | org-plugin           | Roam-scan skip; outstanding-caller warning                                                                              | ✅          |
| CD.9  | lattice + org-plugin | H4: a `completion-source` accept hook; `[[` → create                                                                    | ⛔ deferred |

**OC.7d does not get a slice.** It is `Caller { on_commit: None }` and lands
inside CD.4 — see the design §5. The archived plan's OC.7d entry gets marked
done with a pointer here, and the assertion that pins the *wrong* behaviour
today is inverted in CD.4.

---

## CD.1 — `Effect::FocusBuffer(u32)` ✅

Show a buffer by id in the active pane. Design §3 H1.

**Landed.** Host-applied through `Editor::do_focus_buffer`, next to
`BufferNext`, rather than peer-applied: that way the off-keystroke drains get
it too. So the TUI and GPUI arms are entries in the host-applied no-op lists
rather than bodies. The unknown-id check is made before `activate_buffer`,
which would otherwise echo `buffer #N not found`. The WIT case is appended
last. Tests: `lattice-host/tests/focus_buffer_effect.rs` and
`boundary_effect::focus_buffer_round_trips`. The "from a plugin action" case
is exercised end to end by CD.4, whose commit is the first real producer.

**Touches.** `lattice-grammar/src/effect.rs` (the native variant plus its arms
in the mutation / Visual-exit classifiers); `wit/types.wit`;
`lattice-plugin-host/src/boundary_effect.rs` (**both** directions);
`lattice-ui-tui/src/app/dispatch.rs`; `lattice-ui-gpui/src/lib.rs`.

**Behaviour.** A dead or unknown id is a no-op with a `debug!`.

**Tests.** Focus an existing buffer by id from a plugin action; focus a deleted
id and assert no panic and no pane change; round-trip the WIT↔native mapping
both ways.

**Audit before commit.**
`grep -rn "Effect::FocusBuffer" crates/lattice-ui-gpui/ --include="*.rs"` —
an empty grep means GPUI was missed.

---

## CD.2 — `open-buffer-at-payload` gains `content` and `activate-minor` ✅

Design §3 H2.

**The verification failed, and the design survived it.** `do_edit` on a
missing path *refused* (`open error: No such file`), and `lattice newfile`
refused to start. That was a vim divergence, not a constraint. vim 9.2 opens
an empty unmodified buffer, `:w` creates the file, and a missing parent
directory fails only at write time with E212. So the fix was
`Document::open_or_new` and the `"path" [New]` echo, not the eager-write
fallback, and "abort touches no disk" holds as designed. Two TUI tests had
pinned the old behaviour (`edit_unknown_path_emits_error`, and an
`edit_refuses_when_dirty` that only passed because the open failed); both
now assert what they meant.

**Landed.** One host body, `Editor::open_buffer_at`, for the TUI, the GPUI
and the off-renderer drain. The seed is an edit on the new document before
its actor spawns, so syntax is built from the seeded text and the buffer is
modified. Tests: `lattice-host/tests/open_buffer_at_seeds_a_new_file.rs`,
`folds::open_buffer_at_seeds_a_new_file_and_activates_its_minor` (TUI), and
`lattice-core`'s `open_or_new_*`. The `lattice-core` test `tempdir()` got a
counter while I was there: it collided under parallel runs.

**Verify first, before writing anything else.** The applier is
`do_edit(path, force)` (`crates/lattice-ui-tui/src/app/dispatch.rs:1055`). Drive
it against a **nonexistent** path and confirm it opens an unsaved buffer rather
than refusing. If it refuses, stop and re-open the design: the fallback (write
the draft eagerly at capture open) costs the "abort touches no disk" property
and is a design change, not an implementation detail.

**Behaviour.** `content` seeds **only when the file is absent**. `activate-minor`
runs before the buffer is shown, so its keymap is live on the first keystroke
rather than the second — the ordering `open_synthetic_buffer_seeded` documents.

**Tests.** Seed a fresh path and assert content + cursor; reopen an existing
file with `content: Some(…)` and assert it is **ignored**; assert the minor is
active before the first frame. Both renderers, same patch.

---

## CD.3 — `host-services.delete-file` ✅

Design §3 H3. `read-file`'s peer, on the same `fs:write` grant.

**Landed.** The design said `read-file`'s grant, but `read-file` accepts
`fs:read` *or* `fs:write`. Deleting takes a writable prefix specifically, via
a new `grant_permits_write`. A directory is refused rather than removed.
Tests: unit tests in `host_services` (writable grant, absent file, read-only
grant, outside the grant, symlink out, directory), and
`tests/delete_file_seam.rs` through the multiseam fixture's
`multiseam-delete-file` action on the sync linker. The generated
`docs/dev/reference/plugin-api.md` was regenerated.

**Behaviour.** Re-checks the grant host-side. A path outside the grant is `err`
with the host's own boundary message. An absent path is `ok`.

**Tests.** Delete a granted path; refuse an ungranted one; delete an absent path
and assert `ok`; **assert it works from the grammar seam's sync linker** — the
whole reason it is a host seam, and the case a test on the async linker passes
without covering.

---

## CD.3b / CD.3c / CD.3d — commit safety (added 2026-09-17)

Not in the original plan. Found while starting CD.4: design §6 assumed a failed
target write would leave the draft on screen, but the host applied every effect
regardless, and the guest's cleanup host calls would run *before* the write was
applied. So a failed filing would have deleted the draft file and its state and
closed the buffer. The same path already lost text in the synthetic-buffer
capture whenever the target was outside the grant.

emacs's `org-capture-finalize` was read for the answer (design §3 H5–H7):
the target is resolved at open, and a failed `save-buffer` unwinds the rest of
finalize. Decided with Dhruva: take both halves.

- **CD.3b** — `can-write-file(path)`: the boundary's grant check plus the
  applier's resolution checks, as a query. Tests: each refusal names itself; an
  absent file under an existing, granted directory is `ok`; runs on the sync
  linker.
- **CD.3c** — `apply_write_to_file` returns whether it landed; a failure stops
  the remaining effects of the batch it is in. The authorizer's denial does the
  same (the reversed decision is recorded in `effect_authorizer.rs`). Tests: a
  failed write leaves a following `BufferDelete` unapplied; a denied write drops
  its later siblings; effects *before* the write still apply; a successful write
  changes nothing.
  *Landed:* a failed **save** does not stop the batch. The text has landed
  in the target buffer, which `:q` guards, and a retry would file the entry
  twice.
- **CD.3d** — `Effect::InvokeCommand { id, args }`, host-applied through the
  picker's invoke path (action with typed args, else ex line). Tests: an action
  and an ex-command each run from an effect; one after a failed write does not.

## CD.4 — File-backed captures, store-backed state, simultaneity, the caller

Design §§2, 4, 5, 6, 12. The largest slice; one slice because identity, state
and the buffer's substrate cannot change independently.

**Options.** `org.directory` (new); `org.capture-drafts-directory` (default
`{org.directory}/captures`, falling back to `org.capture-file`'s directory).
Neither set → capture works, `:w` refuses **and says why**.

**Identity.** `hash6` over `(prefix, key, title-if-any, counter)`, re-rolled
while live. Buffer `*org-capture:{key}:{hash6}*`, file `{drafts}/{hash6}.org`.

**State.** Delete `PENDING_CAPTURE`. `CaptureState { dest, label, caller }` under
`capture/{hash6}`. `Caller { buffer, path, at, on_commit }` — `on_commit` stays
`None` for every path in this slice; CD.6 is what first sets it.

**Open** checks the target with `can-write-file` (CD.3b), then opens through
`Effect::OpenBufferAt` (CD.2). **Commit** returns the write, the caller focus,
`BufferDelete`, and `invoke-command("org-capture-cleanup", [id])` last, in the
order design §6 fixes. It makes no mutating host call itself. **Discard**
cleans up directly and focuses the caller without filing.

**Tests.**
- two captures of the *same* template open two buffers and two files;
- commit the **second** first; the first is untouched and still committable —
  the property a global stack would not have;
- `:w`, close, reopen the file, commit: the entry lands at the right target
  (the restart path without restarting);
- discard before any `:w` leaves nothing on disk;
- discard after `:w` deletes the draft file;
- commit deletes the draft file **and** the buffer;
- **OC.7d:** a plain `<leader>oc` fired from a file returns focus to that file
  on both commit and discard. Invert the assertion that pins the old behaviour.
- a target outside the grant is refused **at open**, and no buffer opens;
- a target that becomes unwritable after open: `C-c C-c` echoes the failure,
  and the buffer, the draft file and the store entry all survive; fixing the
  cause and committing again files it.

**Docs.** Amend `org-capture.md` §8 — "One capture in flight" and "Aborting
creates nothing" are now wrong. Point them at the new page rather than editing
them into agreement, so the history stays readable.

**Landed** (`lattice-org-plugin`).
- `capture_drafts.rs` holds the pure half: the id (FNV-1a over prefix, key,
  title, a per-seam counter and the clock, re-rolled while the store or the
  disk has it), drafts-directory resolution, the path test, and the msgpack
  encoding.
- `CaptureState` replaced `PENDING_CAPTURE`, and the `Target` family is
  serde.
- **Drafts directory:** `capture-drafts-directory`, else
  `{org.directory}/captures`, else `captures/` beside `capture-file`. With
  none set, drafts go to an unwritable sentinel,
  `/set-org.directory-to-save-capture-drafts/`. The capture works, `:w`
  fails with that path in the error, and opening echoes the fix. It is a
  path rather than a synthetic buffer, so identity-by-path holds everywhere.
- **Grant requirement:** the drafts directory must sit inside the plugin's
  `fs:write` grant, or cleanup reports it could not delete the draft.
  Capture also needs `state:write`, which the shipped manifest already has.
  Without it the capture refuses to open rather than opening something it
  could never file.
- **Roam creates** go through the same drafts with no caller yet (CD.6/CD.7).
- **Buffer names** in the tests became path lookups (`*org-capture:t*` no
  longer exists). The roam harness stopped applying host-applied effects a
  second time; it had been closing two buffers per commit.
- **OC.7d:** the archived `org-capture` plan has no OC.7d row to mark, so
  there was nothing to update there. The inverted assertion in
  `the_prompt_the_menu_opens_actually_files_the_note` is the record.

Tests added (`org_structure.rs`): two captures of one template committed in
either order; save → close → reopen → commit; discard before and after `:w`;
discard returns to the caller; a target outside the grant refused at open;
a commit whose target directory vanished keeps the draft, its file and its
state, and succeeds on retry.

---

## CD.5 — The drafts picker ✅

**Landed** on `<leader>oC`, not the planned `<leader>od`, which is org-mode's
deadline. The universal minor's binding loses to the major inside an org file,
which is exactly where drafts get resumed. Decided with Dhruva.
- **Rows:** `label: first line of the draft`, or `label: (not saved)`,
  annotated with the id.
- **Accept:** routes `invoke-command org-capture-resume <id>`, which opens
  the file with `activate-minor`.
- **Empty store:** the chord echoes `org: no capture drafts` and opens
  nothing.
- **No `<C-d>`:** discarding deletes a file, which that key must never do.
- **Test harness:** the org defaults attach to a newly opened org buffer on
  the tick after its major is entered, so `open_t` settles them.
- **Bench:** `plugin_store/capture_drafts/{1,10,100}` (`benchmarks.md`).


Design §7. `:org-capture-drafts` + `<leader>od`, `store_keys("capture/")`,
reopen with `activate-minor`.

**Drive the chord in a test.** `<leader>od` must be pressed, not read off the
keymap — `org-capture.md` §6 records `<C-x>o` shipping in a design doc despite
being unfirable, found only by driving it. Assert the major's `<leader>oh` still
resolves alongside it.

**Tests.** An empty store yields a picker that *says* so rather than looking
broken; a saved-and-closed draft is listed with a legible label; accepting a row
opens the file with the minor active and `C-c C-c` files it.

---

## CD.6a — a picker can open on a seeded query ✅ (added 2026-09-17)

CD.6's region parity needs the node picker to open **filtered to the
selection**, which is org-roam's `completing-read` initial input. No host path
did that for a static source: `initial_query` belongs to a live source's own
spec. `open-picker-payload` gains `query: option<string>`, appended last.
`Editor::pending_picker_query` carries it from the open to the seat, where it
takes precedence over a live source's own initial query. A refused open clears
it, the same rollback the fill target has (YR.6). TUI and GPUI both pass it
through.

**Tests** (`a_picker_opens_on_a_seeded_query.rs`): the seed is in the prompt
with the caret after it, and the rows equal clearing the prompt and typing the
seed (compared rather than listed, because fuzzy matching over tempdir paths
can subsequence-match a short seed). No seed gives an empty prompt, and a
refused open leaves no seed for the next picker.

---

## CD.6b — `host-services.clamp-position` ✅ (added 2026-09-17)

Found while planning CD.6. The plan's "clamped" write-back had no seam to clamp
with: `source-line` serves only multibuffer sources, and `apply-edit` drops a
stale or closed target with a `debug!` line. See design H8 for the shape and
the two rejected alternatives.

The buffer store gets its own slot on the plugin host (`set_buffer_store`),
wired by the loader and stamped into every store, rather than borrowing
`project`'s: whether a buffer exists must not depend on project resolution
being wired. `WiredSeams::buffer_store` joins the boot pin.

**Tests.** `clamp_position_on_the_sync_linker.rs` goes through the multiseam
fixture on the SYNC linker, where a commit asks. It covers a position kept, a
line past the end, a byte past the end (stopping before the newline), the empty
line after a trailing newline, an unknown buffer and an unwired host. A unit
test in `host_services.rs` covers an empty buffer, byte-versus-character length,
and range order. `boot_regression_pins.rs` confirms the loader wires it.

---

## CD.6 — `create-and-insert` opens a child capture ✅

**Landed** (org plugin `5ebf2ad`), on CD.6a (the seeded query) and CD.6b
(`clamp-position`), both found missing while planning this slice. What differs
from the plan below:

- **The origin lives in the plugin store**: `capture-origin` for the picker's
  origin, and `capture-caller/<id>` for the parked caller. A guest
  thread_local is per seam. The id rides the template chooser as a third
  argument. Leftover parked callers are cleared at the next
  create-and-insert.
- **The region form is its own action**, `org-roam-insert-node-region` on
  Visual `<C-c>ni`, because only an action receives the selection. Visual's
  inclusive end is made exclusive by one character.
- **"Commit the middle child before the innermost" cannot land the innermost
  link.** Filing the middle closes the innermost's caller, which is the
  closed-caller case. The any-order test uses siblings instead; design §8 now
  states exactly what the property is.
- **The harness needed two fixes**: `OpenPicker` through
  `open_picker_for_effect`, and `set_buffer_store` on the hand-built loader.
  The second was the "harness bypasses install" trap: unwired, every caller
  read as closed.
- **Found along the way:** `delete-file` refused never-saved drafts under a
  symlinked grant (lattice `ee38466c`).

Tests: the eight below, all in `tests/org_roam_index.rs`, plus "without
templates, still one step".

---

### As planned

Design §8, §10. The write-back half.

`:org-roam-create-and-insert` stops writing the note and runs the existing roam
create flow. The origin — buffer id, path, **selection-or-cursor as a `Range`** —
is stashed when the **picker opens** and claimed by `RoamDraft::open` on a
**node-id token match**.

Region parity: an active selection seeds the picker title and is **replaced** by
the link on commit.

**Tests.**
- depth 3: capture → `C-c n i` create → `C-c n i` create; commit innermost out,
  and assert each link lands in its own caller and focus walks back;
- commit the middle child **before** the innermost — allowed, and the
  innermost's link still lands (the any-order property);
- discard a child: the caller regains focus, no link, no note;
- **`<Esc>` the template chooser after `create-and-insert`, then fire a plain
  `<leader>oc t`: assert the plain capture writes no link anywhere.** The token
  property, and the exact bug a bare thread-local would have;
- an active selection is replaced by the link, and seeded the title;
- caller buffer deleted before the child commits: the note is filed, the echo
  names it, nothing panics;
- `at` past the caller's current end: clamped, note filed, link present;
- caller is a **plain org file**, not a capture: identical behaviour.

---

## CD.7 — `${origin}`, the backward reference ✅

**Landed** (org plugin `5ed3237`). What differs from the plan below:

- **The reference to a draft without an `:ID:` is a link to its target
  file**, not `%a`. A draft's file is deleted when it is filed. Design §9 was
  amended.
- **Placement is found with a sentinel**: the draft is built with a
  private-use marker for `${origin}`, so a reference placed in `body`,
  `body-file` or `head` is detected alike. The target path is filled
  without it.
- **"From a capture" is `capture_state_of`**: the path names a capture, and
  the store confirms it.
- **The callerless finalize was missing** and is built here:
  `open_on_commit`, with serde default.
- **Nested `C-c n c` is not tested**, because lattice has no
  `org-roam-capture` verb. When it lands, it parks a caller the same way.

---


Design §9. The no-write-back half.

`${origin}` joins OR.11a's `${title}` / `${slug}` / `${id}`, expanded in
`roam_capture::expand_fields` — before `capture::expand_with`, for OR.11a's
reason. Option `org.roam-capture-reference-origin`, default on. When on and the
template names no `${origin}`, append a `Reference: <link>` line.

Fires for: nested `C-c n f` → Create, and nested `C-c n c`. **Not** for
`C-c n i` → Create (the forward link already exists) and not for the non-nested
cases (no caller).

"From a capture" is `document.path()` under the drafts directory — a path test,
not carried state.

**Tests.**
- nested `C-c n f` → Create: the new note carries `[[id:…]]` to the caller, and
  **no** link is written into the caller;
- nested `C-c n c`: same;
- `C-c n i` → Create: forward link only, **no** `${origin}` appended — the
  "exactly one link" rule, and the one a copy-paste of the other two would break;
- caller is a plain org heading with no `:ID:`: the reference is the **`%a`
  link**, and the origin file is **not modified** (no id minted);
- a template naming `${origin}` gets it there and gets **no** appended line;
- option off: no reference anywhere;
- non-nested `C-c n f` → Create opens the new note (emacs) and records no caller.

---

## CD.8 — Scan interaction and the outstanding-caller warning ✅

**Landed** (org plugin `635e899`), as planned. `roam_scan::indexable` guards
both the cold walk and the watcher. The warning fires on discard as well as on
commit. The agenda pin rides the existing `agenda-files` test. The roam
harness now sets the drafts directory before the roam directory, since the
latter starts a scan.

**A load flake to watch:** the roam suite's `pick` helper failed twice under
concurrent load (once during a `zola build`), after a find-picker create, then
passed three consecutive full runs. Its panic now reports the picker, the
chooser, any pending build, the prompt and the open buffers, so the next
occurrence names its cause.

**Docs:** there is no separate `docs/user` page. `docs/user/org.md` is thin by
design, since the plugin's `doc/org.md` and `doc/roam.md` are the reference
and ship as `:help`. Its Capture and Roam rows name the drafts and nesting
surfaces instead.

---


Design §11.

- `roam_scan`'s walk skips the drafts directory, alongside `is_org(p)`. Test
  with `org.roam-directory` deliberately set to the drafts directory's parent.
- Committing a capture that is some other draft's caller echoes how many
  write-backs will not arrive. Scan `store_keys("capture/")` rather than
  maintaining a child list — no bookkeeping to leave stale.
- **No agenda work.** `walk_candidates` is already `max_depth(Some(1))` (OA.0d).
  A test pins it anyway: a draft under `{org.directory}/captures/` does not
  appear in `gr`.

---

## CD.9 ⛔ deferred — H4, a `completion-source` accept hook

Design §3 H4. Deferred, not dropped: it is the one row of the §8 verb table that
cannot be built, and it comes back when the ABI question below is answered.

`wit/completion-source.wit` is generator-only by a decision recorded as "option
A, locked with Dhruva" — a guest produces candidates and the host inserts the
text; there is no accept hook. The locking argument was about **per-candidate**
calls on the synchronous keystroke pipeline, which an accept hook does not make:
an accept fires once, on a user-initiated event, off that path — the shape
`picker-accept-outcome` already has.

**Open question that gates it.** What may a guest return from an accept? The
narrow answer is a `picker-accept-outcome`-shaped routing token, reusing the
`RoutingPayload::InvokeCommand` machinery `roam_insert` already uses. The wide
answer is a `list<effect>`, which is a much larger ABI surface on an interface
whose whole design is about *not* letting a guest near the keystroke path.

Revisit when `[[` → create is wanted enough to answer that; `C-c n i` covers the
intent meanwhile.

---

## Benches

`store_keys("capture/")` grows with the number of live drafts and is read on
every drafts-picker open and every caller-bearing commit. Bench at 1 / 10 / 100
drafts and record in `benchmarks.md`. Nothing else here is on a hot path —
capture open and commit are user-initiated single events.

## Docs to update as slices land

- `org-capture.md` §8 — the two "known gaps" (CD.4).
- `org-roam.md` §5 — create-and-insert's new shape (CD.6), `${origin}` (CD.7).
- `plugin-host.md` — the seams (CD.1–CD.3, and CD.9 if it lands).
- `wit/types.wit`, `wit/host-services.wit`, `wit/completion-source.wit` doc
  comments.
- ~~A user page in `docs/user/`~~. Settled at CD.8: the user-facing surfaces
  are documented in the plugin's `doc/org.md` / `doc/roam.md`, which ship as
  `:help`. `docs/user/org.md` stays thin by design and names them in its
  Capture and Roam rows, and the site sync and `zola build` were run for that
  edit.
