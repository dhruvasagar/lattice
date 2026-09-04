# The focused surface

Authoritative design for **which buffer an interaction acts on**. Extends
[`popup-unification.md`](popup-unification.md) — §3 made popup content a
registry buffer and §8–§9 recorded two bugs from asking "which buffer is
this" twice. This fragment removes the question.

Sequencing lives in
[`docs/dev/operations/slice-plans/focused-surface.md`](../operations/slice-plans/focused-surface.md).

## 1. The defect, stated once

**Focusing a popup moves the caret but not the active document.**
`Editor::focus_help_popup` sets `cursor`, `scroll`, `active_buffer =
Help` and `popup_focused = true`, and never touches `self.document`. So
while the user is "in" a popup, every buffer-scoped verb still resolves
against the file behind it.

Reported in use, 2026-09-04, as three separate bugs that are one bug:

> focus an LSP hover popup, press `/` — the search, the hlsearch
> highlighting and the incsearch real-time matching all run in the
> background buffer until I hit `<CR>`; then the cursor moves *within the
> popup* and jumps to the match.

`execute_search` reads `self.document` (the file), so matching happens
there; `active_buffer == Help` makes the renderer paint `self.cursor`
inside the popup, so the caret lands in the popup at a *document* line
number. Two identities, one cursor.

Search is only the verb that made it visible. `j` / `k` in a focused
popup already move the caret bounded by the **document's** line count —
it looks right because popups are short and both start near zero.

**28 `popup_focused` special-cases** in `dispatch.rs` exist to paper over
the split (`ensure_cursor_visible` clamps to `popup_viewport_height`,
`scroll_wrap_width` substitutes `popup_viewport_width`, …). They are the
symptom, not the mechanism: each one re-derives, for one call site, the
fact that the focused surface is not the active document.

## 2. The model: focus is a stack, and the editor already has one

The fix is not new machinery. `focus_editing_buffer` /
`restore_editing_buffer` already do exactly the right thing for the
minibuffers: swap `self.document` and `document_buffer_id` to the focused
buffer, stash what was there, and restore it on the way out. The document
pane keeps rendering its own buffer because the renderer routes it
through the buffer-keyed path when it is not the active document (MB.1).

So a focused popup is not a special state. **It is a focused buffer**, and
it rides the same seam.

One thing has to change to make that true: `minibuffer_focus` is a single
`Option` guarded by `if self.minibuffer_focus.is_none()`, so a second
focus does not stash and one restore pops everything. Focus popup, then
press `/`, and the restore would return to the *document*, skipping the
popup. Focus is therefore a **stack**:

```
    []                       editing the file
    [popup]                  K K — the popup has focus
    [popup, search-line]     …and `/` inside it
```

`restore_editing_buffer` pops one frame. `<Esc>` from the search line
lands back in the popup with the popup's own cursor; `q` then pops the
popup and lands back in the file at the position it kept the whole time.

**Nothing is a special case of anything.** The command line, the search
line, the prompt line and a focused popup are four surfaces that take
focus; the stack is what says which one has it, and `self.document` is
always the one that does.

## 3. What this deletes

Every `popup_focused` special-case that exists because the popup was not
the active document. `ensure_cursor_visible`'s clamp and
`scroll_wrap_width`'s substitution stay — a popup's viewport genuinely
differs from its pane's, and that is geometry, not identity. The ones
that ask "is the popup focused, so should I read somewhere else for the
buffer's content / line count / folds / syntax" go, because after this
the answer is `self.document` for all of them.

The renderer keeps `popup_focused`: something has to paint a border and
decide which surface is dimmed. That is a *presentation* flag and stays.

## 4. What it costs, stated plainly

Every path that assumes `self.document` is the user's FILE while a popup
is focused changes meaning. The ones that matter:

- **Saving.** ~~`:w` with a popup focused must not write the popup.~~
  **Wrong, corrected 2026-09-04 in review.** Being read-only governs
  MODIFYING a buffer, not exporting it: `:w somewhere` on an unnamed
  read-only buffer is ordinary vim, and refusing it takes away something
  a user has an obvious reason to want — the hover text is often the
  thing worth keeping. `:w` follows focus like every other buffer verb,
  and needs no special case: `do_write` acts on `self.document`.
- **LSP.** Position-anchored requests (hover, definition, references)
  read the active document — and also need no special case, for a
  reason that only became visible once it was tested. A popup buffer has
  no URI, so a request from inside one bails on the generic "no URI = no
  LSP for this buffer" path rather than asking the server about the file.
  `K` additionally returns early while a popup is focused, by design
  since 5.5.LSP.1.
- **Syntax and folds.** Already per-buffer (`buffer_locals`), so they
  follow `document_buffer_id` correctly once it moves.
- **Undo.** Per-buffer; a popup is read-only, so its history stays empty.

**Both predicted special cases turned out to be unnecessary**, and that
is the strongest evidence for the model: once the focused surface IS the
active document, the generic answers are the right ones. The distinction
worth keeping is not a list of verbs that opt out — it is that the
published state names both identities (`document_buffer_id` and
`active_pane_buffer_id`) so a call site picks deliberately.

The risk is regressions in paths nobody thought about, which is why this
lands as slices with a test per surface rather than as one change.

## 5. Why not the alternatives

- **Refuse search in a popup.** Coherent and one line, and it leaves
  `j`/`k` still bounded by the wrong buffer. It answers the report, not
  the defect.
- **Dismiss the popup, then search.** Same, and it throws away the
  content the user was reading — already the complaint in the first
  report of this chain.
- **Give search its own "target buffer" parameter.** Fixes one verb. The
  next buffer-scoped verb finds the same split, which is how this bug
  arrived in the first place (it was `:describe-buffer` and `/` a week
  ago, hover and `/` today).

## 6. Paramount-goal alignment

**#1 Performance.** Neutral: a focus swap is a pointer move plus the
per-buffer state fetch `focus_editing_buffer` already does for the
minibuffers, on a keystroke that already rebuilds the world.

**#3 Extensible vim modal editing.** This is the goal being served. `/`,
`n`, `N`, `j`, `k`, text objects and every future motion act on the
surface the user is looking at, because there is one answer to which
buffer that is — rather than a growing list of verbs taught to ask.

**Everything is a buffer.** §3 of `popup-unification.md` said popup
content is a registry buffer. This is that sentence applied to input:
if it is a buffer, focusing it focuses a buffer.
