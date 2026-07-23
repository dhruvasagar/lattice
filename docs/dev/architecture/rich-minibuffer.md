# Rich minibuffer

Authoritative design for Lattice's **rich minibuffer**: the `:` command
line becomes a **readline-grade editing surface** backed by a real
buffer, and `<C-x><C-e>` **expands** the command into a full editing
mini-buffer (the bash/zsh `edit-and-execute-command` affordance) with the
complete vim grammar, syntax highlighting, error highlighting, and
completion. Command history is walkable in-line and browsable through a
fuzzy **picker**. This builds out `design.md` §5.9.10 (committed, unbuilt)
around a two-tier model.

The guiding principle stays the project's own — **everything is a
buffer** — but applied with progressive disclosure:

- The **`:` line** is a one-line buffer edited **readline-style** (insert
  only, faithful vim-insert / emacs / readline chords). No modal editing:
  most commands are short and want a fast, familiar prompt.
- **`<C-x><C-e>`** opens the command as a **full editing mini-buffer** —
  a regular buffer with the whole vim grammar and rich tooling — for the
  rare complex command. On quit, the edited text returns to the `:` line
  to execute. Exactly bash/zsh's "open the command in `$EDITOR`."

Companion to `design.md` (§5.2 modal engine, §5.9.10 minibuffer, §5.12
options), `mode-architecture.md` (mode-owned surfaces), `picker.md` (the
history picker), and `config` (the expand-layout option). Sequencing:
`../operations/slice-plans/rich-minibuffer.md`.

## 1. The problem with the status quo

`Editor.command_line: String` is a dumb accumulator — append on keypress,
backspace on `<BS>`, submit on `<CR>`. **No cursor movement, no mid-line
editing, no registers.** You cannot fix a typo three words back without
deleting everything after it. History exists (`command_history: Vec<String>`,
`<Up>`/`<Down>`) but there is no picker, and no way to lay a long command
out and edit it comfortably.

The readline insert-mode chords (`<C-a>`/`<C-e>`/`<C-b>`/`<C-f>`/`<C-w>`/
`<C-u>`/`<C-k>`) already exist as **universal insert-mode grammar** in
`keymap_insert.rs`. The `:` line never sees them because it is a `String`
with a hand-rolled input path (`translate_command`), not a buffer flowing
through the normal insert dispatcher. That is the whole bug.

## 2. Tier 1 — the readline `:` line (a buffer, insert-only)

The command line becomes a **one-line synthetic `Document`**
(`*command-line*`, unlisted, `NoFile`) with major mode
**`command-line-mode`**, created through the mode-owned creation seam
(`ModeActivator::ensure_named_document`, `mode-architecture.md` §5.4) when
`:` is pressed, and focused while active.

**Why a buffer and not a `String`+cursor:** so the `:` line **reuses the
universal insert-mode readline grammar** rather than re-implementing it.
`<C-w>`/`<C-u>`/`<C-a>`/`<C-e>`/`<C-b>`/`<C-f>`/cursor-keys are the *same*
chords that edit any Insert-mode buffer — per the directive that readline
editing is universal grammar, not a minibuffer-only keymap. Completion (it
already exists on the cmdline) and a compact live error hint come along as
ordinary buffer decorations.

**`command-line-mode` constrains it to insert-only** — there is no
Normal-mode entry in the `:` line. The mode owns the small surface a
command line adds over a plain buffer (mode-ownership, `mode-architecture.md`
§5.3):

- **readline editing** — inherited (universal insert grammar); the mode
  adds nothing here.
- **`<CR>`** — **submit**: read the buffer text → the existing `:`-parser
  front-end → `CommandInvocation` → push onto `command_history` → close →
  restore the prior buffer → dispatch.
- **`<Esc>`** and **`<C-c>`** — **cancel**: abort, restore prior buffer,
  no history push. (`<Esc>` cancels cleanly *because* there is no
  Normal-mode to fall into — the collision that a full-modal `:` line
  would create simply does not arise.)
- **history walk** — `<C-p>`/`<C-n>` (and `<Up>`/`<Down>`) seed the buffer
  from `command_history`, preserving in-progress text as
  `command_history_pending` (exists today); prefix-filtered walk (type
  `:e ` then `<C-p>` → only `:e …`) is a polish.
- **`<C-x><C-e>`** — **expand** into the tier-2 mini-buffer (§3).

`ModalState::Command` becomes the "command-line buffer is focused" flag
rather than a bespoke typing mode; keys route through the normal Insert
dispatcher. The `command_line: String` field and `translate_command`
retire.

## 3. Tier 2 — `<C-x><C-e>` expand the command line *in place*

Tier 2 is **not a separate buffer or a new pane** — it is the **same
`*command-line*` surface expanding in place**. Only in
`command-line-mode`, **`<C-x><C-e>`** grows the one-row command line
**upward, pushing the mode-line (and the buffer content) above it**,
into a full-width multi-row **mini-buffer band** at the bottom of the
frame, and **enables full modal editing** on it. Modelled on bash/zsh's
`edit-and-execute-command`, but the "editor" is the command line itself
grown large:

- **Same buffer, now full-modal.** `<C-x><C-e>` toggles the buffer's
  `expanded` state: the presentation grows (echo row → band) and the
  *complete* vim grammar (Normal / Insert / Visual, motions, operators,
  `.`-repeat, registers, undo, multi-line) turns on. There is **one
  command being edited** the whole time — tier 1 and tier 2 are two
  sizes of one surface, not two buffers with copy-back.
- **`:` is a no-op inside the mini-buffer.** Because you are *already in*
  the command line, the `:` "enter-command-line" chord does nothing
  here (it must not open a nested command line). In tier 1 (insert-only)
  this is automatic — `:` just inserts a literal colon while typing a
  command; in tier 2's Normal mode it is an **explicit no-op guard** the
  mode contributes. This is the one place the command line behaves
  *unlike* an ordinary buffer, and it is deliberate.
- **Rich tooling:** `command-line` grammar **syntax highlighting**, live
  **error highlighting** (unknown command / bad args, from the same
  parser that runs on submit), **parameter hints** (from `ArgSpec`
  metadata), and **completion** (the same completion source as the
  one-row line). §5.
- **Collapse returns to the one-row line for review, not auto-execute.**
  `<C-x><C-e>` again (or a mode chord) collapses the band back to the
  one-row readline `:` line with the edited text; the user presses
  `<CR>` to execute. `<C-c>` cancels (discards the expanded edits, keeps
  the pre-expand text). *(Bash executes on save-quit; we deliberately
  collapse-for-review — one execution point, `<CR>` in the `:` line.)*

## 4. History — walk + fuzzy picker

`command_history` stays the model (per prompt-kind: command vs search).
Two surfaces:

- **Walk** (tier 1) — `<C-p>`/`<C-n>` + `<Up>`/`<Down>`, seeding the `:`
  line from history, pending text preserved.
- **Picker** — **`q:`** (Normal-mode chord, vim muscle memory) and the
  **`:history`** ex-command open a **fuzzy picker** over history (the
  `picker.md` primitive `:error-list` / `:diagnostics` use). The modern
  replacement for vim's command-line *window*: fuzzy-filter past commands
  instead of scrolling a scratch buffer. **Accept loads the picked
  command into the editable `:` line — it does NOT execute** — so you
  tweak (or `<C-x><C-e>` it) and `<CR>`.

## 5. Tooling / decorations

Because both tiers are buffers, the §5.9.10 richness is decoration work,
off the UI thread (paramount #1) — the renderer only reads published
rows:

| Affordance | `:` line (tier 1) | expanded (tier 2) |
|------------|-------------------|-------------------|
| Completion popup | ✅ (exists) | ✅ (same source) |
| Live error indicator | ✅ compact (echo) | ✅ inline squiggle |
| Parameter hints (`ArgSpec`) | optional virtual hint | ✅ |
| Syntax highlighting | optional | ✅ (`command-line` grammar) |
| Registers / undo / Visual | — (insert-only) | ✅ (regular buffer) |
| `:s///` substitution preview | ✅ | ✅ |

All are decoration providers contributed by the two command-line modes.

## 6. Unification

The substrate is prompt-agnostic. `/` `?` search (`search-line-mode`),
future `git-commit-line`, `repl-input`, and interactive `input()` prompts
become the same one-line-buffer + mode pattern, each with its own tier-2
expand. `/` migrates off its parallel `String`, deleting the duplicate
input path and gaining rich editing + history for free.

## 7. Paramount-goal alignment

- **#1 Performance.** Both tiers are the already-benched buffer hot path;
  opening `:` is a focus swap, tier-2 is a split with a one/few-line
  buffer. Decorations/highlighting run off-thread. The `:` line stays on
  the keystroke→glyph path — the ratchet must not move.
- **#2 Extensibility.** New prompt kinds are a new mode on the same
  substrate — zero host change; plugins get rich prompts for free.
- **#3 Vim modal / grammar-is-the-API.** Tier 1 reuses universal insert
  grammar; tier 2 runs the *whole* grammar. The `:` parser is a front-end
  reading a buffer — the deepest form of "the vim grammar is the public
  command API."
- **#4 Async.** Decoration/highlight production is event-driven + off the
  actor thread.

## 8. Rejected alternatives

- **`String` + cursor + readline ops on the `:` line.** Rejected
  (heuristic #1 + the universal-grammar directive): re-implements the
  readline chords as cmdline-exclusive code, never gets completion /
  decorations / registers, and would be replaced by the buffer substrate.
- **Full modal editing *in* the one-row `:` line.** Rejected: collides
  with vim's `<Esc>`-cancels-`:` muscle memory and taxes the common
  (short) command with modal complexity. The two-tier model gives full
  modal where it belongs — the opt-in `<C-x><C-e>` *expanded* line.
- **Tier 2 as a separate split pane / new buffer.** Rejected: it is the
  **same** command-line surface grown in place (pushing the mode-line
  up), not a distinct buffer/pane you navigate to — so there is no
  copy-back, and the `:`-no-op / command-line semantics hold across both
  sizes. A separate pane would be "just another buffer" and would
  wrongly let `:` open a nested command line.
- **Execute-on-quit for `<C-x><C-e>` (bash-faithful).** Rejected in favour
  of return-to-`:`-line-for-review — one execution point (`<CR>`), and a
  final look before running a command you just heavily edited.
- **Vim's command-line *window*.** Superseded by the fuzzy picker (§4):
  same "edit a past command as text" power, fuzzy-filterable, consistent
  with every other Lattice list surface — and `<C-x><C-e>` already
  provides the full-buffer edit.

## 9. The expand size option

The expansion is **in place** by default — the command line grows into a
full-width bottom band, pushing the mode-line up (§3). A typed option
(§5.12), e.g. `command-line.expand-height`, controls how tall it grows
(`half` the frame default, or a fixed row count, or `full`). A `popup`
alternative (centered floating band) may be offered later, but the
default is the in-place upward expansion — *not* a separate split/pane
the user navigates to. Typed + `:set`/`:customize`-able like every
option; the mode reads it when expanding.

## 10. Impact surface (for the slice plan)

- **Modal / input** (`lattice-grammar::modal`, host dispatch, `input.rs`):
  `Command` becomes a focus state over a real buffer; retire
  `translate_command`; route `:` keys through the Insert dispatcher.
- **Buffer / focus / lifecycle** (`lattice-host`): open/close, focus-swap
  + suspend/restore prior buffer, submit → parser → history → dispatch;
  tier-2 expand = open a split with `command-line-edit-mode` seeded from
  the `:` text, accept writes back.
- **Modes** (`lattice-mode` / a minibuffer crate): `command-line-mode`
  (insert-only, submit/cancel/history/expand chords + handlers) and
  `command-line-edit-mode` (full-buffer + accept/cancel), decoration
  providers, the `command-line` grammar.
- **Render** (`lattice-ui-tui`, `lattice-ui-gpui`, lockstep): echo-area
  row draws the tier-1 buffer's line + cursor; tier-2 is a normal
  split/popup buffer render.
- **Config**: the `command-line.expand-layout` option.
- **History picker**: reuse the picker primitive; `q:` + `:history`;
  accept-into-editable-line.
