# Refactoring: LSP refactor commands, then structural operators (RF-series)

Design: [`lsp-architecture.md` §16](../../architecture/lsp-architecture.md)
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

## Slices

| Slice | What | Status |
|---|---|---|
| RF.1 | Code actions move onto §16's split: `LspRequest::CodeActions { only, on_selection }` replaces `Effect::LspCodeAction`, host-applied, so both renderers drop their arm. `:lsp-refactor` / `:lsp-extract` / `:lsp-inline` are three filter values. `only` is sent as `context.only` and applied to the response. `:'<,'>` asks about the last selection; the selection is head-inclusive and `V` covers whole lines. WIT unchanged: `lsp-code-action` maps to the unfiltered form, the filtered forms refuse to cross. | ✅ |
| RF.2 | Structural-editing plugin on `tree-sitter.wit`. Design: [`structural-editing.md`](../../architecture/structural-editing.md), agreed 2026-11-14. Split into RF.2a–f below. | 🚧 |
| RF.2a | Verify the seam assumptions before building on them (design §6): two `apply-edit` effects from one action call undo as one step, and plugin instance state survives between calls. Fix host-side if the first does not hold (it would fix `gc` too). | 📝 |
| RF.2b | `plugins/structural` crate, registered as a core plugin (`CORE_PLUGINS`, `build_guest`), `structural-mode` minor mode active where a parse tree exists; `:structural-expand` / `-shrink` with per-buffer history; `<M-o>` / `<M-i>` in Normal and Visual. | 📝 |
| RF.2c | `:structural-swap-next` / `-prev`, counted; error-node and no-sibling refusals. | 📝 |
| RF.2d | `:structural-raise`, cursor and Visual forms. | 📝 |
| RF.2e | `:structural-split` / `-join` on comma-separated nodes; trailing comma preserved; refusal for anything else. | 📝 |
| RF.2f | Bench (swap / raise / expand on a large file) into `benchmarks.md`; user docs and `:help structural-mode`; correct the `lsp-selection-range-mode` page's "syntactic fallback" line. | 📝 |

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
