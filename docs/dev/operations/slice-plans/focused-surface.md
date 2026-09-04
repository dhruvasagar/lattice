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
| FS.1 | Focus is a stack, not an `Option` | 📝 |
| FS.2 | A focused popup rides the focus seam | 📝 |
| FS.3 | `/` in a popup searches the popup | 📝 |
| FS.4 | The verbs that must NOT follow focus — save and LSP | 📝 |
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

## FS.1 — Focus is a stack, not an `Option` 📝

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

**Tests:** two frames push and pop in order; one frame behaves exactly as
today (the regression risk is the single-frame path, which every existing
minibuffer test covers — those passing IS the assertion); popping an empty
stack is a no-op rather than a panic.

---

## FS.2 — A focused popup rides the focus seam 📝

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

## FS.4 — The verbs that must NOT follow focus 📝

Two paths must keep resolving against the PANE's buffer, and after FS.2 they
must say so rather than getting it by accident:

- **`:w`** with a popup focused must not write the popup. The save path
  refuses a buffer with no path today; that refusal becomes load-bearing and
  gets a test naming why.
- **Position-anchored LSP** (hover, definition, references) must ask the
  server about the FILE. A hover requested from inside a hover popup asking
  about the popup's own prose is the failure this prevents.

The distinction the design names — *focused surface* versus *pane buffer* —
gets an accessor each so a future call site picks deliberately instead of
reaching for whichever is in scope.

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
