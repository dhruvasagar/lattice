# Org capture: many at once, savable, and stackable

> **Where the code is.** The capture logic this page describes is implemented in
> [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), a
> **separate repository**. What lives in *this* tree is a small set of generic
> host seams (§3) that name no org concept. See
> [`plugin-host.md`](plugin-host.md).

**Status:** 📝 planned. Extends [`org-capture.md`](org-capture.md) §8 and
[`org-roam.md`](org-roam.md) §5, both of which this page contradicts in places —
where they disagree, this page is newer and says so explicitly. Slice plan:
[`../operations/slice-plans/org-capture-drafts.md`](../operations/slice-plans/org-capture-drafts.md).

## 1. What is wrong with one capture at a time

`org-capture.md` §8 shipped the capture buffer and closed with two "known gaps"
stated as acceptable:

> **One capture in flight.** The finalize handler recovers its target from
> guest-side state, because the action context carries a `buffer-id` but no
> buffer *name* […] Emacs's default is likewise one.

That defence does not survive contact with how capture is used. Ideas do not
queue. You are part-way through writing a todo, a second unrelated thing occurs
to you, and the choice today is to lose one or to finish the first one badly.
"Emacs's default is likewise one" was also not quite true — emacs keeps its
capture plist **buffer-local** (`org-capture-current-plist`), which is precisely
why several capture buffers can coexist there.

The second gap is sharper, and it is the one the rest of this page turns out to
be about:

> **The pane does not return to where the capture was fired from.** […] A guest
> cannot fix it: `switch-buffer` is a picker-accept outcome, not an `effect`.
> Tracked as OC.7d.

Three things are wanted:

- **Simultaneity.** Every capture gets its own buffer; any of them can be
  committed, in any order, or never.
- **Drafts.** A capture can be *saved* without being committed, survive the
  session, and be resumed later.
- **Stacking.** A roam capture started from inside another capture is a child;
  committing it returns you to the parent, and — depending on which verb you
  used — either writes a link into the parent or writes a reference back to it.
  To arbitrary depth.

## 2. The move that makes the first two fall out

**The capture buffer becomes a real file**, at
`{org.capture-drafts-directory}/{capture-id}.org`.

Everything else follows from that decision rather than being built:

| Wanted | What provides it |
|---|---|
| save a draft | `:w`. It is a file buffer. Nothing is implemented. |
| resume tomorrow | the file is on disk; the state is in the plugin store (§5) |
| "which capture is this?" | `document.path()` — **already in the WIT** |
| dirty-buffer protection | the host's existing `:q` guard, for free |
| `org-mode` grammar in the draft | `.org` resolves the major from the extension |
| "is my origin a capture buffer?" | its path is under the drafts directory (§8) |

The identity line inverts what looked like the blocking problem. The guest
**cannot** learn which buffer it is in today: `action-context` carries
`buffer-id` but the guest never learns the id of a buffer it opened, `document`
has no `name()`, and there are no buffer-lifecycle events to subscribe to. The
obvious fix was to add `document.name()`.

A file-backed draft needs none of it. `document.path()` has existed since OM.6b
and the capture id is the basename. **Persistent drafts removed a host change
rather than adding one** — which is also why simultaneity and drafts are one
design and not two: a synthetic-buffer simultaneity slice would add
`document.name()` and then take it back out.

### What this costs, said plainly

Aborting **no longer creates nothing by accident**. `org-capture.md` §8 called
that out as something that "falls out of the buffer model rather than being
cleaned up". It now falls out only for an *unsaved* draft: the buffer opens on a
path that does not yet exist, so `C-c C-k` before any `:w` still touches no
disk. Once you save, abort has something to delete, and it deletes it (§6). The
property degrades from structural to enforced-and-tested. That is a real loss,
accepted for what drafts buy.

## 3. The host seams

All generic; none names an org concept. Every one needs TUI **and** GPUI arms in
the same patch.

### H1 · `Effect::FocusBuffer(buffer-id)`

Show a buffer by id in the active pane.

**The missing peer of `apply-edit`'s `target: u32`.** A guest can today *edit*
any buffer by id and cannot *show* one; `open-buffer` takes a path and
`open-synthetic-buffer` takes a name, so neither serves a buffer the guest knows
only as an id — which is the only form a caller is known in.

It also closes OC.7d, pinned today by an assertion asserting the *wrong*
behaviour so that the day it changes a test says so. That day is this design.

A dead or unknown id is a **no-op with a `debug!`** — an effect naming a buffer
that closed between the action and its application is an ordinary race, not a
guest bug. Never `info!`, per the diagnostic-log rule.

### H2 · `open-buffer-at-payload` gains `content` and `activate-minor`

```wit
record open-buffer-at-payload {
    path: option<string>,
    position: position,
    force: bool,
    /// Seed text, applied only when the file is NOT on disk.
    content: option<string>,
    /// A minor to activate alongside the major the path resolves.
    activate-minor: option<string>,
}
```

**OC.7a applied to the file-backed path**, with OC.7a's argument verbatim: the
`modes` WIT seam is declaration-only, so a plugin mode has no `on_activate` hook
and a guest would otherwise get a buffer it could not put a character into. That
reasoning never depended on the buffer being synthetic; it was only ever
exercised there.

`content` seeds **only when the file is absent**, mirroring
`open-synthetic-buffer-payload`'s existing rule and for the identical reason —
reopening a saved draft must never clobber what was typed into it.

> **To verify in the first slice.** The applier is `do_edit(path, force)`, i.e.
> `:e`. `:e` on a nonexistent path is expected to open an unsaved buffer, as in
> vim. If it refuses, the fallback is to write the draft eagerly at capture
> open — and that costs the "abort touches no disk" half of §2, so it is a
> design change, not an implementation detail.

### H3 · `host-services.delete-file(path) -> result<_, string>`

Gated on the same `fs:write` grant capture already needs in order to write, so
this costs no new capability. Nothing in the tree can delete a file today.

**It must be a host seam rather than WASI**, for `read-file`'s already-recorded
reason: finalize is a *grammar action*, which the host calls synchronously from
a separate sync linker, and `wasmtime-wasi`'s sync filesystem shim blocks on a
runtime internally — a guest `std::fs::remove_file` there panics rather than
deleting. `read-file` is the precedent; this is its peer.

Deleting a path that is not there is `ok`, matching `store-delete`'s "a
retraction that has already happened is not an error".

### H4 · An accept hook on `completion-source` — deferred, and why it is not free

Making the `[[` completion source able to *create* a node (and therefore open a
capture) is **not possible today**. `wit/completion-source.wit` is generator
only: a guest produces `raw-candidate`s and the host inserts the text. There is
no accept hook and no routing token — no equivalent of `picker-accept-outcome`.

The interface says why, and it is a locked decision:

> **Generator only, by design (option A, locked with Dhruva).** […] `matches` +
> `annotate` run *per candidate* on the synchronous keystroke pipeline, so
> crossing them to an async, actor-bound guest per item would fire hundreds of
> boundary calls per keystroke (paramount #1).

**That argument does not reach an accept hook.** An accept fires once, on a
user-initiated event, off the keystroke path — the same shape
`picker-accept-outcome` already has and pays for. So the decision is reopenable
rather than contradicted. But it is a new export on a locked interface with its
own ABI question (what a guest may return), so it is a **separate trailing
slice**, not part of this plan's spine. `C-c n i` covers the same intent
meanwhile.

### H5 · `host-services.can-write-file(path) -> result<_, string>`

Would a `write-to-file` of this path from this plugin land? It checks the same
grant test the boundary applies (`EffectAuthorizer::permits_write`), and the
same resolution the applier does: not a directory, parent present, existing
content readable. `err` names which check failed.

Capture calls it **when the capture opens**, so a misconfigured target is
reported before anything is typed. That is emacs's order:
`org-capture-set-target-location` visits the target file and finds the heading
before the capture buffer exists, and any error there aborts the capture with
`Capture template 'x': …`.

### H6 · A failed write stops the rest of its action's effects

`apply_write_to_file` reports whether the write landed. When it did not —
including a write the boundary **denied**, which it replaces with an echo —
the effects after it in the same action are **not applied**. The ones before
it are.

This is emacs's finalize. `org-capture-finalize` calls `save-buffer`, and an
error there unwinds the rest: the capture buffer is not killed and the window
layout is not restored, so the user is still looking at their text. Before H6,
lattice applied every effect regardless: a capture whose target was outside the
grant echoed a denial and then closed its buffer, and the text was lost. That
was a live bug in the synthetic-buffer capture, not only a hazard for drafts.

It reverses a recorded decision in `effect_authorizer.rs` ("the rest of a
`Many` is preserved: one denied write must not silently cancel the other things
an action did"). That was right for independent effects and wrong for a
*commit*, where the later effects presume the write happened. An action whose
effects really are independent should not put a write first.

### H7 · `Effect::InvokeCommand(command-ref)` — run a command after the effects before it

The picker's `invoke-command` outcome, available as an effect: a registered
action (dispatched with its typed args) or an ex-command. It is applied in
sequence, so it runs only if every effect before it did — which, with H6, means
only after a successful write.

Capture needs it because its cleanup (`store-delete`, `delete-file`) is made up
of host *calls*, and a guest's host call runs **during** the action — before
the host applies any effect the action returns. Cleaning up there would delete
the draft and its state before the write was even attempted. Commit therefore
returns its cleanup as `invoke-command("org-capture-cleanup", [id])` after the
write; if the write fails, H6 skips it and the draft survives intact.

### Not added: `document.name()`

Considered and dropped once the buffer became file-backed (§2). Recorded because
it is the obvious first answer to "which capture is this?" and someone will
propose it again.

## 4. Identity and naming

```
file     {drafts}/{hash6}.org
```

`hash6` is the first six hex characters of a hash over
`(prefix, template key, title-if-any, monotonic counter)`, re-rolled by bumping
the counter while the id is already live.

The title feeds the hash where there is one. It cannot be the *whole* input:
two captures of the same title must still differ, and the counter is what makes
uniqueness a guarantee rather than a probability. No host call is involved and
there is no failure path — buffers are session-scoped, so session-scoped
uniqueness suffices, and `new-uuid` can fail where a counter cannot.

**The property this deletes.** `org-capture.md` §8 named the stable name as
deliberate: "stable so re-firing the same template returns to the note in
progress rather than starting a second". Re-firing now starts a second. That is
the trade simultaneity *is*, not a side effect of it.

**A draft has no synthetic buffer name.** It is an ordinary file buffer, and
`:ls` shows it by its path. (An earlier draft of this page named the buffer
`*org-capture:{key}:{hash6}*`; a file buffer has no such slot, and giving it one
would be the host change §2 set out to avoid.)

**The cost this creates.** `captures/a3f9c1.org` in `:ls` says nothing about
which draft it is. That is why the drafts picker (§7) is load-bearing rather
than a convenience — it is the front door, and `:ls` is not.

## 5. Where a capture's state lives

Not in guest memory. `host-services.store-put/get/delete/keys` already exists,
org already holds `state:write`, and the store **persists across sessions** —
which drafts require, because a draft resumed tomorrow must still know where it
files.

```
key    "capture/{hash6}"
value  msgpack(CaptureState)
```

```rust
struct CaptureState {
    /// Unchanged from OR.11b: where the finalized text lands, and `:clock-in`.
    dest: CaptureDestination,
    /// The template's description — **fixed for the capture's life**. The
    /// picker pairs it with the draft's current first non-empty line, read
    /// from the file when the picker opens. Storing that line here instead
    /// would go stale the moment you typed, and a picker row naming a
    /// sentence the draft no longer contains is worse than no row detail.
    label: String,
    /// Where this capture was fired from, and what — if anything — to write
    /// there when it commits. `None` for a capture with no caller (§8).
    caller: Option<Caller>,
}

struct Caller {
    /// The fast path: valid this session. `FocusBuffer`'s argument and
    /// `apply-edit`'s target.
    buffer: u32,
    /// The slow path: survives a restart, when the caller had a path at all.
    path: Option<String>,
    /// Collapsed = an insertion point. Non-empty = a **region the write-back
    /// replaces** (org-roam-node-insert with an active selection, §10).
    at: Range,
    /// `Some` only when committing should write something back — org-roam's
    /// link. `None` means "return focus, insert nothing", which is what a
    /// plain org capture and a nested `C-c n f` / `C-c n c` both want.
    on_commit: Option<String>,
}
```

`PENDING_CAPTURE`'s single `Option` slot is deleted outright. Nothing about "one
capture in flight" survives it.

### The two axes, and why they are one record

A caller answers two independent questions — *where do I land when this ends*
and *does anything get written there* — and separating them is what lets plain
org-capture and org-roam share one mechanism instead of roam inventing
"stacking" as a private concept:

- Plain `<leader>oc` sets a caller with `on_commit: None`. Committing files the
  entry and returns you where you fired it. **That is OC.7d, obtained for free
  rather than as its own feature.**
- `org-roam-node-insert` → Create sets `on_commit: Some(link)`.
- A nested `C-c n f` → Create or `C-c n c` sets a caller with `on_commit: None`.

Nothing in the substrate knows what a link is. `on_commit` is a string. If plain
org ever grows a link-inserting verb, it registers a caller with a payload and
needs no new mechanism — which is what "do not limit this to org-roam" buys
without building anything speculative.

### Why `on_commit` is captured at open rather than re-derived at commit

The description half of a roam link is the title *as typed into the picker*, and
`roam_insert::link_for` already argues why: "the description is part of the
SENTENCE you are writing — you may want 'the chicken recipe' where the node is
titled 'Honey Garlic Chicken Breast', and a link that rewrote itself would edit
your prose." Emacs agrees (`org-roam-capture--get :link-description`).

So editing `#+title:` inside the draft retitles the note and leaves the caller's
prose alone. That is the correct asymmetry, not an oversight.

## 6. The flows

**Open.** Expand the template, then

```rust
Effect::OpenBufferAt {
    path: draft_path,
    position: /* the %? point */,
    content: Some(expanded),
    activate_minor: Some("org-capture-mode"),
}
```

plus a `store_put`. The major is not named: `.org` resolves `org-mode` from the
extension, and a capture buffer wanting org's grammar, motions and folding is
what `org-capture.md` §8 already argued for.

**Save.** `:w`. There is nothing to implement, and that is the point of §2.

**Open, validated first.** Before the buffer opens, `can-write-file` (H5) is
asked about the resolved target. A refusal is echoed as
`org: capture template 't': <reason>` and **nothing opens** — the emacs order,
where the target is visited and located before the capture buffer exists.

**Commit — `C-c C-c`.** `doc.path()` → basename → `hash6` → `store_get`. The
action performs **no** host call that changes anything; it returns, in order:

1. file the entry into the target (unchanged from OC.5a/OC.11);
2. if `caller.on_commit` — `Effect::ApplyEdit` replacing `caller.at` in
   `caller.buffer` with the link;
3. `Effect::BufferDelete(true)` — **before** the focus, because it closes the
   *active* buffer, and after step 4 that would be the caller;
4. if `caller` — `Effect::FocusBuffer(caller.buffer)`; if no `caller` — run the
   verb's own finalize instead (roam's `find-file`, §8);
5. `Effect::InvokeCommand("org-capture-cleanup", [hash6])`, whose action does
   `store_delete` and `delete-file(draft_path)`.

The target write goes **first**, and H6 is what makes that ordering mean
something: if it fails, steps 2–6 are skipped, so the draft stays on screen,
its file and its state stay as they were, and `C-c C-c` can be retried once
the cause is fixed. The target was also checked at open (H5), so this is the
rare case — a file that became unwritable while the capture was open.

Cleanup is last and is its own action because a host *call* made inside the
commit action would run before the host applied the write — see H7.

**Discard — `C-c C-k`.** `store_delete`, `delete-file` (absent is `ok`),
`BufferDelete`, and `FocusBuffer(caller.buffer)` when there is a caller. Nothing
is filed and nothing is written back. Discard has no write to wait for, so its
host calls run directly: there is nothing for them to run ahead of.

**Neither is compulsory.** A capture with a caller is not a modal lock: you can
`:w` it, walk away, work in the parent, open a third capture, and come back
through the picker hours later. The caller says where you *land* when the
capture ends — not that it must end now. This is the whole reason the state is
in the store rather than on a stack.

## 7. Resuming a draft: the picker is the only door

`:org-capture-drafts`, on `<leader>od`. `store_keys("capture/")` gives one row
per draft, labelled from `CaptureState.label`, routed to an ex-command that
reopens the file **with `activate-minor`** so `C-c C-c` is live.

Opening a draft any other way — `:e`, the file picker — gives a plain org file
with no capture chords. Honest but silently inert, and the accepted cost of not
growing a path-matching `ActivationPolicy` (the variants today are
`manual | global | universal | majors`, so a fourth would be a fourth host
change for a door the picker already opens).

> The `<leader>od` chord must be **driven in a test**, not read off the keymap.
> `org-capture.md` §6 records the precedent: `<C-x>o` was specified, shipped in
> a design doc, and could never have worked, because org's major already binds
> a terminal `<C-x>`. "The collision was found by driving the real chord in a
> test rather than by reading the keymap."

## 8. Stacking: which verb does what

The rule the table encodes: **a stacked capture creates exactly one link** —
forward (caller → new note) when the verb inserts, backward (new note → caller)
when it does not. Never both, never neither.

| Verb | Child capture | Caller | `on_commit` | `${origin}` back-reference |
|---|---|---|---|---|
| `<leader>oc` (plain org-capture) | — | **always** | none | no — not a roam note |
| `C-c n i` → an existing node | no, inserts now | — | — | — |
| `C-c n i` → **Create** | yes | yes | **the link** | no — the forward link exists |
| `C-c n f` → **Create**, from a capture | yes | yes | none | **yes** |
| `C-c n f` → **Create**, from elsewhere | yes | **no** | — | no — opens the note |
| `C-c n c`, from a capture | yes | yes | none | **yes** |
| `C-c n c`, from elsewhere | yes | **no** | — | no |
| `[[` completion → create | 📝 blocked on H4 | | | |

**"From a capture" is a path test, not extra state.** The origin is a capture
buffer exactly when `document.path()` is under the drafts directory. No flag has
to be carried and none can go stale.

**Non-nested `C-c n f` opens the new note, which is emacs.**
`org-roam-node-find` uses `:finalize 'find-file` — you asked for a note and you
are taken to it. The substrate states this as one rule rather than a roam
special case: *a capture with a caller returns to it; one without runs the
verb's own finalize.* Plain org-capture always has a caller, so it always
returns.

**The caller works identically when the parent is not a capture.** A caller is a
buffer id, a path and a range; writing a link back into a normal org file — or a
non-org buffer — is the same operation. Emacs deliberately allows this too:
`org-roam-node-insert` works wherever you are.

### The stack is a parent pointer, not a stack

Each capture holds `Option<Caller>` naming *its* caller. There is no global
stack anywhere.

This is strictly better than emacs and the difference is visible:
`org-roam-capture--info` is a **dynamic variable**, so a nested roam capture
clobbers its parent's. Per-capture callers give unbounded depth *and* let
captures be committed in **any** order — the innermost need not go first,
because nothing depends on a stack discipline being maintained.

### `:org-roam-create-and-insert` stops writing the note

Today it mints an id, writes the file with `save: true`, and inserts the link in
one hop. `%?`, `%^{…}` and `${…}` therefore never get a surface — the same
defect OR.11b fixed for `:org-roam-create-node` and did not fix here. It now
runs the **existing** roam create flow (template chooser → questions → capture
buffer). One flow, not a second one that can drift.

### The origin is stashed at picker-open, and claimed by token

Two things the create row needs live on `action-context` at the moment the
**picker is opened**, not when the row accepts: the origin `buffer-id`/`cursor`,
and `selection` (§10). A picker's `init` receives args and a `picker-context` —
no cursor, no document, the wall OR.9 and `roam_insert` both hit.

So the origin is stashed when the picker opens and claimed at accept. Two
defences, because a bare thread-local here is the bug this repo keeps
re-introducing:

- **The open overwrites unconditionally.** It is the one action guaranteed to
  precede any accept — the `PENDING_QUESTIONS` defence, valid for the same
  reason.
- **The resume is keyed to the minted node id.** `RoamDraft::open` claims it
  only on a token match, and the roam flow already threads `id` end to end
  (`QuestionFlowKind::Roam { title, key, id }` → `roam_draft(…, id, …)`). An
  **abandoned** create — `<Esc>` at the template chooser — leaves a stash keyed
  to an id no later capture will present, so it is **inert** rather than stale.
  Without the token, the next plain `<leader>oc t` would consume it and file a
  link into a buffer the user was no longer in.

`org-capture.md` §5 records this failure twice already (`CAPTURE_ORIGIN`,
`PENDING_QUESTIONS`), where the only defence available was "starting a new flow
overwrites unconditionally". Here there is a natural key, so the failure is
removed structurally instead of defended against.

## 9. `${origin}` — the backward reference

In the two cases where nothing is written into the caller, the new note carries
a link **to** the caller. That link is the only connection those two cases
produce, which is why it exists at all.

**It is a roam template field**, joining `${title}` / `${slug}` / `${id}` from
OR.11a. A template that names `${origin}` places it exactly where the author
wants:

```org
:PROPERTIES:
:ID: ${id}
:END:
#+title: ${title}

From: ${origin}

%?
```

**When the option is on and the template does not name `${origin}`**, a
`Reference: <link>` line is appended to the body. Two placement rules is one
more than ideal; the alternative — a fixed site nobody can move — puts the
reference below the caret for every template ending in `%?`, which is worse.

Consumed **at open**, not at commit: it is text in the child's body, so it goes
through `roam_capture::expand_fields` with the other `${…}` fields, before
`capture::expand_with` runs. That ordering is OR.11a's and holds for OR.11a's
reason — user data must not become template syntax.

**What the link points at:**

| Origin | `${origin}` expands to |
|---|---|
| a roam node, or an uncommitted roam draft (it already carries `:ID:`) | `[[id:…][Title]]` |
| a plain org heading, or any other buffer | the **`%a` link**, verbatim — `[[file:path::line][…]]` |
| nothing (no origin buffer) | empty |

**No `:ID:` is minted in the origin.** org-roam's own `node-insert` does mint one
when linking to an id-less heading, and `:org-roam-id-create` exists — but
capturing would then silently edit and dirty a file you were only reading, which
is a side effect well beyond what the verb advertises. The `%a` form is what org
already produces for exactly this question, so there is one annotation format
rather than two.

**One dangling case, named.** Linking to an *uncommitted* draft's `:ID:` works
because id links resolve through the index regardless of file location — the
draft's file moves on commit and the link still resolves. If the parent draft is
**discarded** instead, the child holds a link to an id that never existed. This
is the price of being able to reference a note you have not finished writing,
and the honest alternative (refuse to reference an uncommitted draft) removes
the feature in the nested case, which is the only case it is for.

Gated by `org.roam-capture-reference-origin`.

## 10. Regions, and where the write-back lands

`Caller.at` is a **range**, not a position, and this is full emacs parity for
`org-roam-node-insert`:

- an active selection **seeds the picker's title** with the region text;
- committing **replaces the region** with the link.

That is how the verb is actually used — you select a phrase in your prose and
turn it into a linked note. A collapsed range is an ordinary insertion point, so
one shape serves both.

`at` is **clamped** to the caller's current extent on use. Emacs uses a marker
here (`:insert-at (point-marker)`), which tracks edits and is therefore correct
in every case. Lattice has no guest-visible marker primitive, and adding one — a
mark registry with edit-transform and buffer-scoped lifetime — is a new host
subsystem far beyond this feature that would gate the whole plan on it.

So the recorded range is stale in exactly one situation: you deliberately
switched back to the caller and edited it *while a child capture was open*.
Simultaneity is what makes that possible for the first time, so the risk is
created by this design rather than pre-existing. It is accepted knowingly and
pinned by a test that asserts the clamp rather than an exact offset.

Rejected: inserting at the caller's **live cursor** on resume. It never lands in
stale text, but if the caret moved for any reason the link goes somewhere the
user did not ask for — silently wrong rather than visibly stale.

## 11. Interactions with the scans

**The agenda is already safe, and by design.** `walk_candidates` runs at
`max_depth(Some(1))` — "the files a configured directory names, **one level, not
the subtree**" (OA.0d, which cites emacs's own `directory-files` behaviour). A
drafts directory *under* `org.directory` is therefore invisible to `gr` with no
exclusion mechanism, and a user who wants half-written entries in their agenda
opts in by naming the subdirectory in `agenda-files` — which is how emacs users
do it, and why emacs never needed a recursion flag either.

**The roam scan is not safe and needs one line.** `roam_scan` walks with
`host_services::walk(&root)`, which **is** recursive. A saved roam draft carries
`:ID: ${id}` by construction, so with `org.roam-directory` overlapping the
drafts directory a draft would be indexed as a real node — appearing in
`roam_find` and node-insert as a note whose file then gets deleted out from
under the index on commit. The walk skips the drafts directory, alongside the
existing `is_org(p)` filter.

Note the tension with §9: an uncommitted draft's `:ID:` is *linkable* but the
draft is *not indexed*. That is deliberate. A draft is a note you are still
writing, so it should not appear in "find a node"; a link to it resolves once it
lands, which is when it becomes a node.

## 12. Options

| Option | Default | Why |
|---|---|---|
| `org.directory` | *(unset)* | New. Emacs has exactly this variable and both org-capture and org-roam key off it; lattice has only `roam-directory`, `capture-file` and `agenda-files`, so a drafts path has no base to hang from. |
| `org.capture-drafts-directory` | `{org.directory}/captures` | Where drafts live. |
| `org.roam-capture-reference-origin` | on | §9's backward link. Off means the two no-write-back cases produce no connection at all, which is a legitimate preference. |

With `org.directory` unset, fall back to the directory holding
`org.capture-file`. With neither set, a draft still captures — it just cannot be
*saved*, and the first `:w` **says why** rather than writing somewhere
surprising.

This reverses nothing. `org-capture.md` §2 rejected `org.directory` as a way to
*find a templates file* — "config belongs where the user's config is". A drafts
directory is data, not config, and the rejection does not reach it.

## 13. Paramount-goal alignment

**UX (higher court).** The page is a UX argument throughout: ideas do not queue,
a half-written note must be savable, and a link you asked for must arrive where
you asked for it. The one UX regression this design *creates* — a stale write-back
range after concurrent editing of a caller (§10) — is named rather than
smuggled, and the alternative that avoids it is worse.

**#1 Performance.** Nothing here is on the typing path. Template expansion
happens at capture open, the target read at commit, `store-keys` only when the
drafts picker opens or a caller-bearing capture commits. The one growth term is
`store_keys("capture/")` scaling with live drafts, bounded by how many notes a
human leaves unfiled, and benched rather than assumed.

**#2 Extensibility.** Six host seams, none of which knows what a capture is.
H5–H7 came from reading emacs's finalize (the first slice's review): a guest
cannot sequence its own cleanup after a write it does not apply, and nothing
stopped a batch after a failed write.
`FocusBuffer` completes a pair — `apply-edit` targets a buffer by id, and now
something can show one. `open-buffer-at`'s two fields are OC.7a's argument
applied where it always also held. `delete-file` is `read-file`'s peer on a
grant capture already needs. `on_commit` is a string, so the substrate is
org-agnostic and roam is merely its first payload. The acid test holds: no
`Editor::` method and no host `Action` variant is added.

**#3 Vim modal editing.** A draft is an ordinary file buffer, so `:w`, `:q`, the
dirty-buffer guard and the whole grammar apply with no special case. The
capture-specific pair stays on `org-capture-mode`, a minor, so `C-c C-c` does
not file every org file you touch. Region parity (§10) is Visual mode feeding a
verb, which is what `Range::Selection` is for.

**#4 Asynchronicity.** Unchanged. Capture is synchronous grammar-seam work by
construction, which is exactly why `delete-file` and `read-file` must be host
seams rather than WASI.

**Everything is a buffer.** A capture stops being a synthetic special case and
becomes a file — the more buffer-ish of the two. It gains save, reopen and dirty
tracking by being ordinary, rather than by having each re-implemented.
