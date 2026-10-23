# Snippet integrity — slice plan

> **Slice plan.** Sequencing, slice IDs, status.
> Design contract: [`../../architecture/insert-completion.md`](../../architecture/insert-completion.md)
> — §6.3 (active-snippet minor mode), the snippet engine principle (§1.6);
> [`../../architecture/select-mode.md`](../../architecture/select-mode.md) — placeholder overtype.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

**Status: 🚧 — SI.1–SI.6 ✅ (2026-10-23).**

## Why

Snippets have tests, and none of them type inside a placeholder and then
press `<Tab>`. An audit (2026-10-23, three passes: grammar + loading, the
live session, every expansion path in both renderers) found the session
works only for the path the tests walk. Confirmed in code:

1. **Placeholder ranges never follow an edit.** `ActiveSnippet::shift_ranges_after`
   has no production caller and nothing that edits the buffer touches
   `snippet_session`. Overtype `${1:i}` with `idx` and `<Tab>` lands two bytes
   off; mirrors and nested placeholders go stale with it.
2. **LSP never sends snippets.** The client's `textDocument` capabilities
   omit `completion`, so `snippetSupport` / `resolveSupport` are unadvertised.
   When a snippet item does arrive its `textEdit` range is ignored, and an
   `additionalTextEdits` import above it shifts every tabstop.
3. **Names collide across languages.** `SnippetRegistry::all_by_name` is keyed
   by name; accept looks the body up by name alone, so another language's body
   can expand. A user pack cannot override a builtin on `<C-x><C-s>` (it takes
   `.first()`, the builtin).
4. **No re-indent.** The rendered text is spliced raw: continuation lines start
   at column 0 inside a block and `\t` stays a tab under `expandtab`.
5. **Two language resolvers disagree.** Completion uses an extension map
   (`.rb` → `rb`, so `ruby.json` never matches), the picker `Lang::detect`;
   the picker omits `*` snippets.
6. **Select-mode navigation keeps a stale anchor** on an empty or one-char
   stop, and the Select head is `end - 1` bytes — mid-character for `${1:café}`.

Reported by the audit, to be proven by each slice's first test: the session
outlives `<C-c>`, a click, a buffer switch and undo; landing on `$0` swallows
the next `<Tab>`; no implicit final stop without `$0`; format transforms do not
parse, parsed ones never apply; malformed nesting parses in exponential time;
choice text mangles non-ASCII; one bad entry drops its whole file; an unknown
variable is dropped instead of becoming a tabstop; `CURRENT_*` are UTC;
`TM_SELECTED_TEXT` / comment / workspace variables are never filled;
`CLIPBOARD` reads the `+` register, not the OS clipboard; the snippet dir is
hard-coded; no placeholder highlight, no choice list. The user docs describe
`:snippet-next`/`-prev`/`-leave`, `<C-l>`/`<C-h>` and rippling mirrors, none
of which exist.

Both renderers share the host's expansion code, so every finding is in both.

## Decisions

- **Ranges are tracked by the `Document` (2026-10-23).** `Document` already
  transforms its selections through every applied edit, synchronously;
  tracked ranges ride the same loop with per-end gravity (start sticks left,
  end sticks right, so typing at an empty `$2` grows it). Correct for every
  edit path — operators, undo, LSP `applyEdit`, plugins — by construction.
  Rejected: shifting at the host's `publish_document_changed` (snippet logic in
  the host; misses edits to a non-active buffer) and subscribing to
  `Event::DocumentChanged` (asynchronous — `x<Tab>` races the shift).
- **Mirrors update live (2026-10-23)**, in the edit's undo group.
- **Multi-cursor expansion is deferred** (SI.13).

## Slices

| Slice | What | Gate | Status |
|---|---|---|---|
| SI.1 | Conformance scenarios for both peers, through real keys: accept → expand → first stop selected; `<C-x><C-s>`; `<Tab>` / `<S-Tab>` walk; `<Esc>` leaves | pins today's passing behaviour in both renderers | ✅ |
| SI.2 | Tracked ranges in `Document` with per-end gravity; the session holds them; overtyping a parent drops its nested stops (finding 1) | test: type in `$1`, `<Tab>` lands on `$2` — fails without; edits outside, multi-line, an operator, undo; bench: per-edit cost with N tracked ranges | ✅ — `lattice_core::tracked`; the `Document` moves the ranges in `transform_for_edit` beside the selections; bench: +30 ns a keystroke for 8 ranges |
| SI.3 | Navigation: Select `<Tab>`/`<S-Tab>` onto empty and one-char stops; char-boundary Select head; implicit final stop without `$0`; landing on `$0` ends the session so the next `<Tab>` inserts (finding 6) | tests per case, each failing without | ✅ — empty stops enter Insert before moving; a `SelectionChange` naming its `visual` kind is an explicit span (one-char defaults); `placeholder_selection` ends on a char boundary, and Visual/Select's extent (host `selection_extent`, grammar `Range::Selection`) covers the head's whole character — `vd` on `é` had deleted nothing |
| SI.4 | Lifecycle: the session ends on leaving Insert/Select, the cursor leaving the snippet, buffer switch or close, undo past the expansion; expanding inside a session | tests per exit path | ✅ — the session-backed-minor predicate takes a `SessionProbe` (buffer, modal, cursor, document); `SnippetSession::reconcile` ends the session outside Insert/Select or with the cursor outside the snippet, untracking its ranges; a nested expansion releases the outer one. Undo and buffer switches are covered by the same rule (both leave Insert); a closed buffer's session dies with its document. Found on the way: Insert binds no `<Up>`/`<Down>` |
| SI.5 | Registry identity + language: key by `(language, name)`; user overrides builtin on a shared prefix; one language resolver for completion, `<C-x><C-s>`, the picker, with VS Code ids (`typescriptreact`, `shellscript`, `ruby`); the picker lists `*` (findings 3, 5) | tests: same name in two languages expands the buffer's; override; `.rb` reaches `ruby` | ✅ — `SnippetRegistry` keyed by `(language, name)`, candidate and picker payloads carry `snippet_key`; later packs go first on a prefix and replace a same-named snippet; `lattice_core::language::language_id_for_path` (LSP ids) is the one resolver, `lookup_languages` adds the React→base fallback |
| SI.6 | Indentation: continuation lines take the trigger line's indent; `\t` follows `expandtab` / `shiftwidth` (finding 4) | tests: nested expansion, expandtab on/off | ✅ — `RenderedSnippet::reindented(base, level)` rewrites the text and remaps every stop; the host passes the trigger line's indentation and one `IndentUnit` step for both the snippet and the LSP expansion paths |
| SI.7 | Mirrors: placeholder text copied to mirrors; focus the copy holding it; typing updates mirrors live in one undo group | tests: render, focus, live update, one `u` | 📝 |
| SI.8 | Parser: format transforms parse and apply; UTF-8 choices; linear-time malformed nesting + depth cap; per-entry load errors with a user-visible count; unknown variable → tabstop; local `CURRENT_*` | tests per item | 📝 |
| SI.9 | LSP: advertise completion capabilities; honour `textEdit`; tabstops correct after `additionalTextEdits`; Select on a default; run `command` (finding 2) | tests with the in-process server | 📝 |
| SI.10 | Variables + surround: `TM_SELECTED_TEXT` from Visual, OS clipboard, comment strings from the major mode, `WORKSPACE_*`, `RELATIVE_FILEPATH` | tests per variable | 📝 |
| SI.11 | Loading: `snippet.dirs` option, project `.lattice/snippets`, `.code-snippets`, friendly-snippets `package.json` layout | tests per layout | 📝 |
| SI.12 | Feedback: placeholder highlight (theme element, both renderers); choice list on `${1\|a,b\|}`; remaining doc drift | tests: decoration present in both peers; choice list opens | 📝 |
| SI.13 | Multi-cursor: expand at every cursor | — | ⛔ deferred — revisit once single-cursor sessions are sound; until then expansion applies at the primary only, documented in `multi-cursor.md` |
