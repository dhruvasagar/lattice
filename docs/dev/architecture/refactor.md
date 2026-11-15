# Refactoring: `refactor-mode`

> **Design fragment — agreed 2026-11-14, re-scoped 2026-11-15** (one mode owns
> every refactor command: §2; the structural v1 set and keys: §10). Contracts,
> rationale, rejected alternatives. Sequencing:
> [`../operations/slice-plans/refactor.md`](../operations/slice-plans/refactor.md)
> (RF). Builds on [`plugin-treesitter-seam.md`](plugin-treesitter-seam.md)
> (the WASM tree API) and on [`lsp-architecture.md`](lsp-architecture.md) §16
> (the `LspRequest` split the server's refactorings go through); sits beside
> [`tree-sitter-text-objects.md`](tree-sitter-text-objects.md) (the native
> structural objects, which it does not replace).

## 1. Why

Refactoring comes from two sources, and a user should not have to know which.

- **The language server's refactorings** — extract, inline, rewrite — through
  the code-action request, by name: `:lsp-refactor`, `:lsp-extract`,
  `:lsp-inline` (RF.1). Precise where a server offers them, absent where none
  is attached, and only what that server implements.
- **Edits by syntax node** — grow and shrink a selection a node at a time, swap
  two arguments, raise an expression out of the call around it, split and join
  a list. No server offers these, and `:lsp-expand-region` grows a selection
  only where a server implements `selectionRange`. Every language lattice
  highlights already has a tree-sitter parse, so they work everywhere, server
  or not. (The `lsp-selection-range-mode` help page says expand-selection
  "falls back to the syntactic implementation". There is none today; §3 is
  it, and the page is corrected when it lands.)

## 2. One mode owns all of it: `refactor-mode`, in the bundled `refactor` plugin

A feature set is a mode (`CLAUDE.md`, "A feature set is a mode"): every
refactor command, key and handler belongs to `refactor-mode`, which the bundled
`refactor` plugin declares, beside `comment` and `auto-pair`, built by
`cargo xtask build-core-plugins` and shipped with the editor. One gate turns it
off (`:set refactor.enabled=false`), one page documents it (`:help refactor`),
one keymap layer holds its keys.

This re-scoping (2026-11-15) moves RF.1's three commands out of
lattice-grammar's global ex-command registry, where they landed beside every
other `lsp-*` command, and into the mode (§8). They keep their `lsp-` names:
the ex-command naming rule names a command for what it is coupled to, and these
ask the server.

- **Paramount #2 (extensibility).** The tree seam (TS.1–TS.3) exists so that
  "structural motions / text objects / folds / refactors" can be plugins. This
  is the refactor case, and the first consumer that edits rather than reads. A
  gap it hits is one every third-party structural plugin would hit, found by
  us first — and §8's are two such gaps.
- **Paramount #1 (performance)** holds: each structural operation is one
  synchronous action call, fuel-bounded, walking a distance proportional to
  tree *depth* from the cursor. No whole-file work, no query over the buffer.
  Budget: under 1 ms p99 in a debug build on a 2 300-line file, measured by a
  bench (§7). An LSP refactor is one effect returned; the request runs on the
  host's async substrate as before.
- **Native grammar stays native.** The vim grammar does not cross the WASM
  boundary. None of this is vim grammar — vim has no such commands — it is
  extension, which is what WASM is for.

## 3. The structural operations (v1)

All language-agnostic: they use only named nodes, parents and named siblings,
never a node kind, so they work in every grammar with no per-language table.

| Command | What |
|---|---|
| `refactor-expand` (`<M-o>`) | Grow the selection to the smallest named node strictly larger than it (from Normal: the node at the cursor). Enters Visual charwise. A count grows that many levels. |
| `refactor-shrink` (`<M-i>`) | Step back to the previous, smaller selection from this run of expands; past the first, back to the cursor in Normal. |
| `:refactor-swap-next` / `-prev` | Swap the list item at the cursor with its next / previous one, and follow it. "The item" is the smallest named node at the cursor whose gap to a neighbouring named sibling is a list separator — a comma, a semicolon, or whitespace holding a line break — so on `a` in `f(a + 1, b)` it is `a + 1`: the gap between `a` and `1` is ` + `, which separates operands, not items. (RF.2c corrected the first draft's "smallest named node that has a named sibling", which picks `a` and swaps it with `1`.) The text between them stays put. A count moves the item that many places, fewer if the list ends first. |
| `:refactor-raise` | Replace the expression around the node at the cursor with the node. "Around" is the node's parent, unless that parent is a bracket glued to the sibling before it — the `(…)` of a call, the `<…>` of a generic, the `[…]` of an index — which is half an expression, so its parent is replaced instead: raising `b` out of `f(a, b)` gives `b`, while out of `let v = [a, b];` it gives `let v = b;`. Brackets and adjacency are read from the text, not node kinds. Refused at top level, where the only thing around is the file. With a Visual selection, the node the selection covers is raised — once an ex-command can see the selection (RF.3b's seam; see §8). |
| `:refactor-split` | Put each element of the comma-separated node at the cursor on its own line, one indent deeper, the closing delimiter on a line of its own at the node's indent. "One indent" is the buffer's own `shiftwidth` / `expandtab`, read through `config.get-option-in-buffer` (RF.2e), so a mode's buffer-local setting — `go-mode`'s `noexpandtab` — is honoured; the global `get-option` would have split Go with spaces. |
| `:refactor-join` | The inverse: the node's elements on one line, `, ` between them. |

`swap`, `raise`, `split` and `join` are each **one undo unit**: `u` puts both
halves of a swap back. `expand` / `shrink` change no text.

**Split / join stay language-agnostic by acting only on comma-separated
nodes.** A node qualifies when it has two or more named children and the text
between each pair is a comma plus whitespace. That is arguments, parameters,
arrays, tuples, struct literals and their kin in every C-family grammar,
Python, JS and Rust, read from the text rather than from node kinds. The text
before the first child (`(`, `vec![`) and after the last (`)`, `]`) is kept
verbatim, and so is a trailing comma: present before a split, present after
it; absent, absent — the plugin never decides a language's comma style. The
indent unit is the buffer's `shiftwidth` / `expandtab`. A node that does not
qualify (a block of `;`-separated statements, a Python suite) refuses with an
echo: those need per-language rules, and a wrong split there is worse than
none.

Not in v1, named so they are not lost: split / join of non-comma nodes
(statement blocks; per-language rules), converting between forms (`if` ↔
ternary, quote styles; per language), dot-repeat of the edits (§6),
multi-cursor.

## 4. Grammar surface

**Actions, and ex-commands where the cursor is enough.** An action receives the
document and the tree (`apply-action`, TS.1); an operator does not
(`grammar.wit`: "No `tree`, deliberately"). Nor is any of these an operator
over a motion: swap and raise act on *a node*, which the cursor already names.

Expand and shrink are **actions only** — keys and the command palette, no `:`
form. Typing `:` leaves Visual, and an ex-command is handed no selection
(`ex-command-context` carries the cursor and buffer, nothing more), so a
`:refactor-shrink` could never know what to shrink and a
`:refactor-expand` would always start from the cursor. Swap, raise, split and
join act on the node at the cursor, so they are ex-commands too, under their
own names (raise's Visual form waits on the same missing selection, and
arrives with §8's ex-command seam, which `:'<,'>lsp-extract` needs for the
same reason): actions and ex-commands share one name index in the command
registry, so a plugin cannot give both the same name.

**Keys.** `<M-o>` / `<M-i>` (Alt-o / Alt-i) expand / shrink — `<M-o>` and `<M-i>` both in Normal and Visual. `<M-i>` has nothing to shrink in Normal and says so, but is bound there anyway: Normal's keymap lookup strips an unbound Alt, so a bare `<M-i>` would run `i` and enter Insert. Helix's
keys; vim has no convention here, and nvim-treesitter's `gnn` / `grn` would
collide with lattice's `gr`. Swap, raise, split and join are commands only,
bound in `init.rs` by whoever wants them. The chords live on the plugin's
minor mode, `refactor-mode` — never at the builtin layer (modes own their
full surface). The mode is `global`, on every document buffer, because there is
no activation policy for "has a parse tree"; in a buffer without one, the
actions echo that there is no syntax tree rather than the key going dead. A key
that works in one file and silently does nothing in the next reads as broken.

**Expand / shrink beside the LSP's version: two commands, not one that picks.**
A server's `selectionRange` and the parse tree disagree on where expressions
begin. One key answering from whichever happened to be available would grow
the same selection differently from one minute to the next, as a server
attaches or restarts.

## 5. Contracts

- **The snapshot is the one handed to the call.** Every position is computed
  against the tree minted with the document at the same instant (seam §7), so
  no edit is computed against stale text.
- **Edits as `apply-edit` effects, last first.** A swap is two replacements;
  the plugin emits the later one first, so the earlier range is still valid
  when it lands. The selection or cursor follows as a `selection-change` /
  `cursor-move` after the edits.
- **Nothing to do is said, not silent.** No tree (no grammar for the buffer, or
  the `tree-sitter` grant withheld), no sibling to swap with, or nothing to
  raise into: one echo naming why, and no edit.
- **Error nodes are not edited.** If the node or its sibling `is-error`, the
  operation refuses: rearranging half-parsed text makes it worse.

## 6. What the seam has to grow (found by designing this)

1. **One undo unit for a multi-edit result — was missing; fixed in RF.2a.**
   Each `apply-edit` effect became its own `Action::ApplyEdit` and so its own
   undo entry: `gc2j` then `u` took back the last line only, a live bug in the
   shipped comment plugin. The host now coalesces a run of `ApplyEdit`s for
   the same buffer into one `Action::ApplyEdit { edits }`, applied through
   `apply_edit_batch` — one undo step, edits in order, the last cursor wins.
   In order means the plugin still emits a swap's later range first (§5).
2. **Shrink needs the expand history — no seam change.** The loader
   instantiates a grammar plugin once, at load, and every action call reuses
   that `Store`, so guest memory persists between calls. The history lives
   there, per buffer, dropped once the selection no longer matches the last
   expand. Only the grammar instance sees it — the event and config seams run
   separate instances (plugin-host.md, durable storage) — which is fine: all
   six commands are grammar actions.
3. **Dot-repeat** of a plugin action needs the host to record plugin actions as
   repeatable changes: a seam-wide change of its own, after v1.

## 7. Goals, tests, bench

- **UX (higher court):** an operation that cannot apply leaves text and
  selection exactly as they were. One edit batch, one reparse, no flicker.
- **#1:** depth-bounded walk per call. A plugin-host bench records swap, raise
  and expand on a large file, joining the typed-call rows in `benchmarks.md`.
- **#2:** built on the public seam only; WIT stays the API, nothing private.
- **#3:** the vim grammar is untouched; these sit beside it.
- **#4:** synchronous by design — a few node walks, no I/O.
- **Tests:** each operation on Rust and on a language with different list
  syntax (Python), plus every refusal in §5, end to end through the real
  keystroke path.

## 8. The server's refactorings join the mode

**What moves.** The registration and the per-command shape of
`:lsp-refactor`, `:lsp-extract` and `:lsp-inline` — today `code_actions_spec`
and three entries in lattice-grammar's `ex_commands.rs` — become ex-commands
the `refactor` plugin registers, each returning one
`lsp(code-actions { only, on-selection })` effect.

**What stays, and why that is not a half-migration.** The request, the
`context.only` filter, the row filter and the picker stay host-side behind
`LspRequest::CodeActions` (§16 of `lsp-architecture.md`). That is the
substrate every LSP surface uses: a mode says *what* to ask as data, the host
runs the async request. It is the same split `lsp-mode` lives by, and the
host holds no refactor-specific code — `:lsp-code-action`, the unfiltered
picker, uses the same request with `only: none`. `:lsp-code-action` itself
stays with `lsp-code-action-mode`: it is the server's general menu, not a
refactoring.

**The two seam gaps, both found by this move.**

1. *A plugin cannot ask for a filtered code action.* The WIT effect has
   `lsp-code-action` with no fields, and the boundary refuses
   `CodeActions { only: Some(_), .. }` ("no WIT mirror yet", RF.1).
2. *An ex-command cannot tell it was given the selection.*
   `ex-command-context` carries the cursor and the buffer, no range, so
   `:'<,'>lsp-extract` from a plugin could not ask about the selection.

Both are API additions every plugin gains, not refactor-specific ones. How to
add them is RF.3a's first question: widening `lsp-request` and
`ex-command-context` changes types that existing components were built
against, which wasmtime's instantiation check may reject; new functions on an
interface are additive. The answer, checked against a component built for the
current package, decides whether the plugin-API publish is a patch or a
bump, and is recorded here when it is known.

## 9. Rejected alternatives

- **Native, in `lattice-syntax`** beside the text objects. It would reach
  `SyntaxSnapshot` directly and skip the seam's limits — which is the argument
  against it: those limits are what plugin authors live with, and a refactor
  feature built within them is the test of whether they are right. (No new
  crate either way; heuristic #6.)
- **Per-language query tables** (nvim-treesitter-textobjects' `@parameter`
  swaps): precise where a table exists, dead where it does not. The
  named-sibling rule covers every grammar; a table can refine it later for a
  language that proves the generic rule wrong.
- **One `expand-region` over both sources** — §4.

## 10. Decisions

1. **Placement:** a bundled plugin (§2), over native (§9).
2. **The v1 set:** expand / shrink, swap, raise, and split / join on
   comma-separated nodes (§3).
3. **Keys:** `<M-o>` / `<M-i>` for expand / shrink on `refactor-mode`;
   everything else commands only (§4).
4. **Scope (2026-11-15):** one `refactor-mode` owns every refactor command,
   the server's included (§2, §8) — over keeping the LSP refactors with
   `lsp-code-action-mode`, which would leave "refactor" with two owners, and
   over a native mode, which would undo decision 1.
