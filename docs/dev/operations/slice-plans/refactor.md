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
| RF.2d | `:refactor-raise`, cursor and Visual forms. | 📝 |
| RF.2e | `:refactor-split` / `-join` on comma-separated nodes; trailing comma preserved; refusal for anything else. | 📝 |
| RF.2f | Bench (swap / raise / expand on a large file) into `benchmarks.md`; user docs (`:help refactor` grows a section per operation); correct the `lsp-selection-range-mode` page's "syntactic fallback" line. | 📝 |
| RF.3 | The server's refactorings join `refactor-mode` (design §8). Split into RF.3a–c. | 📝 |
| RF.3a | Settle how the two seam additions cross WIT — a filtered code-action request from a plugin, and an ex-command seeing that it was given `'<,'>` — by checking a component built for the current package against each shape (widened `lsp-request` / `ex-command-context`, or new functions). Record the answer in design §8; it decides patch vs bump for the plugin-API publish. | 📝 |
| RF.3b | Add them: WIT, boundary mapping (replacing RF.1's "no WIT mirror yet" refusal), host, regenerated reference, plugin-API CHANGELOG. | 📝 |
| RF.3c | Move `:lsp-refactor` / `:lsp-extract` / `:lsp-inline` into the plugin: registered by `refactor`, removed from lattice-grammar's `ex_commands.rs` (with `code_actions_spec`); `:lsp-code-action` stays with `lsp-code-action-mode`. End-to-end test that each reaches the host's code-action request with its filter and selection; `:help refactor` documents them. | 📝 |

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
