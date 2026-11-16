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
| RF.3a | Settle the WIT shape of a plugin's code-action request, per design §9.1: `code-actions { only, range: option<range>, otherwise: option<command-ref> }` (widened, in the unpublished 0.5.0) and the native `LspRequest::CodeActions` gaining the same two fields. Check the widened shape against a component built for the current package; record the answer in design §8. The other half, an ex-command seeing the Visual selection, is ❌ dropped (2026-11-15): extract and inline became operators (RF.4), which get their range natively. | 📝 |
| RF.3b | Add it: WIT, boundary mapping (replacing RF.1's "no WIT mirror yet" refusal), host. The host asks about `range` when given; invokes `otherwise` when no server is attached, none returns an action, or the server advertises `codeActionKinds` and none matches (no round trip); invokes nothing, with an echo, if the text version changed while waiting. `otherwise` runs from the async landing and reaches the screen with no key (test asserts it without a further dispatch). Regenerated reference, plugin-API CHANGELOG. Tests: each branch, plus the stale guard. | 📝 |
| RF.3c | Move the commands into the plugin, removed from lattice-grammar's `ex_commands.rs` (with `code_actions_spec`): `:lsp-refactor` keeps its name (it lists every refactoring the server offers); `:lsp-extract` / `:lsp-inline` become `:refactor-extract` / `:refactor-inline`, the cursor shorthand of RF.4's operators with the `lsp,native` cascade (design §9). `:lsp-code-action` stays with `lsp-code-action-mode`. End-to-end test that each reaches the host's code-action request with its filter and range; `:help refactor` documents them. After RF.4g. | 📝 |
| RF.3d | `:refactor-raise`'s Visual form. ❌ Dropped (2026-11-15): RF.4b's raise operator is that form — in Visual an operator acts on the selection. | ❌ |
| RF.4 | Every refactor is an operator over a range — Visual, motion, or text object (`aa`, `af`, …) — with the `:` commands as the cursor shorthand (design §9). Split into RF.4a–j. | 🚧 |
| RF.4a | Plugin API 0.5.0: `apply-operator` gains `tree: option<borrow<tree-snapshot>>` under OT.7's freshness gate; the native `OperatorContext` carries the syntax snapshot (the `TextObjectContext` scope-resolver precedent). Package bump (`cargo xtask bump-plugin-api 0.5.0`, plus the hand-kept pins and guest locks); all nine `apply-operator` guests updated (six plugins, two fixtures, the `lattice new-plugin` scaffold); CHANGELOG names the break; `tree_seam.rs` gains the operator pair (granted / ungranted) and `perf_ratchet` still holds. The production gate — the actor route filling `syntax` for an operator — is proven by RF.4b's end-to-end test, since a seam test that builds its own env cannot (`plugin-treesitter-seam.md` §5.1). | ✅ |
| RF.4b | Raise operator `<leader>rr{obj}`: the smallest named node spanning the range (whitespace trimmed) is raised by RF.2d's rule; `:refactor-raise` becomes its cursor form over one shared function. Range trimmed of whitespace / `,` / `;`, then must be exactly one node (refused, never widened). Found and fixed on the way: a plugin operator's chord never had `<leader>` expanded (`HostOperatorChordWirer::wire` parsed it verbatim, so it failed and the loader logged "does not parse"), and `refactor` needed `grammar:chord`. Found, not fixed here: `aa` / `ia` capture declaration parameters only, not call arguments (RF.4b′). Tests drive real keys: `<leader>rriw`, Visual after `<M-o>`, a trailing-comma selection, `i(` refused — which also proves RF.4a's production gate. | ✅ |
| RF.4b′ | `aa` / `ia` on call arguments, the nvim-treesitter-textobjects convention: `(arguments (_) @parameter.*)` and peers in each language's `textobjects.scm` (lattice-syntax, N.1.4c), so `<leader>rraa` / `cxaa` reach an argument. Proposed; needs Dhruva's go (it changes what `daa` does in a call). | 📝 |
| RF.4c | Split / join operators `<leader>rs{obj}` / `<leader>rj{obj}` over RF.2e's rule; the commands become their cursor forms (an empty range at the cursor). The nearest list around the range, not an exact match as raise demands — split / join change only whitespace. | ✅ |
| RF.4d | Exchange: `cx{obj}` marks (tinted) then swaps, `cxx` linewise, Visual `X`, `cxc` clears; one replacement spanning both ranges; containment → the larger is replaced by the smaller; partial overlap refused, mark kept; an edit to the marked text drops the mark with an echo. The highlight needed a seam (decided 2026-11-15, design §9): `track-range` / `untrack-range` effects and `document.tracked-range`, additive in 0.5.0, ids moved into a per-plugin space by the `EffectAuthorizer`, tinted natively by `RangeHighlightRegistry` from the published snapshot. Found and fixed: the operator trampoline built its `document` snapshot by hand and dropped the tracked ranges; a plugin mode binding an operator in Visual got a bare invocation that waited for a range (now `Range::Selection`, like Visual `x` / `s`); a mark in Visual did not end Visual (`effect_mutates_or_yanks`). Tests: unit (plan, authorizer spaces, document read, tint fallback, boundary round-trip) and e2e through real keys (swap + one `u`, the tint shown without a further key, the mark following an edit before it, an edited mark dropped, `cxc`, `cxx`, Visual `X`). Follow-up: a plugin-owned `refactor.exchange` element once grammar-world plugins can register theme elements. | ✅ |
| RF.4e | Native extract variable, `<leader>re{obj}`: one expression or refused; declaration before the enclosing statement (read from text: the smallest node strictly around the expression that starts a line, its parent starting earlier) at its indent; per-language table (rust, python, ruby, go, javascript / typescript / tsx, cpp, java, lua; c and bash refused) + the `refactor.declaration` option (`register-options`, read with `get-option-in-buffer`; global only until RF.4e′); one snippet edit with the name linked at both sites, through a new additive `insert-snippet { range, body }` effect (verbatim — `expand-snippet-over` names a registered snippet and re-indents). Extract function is not offered natively. Tests: unit (templates, escaping, the table) and e2e (Visual extract + the name typed once at both sites, one `u`, `i(` refused, Python, the declaration option via `:set`). Known limit: a match arm starts a line and counts as a statement. | ✅ |
| RF.4e′ | Plugin options can be buffer-local. Today overrides and `ResolvedOptions` are keyed by `TypeId`, which a WIT-registered option lacks, so `:setlocal <plugin-option>` is E518 and `get-option-in-buffer` returns the global value. Needs a key a runtime option has (an option key over `TypeId` or registry index) across `lattice-config` and its ~50 override sites. A config-substrate change, not refactor's; needs Dhruva's go and its own design note. | 📝 |
| RF.4f | Native inline variable, `cri{obj}` (Visual `<leader>ri`), on RF.4h's `locals.scm` and RF.4i's barriers (design §9.1): uses replaced by the value (parenthesised unless one primary node), declaration removed, one replacement. Refused when the name is reassigned in scope, or when the value contains a call and a use sits below a `@refactor.hoist-barrier` or there is more than one use. After RF.4i. | 📝 |
| RF.4g | The LSP rung: `cre` / `cri` return RF.3b's request with `only: extract` / `inline`, the operator's range, and `otherwise` naming the native command (`refactor-extract-native` / `-inline-native`, with the range and text version as args). `refactor.source` (`lsp,native` default; `lsp`, `native`) orders the rungs; `native` skips the server. A server offering several actions shows the picker. Inline's `otherwise` echoes until RF.4f. Tests: no server → native edit; server stub with an action → picker; stub with none → native; `refactor.source=lsp` with none → echo, text unchanged. | 📝 |
| RF.4h | `locals.scm` in `lattice-syntax`, per design §9.1: compiled in `build_config` (today it discards JS/TS's), authored for every shipped language with a grammar (adapted from nvim-treesitter, Apache-2.0: notice on each file + a `NOTICE` entry — confirm when landing), and exposed through additive `tree-snapshot.language-query(name) -> option<query>` returning the host's compiled query. Tests: each language's query compiles; a definition / reference / scope fixture per language; the seam returns `none` for an unknown name and for a language without the file. Bench: boot compile cost into `benchmarks.md`. | 📝 |
| RF.4i | Native extract rebuilt on query data (design §9.1): `refactor.scm` per language in the plugin (`@refactor.statement`, `@refactor.hoist-barrier`), compiled once per instance; the declaration goes before the innermost `@refactor.statement`; refused below a barrier inside that statement and when the expression reads a name `locals.scm` defines inside the statement before it; a language without `refactor.scm` refused by name. RF.4e's text rule removed. Test that the declaration table and the `refactor.scm` set agree. E2e per hazard (scope, `&&`, ternary, loop condition, closure, comprehension, `match` arm) in at least Rust, Python and one of Go / TypeScript, each refused with its reason; the RF.4e happy paths still pass. | 📝 |
| RF.4j | The `c` family (design §9, decision 10): Normal chords moved to `crr` / `cre` / `crj` / `cr<CR>` (split off `s`, abolish's snake_case); Visual `<leader>r{r,e,j,<CR>}` bound on `refactor-mode`'s Visual layer. **All of it is `refactor-mode`'s** ("modes own their full surface"): the Normal chords come from each operator's `chord`, which `HostOperatorChordWirer` binds at `KeymapLayer::MinorMode(refactor-mode)` on Normal's operator-pending path only (`operator_chord_wirer.rs`), never `Builtin`, never Visual; the Visual forms are the mode's keymap bindings; the handlers are the plugin's operator callbacks; the host keeps only generic primitives. So no `c…` chord is bound in Visual. `<leader>rs` retired. Tests through real keys: every Normal chord (the raise / split / join / extract suites moved over), the Visual forms (split, join, raise, extract), Visual `c` still changing at once, and `refactor.enabled=false` leaving `cre` and Visual `<leader>re` inert. The layer scoping of every chord continuation is CM.2's `plugin_operator_chords_stay_in_their_layer.rs`, so no `:describe-key` test was added. Found on the way: the TUI refactor harness never wired `subscribe_mode_gates`, and dropped its loader (the gate holds a `Weak`), so `:set refactor.enabled=false` had no listener; the harness now does both, as `install` does. | ✅ |

## Order from here (decided 2026-11-15)

Extract and inline go server first, then native on query data, then a
refusal (design §9.1). So the server rung comes before more native work:

0. **RF.4j** — the `cr` keys, first, so every later slice binds the
   final chord.
1. **RF.3a → RF.3b** — the widened code-action request with `range` and
   `otherwise`.
2. **RF.4g** — extract (and inline's server half) ask the server first.
3. **RF.4h** — `locals.scm` in `lattice-syntax`, exposed to plugins.
4. **RF.4i** — native extract rebuilt on `refactor.scm` + locals; the text
   rule removed.
5. **RF.4f** — native inline on the same data.
6. **RF.3c**, then **RF.2f** (bench + docs, last).

RF.4b′ and RF.4e′ wait on a go and do not block any of these.

Every operator in this series (exchange, raise, split / join, extract,
inline, and case coercion if it comes) is owned by `refactor-mode`: its
chord, its Visual binding, its handler, its options and its `:help`. A new
refactor adds nothing to `lattice-host` but generic seams.

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
