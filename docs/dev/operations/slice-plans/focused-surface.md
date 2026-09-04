# The focused surface — slice plan

> **Status: Active.** Opened 2026-09-04. Implements
> [`focused-surface.md`](../../architecture/focused-surface.md), which extends
> [`popup-unification.md`](../../architecture/popup-unification.md) §3, §8, §9.

Design owns *what* and *why*; this file owns *when* and *in what order*.

All slices land in `lattice`. Renderer suites run SEPARATELY
(`lattice-ui-tui` and `lattice-ui-gpui` in their own `cargo test` runs) — a
combined run times out `settle_mode`.

## Status

| Slice | Title | Status |
|---|---|---|
| FS.1 | Focus is a stack, not an `Option` | ✅ |
| FS.1b | Focus REPLACES as well as nests — a prompt superseding a prompt | ✅ |
| FS.2 | A focused popup rides the focus seam | ✅ |
| FS.3 | `/` in a popup searches the popup — **absorbed by FS.2** | ✅ |
| FS.4 | The verbs that must NOT follow focus — **they all follow it** | ✅ |
| FS.5 | Delete the `popup_focused` reads that were compensating | ✅ |
| FS.6 | Retire the duplicate restore FS.2 handed FS.5 | ✅ |

FS.1 blocks FS.2 (nesting a search line inside a focused popup needs the
stack). FS.2 blocks FS.3 and FS.4. FS.5 is cleanup and lands last, when the
tests that would catch an over-deletion exist. FS.1b is a correction to FS.1
and landed after FS.5, when the org suite caught what FS.1 had broken. FS.6
finishes the job FS.2 scoped and FS.5 did not take.

**Known pre-existing red, unrelated:**
`typing_after_popup_open_live_refilters_candidates` and
`backspace_after_popup_open_live_refilters` in `lattice-ui-tui` fail on clean
HEAD (proven by stashing, 2026-09-04) — command-line completion no longer
extends `descr` to `describe-`. Not this plan's, but it means "2 failures" is
the green baseline for that crate until someone fixes it.

---

## FS.1 — Focus is a stack, not an `Option` ✅

`minibuffer_focus: Option<MinibufferFocus>` becomes a stack.
`focus_editing_buffer` pushes a frame; `restore_editing_buffer` pops one.

**The guard it removes is the bug.** Today `focus_editing_buffer` stashes only
`if self.minibuffer_focus.is_none()`, so a second focus records nothing and a
single restore returns to the first frame's origin — skipping whatever was
focused in between. That is correct while only one surface can take focus at a
time, and false the moment a popup can hold focus while `/` opens inside it.

The accessors stay honest by construction: `command_line_active()` and
`search_line_active()` ask about the TOP frame, and both are already written
in terms of `search_line`, which is unaffected.

`minibuffer_focus: Option<_>` became `focus_stack: Vec<_>`, read through
`Editor::focused_surface()` so "is anything focused" and "what is focused"
stay one question — reaching for the field directly is how they drift.
`focus_editing_buffer_at` landed with it: the minibuffers want the top of
their one fresh line, a popup wants the view state it was left at, and that
difference is the whole reason focusing a popup was hand-rolled instead of
going through this seam (FS.2 uses it).

Tests: `focus_is_a_stack.rs`, four — two frames unwinding one at a time to
the surface each was entered from; the accessor reading the innermost frame;
an empty pop being a no-op (several cancel paths call it without proving a
frame exists, and a panic there would take the editor down on a stray
`<Esc>`); and the single-frame path behaving exactly as before. The first two
were verified to fail with the `is_none()` guard restored.

Gate: 1420 green in `lattice-host`, fmt clean, zero warnings.

---

## FS.1b — Focus REPLACES as well as nests ✅

Design: [`focused-surface.md`](../../architecture/focused-surface.md) §2.1,
added by this slice. Landed after FS.5.

**FS.1 removed one wrong guard and left the opposite assumption behind it.**
The `is_none()` guard meant a second focus recorded nothing; making the push
unconditional fixed nesting and broke the case that is not nesting. Org's
`org-set-property` opens a prompt from a prompt's submit, and as two frames
the final `<CR>` popped the user back into the prompt they had just answered
instead of into their file. The org suite caught it — `focus_is_a_stack.rs`
did not, and that is the more interesting failure.

**Why the test missed it.** The fixtures were two synthetic buffers standing
in for "surfaces", chosen because "the stack is about frames, not about
kinds". That is true of the stack and false of the *decision*: whether a
focus nests or replaces depends entirely on which surfaces they are. The
file passed while modelling a state the editor never produces — the same
mistake `install_help` made for three rounds (FS.5). The fixtures are
production shapes now: a real popup with a real search line for nesting, two
real prompt lines for replacement.

`MinibufferFocus` gained `focused_buffer` — the surface the frame focused,
not what it took focus from. That is what lets a focus tell "I am nesting
inside the focused thing" from "I am replacing it"; without it the two are
indistinguishable at the push. The popup's frame is exempt via
`popup_focus_depth`, so a `/` inside a focused popup still nests.

Tests: `focus_is_a_stack.rs`, five — the two nesting tests rewritten onto
production fixtures, plus `a_prompt_superseding_a_prompt_replaces_its_frame`
asserting one frame, the origin being the FILE, and one restore landing
there.

Gate: 1438 green in `lattice-host`, fmt clean, no new clippy; 779 green
across `lattice-org-plugin`, the suite that caught it.

---

## FS.2 — A focused popup rides the focus seam ✅

`focus_help_popup` stops hand-rolling half a focus swap and calls
`focus_editing_buffer(popup_buffer)`, seeding the popup's own stashed cursor /
scroll instead of the zeroes the minibuffers want. `dismiss_popup` pops the
frame instead of restoring `prev_pane_for_popup` by hand.

`prev_pane_for_popup` is then redundant with the stack frame and goes — but
NOT in this slice: it carries `modal` restoration (PU-A.1b) and the pane's
buffer identity, and proving the frame covers both is FS.5's job.

**What this slice must prove**, because it is the moment the meaning of
`self.document` changes under a focused popup:

- the document pane still renders the FILE while the popup has focus (the
  buffer-keyed path MB.1 built for the command line);
- `j` / `k` in a focused popup are bounded by the POPUP's line count — the
  latent defect this fixes, which no test covers today;
- dismissing returns the file, its cursor, its scroll and its modal state
  exactly as before (the PU-A.1b guarantee).

**FS.3 came with it rather than after it**, exactly as that slice predicted:
`execute_search` reads `self.document`, so once the popup IS the active
document the search targets it with no further change. The slice's value was
its tests, and those landed here.

**The renderer needed the same generalisation.** `position_help_popup` was
already taught (a commit earlier) not to read the anchor from `ad()` while a
MINIBUFFER was focused; FS.2 makes `ad()` diverge from the pane for a second
reason, so the test regressed in the opposite direction. The predicate is now
the honest one — `ad().document_buffer_id == ad().active_pane_buffer_id` —
using the two identities the published state already carries by name, rather
than inferring one from a `BufferKind`.

`dismiss_popup` unwinds to `popup_focus_depth` with `>=`, so a `/` line
opened inside the popup is popped WITH it. Leaving a minibuffer focused over
a popup that no longer exists is the wedge this initiative started from.

Tests: `a_focused_popup_is_the_active_buffer.rs`, seven — the popup is the
active document and its content is what `self.document` holds; the pane still
holds the file; `/` finds a word that exists ONLY in the popup and lands the
caret on the popup's line; the file's caret is untouched by it; `<Esc>`
returns to the popup rather than past it; dismissing takes a nested search
line with it; and dismissing restores the modal state (PU-A.1b). Three were
verified to fail with the half-swap restored.

The fixtures share no words between file and popup and differ in length —
one where both contain the search term passes whichever buffer the verb
resolved against, which is how this survived.

Gate: 1427 green in `lattice-host`; 1741 in `lattice-ui-tui` (the two
pre-existing completion failures above); 37 in `lattice-ui-gpui`; fmt clean,
no new clippy.

---

## FS.3 — `/` in a popup searches the popup ✅

Falls out of FS.2 rather than being implemented: `execute_search` reads
`self.document`, which by then IS the popup. What this slice adds is the
proof, at the level the user sees:

- incsearch highlights inside the popup while typing, and the file behind is
  untouched;
- `<CR>` moves the caret to the match **in the popup**, and the file's cursor
  has not moved;
- `n` / `N` walk matches within the popup;
- `<Esc>` returns to the popup at the position it had before `/`.

**The test types one character per dispatch**, because a pre/post-dispatch
comparison is what the auto-dismiss spans — seeding the pattern and
dispatching once passes on a broken build. That hole cost two rewrites of
`searching_from_a_focused_popup.rs` already.

---

## FS.4 — The verbs that must NOT follow focus — **they all follow it** ✅

**This slice's premise was wrong, and finding that out was its value.** It
predicted two verbs that had to opt out of following focus. Neither does.

- **`:w` follows focus, and should.** The plan said saving must not write the
  popup and that the save path's refusal of a pathless buffer should become
  load-bearing. Corrected in review: read-only governs MODIFYING a buffer,
  not exporting it. `:w somewhere` on an unnamed read-only buffer is ordinary
  vim, and refusing it would take away something a user has an obvious reason
  to want — the hover text is often the thing worth keeping. `do_write` acts
  on `self.document`, so it already writes the popup, and `:w` with no path
  gives the generic "no file name (use :w <path>)".
- **LSP follows focus, and degrades correctly.** A popup buffer has no URI, so
  a position-anchored request from inside one bails on the generic "no URI =
  no LSP for this buffer" path instead of asking about the file. `K` returns
  early while a popup is focused anyway, by design since 5.5.LSP.1.

So the slice is four tests and no production code, which is the outcome the
model predicts: once the focused surface IS the active document, the generic
answers are right. What survives from the plan is the naming — the published
state carries `document_buffer_id` and `active_pane_buffer_id` separately, so
a future call site picks deliberately rather than reaching for whichever is
in scope.

**A real bug surfaced while testing this and is NOT part of this slice:**
`:w {file}` calls `save_as`, which adopts the path — that is vim's `:saveas`,
not vim's `:w {file}` (write a copy, keep your identity). Global, not
popup-specific. Fixed separately.

Gate: 1430 green in `lattice-host`.

---

## FS.5 — Delete the `popup_focused` reads that were compensating ✅

Three identity reads deleted, one identity WRITE fixed, and two more
hand-rolled copies of the focus swap found by the deletions.

**Deleted** — each provably redundant once a focused popup is the active
document: `active_text`'s popup branch (the `Help` arm returns the same
buffer, and called itself "a fallback during the transition"),
`active_document_path`'s (the `Help` arm already answers `None`), and
`active_cursor`'s (every arm returns `self.cursor`).

**Kept, and now labelled** — `ensure_cursor_visible`'s viewport clamp and
`scroll_wrap_width`'s width substitution are GEOMETRY (a popup's viewport
really does differ from its pane's), and the renderer's reads are
PRESENTATION. Identity is fixed; neither of those is.

**The write was the user-visible one.** `snapshot_active_pane`'s tail was
unconditional — the `match` above it only chose *extra* stashing, so every
kind fell through to write the hot-path cursor into the active pane whether
or not it described that pane. With a popup focused it described the POPUP,
so pressing `/` stashed `(0, 0)` into the pane the FILE renders from:

> the underlying buffer still scrolls to the top and the popup also jumps a
> bit as a result […] the jump happens at the instant I type `/`

Both renderers read that stash for a non-focused pane — GPUI's own comment
records the READ side of this being fixed there once already ("without this
guard, opening `:describe-buffer` scrolled the background document to line
0"). So the defect was the host writing a value neither renderer should have
been handed, and the fix reaches both by construction. The test asserts the
two moments separately, because the report distinguishes them: pressing `/`,
and then typing into it.

**Two more copies of the focus swap surfaced**, which is what the deletions
are for — they stop compensating, and every place the invariant does not
hold fails loudly:

- `open_popup_buffer`'s `Steal` arm, the second way a popup takes focus and
  the one FS.2 missed. Nine help-motion and help-search tests failed the
  instant `active_text` stopped covering for it. Now on the same seam, with
  a popup-already-focused branch so a help→help link follow replaces content
  instead of stacking a frame that would need two dismisses.
- `install_help`, the TUI test helper — a third copy, hand-setting six
  fields. Its own comments record two earlier rounds of the same drift. It
  calls the production path now, which is what stops there being a fourth.

Two TUI tests read the FILE through `editor.document` and needed correcting
rather than fixing: that expression means the popup while one is focused,
which is the whole point of the initiative. They name the buffer they mean
now, and the read-only one gained the assertion it was missing (the POPUP is
unchanged too, not just the file).

Gate: 1435 green in `lattice-host`; 1741 in `lattice-ui-tui` (the two
pre-existing completion failures); 37 in `lattice-ui-gpui`; fmt clean, no new
clippy.

---

## FS.6 — Retire the duplicate restore FS.2 handed FS.5 ✅

**FS.2 scoped this and FS.5 did not take it**, which the status icons could
not show: both slices read ✅ while `dismiss_popup` still carried the comment
"FS.5 retires the older of the two once that is proven rather than assumed."
Found by checking the source against the plan before archiving, which is the
whole reason that rule exists.

`dismiss_popup` ran BOTH restores — the focus-stack unwind and the
`prev_pane_for_popup` stash. They agreed only because a focus-stealing popup
wrote the stash with the same values the frame stashed, and the stash ran
SECOND, so on any disagreement it won silently. That is the shape FS.5 exists
to delete.

**FS.2's "`prev_pane_for_popup` is redundant and goes" was too broad**, which
is why the deletion needed re-deriving rather than executing. The field has a
second consumer (`bury_buffer`, for `q` out of a buried buffer) and two
writers that are not popups at all (`activate_help_in_pane`,
`Effect::OpenSyntheticBuffer`). Those paths genuinely MOVE the pane and push
no frame, so the stash is still the only thing that can put it back. The
field stays; what goes is writing it *in parallel with a frame*.

So: the two focus-stealing popup paths (`focus_help_popup` and
`open_popup_buffer`'s `Steal` arm) stop writing it, and `dismiss_popup`
chooses — frame if one exists, stash otherwise. What the frame does not
restore is `pane.buffer` / `pane.buffer_id`, and it does not need to:
`focus_editing_buffer_at` never touches the pane tree, so an overlay popup
leaves the pane on its own buffer throughout and the stash was writing back
identical values.

**The disagreement was reachable, and that is the new test.** Dropping the
write alone would have been a bug: `Effect::OpenSyntheticBuffer` sets the
stash only when empty and documents it as sitting unused "until GC'd by the
next successful `dismiss_popup`", and a focused popup no longer overwrites
it — so a leftover from an unrelated in-pane open would be applied on top of
a correct restore, landing the user at another buffer's cursor. The unwind
branch drops the stash instead, which is the GC the old overwrite-then-take
did by accident.

Tests: one added —
`a_stale_pane_stash_does_not_clobber_the_dismiss_restore`, which seeds
exactly that leftover and asserts the frame's line, scroll and modal survive
it. Three existing tests asserted the retired mechanism rather than the
guarantee (`prev_pane_for_popup.is_some()`, "prev captured for dismiss") and
were pointed at the frame — same guarantee, one mechanism.
`state_b_dismiss_restores_prev_pane` was renamed to
`state_b_dismiss_restores_the_origin_buffer`: it never cared which mechanism.

Gate: 1439 green in `lattice-host`; 1741 in `lattice-ui-tui` (the two
pre-existing completion failures, re-proven on clean HEAD by stashing); 37 in
`lattice-ui-gpui`; 779 across `lattice-org-plugin`; fmt clean, no new clippy.

---

## Follow-ups this plan surfaced but did not take

- **`:saveas`.** `:w {file}` no longer adopts the path (fixed separately);
  renaming an already-named buffer now has no spelling. Needs a new `Effect`
  variant — WIT ABI plus both renderer classifiers — for a rare verb.
- **Anchored-popup scroll differs between peers.** The TUI resolves a
  cursor-anchored popup against the pane's LIVE scroll; GPUI freezes it at
  `doc_scroll_at_anchor`. Both are defensible and neither is the reported
  bug: with the doc scrolling under a State-A popup, the TUI's follows the
  anchor line and GPUI's stays put.
- **Two pre-existing `lattice-ui-tui` failures** (command-line completion no
  longer extending `descr` to `describe-`), red on clean HEAD since before
  this plan opened.
