# Refactoring: `refactor-mode`

> **Design fragment — agreed 2026-11-14, re-scoped 2026-11-15** (one mode owns
> every refactor command: §2; the structural v1 set and keys: §11; the operators: §9). Contracts,
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
  the code-action request, by name: `:lsp-refactor`, and first in line for
  `:refactor-extract` / `:refactor-inline` and `cre` / `cri` (RF.1, §9.1). Precise where a server offers them, absent where none
  is attached, and only what that server implements.
- **Edits by syntax node** — grow and shrink a selection a node at a time, swap
  two arguments, raise an expression out of the call around it, split and join
  a list. No server offers these, and `:lsp-expand-region` grows a selection
  only where a server implements `selectionRange`. Every language lattice
  highlights already has a tree-sitter parse, so they work everywhere, server
  or not. (The `lsp-selection-range-mode` help page used to say
  expand-selection "falls back to the syntactic implementation". There was
  none; §3 is the syntactic one, and it is separate, not a fallback. RF.2f
  corrected the page.)

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
| `:refactor-raise` | Replace the expression around the node at the cursor with the node. "Around" is the node's parent, unless that parent is a bracket glued to the sibling before it — the `(…)` of a call, the `<…>` of a generic, the `[…]` of an index — which is half an expression, so its parent is replaced instead: raising `b` out of `f(a, b)` gives `b`, while out of `let v = [a, b];` it gives `let v = b;`. Brackets and adjacency are read from the text, not node kinds. Refused at top level, where the only thing around is the file. With a Visual selection, the node the selection covers is raised — by §9's raise operator, since an ex-command is handed no selection. |
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
document and the tree (`apply-action`, TS.1); an operator did not
(`grammar.wit`: "No `tree`, deliberately"). The v1 set was therefore built as
commands on *the node at the cursor*. §9 makes every structural edit an
operator over a range as well — the primary surface — and these commands
remain as the cursor shorthand and the palette entry.

Expand and shrink are **actions only** — keys and the command palette, no `:`
form. Typing `:` leaves Visual, and an ex-command is handed no selection
(`ex-command-context` carries the cursor and buffer, nothing more), so a
`:refactor-shrink` could never know what to shrink and a
`:refactor-expand` would always start from the cursor. Swap, raise, split and
join act on the node at the cursor, so they are ex-commands too, under their
own names (raise's Visual form is §9's operator): actions and ex-commands share one name index in the command
registry, so a plugin cannot give both the same name.

**Keys.** `<M-o>` / `<M-i>` (Alt-o / Alt-i) expand / shrink — `<M-o>` and `<M-i>` both in Normal and Visual. `<M-i>` has nothing to shrink in Normal and says so, but is bound there anyway: Normal's keymap lookup strips an unbound Alt, so a bare `<M-i>` would run `i` and enter Insert. Helix's
keys; vim has no convention here, and nvim-treesitter's `gnn` / `grn` would
collide with lattice's `gr`. The swap, raise, split and join *commands* have
no default keys; their operator forms do (§9). The chords live on the plugin's
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
- **#1:** depth-bounded walk per call. `crates/lattice-plugin-host/benches/refactor.rs`
  records expand, swap, raise, split and native extract / inline on a
  2000-function file (RF.2f, `benchmarks.md`).
- **#2:** built on the public seam only; WIT stays the API, nothing private.
- **#3:** the vim grammar is untouched; these sit beside it.
- **#4:** synchronous by design — a few node walks, no I/O.
- **Tests:** each operation on Rust and on a language with different list
  syntax (Python), plus every refusal in §5, end to end through the real
  keystroke path.

## 8. The server's refactorings join the mode

**What moved (RF.3c).** `:lsp-refactor`, `:lsp-extract` and `:lsp-inline`
were three entries in lattice-grammar's `ex_commands.rs`. They are now
ex-commands the `refactor` plugin registers. `:lsp-refactor` returns one
`lsp-code-action { only: refactor, range }` effect. Extract and inline,
renamed below, take the operators' path: server first, natively when it
has nothing (§9.1).

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
   **Closed (RF.3b):** the request crosses whole, below.
2. *An ex-command cannot tell it was given the selection.*
   `ex-command-context` carries the cursor and the buffer, no range, so
   `:'<,'>lsp-extract` from a plugin could not ask about the selection.
   *Dropped 2026-11-15, reopened and closed 2026-11-16 (RF.3c, decided
   with Dhruva):* §9 made extract and inline operators, which get their
   range natively. But moving the commands as cursor-only would have
   regressed the documented `:'<,'>lsp-extract`, and every plugin's `:`
   command would have kept seeing less than a native one, the asymmetry
   OC.10 closed for `cursor` and `buffer-id`. So `ex-command-context` gained
   `range: option<range>`, resolved by the host: exactly what was selected
   for `:'<,'>`, whole lines for any other range.

   Building it found the same gap natively. `execute_ex_line` makes `'<,'>`
   whole lines before dispatch (vim's ex addresses are lines), so
   `:'<,'>lsp-code-action` could not tell it had been given the selection,
   and asked about the caret. The exact selection now rides beside the lines
   (`ExCommandContext::selection`, through `DispatchEnv::ex_selection`). The
   code-action command sends it as the request's range. The WIT field
   projects it, and falls back to the lines for any other range.

Both are API additions every plugin gains, not refactor-specific ones. How to
add them is RF.3a's first question: widening `lsp-request` and
`ex-command-context` changes types that existing components were built
against, which wasmtime's instantiation check may reject; new functions on an
interface are additive. The answer, checked against a component built for the
current package, decides whether the plugin-API publish is a patch or a
bump, and is recorded here when it is known. RF.4a already made the
publish 0.5.0, so a widened shape costs nothing extra. What the request
must carry is settled in §9.1: an explicit `range` (an operator's, not the
cursor's or the last selection's) and an `otherwise` command for when the
server has nothing.

**Settled (RF.3a).** The field-less `lsp-code-action` case is widened in
place, not joined by a second case:

	enum code-action-kind { refactor, extract, inline }
	record code-action-request {
		/// The kind family to ask for; `none` is every action.
		only: option<code-action-kind>,
		/// The text to ask about; `none` is the cursor, or the live
		/// Visual selection if one is up.
		range: option<range>,
		/// Run when the server has nothing to offer (§9.1).
		otherwise: option<command-ref>,
	}
	effect: lsp-code-action(code-action-request)

The request with every field `none` is the old unfiltered one. A second
case beside the old one would be two ways to ask the same question for
good, which is the asymmetry §9 rejected for `apply-operator`; widening
breaks only components built against `lsp-code-action` with no payload,
and nothing in this repository or the org plugin returns it. RF.4a already
made the publish 0.5.0, so there is no patch release to protect, and the
instantiation check that would decide patch-or-bump has nothing left to
decide. `command-ref` is reused as it is: its `id` is a registered command
*name* (the `confirm` / `invoke-command` precedent), never a host id.
`:'<,'>` has no WIT form, deliberately: §8's second gap was dropped, so the
native `on_selection` flag stays native.

Natively, `LspRequest::CodeActions` gains `range: Option<Range>` and
`otherwise: Option<CommandRef>`, with `range` winning over `on_selection`
when both are given. `CommandRef { id, args }` is a new lattice-grammar
type, the same pair `Effect::InvokeCommand` carries.

## 9. Operators: every refactor over a range

The v1 commands act on the node at the cursor. That is the weakest part of
the vim grammar to build on: its power is that an operator composes with
*any* range — a Visual selection, a motion, or a text object, including the
syntax objects `af`/`if`, `ac`/`ic`, `aa`/`ia`, `al`/`il` that
`lattice-syntax` already resolves from `textobjects.scm`. `crriw`
raises an identifier out of its call; `<leader>rr` in Visual raises the
selection; `cxaa` … `cxaa` exchanges two parameters that are not neighbours.
(RF.4b found that `aa` / `ia` capture a declaration's parameters but not a
call's arguments, unlike the nvim-treesitter-textobjects convention; widening
them is a `lattice-syntax` query change, tracked in the slice plan.) Every refactor is therefore an
**operator** first; the `:refactor-*` commands stay as the cursor shorthand
and the palette entry, running the same code (paramount #3: the grammar is
the public API).

| Operator | Keys | On the range |
|---|---|---|
| exchange | `cx{obj}` twice; `cxx` (line); Visual `X`; `cxc` clears the mark | The first use marks the range, highlighted; the second swaps the two, as vim-exchange does. One replacement spanning both, so one undo step. A range inside the other replaces the larger with the smaller (vim-exchange's rule); a partial overlap is refused and the mark kept; `cx` on the marked text again cancels. An edit that changes the marked text drops the mark and says so. Text only: no tree, so it works in a buffer with no parse. Visual is `X`, not `cx`, because Visual `c` changes the selection at once. |
| raise | `crr{obj}`; Visual `<leader>rr` | The node the range covers replaces the expression around it, §3's rule. The range is trimmed of whitespace and separators (`,` `;`) first, then must be exactly one named node: one that is not is refused, never widened to the node around it, because raising more than was selected deletes text the user did not point at. In Visual this is "raise what I selected", the form RF.2d could not offer. |
| split / join | `cr<CR>{obj}` / `crj{obj}`; Visual `<leader>r<CR>` / `<leader>rj` | The nearest comma list around the range, §3's rule. Nearest, not exact as raise demands: split and join change only whitespace, so acting on the list around an argument (`cr<CR>iw`) deletes nothing. |
| extract | `cre{obj}`; Visual `<leader>re` | The expression the range covers becomes a variable — see below. |
| inline | `cri{obj}`; Visual `<leader>ri` | The variable the range names (its declaration or a use) is replaced by its value at every use, and the declaration removed — see below. |

**Keys: the `c` family (decided 2026-11-15).** `cx` is vim-exchange's key
and muscle memory carries across editors. The rest take `cr`, on the same
reasoning vim-exchange used: in stock vim `c` followed by `r` does nothing,
because `r` is not a motion, so the prefix is free and reads as grammar
("change: refactor"). That keeps `<leader>` for the user and costs no
leader timeout. Two consequences:

- **Visual cannot use `cr`.** Visual `c` changes the selection the moment
  it is pressed, which is why exchange's Visual form is `X`. Binding a `c…`
  chord in Visual would make `c` wait for the next key there, a latency
  regression on one of the most-used Visual keys. So Visual keeps
  `<leader>r{x}`, the same letter after the prefix.
- **vim-abolish's case coercions live on `cr`** (`crs` snake_case, `crc`
  camelCase, `crm` MixedCase, `cru` UPPER_CASE, `cr-` kebab-case, `cr.`
  dot.case, `crt` Title Case), and many vim users have them in their
  fingers. Refactor takes none of those letters, so split is `cr<CR>`
  ("break onto lines"), not `crs`. Case coercion is itself a refactoring;
  if it joins `refactor-mode`, it takes abolish's letters, and `cr` is then
  the whole family.

All of them live on `refactor-mode`'s layer. Rejected: `<leader>r{x}`
everywhere (the first layout, RF.4b–e) — consistent between Normal and
Visual, but it takes the user's leader space and adds a timeout to every
refactor.

**The exchange mark is a tracked range (RF.4d).** The mark has to follow
every edit made between the two `cx`s, by any path, and show on screen while
it waits. The document already keeps ranges in place across its own edits
(`lattice_core::tracked`, a snippet's placeholders), and the host already
tints text for modes (`RangeHighlightRegistry`); neither crossed WIT. So:

- **`track-range` / `untrack-range` effects**, carrying a buffer, a guest
  id, a range and an optional theme element. Effects, not host calls: they
  are applied after the callback returns, in order with the edits it
  returned, and a host call from the grammar seam would re-enter the actor
  that is running it.
- **`document.tracked-range(id)`**, read from the same snapshot as the text,
  answering the range's current position and whether its text was replaced
  (`collapsed`).
- **Ids are per plugin.** A guest names its range with a `u32`; the
  trampoline's `EffectAuthorizer`, the one place that knows which plugin
  returned an effect, moves it into that plugin's space (top bit set, the
  plugin's numeric id beside it), and `tracked-range` reads from the same
  space. Native ranges come from `TrackedRangeId::next`'s counter, which
  never sets the top bit, so a plugin can reach neither another plugin's
  range nor a snippet's.
- **The tint is native.** The registry keeps which id gets which element and
  reads the range from the snapshot it is publishing, so the tint moves with
  the text and no guest code runs per publish (paramount #1). The `Track`
  reaches the document through its mailbox, so the publish straight after it
  may hold the snapshot from before; at the same text version the tracked
  position is still right and is used, so the tint shows at once rather than
  on the next key.

The mark uses the `search.current` element, vim-exchange's `IncSearch` link,
because a grammar-world plugin cannot register theme elements yet (that
lives in the `theme-plugin` world). A plugin-owned `refactor.exchange`
element is the follow-up.

Rejected for the mark: a static "paint these ranges" import (the tint would
not move with an edit above it — a pixel change to text the user did not
edit), and no tint at all (vim-exchange's convention is to show what is
pending). Decided 2026-11-15.

**Operators need the tree.** `apply-operator` gains
`tree: option<borrow<tree-snapshot>>`, the argument the motion, text-object,
action and ex-command callbacks already take, under the same OT.7 freshness
gate; the native `OperatorContext` carries the syntax snapshot the way
`TextObjectContext` carries its scope resolver. Changing a signature is
breaking, so this is plugin API **0.5.0**: in-tree plugins rebuild, external
ones re-pin. Rejected: an additive `tree-sitter.tree-for(buffer)` import (no
bump, but a second way to get a tree that only operators would use), and a
second `apply-tree-operator` export (two operator entry points forever) —
both keep an asymmetry to avoid a version number.

**Extract and inline are not LSP-only.** Because they work without a
server, the generic names are earned: `:lsp-extract` / `:lsp-inline` become
`:refactor-extract` / `:refactor-inline`. `:lsp-refactor` keeps its name: it
lists *every* refactoring the server offers, so it is genuinely LSP-only.

### 9.1 Server first, query data second (decided 2026-11-15)

Whether an extract or an inline is *correct* depends on what the language
means, not on its shape. RF.4e's native extract showed where shape runs
out:

| Hazard | Example | Who can see it |
|---|---|---|
| Scope | `let y = { let a = 1; a + 2 };` — extracting `a + 2` lifts it above `let y`, where `a` does not exist | tree-sitter, with `locals.scm` |
| Evaluation order | `f(x)` out of `a && f(x)`, a ternary branch, a `while` condition, a block-less closure or a comprehension: it now runs unconditionally, or once | tree-sitter, with per-language query data |
| Where a statement is | a Rust `match` arm starts a line and is not a statement; Go's `:=` is illegal outside a function body | tree-sitter, with per-language query data |
| Types | TypeScript: a lambda lifted out of an argument loses the parameter types it was inferring | a language server only |
| Ownership | Rust: extracting moves a value, or shortens a temporary's lifetime | a language server only |

So each refactor has three rungs, and each rung is used for what only it
can know:

1. **The language server, first.** Extract variable / function / constant
   and inline are standard code actions (`refactor.extract`,
   `refactor.inline`). rust-analyzer, gopls, tsserver, clangd and jdtls offer
   them; Python, Ruby and Lua coverage is patchy. The server knows types,
   scopes and uses, so where it answers it is the right answer.
2. **Native, on query data.** Every language fact the plugin needs is a
   query file, not plugin code. A language is added by adding files
   (paramount #2), and a rule is wrong in one place that a test names.
3. **Refusal, with the reason.** A native edit that cannot be shown safe
   is refused, never guessed. A silent change in behaviour is worse than no
   refactoring: `u` undoes the text, but nothing tells the user the
   meaning changed. This is raise's rule ("refused, never widened") applied
   to meaning.

`refactor.source` (`lsp,native` by default; `lsp` or `native` alone) orders
the first two rungs. A buffer with no server goes straight to native.

**The cascade lives host-side, as data (RF.3a).** The server is asked
asynchronously and the plugin's operator has already returned, so the
plugin cannot see the answer and decide then. The operator returns one
effect, which carries what to do if the server has nothing:

	lsp(code-actions {
		only: some(extract),
		range: some(<the operator's range>),
		otherwise: some(command-ref { id: "refactor-extract-native", args }),
	})

The host asks the server. If actions come back, they open as today's
picker; a server offering several (variable, function, constant) lets the
user choose. If none come back, or no server is attached, or the server
advertises `codeActionKinds` and none matches `only` (no round trip at
all), the host invokes `otherwise`. Two rules make that safe:

- **An answer, not a timeout.** `otherwise` runs only on an answer that
  says "nothing": no server, a server whose advertised kinds exclude the
  request, or an empty list. A server that errors is reported as its own
  failure and nothing else runs; one that hangs is waited for, and `<C-g>`
  stops it. This is the format chain's rule (`:format` "cascades on
  availability, not on failure"): falling back on failure needs a timeout,
  and no timeout is right.
- **A stale range is not acted on.** The host records the buffer's text
  version when it asks. If the text changed before the answer came, it
  invokes nothing and says so ("the buffer changed while the server was
  asked"). It is generic, not refactor's rule: any `otherwise` was computed
  for the text as it was.
- **The result reaches the screen with no key pressed.** The server's answer
  lands asynchronously, so `otherwise` runs from that landing and goes
  through the host's async-landed path (`CLAUDE.md`, "async results must
  reach the screen without a keypress"). Its test asserts the native edit
  is visible without dispatching another key.

`otherwise` is a `command-ref` rather than a list of effects because a WIT
variant cannot contain itself, and because a command is computed against
the snapshot it runs on, not one taken before the round trip. It is an
API addition every plugin gains ("ask the server, else do this"), not a
refactor-specific one; the format chain is the next user.

**Native facts are queries, owned where they belong.** A plugin can
already compile and run a query against its tree (`compile-query`,
`run-query-ranges`), so no seam is needed to *run* one. The question is
who owns each file:

- **`locals.scm` belongs to `lattice-syntax`**, beside `textobjects.scm`:
  scopes, definitions and references are language knowledge that more than
  refactoring will read (local-symbol highlighting, `gd` without a server).
  Today `lattice-syntax` takes a locals query for JavaScript and TypeScript
  and discards it (`registry.rs`, "locals.scm support deferred"); the other
  languages have none. It compiles them and exposes them to plugins through
  one additive call, `tree-snapshot.language-query(name) -> option<query>`,
  answering the host's compiled query for the snapshot's language, so a
  plugin neither ships its own copy nor recompiles it.
- **`refactor.scm` belongs to the `refactor` plugin**, embedded and
  compiled once per language per instance: facts only refactoring reads
  (a mode owns its surface). Captures:
  - `@refactor.statement`: a node a declaration may go before, as a child
    of the block that holds it. It replaces RF.4e's text rule ("the
    smallest node that starts a line"), which is what put a declaration
    inside a `match` arm.
  - `@refactor.hoist-barrier.<why>`: a subtree whose code does not run
    exactly once, in order, when its statement runs. `<why>` is one of
    `short-circuit` (the right operand of `&&` / `||` / `??`), `branch` (a
    conditional's branches, a `match` arm), `loop` (a loop's condition,
    step or body), `closure` (a closure or lambda body), `comprehension`
    or `default` (a default argument). An expression inside one, below the
    chosen statement, is refused, and the refusal says which. The reason
    rides in the capture name because a capture carries no other string
    (see the declaration form below); the plugin maps the six to sentences,
    and a test pins that each has one.
- **The declaration form stays a table in the plugin**, one line per
  language, because a query capture has no way to carry a string (the
  seam does not expose `#set!` properties, and is not widened for this).
  A test asserts the table and the `refactor.scm` set name the same
  languages.

**Native extract, rebuilt on these.** The expression must be one named node
(unchanged). The declaration goes before the innermost
`@refactor.statement` holding it. It is refused when the expression lies
below a `@refactor.hoist-barrier` inside that statement, or when it reads a
name that `locals.scm` says is defined inside the statement before the
expression (it would be lifted above its own definition). A language with
no `refactor.scm` is refused by name, with "a language server can do this";
RF.4e's text heuristic is removed, not kept as a fallback, because it is the
part that produced wrong code. Extract function stays server-only.

**Native inline, on the same data (RF.4f).** The variable's definition and
uses come from `locals.scm`. Each use is replaced by the value,
parenthesised unless the value is one primary node, and the declaration
removed, in one replacement. Refused when the name is assigned again in its
scope (each use would read a different value), and when the value contains
a call and a use sits below a `@refactor.hoist-barrier` or there is more
than one use (the call would run a different number of times).

*Resolution.* A reference means the visible definition in the innermost
`@local.scope` around it, the latest when one scope holds several. Visible
means its declaration has ended, so the `x` in `let x = x + 1` is the outer
one. This is the one rule the plugin applies to `locals.scm`, for every
language, with no per-language code.

*What `locals.scm` cannot say, `refactor.scm` adds*, built as RF.4f found
each gap:

- `@refactor.binding` with `.name` and `.value`, in one match: a
  declaration of one name with one value, which is what inline deletes. A
  `let mut`, a pattern, a parameter and a multi-name declaration are not
  bindings, so inline names why rather than guessing.
- `@refactor.assigned`: a name written to. In Python, Ruby and Lua binding
  *is* assignment, so the binding's own name is captured too, and a second
  definition of the name in the same scope counts as a write. A Rust
  second `let` is not captured, so it shadows (the first inlines into the
  uses before it) rather than refusing.
- `@refactor.effect`, widening "contains a call" to *runs code or makes a
  new object each time*: a call, `new`, a macro, a list, map or struct
  literal, a closure, `await`. Copying `[]` to two uses makes two lists.
- `@refactor.compound`: a value whose root is an operator, parenthesised
  at a use unless the use is a whole argument, statement or right-hand
  side.
- `@refactor.shorthand`: `S { x }` and `{ x }` name a field as well, so
  the use becomes `x: value`.
- `@refactor.opaque`: text that may read the name with no node for it.
  Rust's `format!("{x}")` is a use `locals.scm` cannot see, and missing it
  would leave the code not compiling, so inline refuses.
- `@refactor.declarator-only`: Java's `{1, 2}` and C++'s `{…}` / `(…)`
  initialisers mean something only in their declaration.
- **Bodies** as barriers: a loop's or a branch's block, and a nested
  function's. Extract is unaffected, since only a barrier inside the chosen
  statement counts there, and a body holds statements of its own. Inline
  moves a value *into* one.

A name the value reads must also mean the same at every use as at the
declaration, and must not be written after it. Otherwise `let y = x;
x += 1; f(y)` would become `f(x)`, reading the new `x`.

Inline reads the variable's scope, not the file (RF.2f). Every use of a
variable lies inside the scope that defines it, so that scope's facts decide
everything the plan checks. It starts at the innermost scope around the
cursor and steps out one scope at a time while the name's definition is not
among what it gathered: a use nested below its declaration. Only a name with
no definition at all reaches the whole file, and refuses there. RF.4f read
the whole file on the grounds that inline is a one-off command, never per
keystroke. The bench disproved that: on 2000 functions the guest ran out of
fuel before answering, so `cri` failed outright in a large file. Bounded,
the same case takes 64 µs.

**What stays out of reach natively**, by design: types and ownership. A
native extract that compiles in Rust may still move a value the next line
uses; that is the server's to catch, and the help page says so.

**The query sources (settled 2026-11-16, RF.4h).** Written for lattice
against each pinned grammar, not adapted from nvim-treesitter's Apache-2.0
files. This keeps the repository MIT with no `NOTICE` obligation, and the
queries are tested against OUR grammar versions. nvim-treesitter's track
their own grammar revisions, so adapting them would have meant rewriting
them anyway, which removed the argument for copying. They cover the ten
languages refactoring acts on: rust, python, ruby, go, javascript,
typescript, tsx, cpp, java and lua. A language without one is refused by
name, as it is today.

**The capture contract** has four names, not three: `@local.scope`,
`@local.definition`, `@local.reference` (a catch-all on the grammar's
identifier kind) and `@local.member`. In Python, Ruby, Lua, Java and Go's
keyed literals, a member or label name (`obj.x`, `f(x=1)`, `{k = v}`,
`Point{X: 1}`) has the same node kind as a variable. A tree-sitter query
cannot exclude it from the catch-all, since it has no negated child pattern
and the seam evaluates only text predicates. So the file marks those
positions, and the rule is that **a node captured as a definition or a
member is not a reference**. Without the fourth name, inlining `x` would
rewrite `obj.x`. `lattice_syntax::locals` states the contract, and the WIT
doc of `language-query` repeats it.

*Built before this decision (RF.4e).* Native extract shipped with the text
rule above and the declaration table. The snippet needed one seam:
`expand-snippet-over` names a *registered* snippet and re-indents its
lines, and extract carries a body of copied code that is already indented.
So `insert-snippet { range, body }` (additive, 0.5.0) expands a body as
written, in one edit, with its session; the guest escapes `$`, `}` and `\`
in the code it copies. That part stays.

*Gap found and closed (RF.4e′).* A plugin's option could not be
buffer-local. Overrides and `ResolvedOptions` were keyed by the option's
Rust `TypeId`, and an option a plugin registers over WIT has none, so
`:setlocal` answered E518 for it and `get-option-in-buffer` could only
return its global value. Options are now keyed by `OptionKey`, which is
the name for a plugin option (`plugin-mode-options.md` §8). Extract read
`refactor.declaration` through `get-option-in-buffer` all along, so it
turned buffer-local with no plugin change.

### 9.2 Options weighed for §9.1

- **Native first, server as an opt-in.** *UX:* worse — the default result
  is the one that cannot see types, in exactly the languages with the best
  servers. Rejected.
- **No native rung; server only.** *UX:* worse — Python with pyright, Lua,
  and any buffer without a server get no extract or inline at all.
  *Paramount #2:* nothing is lost by keeping native, since its knowledge
  is data. Rejected.
- **Keep RF.4e's text heuristic as native's fallback for languages without
  `refactor.scm`.** *UX:* it is the source of the wrong edits; a fallback
  that is sometimes wrong cannot be told apart from one that is right.
  *Heuristic #1:* "it works for the easy cases" is the risk-aversion
  argument. Rejected.
- **For the cascade, the plugin asks which kinds the server supports and
  decides itself.** Synchronous and simple, but a server that answers
  `codeActionProvider: true` advertises no kinds, so the plugin still could
  not know whether native should run; it needs a new LSP read seam and still
  ends in a host-side fallback. Rejected for `otherwise`.
- **The host calls the plugin back with the server's answer.** Most
  general, but a new async callback seam for one decision the host can make
  from data. *Paramount #4* holds either way; `otherwise` is the smaller
  surface. Rejected for now; revisit if a plugin needs to *read* the
  actions.
- **`locals.scm` embedded in the plugin.** Works today with no seam, but a
  second copy of language knowledge `lattice-syntax` should own, and no
  other consumer could use it. Rejected (heuristic #6: the owning crate is
  `lattice-syntax`; no new crate either way).

## 10. Rejected alternatives

- **Native, in `lattice-syntax`** beside the text objects. It would reach
  `SyntaxSnapshot` directly and skip the seam's limits — which is the argument
  against it: those limits are what plugin authors live with, and a refactor
  feature built within them is the test of whether they are right. (No new
  crate either way; heuristic #6.)
- **Per-language query tables for the structural edits** (nvim-treesitter-textobjects' `@parameter`
  swaps): precise where a table exists, dead where it does not. The
  named-sibling rule covers every grammar; a table can refine it later for a
  language that proves the generic rule wrong. *Extract and inline are the
  exception (§9.1):* swap, raise and split move text the user pointed at,
  so a generic rule cannot change meaning; extract and inline move code
  past other code, and no generic rule says when that is safe. There a
  table that is dead where it does not exist (a refusal) is what is
  wanted.
- **One `expand-region` over both sources** — §4.

## 11. Decisions

1. **Placement:** a bundled plugin (§2), over native (§10).
2. **The v1 set:** expand / shrink, swap, raise, and split / join on
   comma-separated nodes (§3).
3. **Keys:** `<M-o>` / `<M-i>` for expand / shrink on `refactor-mode`;
   everything else commands only (§4).
4. **Scope (2026-11-15):** one `refactor-mode` owns every refactor command,
   the server's included (§2, §8) — over keeping the LSP refactors with
   `lsp-code-action-mode`, which would leave "refactor" with two owners, and
   over a native mode, which would undo decision 1.
5. **Operators (2026-11-15):** every refactor is an operator over a range —
   exchange, raise, split / join, extract, inline — with the cursor commands
   as shorthand (§9). Keys `cx` (vim-exchange) and, since decision 10,
   `cr{r,e,i,j,<CR>}` in Normal with `<leader>r{…}` in Visual.
6. **The operator tree seam:** widen `apply-operator`, plugin API 0.5.0 (§9).
7. **Extract / inline sources:** `lsp,native`; native extract does variables
   only (§9).
8. **The exchange mark (2026-11-15):** a tracked-range seam — two effects
   plus `document.tracked-range`, ids per plugin, a native tint (§9) — over
   a static highlight import or no highlight.
9. **Extract / inline rungs (2026-11-15):** language server first, then
   native on query data (`locals.scm` in `lattice-syntax`, `refactor.scm`
   in the plugin), then a refusal with the reason; RF.4e's text heuristic
   removed. The cascade is host-side data, `code-actions { only, range,
   otherwise }`, with a stale-range guard (§9.1, §9.2).
10. **The `c` family (2026-11-15):** refactor operators move from
    `<leader>r{x}` to `cr{x}` in Normal — `crr` raise, `cre` extract, `cri`
    inline, `crj` join, `cr<CR>` split — keeping `<leader>r{x}` in Visual,
    where a `c…` chord would delay Visual `c`. abolish's coercion letters
    stay free (§9).
