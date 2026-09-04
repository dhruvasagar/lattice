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
| FS.2 | A focused popup rides the focus seam | ✅ |
| FS.3 | `/` in a popup searches the popup — **absorbed by FS.2** | ✅ |
| FS.4 | The verbs that must NOT follow focus — **they all follow it** | ✅ |
| FS.5 | Delete the `popup_focused` reads that were compensating | 📝 |

FS.1 blocks FS.2 (nesting a search line inside a focused popup needs the
stack). FS.2 blocks FS.3 and FS.4. FS.5 is cleanup and lands last, when the
tests that would catch an over-deletion exist.

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

## FS.3 — `/` in a popup searches the popup 📝

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

## FS.5 — Delete the `popup_focused` reads that were compensating 📝

28 sites read `popup_focused` in `dispatch.rs`. After FS.2 they fall into
three groups, and this slice sorts them:

- **Geometry — keep.** `ensure_cursor_visible`'s viewport clamp and
  `scroll_wrap_width`'s width substitution: a popup's viewport genuinely
  differs from its pane's. Identity is fixed; geometry is not.
- **Presentation — keep.** Border, dimming, which surface the renderer
  paints as active.
- **Identity — delete.** Anything that reads elsewhere for content, line
  count, folds or syntax because the popup was not the active document.
  These are the ones the defect created.

`prev_pane_for_popup` retires here if the stack frame proves sufficient for
modal restoration too.

**Deletion is the deliverable and the risk**, so it lands last, behind FS.2–4's
tests. A `popup_focused` read that survives this slice should carry a comment
saying which group it is in — the next reader should not have to re-derive it.
