# Refactoring: `refactor-mode` (RF-series)

Design: [`refactor.md`](../../architecture/refactor.md) — one
`refactor-mode`, in the bundled `refactor` plugin, owns every refactor
command. The server's refactorings go through
[`lsp-architecture.md` §16](../../architecture/lsp-architecture.md)
("Code actions joined the split"). Applying the edits a refactoring returns
is [`workspace-edits.md`](archive/workspace-edits.md) (WE).

## Why

Asked 2026-11-13: "do we have some support for refactoring?" What existed
was the language server's, reached only through `:lsp-code-action`, with
quick-fixes and source actions in the same list. There was no way to ask
for just the refactorings, `:'<,'>lsp-code-action` asked the server about
the cursor rather than the selection (Visual `:` leaves Visual), and a
charwise selection lost its last character on the way. Two layers were
agreed:

1. **(a) the server's refactorings, named:** kind-filtered commands on the
   code-action request. Commands only, no default keys (decided
   2026-11-14: vim has no convention here, and the default chord slots are
   scarce).
2. **(b) structural operators that need no server:** a bundled plugin on
   `tree-sitter.wit`, for languages or setups with no server and for edits
   no server offers (swap arguments, raise, convert between forms).

Re-scoped 2026-11-15: both layers belong to ONE mode, `refactor-mode`, in the
bundled `refactor` plugin ("a feature set is a mode", `CLAUDE.md`). RF.1
landed (a) as global ex-commands in lattice-grammar; RF.3 moves them into the
mode.

## Slices

| Slice | What | Status |
|---|---|---|
| RF.1 | Code actions move onto §16's split: `LspRequest::CodeActions { only, on_selection }` replaces `Effect::LspCodeAction`, host-applied, so both renderers drop their arm. `:lsp-refactor` / `:lsp-extract` / `:lsp-inline` are three filter values. `only` is sent as `context.only` and applied to the response. `:'<,'>` asks about the last selection; the selection is head-inclusive and `V` covers whole lines. WIT unchanged: `lsp-code-action` maps to the unfiltered form, the filtered forms refuse to cross. | ✅ |
| RF.2 | The `refactor` plugin and its structural edits on `tree-sitter.wit` (design §3–§7). Split into RF.2a–f below. | 🚧 |
| RF.2a | Verify the seam assumptions before building on them (design §6): two `apply-edit` effects from one action call undo as one step, and plugin instance state survives between calls. Fix host-side if the first does not hold (it would fix `gc` too). **Result:** the first did not hold (`gc2j` + `u` restored one line); the host now coalesces a command's edits to one buffer into one batch. The second holds (one grammar `Store` per plugin, made at load). | ✅ |
| RF.2b | `plugins/refactor` crate, registered as a core plugin (`CORE_PLUGINS`, `build_guest`), `refactor-mode` global minor mode (no policy exists for "has a parse tree", so a tree-less buffer gets an echo, not a dead key); `refactor-expand` / `-shrink` **actions** (no `:` form — the `:` line leaves Visual and hands an ex-command no selection) with per-buffer history; `<M-o>` / `<M-i>` in Normal and Visual (`<M-i>` bound in Normal only to say so — unbound, Normal's Alt-stripping fallback made it `i`); `:help refactor`. Also fixed: the TUI test helper `attach_test_syntax` parsed at the snapshot's `version` rather than its text version, so OT.7's freshness gate withheld every tree from plugins in TUI tests. | ✅ |
| RF.2c | `:refactor-swap-next` / `-prev`, counted; error-node and no-sibling refusals. The item is the smallest node whose gap to a neighbour is a list separator (comma, semicolon, or a line break), which corrects the design draft's rule (it picked `a` in `a + 1`). One edit rotating the items with their gaps kept verbatim, so one undo step and no second range to keep valid. | ✅ |
| RF.2d | `:refactor-raise`, cursor form. The target is the parent, or the parent's parent when the parent is a bracket glued to the sibling before it (a call's arguments, a generic's `<…>`), read from text not kinds; refused at top level. The Visual form needs an ex-command to see the selection, which it cannot yet — moved to RF.3d. | ✅ |
| RF.2e | `:refactor-split` / `-join` on comma-separated nodes; trailing comma preserved; refusal for anything else. The indent needed a buffer-local option read, which plugins lacked (`get-option` answers the global value): added `config.get-option-in-buffer`, additive, no package bump, backed by a plugin-host view of the editor's `BufferOptionsResolved` snapshots stored on the publisher's thread, so a read after `:setlocal` never lags (decided 2026-11-15 over inferring indent from the file, reading the global value, or deferring to the `=` indenter, which costs a second undo step). | ✅ |
| RF.2f | Bench (swap / raise / expand / exchange / extract on a large file) into `benchmarks.md`; user docs (`:help refactor` grows a section per operation and operator); correct the `lsp-selection-range-mode` page's "syntactic fallback" line. Last, so it covers RF.4. | 📝 |
| RF.3 | The server's refactorings join `refactor-mode` (design §8). Split into RF.3a–d. | 📝 |
| RF.3a | Settle how a filtered code-action request crosses WIT from a plugin (a widened `lsp-request`, or a new function), and record it in design §8. RF.4a already makes the publish 0.5.0, so a breaking shape costs nothing extra. The other half, an ex-command seeing the Visual selection, is ❌ dropped (2026-11-15): extract and inline became operators (RF.4), which get their range natively. | 📝 |
| RF.3b | Add it: WIT, boundary mapping (replacing RF.1's "no WIT mirror yet" refusal), host, regenerated reference, plugin-API CHANGELOG. | 📝 |
| RF.3c | Move the commands into the plugin, removed from lattice-grammar's `ex_commands.rs` (with `code_actions_spec`): `:lsp-refactor` keeps its name (it lists every refactoring the server offers); `:lsp-extract` / `:lsp-inline` become `:refactor-extract` / `:refactor-inline`, the cursor shorthand of RF.4's operators with the `lsp,native` cascade (design §9). `:lsp-code-action` stays with `lsp-code-action-mode`. End-to-end test that each reaches the host's code-action request with its filter and range; `:help refactor` documents them. After RF.4g. | 📝 |
| RF.3d | `:refactor-raise`'s Visual form. ❌ Dropped (2026-11-15): RF.4b's raise operator is that form — in Visual an operator acts on the selection. | ❌ |
| RF.4 | Every refactor is an operator over a range — Visual, motion, or text object (`aa`, `af`, …) — with the `:` commands as the cursor shorthand (design §9). Split into RF.4a–g. | 📝 |
| RF.4a | Plugin API 0.5.0: `apply-operator` gains `tree: option<borrow<tree-snapshot>>` under OT.7's freshness gate; the native `OperatorContext` carries the syntax snapshot (the `TextObjectContext` scope-resolver precedent). Package bump (`cargo xtask bump-plugin-api 0.5.0`, plus the hand-kept pins and guest locks); all nine `apply-operator` guests updated (six plugins, two fixtures, the `lattice new-plugin` scaffold); CHANGELOG names the break; `tree_seam.rs` gains the operator pair (granted / ungranted) and `perf_ratchet` still holds. The production gate — the actor route filling `syntax` for an operator — is proven by RF.4b's end-to-end test, since a seam test that builds its own env cannot (`plugin-treesitter-seam.md` §5.1). | ✅ |
| RF.4b | Raise operator `<leader>rr{obj}`: the smallest named node spanning the range (whitespace trimmed) is raised by RF.2d's rule; `:refactor-raise` becomes its cursor form over one shared function. Range trimmed of whitespace / `,` / `;`, then must be exactly one node (refused, never widened). Found and fixed on the way: a plugin operator's chord never had `<leader>` expanded (`HostOperatorChordWirer::wire` parsed it verbatim, so it failed and the loader logged "does not parse"), and `refactor` needed `grammar:chord`. Found, not fixed here: `aa` / `ia` capture declaration parameters only, not call arguments (RF.4b′). Tests drive real keys: `<leader>rriw`, Visual after `<M-o>`, a trailing-comma selection, `i(` refused — which also proves RF.4a's production gate. | ✅ |
| RF.4b′ | `aa` / `ia` on call arguments, the nvim-treesitter-textobjects convention: `(arguments (_) @parameter.*)` and peers in each language's `textobjects.scm` (lattice-syntax, N.1.4c), so `<leader>rraa` / `cxaa` reach an argument. Proposed; needs Dhruva's go (it changes what `daa` does in a call). | 📝 |
| RF.4c | Split / join operators `<leader>rs{obj}` / `<leader>rj{obj}` over RF.2e's rule; the commands become their cursor forms (an empty range at the cursor). The nearest list around the range, not an exact match as raise demands — split / join change only whitespace. | ✅ |
| RF.4d | Exchange: `cx{obj}` marks (tinted) then swaps, `cxx` linewise, Visual `X`, `cxc` clears; one replacement spanning both ranges; containment → the larger is replaced by the smaller; partial overlap refused, mark kept; an edit to the marked text drops the mark with an echo. The highlight needed a seam (decided 2026-11-15, design §9): `track-range` / `untrack-range` effects and `document.tracked-range`, additive in 0.5.0, ids moved into a per-plugin space by the `EffectAuthorizer`, tinted natively by `RangeHighlightRegistry` from the published snapshot. Found and fixed: the operator trampoline built its `document` snapshot by hand and dropped the tracked ranges; a plugin mode binding an operator in Visual got a bare invocation that waited for a range (now `Range::Selection`, like Visual `x` / `s`); a mark in Visual did not end Visual (`effect_mutates_or_yanks`). Tests: unit (plan, authorizer spaces, document read, tint fallback, boundary round-trip) and e2e through real keys (swap + one `u`, the tint shown without a further key, the mark following an edit before it, an edited mark dropped, `cxc`, `cxx`, Visual `X`). Follow-up: a plugin-owned `refactor.exchange` element once grammar-world plugins can register theme elements. | ✅ |
| RF.4e | Native extract variable, `<leader>re{obj}`: one expression or refused; declaration before the enclosing statement at its indent; per-language declaration table + buffer-local `refactor.declaration` override (read with `get-option-in-buffer`); one snippet edit with the name linked at both sites. Extract function: refused natively ("needs a language server"). | 📝 |
| RF.4f | Native inline variable, `<leader>ri{obj}`: tree-sitter seam runs the language's `locals.scm` (additive, in 0.5.0); uses replaced by the value (parenthesised unless one primary node), declaration removed, one replacement; reassigned variables refused. | 📝 |
| RF.4g | The LSP rung: extract / inline ask the server first when one is attached (`refactor.source`, default `lsp,native`), on RF.3b's filtered code-action request; a server offering several (extract variable / function / constant) shows the picker. Native runs when there is no server or it offers nothing. | 📝 |

Named so they are not lost:

- **Preview before applying** is A.8 `LspRefactorPreviewProvider`
  ([`../../archive/multibuffer-views.md`](../../archive/multibuffer-views.md)),
  the edit shown as a multibuffer to accept or reject. With WE.3 saving by
  default, version control is the review step until then.
- **`:lsp-format`, `:lsp-rename`, `:lsp-complete`, `:lsp-signature-help`**
  still carry their own Effects. Moving them onto `LspRequest` is the same
  migration as RF.1, with no user-visible change; it is a follow-up, not
  part of this series.
- **A WIT mirror for the filtered request**, so a plugin can ask for
  refactorings: the next plugin-API bump.
