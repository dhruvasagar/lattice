# Owner-write caret survival

How a caret survives an edit made by something other than the user's keystroke.

**Status:** design. See `docs/dev/operations/slice-plans/acp-ux-enhancements.md`
(slice OWC) for sequencing.

---

## 1. The problem

A buffer's owner may write into it while the user's caret is inside it. The ACP
conversation buffer is the first place this happens: a background drain projects
the agent transcript into the buffer, whose **last line is an editable prompt**.
When streamed agent output grows the transcript above the prompt, the prompt
line shifts down — and the caret does not follow. It is left stranded, several
lines up, in read-only transcript.

The user keeps typing; the bytes land at the prompt (the read-only gate maps
edits through `EditableTail`), but the caret is drawn somewhere else entirely.

This is not an ACP problem. It is the general question: *what happens to a
position when the text under it moves?* Every editor answers "the position moves
with it". Lattice currently answers "the position stays where it was, and is
clamped if it fell off the end".

## 2. Why the current code cannot answer it

Three facts, each verified:

**Owner writes bypass the host.** `full_replace` / `reproject` call
`handle.apply_edit_batch(..)` on `Arc<dyn lattice_runtime::Document>`, which
sends `ActorMsg::ApplyEditBatch` to the runtime document actor
(`lattice-runtime/src/actor.rs`). The actor calls
`lattice_core::Document::apply_edit_batch` and publishes. The edit never passes
through `Editor::apply_edit_batch_blocking`, so it never sees `Editor::cursor`.
This is deliberate — it is how owner writes bypass the read-only gate.

**Nothing transforms a position across an edit.** There is no `transform`,
`rebase`, or `map_position` anywhere in the workspace. The only thing resembling
one is `Editor::clamp_cursor_to_active_buffer`, a one-directional shrink that
pulls the caret inward when it fell past EOF. When text is inserted *above* the
caret, every branch is a no-op.

**There is no anchor primitive.** `Position` is a plain `(line, byte)`.
`Selection::anchor` is the other end of a visual extent, not a tracked position.
Marks in the position-history ring are immutable snapshots. Nothing rides edits.

## 3. Rejected: transform the caret in the host, on any edit

The obvious move — "when a `DocumentChanged` lands for the active buffer, shift
`Editor::cursor` by the delta" — is wrong, and the reason is instructive.

Keystroke handlers **already** position the cursor themselves, at ~10 sites
(`self.cursor = applied.inserted_range.end` and friends). `apply_targeted_edit`
documents that it "never moves the cursor" and callers rely on it. Multibuffer
already translates composed↔source coordinates on every edit. A host-side
transform layered over all of that double-moves the first, breaks the contract of
the second, and fights the third.

The host is the wrong layer because the host does not own the position. The
*document* does.

## 4. The model

> A document's selections survive edits to that document.

`lattice_core::Document` already owns a `SelectionSet` (`document.rs:33`) that
`apply_edit_batch` never touches. Make it honest: transform the selections as
part of applying the edit, in core, where the `AppliedEdit` already is.

This is a pure, total function of `(Position, AppliedEdit)`. `AppliedEdit`
(`lattice-core/src/buffer.rs:40`) carries `original_range`, `inserted_range`,
`replaced_text`, `inserted_text` and a full `EditDelta` — everything required.

### 4.1 Transform rule

For a position `p` and an applied edit replacing `original_range` with
`inserted_range`:

| Where `p` is | Result |
|---|---|
| strictly before `original_range.start` | unchanged |
| at or after `original_range.end` | shifted by `inserted_range.end - original_range.end` |
| strictly inside `original_range` | clamped to `inserted_range.end` |

The third row is the only judgement call. Clamping to the *end* of the
replacement (rather than the start) is what keeps a caret at the tail of a
rewritten region at the tail afterwards — the common case for an append-shaped
owner write. It is also what vim does when an external process rewrites a
region under the cursor.

Batches apply left-to-right, each edit transformed against the running result.

### 4.2 The host adopts, it does not compute

`Editor::cursor` remains the live caret for the active pane; `PaneLeaf::cursor`
stashes inactive panes. Neither becomes redundant. Instead:

- At the end of each input dispatch, the host writes its caret into the active
  document's selections (one site). This keeps the document's selections a
  faithful base for the next transform.
- When the host observes a **version bump it did not cause** on a buffer — an
  owner write — it adopts that document's primary selection head back into
  `Editor::cursor` (if the buffer is active) or the owning `PaneLeaf::cursor`.

Origin is decidable without a new flag: the host knows which edits it issued,
because it issued them. A per-buffer `last_seen_text_version` distinguishes
"the text moved because I moved it" from "the text moved underneath me".

Keystroke edits are untouched by this: they still set the cursor from their own
`AppliedEdit`, and the write-through simply records the result. No double-move.

### 4.3 Why this is not the anchor primitive

A true anchor (a position registered with the rope, updated on every edit, with
a bias) would subsume this and would also serve marks, LSP positions, and
multibuffer translation. It is the right eventual primitive and it is explicitly
**not** this design: it is a new core layer with its own lifetime and identity
questions. Transforming a `SelectionSet` the document already owns is the 5% of
that work that buys the correctness we need now, and it does not foreclose the
anchor.

## 5. The conversation drain needs no change

An earlier revision of this document asserted that `reproject` rewrites the
prompt on every streamed token and therefore had to be made minimal. That is
wrong, and worth recording so nobody "fixes" it again.

`reproject`'s `last` / `new` are the **transcript** strings returned by
`render_conversation`, not the buffer text. The buffer is
`{transcript}{PROMPT_MARKER}{typed text}`. In the streaming path the edit's range
ends at `text_end(last)` — which, since the transcript ends in `\n`, is exactly
`(prompt_line, 0)`, the start of the prompt line. Its own docstring says so:
"the edit ends at the prompt-line start, leaving the user's in-progress prompt
intact while the agent streams."

So the drain already emits a minimal edit, and the §4.1 rule handles both of its
paths without a special case:

- **Streaming.** The caret sits at `(prompt_line, byte ≥ 2)`, at or after
  `original_range.end == (prompt_line, 0)`. Row 2 applies: the caret shifts to
  `(new_prompt_line, same byte)`. The column is preserved by construction.
- **Send (`clear_prompt`).** The edit deliberately spans through EOF to reset the
  prompt atomically, so the caret lies strictly inside the replaced range. Row 3
  applies: it clamps to `inserted_range.end`, the tail of the freshly appended
  `PROMPT_MARKER` — precisely what the tick callback was re-parking it to.

One rule, both cases, no mode-specific code. That is the evidence that the
transform belongs in the document rather than in the mode.

## 6. What this deletes

The whole compensating mechanism in `crates/lattice-ai/src/acp/conversation_mode.rs`:

- `should_repark_caret(did_clear, text_changed, in_insert)`
- `AiConversationMode::in_insert` and its `Event::ModalModeChanged` subscription
- `AiConversationGuard::_modal_subscription`
- the **prompt-focus** tick callback and its `drain_flag`

None of it survives, because none of it was ever about the prompt. It was about
the caret not riding the edit.

To be precise: what goes is the mode's *caret-reparking* tick callback. The
`TickCallbackRegistry` itself is untouched — it is the host's generic async→effect
seam, and the permission menu (`acp-ux-enhancements.md` §5.3) registers its own
callback on it to open the popup.

It also removes a latent hazard: `Effect::SelectionChange` carries **no
`BufferId`** (`dispatch.rs:2836` assigns to `editor.cursor`, whatever buffer is
active). Every re-park emitted from a background drain was a bet that the
conversation buffer was still focused. The transform is per-document and takes
that bet off the table.

## 7. Blast radius, honestly

Transforming `Document::selections` affects every buffer, because every buffer
is a document. The behaviour change is confined to positions that were
previously left behind:

- **Keystroke edits** — no change. Handlers overwrite the cursor from their own
  `AppliedEdit` immediately after.
- **`apply_targeted_edit`** — its contract is "never moves the cursor". It keeps
  it: the host does not adopt for edits it issued.
- **Multibuffer** — coordinate translation happens above this layer, on the
  composed document. Unchanged.
- **Undo/redo** — emits `AppliedEdit`s. The caret riding an undo to the restored
  text is correct and is what vim does.
- **Read-only synthetic buffers** (dashboard, `*messages*`, lsp-log, help) —
  the in-core selection *transform* still runs (a document's selection stays
  self-consistent), but the host does **not adopt** it into `Editor::cursor`.

The conversation buffer is the only buffer in the workspace that implements
`EditableTail`, so it is the only place a user caret sits inside a buffer an
owner writes to.

### 7.1 The adopt is gated on the editable tail

An earlier revision let the host adopt an owner-write caret move for *any*
active buffer, and claimed a read-only buffer would benefit ("a caret parked
mid-log stops drifting"). That was wrong in practice. Populating a read-only
synthetic buffer is itself an owner write: a whole-buffer replace clamps the
caret into the replaced range, and even an append into an *empty* buffer moves
a caret sitting at the `(0,0)` insertion point to EOF (the `pos >= end` branch
of §4.1). The host then adopted that EOF into `Editor::cursor`, and the
dashboard opened scrolled to the bottom.

So the adopt (`maybe_adopt_owner_write`, gated by `should_adopt_owner_write`)
fires only when the active buffer **has an editable tail** — the one buffer,
the ACP conversation prompt, where an owner streams content while the user's
caret lives in the buffer. Read-only buffers keep the caret the host or user
placed; their document selection may transform to EOF underneath, but that is
never copied to the visible cursor (and self-heals on the next
`write_through_caret`). This closes the whole "populate + forced caret"
class in one place, for append and replace alike, rather than making every
host populate site remember to reconcile.

The cost is the marginal "log tail-follow" the old revision imagined — a
read-only buffer's caret no longer auto-rides appended content. That is
acceptable (arguably better: a cursor that doesn't move on its own), and a
future opt-in could restore it per buffer if wanted.

## 8. Testing

Unit, in `lattice-core`:

- transform of a position before / at / after / inside a replaced range
- insertion at exactly the caret byte (row 2, not row 3 — a caret at the
  insertion point rides forward)
- multi-edit batch, applied left-to-right
- deletion spanning the caret clamps to `inserted_range.end`
- selection set with an extent: both head and anchor transform

Integration, in `lattice-ai`:

- streamed transcript growth shifts the prompt caret by exactly the inserted
  line count, preserving column
- a multi-line prompt (`<C-j>`) keeps its caret's line **and** column across a
  streamed chunk
- a send (`clear_prompt`) clears the prompt and leaves the caret at the fresh
  tail — the row-3 clamp, replacing the deleted tick callback
- a caret parked in the read-only transcript (Normal mode, user scrolled up)
  still rides the insertion above it, and never jumps to the prompt

Regression, in `lattice-host`:

- a keystroke edit does not double-move the cursor
- `apply_targeted_edit` still never moves the cursor
