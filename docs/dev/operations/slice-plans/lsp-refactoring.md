# Refactoring: LSP refactor commands, then structural operators (RF-series)

Design: [`lsp-architecture.md` §16](../../architecture/lsp-architecture.md)
("Code actions joined the split"). Applying the edits a refactoring returns
is [`workspace-edits.md`](workspace-edits.md) (WE).

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
| RF.2 | Structural-operators plugin on `tree-sitter.wit`: its own design fragment first (operator set, grammar surface, what "structural" means per language). | 📝 |

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
