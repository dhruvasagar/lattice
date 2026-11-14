# Structural editing: syntax-tree operations as a bundled plugin

> **Design fragment — agreed 2026-11-14** (placement, the v1 set, keys: §9). Contracts,
> rationale, rejected alternatives. Sequencing:
> [`../operations/slice-plans/lsp-refactoring.md`](../operations/slice-plans/lsp-refactoring.md)
> (RF.2). Builds on [`plugin-treesitter-seam.md`](plugin-treesitter-seam.md)
> (the WASM tree API) and sits beside
> [`tree-sitter-text-objects.md`](tree-sitter-text-objects.md) (the native
> structural objects, which it does not replace).

## 1. Why

RF.1 named the language server's refactorings. They need a server, and a server
offers what it offers: none swaps two arguments, raises an expression out of
the call around it, or grows a selection one syntax node at a time.
`:lsp-expand-region` does that last one only where a server implements
`selectionRange`. Every language lattice highlights already has a tree-sitter
parse, so these edits can work everywhere, server or not. (The
`lsp-selection-range-mode` help page says expand-selection "falls back to the
syntactic implementation". There is none today; this fragment is it, and the
page is corrected when it lands.)

## 2. Where it lives: a bundled WASM plugin

`plugins/structural`, beside `comment` and `auto-pair`, built by
`cargo xtask build-core-plugins` and shipped with the editor.

- **Paramount #2 (extensibility).** The tree seam (TS.1–TS.3) exists so that
  "structural motions / text objects / folds / refactors" can be plugins. This
  is the refactor case, and the first consumer that edits rather than reads. A
  gap it hits is one every third-party structural plugin would hit, found by
  us first.
- **Paramount #1 (performance)** holds: each operation is one synchronous
  action call, fuel-bounded, walking a distance proportional to tree *depth*
  from the cursor. No whole-file work, no query over the buffer. Budget: under
  1 ms p99 in a debug build on a 2 300-line file, measured by a bench (§7).
- **Native grammar stays native.** The vim grammar does not cross the WASM
  boundary. These are not vim grammar — vim has no such commands — they are
  extensions, which is what WASM is for.

## 3. The operations (v1)

All language-agnostic: they use only named nodes, parents and named siblings,
never a node kind, so they work in every grammar with no per-language table.

| Command | What |
|---|---|
| `:structural-expand` | Grow the selection to the smallest named node strictly larger than it (from Normal: the node at the cursor). Enters Visual charwise. |
| `:structural-shrink` | Step back to the previous, smaller selection from this run of expands; past the first, back to the cursor. |
| `:structural-swap-next` / `-prev` | Swap the node at the cursor with its next / previous named sibling, and follow it. "The node" is the smallest named node at the cursor that *has* a named sibling, so on an argument of `f(a + 1, b)` it is `a + 1`, not `a`. The text between them (`, `) stays put. A count repeats. |
| `:structural-raise` | Replace the node's parent with the node: on `bar(x)` in `foo(bar(x))`, the result is `bar(x)`. With a Visual selection, the node the selection covers is raised. |
| `:structural-split` | Put each element of the comma-separated node at the cursor on its own line, one indent deeper, the closing delimiter on a line of its own at the node's indent. |
| `:structural-join` | The inverse: the node's elements on one line, `, ` between them. |

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

**Actions, plus ex-commands of the same name.** An action receives the document
and the tree (`apply-action`, TS.1); an operator does not (`grammar.wit`: "No
`tree`, deliberately"). Nor is any of these an operator over a motion: swap and
raise act on *a node*, which the cursor already names.

**Keys.** `<M-o>` / `<M-i>` (Alt-o / Alt-i) expand / shrink, in Normal and Visual — Helix's
keys; vim has no convention here, and nvim-treesitter's `gnn` / `grn` would
collide with lattice's `gr`. Swap, raise, split and join are commands only,
bound in `init.rs` by whoever wants them. The chords live on the plugin's
minor mode, `structural-mode`, active on buffers that have a parse tree —
never at the builtin layer (modes own their full surface).

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

## 8. Rejected alternatives

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

## 9. Decisions (2026-11-14)

1. **Placement:** a bundled plugin (§2), over native (§8).
2. **The v1 set:** expand / shrink, swap, raise, and split / join on
   comma-separated nodes (§3).
3. **Keys:** `<M-o>` / `<M-i>` for expand / shrink on `structural-mode`;
   everything else commands only (§4).
