# Snippet integrity — slice plan

> **Slice plan.** Sequencing, slice IDs, status.
> Design contract: [`../../architecture/insert-completion.md`](../../architecture/insert-completion.md)
> — §6.3 (active-snippet minor mode), the snippet engine principle (§1.6);
> [`../../architecture/select-mode.md`](../../architecture/select-mode.md) — placeholder overtype.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

**Status: 🚧 — SI.1–SI.9, SI.11 ✅; SI.10, SI.12 🚧 — each waits on one design decision (2026-10-24).**

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
- **Mirrors update live (2026-10-23)**, in the edit's undo group — by the `Document`, as linked tracked ranges (option A over a host post-edit hook or sync-on-`<Tab>`: every edit path mirrors, no extra actor crossing, and LSP linked editing gets the same primitive).
- **Multi-cursor expansion is deferred** (SI.13).
- **TUI / GPUI parity is structural, and checked per slice.** Every
  scenario lives in `lattice-host/src/peer_conformance/snippet.rs` and runs
  in both peers through `peer_conformance_tests!` — real keys through each
  peer's own key path and dispatch tail. The renderer crates hold no snippet
  logic: their `ExpandSnippet` / `ReloadSnippets` arms and startup load call
  host functions, and selection painting reads the host's published visual
  range. Each slice runs GPUI's whole suite (`--features window`), not only
  the conformance subset, before it lands.

## Slices

| Slice | What | Gate | Status |
|---|---|---|---|
| SI.1 | Conformance scenarios for both peers, through real keys: accept → expand → first stop selected; `<C-x><C-s>`; `<Tab>` / `<S-Tab>` walk; `<Esc>` leaves | pins today's passing behaviour in both renderers | ✅ |
| SI.2 | Tracked ranges in `Document` with per-end gravity; the session holds them; overtyping a parent drops its nested stops (finding 1) | test: type in `$1`, `<Tab>` lands on `$2` — fails without; edits outside, multi-line, an operator, undo; bench: per-edit cost with N tracked ranges | ✅ — `lattice_core::tracked`; the `Document` moves the ranges in `transform_for_edit` beside the selections; bench: +30 ns a keystroke for 8 ranges |
| SI.3 | Navigation: Select `<Tab>`/`<S-Tab>` onto empty and one-char stops; char-boundary Select head; implicit final stop without `$0`; landing on `$0` ends the session so the next `<Tab>` inserts (finding 6) | tests per case, each failing without | ✅ — empty stops enter Insert before moving; a `SelectionChange` naming its `visual` kind is an explicit span (one-char defaults); `placeholder_selection` ends on a char boundary, and Visual/Select's extent (host `selection_extent`, grammar `Range::Selection`) covers the head's whole character — `vd` on `é` had deleted nothing |
| SI.4 | Lifecycle: the session ends on leaving Insert/Select, the cursor leaving the snippet, buffer switch or close, undo past the expansion; expanding inside a session | tests per exit path | ✅ — the session-backed-minor predicate takes a `SessionProbe` (buffer, modal, cursor, document); `SnippetSession::reconcile` ends the session outside Insert/Select or with the cursor outside the snippet, untracking its ranges; a nested expansion releases the outer one. Undo and buffer switches are covered by the same rule (both leave Insert); a closed buffer's session dies with its document. Found on the way: Insert binds no `<Up>`/`<Down>` |
| SI.5 | Registry identity + language: key by `(language, name)`; user overrides builtin on a shared prefix; one language resolver for completion, `<C-x><C-s>`, the picker, with VS Code ids (`typescriptreact`, `shellscript`, `ruby`); the picker lists `*` (findings 3, 5) | tests: same name in two languages expands the buffer's; override; `.rb` reaches `ruby` | ✅ — `SnippetRegistry` keyed by `(language, name)`, candidate and picker payloads carry `snippet_key`; later packs go first on a prefix and replace a same-named snippet; `lattice_core::language::language_id_for_path` (LSP ids) is the one resolver, `lookup_languages` adds the React→base fallback |
| SI.6 | Indentation: continuation lines take the trigger line's indent; `\t` follows `expandtab` / `shiftwidth` (finding 4) | tests: nested expansion, expandtab on/off | ✅ — `RenderedSnippet::reindented(base, level)` rewrites the text and remaps every stop; the host passes the trigger line's indentation and one `IndentUnit` step for both the snippet and the LSP expansion paths |
| SI.7 | Mirrors: placeholder text copied to mirrors; focus the copy holding it; typing updates mirrors live in one undo group | tests: render, focus, live update, one `u` | ✅ — option (A), decided 2026-10-23: tracked ranges can be linked; the `Document` copies an edit in one into the rest in the same commit and undo entry (never during undo/redo), reports the copies as `linked_edits` on the snapshot, and the host's edit chokepoints publish them and move the cursor across them. Mirrors render their placeholder's text |
| SI.8 | Parser: format transforms parse and apply; UTF-8 choices; linear-time malformed nesting + depth cap; per-entry load errors with a user-visible count; unknown variable → tabstop; local `CURRENT_*` | tests per item | ✅ — `transform` module (regex + LSP format grammar: case conversions, `+`/`-`/`?:` conditionals, `gim`); variable transforms apply at render, tabstop transforms render from the placeholder's default and are rewritten on leaving it (`<Tab>` / `<S-Tab>` / `<Esc>`, as `Effect::ApplyEdit`s, cursor placed on a scratch copy of the post-rewrite buffer); parser: depth cap 32 + memoised failed `$`s (2^40 → linear), UTF-8 choices with `\$`/`\}`, nested-format transforms; packs load per entry (`PackLoad::skipped`), startup echoes a warning; unknown variables become placeholders; `CURRENT_*` local |
| SI.9 | LSP: advertise completion capabilities; honour `textEdit`; tabstops correct after `additionalTextEdits`; Select on a default; run `command` (finding 2) | tests with the in-process server | ✅ — `completion` client capabilities (snippets, insert/replace, insert-text modes, lazy `documentation`/`detail`; NOT lazy `additionalTextEdits` — accept does not await a resolve — nor label details, which the popup does not render); `textEdit` range extended to the cursor; origin carried through edits applied after it; first stop focused through the shared `focus_first_snippet_stop`; `command` run; `asIs` skips the re-indent. Found, not fixed: the plain-text LSP accept keeps the stale `textEdit` end, and `lsp_position_to_app_byte` assumes UTF-16 whatever encoding was negotiated |
| SI.10 | Variables + surround: `TM_SELECTED_TEXT` from Visual, OS clipboard, comment strings from the major mode, `WORKSPACE_*`, `RELATIVE_FILEPATH` | tests per variable | 🚧 — ✅ OS clipboard (read only when the body uses `$CLIPBOARD`, `SnippetBody::uses_variable`; falls back to `+`), `LINE_COMMENT` / `BLOCK_COMMENT_*` from the language, `WORKSPACE_NAME` / `_FOLDER` and `RELATIVE_FILEPATH` (full path outside the workspace) from the picker's root, `CURSOR_INDEX` / `_NUMBER`, `CURRENT_SECONDS_UNIX`, `CURRENT_TIMEZONE_OFFSET`, `TM_FILENAME_BASE` of a dotfile. 📝 `TM_SELECTED_TEXT`: needs an entry point for expanding around a Visual selection — a design decision |
| SI.11 | Loading: `snippet.dirs` option, project `.lattice/snippets`, `.code-snippets`, friendly-snippets `package.json` layout | tests per layout | ✅ — `lattice_snippet::load_dir` reads a `package.json` (`contributes.snippets`) or a snippets folder (`<lang>.json`, `_global.json`, `*.code-snippets` by `scope`); the host's order is design §8.3's: packages in `snippet-packs/`, `snippet.dirs`, the user's folder, the project's `.lattice/snippets`; `:set snippet.dirs` reloads; conformance boot clears the user's snippet directories |
| SI.12 | Feedback: placeholder highlight (theme element, both renderers); choice list on `${1\|a,b\|}`; remaining doc drift | tests: decoration present in both peers; choice list opens | 🚧 — ✅ doc drift: the active-snippet keys are actions (no `:snippet-*` commands, no `<C-l>`/`<C-h>`), keeping a default is `<Tab>`, choices insert the first option. 📝 placeholder highlight: no mode-contributed inline range decoration exists (`Mode::decorations()` is a stub) — needs a new seam, a design decision. 📝 choice list: the completion popup is Insert-only and a choice is focused in Select; how they meet is a design decision |
| SI.13 | Multi-cursor: expand at every cursor | — | ⛔ deferred — revisit once single-cursor sessions are sound; until then expansion applies at the primary only, documented in `multi-cursor.md` |
