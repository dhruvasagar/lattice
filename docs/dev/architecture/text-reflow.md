# `text-reflow` — reflow, auto-wrap, and who gets to format a range

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Sequencing lives in the slice plan
> ([`../operations/slice-plans/text-reflow.md`](../operations/slice-plans/text-reflow.md),
> RF.0–RF.7).
>
> **Status: design, not yet implemented.** Opened 2026-09-09.
>
> Sibling fragments: [`auto-indent.md`](auto-indent.md) (`=`, the indent
> engine, and the `:format` cascade this generalises — §7 and §8 there are
> amended by §6 here), [`lsp-architecture.md`](lsp-architecture.md)
> (`textDocument/formatting` / `rangeFormatting`, already implemented),
> [`buffer-local-options.md`](buffer-local-options.md) (the `:setlocal`
> resolution stack every option here rides).

## 1. Where we start from

`auto-indent.md` shipped two thirds of the formatting surface:

- `=` — `operator:reindent`, tree-sitter driven, leading whitespace only (IN.7 ✅)
- `:format` + format-on-save — LSP → `formatprg` → per-language table (IN.8b/IN.9 ✅)
- `equalprg` — declared, ⛔ deferred, **zero consumers**

And deferred the last third by name (§13): *"`gq` / `formatexpr` — text reflow
is a separate verb and a separate feature."* This fragment is that feature,
plus the two questions it forced open once someone asked them out loud:

1. Do `formatprg` / `equalprg`-shaped options belong in a 2026 editor?
2. When an LSP server is attached, should it take over the format verbs?

## 2. Three operations, not one

The single most useful thing to state before anything else, because every
confused design in this space comes from missing it:

| operation | what it may touch | source of truth |
|---|---|---|
| **indent** | leading whitespace only | syntax tree (structural) |
| **reflow** | line breaks within a paragraph | `textwidth` (measure) |
| **reformat** | anything | a formatter's whole opinion |

Vim keeps these on `=`, `gq`, and `formatprg`-through-`gq` respectively.
Helix, Zed and VS Code dropped **indent** entirely and kept the other two.
Nobody merged reflow *into* indent.

The temptation is to collapse them because all three "make the code look
right". They are not substitutable, and the direction of the failure is
asymmetric: an indent that reflows is destructive, whereas a reflow that
declines to reindent is merely incomplete.

### Why `=` is not a formatter — restated, because this keeps coming back

`auto-indent.md` §7 argued it from composition: a range operator whose effect
is unbounded rewriting cannot be composed with motions safely, which is what
makes the vim grammar work at all. That argument stands and is not repeated
here.

What §7 did **not** say, and this fragment adds, is that the same reasoning
forbids the *reverse* delegation too — see §5.

## 3. `gq` and `gw` are one operator

Vim's `gq` and `gw` differ in exactly one respect: `gq` leaves the cursor on
the last formatted line, `gw` restores it. There is no other difference. Two
mnemonics for one operation, and the pair is a reliable source of "which one
was it again".

**Both chords bind to one `operator:reflow`, which preserves the cursor.**
Each gets the full operator-pending cross-product, so `gq{motion}`,
`gw{motion}` and `gqi{obj}` compose exactly like `d{motion}`. The doubled
forms are `gqq` and `gww`.

This is not a novel deviation: Zed's vim keymap already maps `"g q"` and
`"g w"` to the same `vim::Rewrap`. Nobody has reported missing the
cursor-position variant.

**Where this stops following Zed:** Zed also collapses the mixed `gqw` /
`gwq` into further spellings of `gqq`. Lattice does not, because it does not
need to and the collapse costs something. `gq` is **linewise** — vim's "format
the lines that {motion} moves over" — so `gqw` already formats the whole
current line; the two agree without giving anything up, while `gqj` still
spans two lines. Binding `gqw` as a fixed current-line chord would spend a
real composition to gain a second spelling of a chord that already exists.

> **Paramount goals:** protects #3 twice — the two verbs keep vim's meaning,
> and the operator+motion composition that makes the grammar a public API is
> preserved rather than special-cased away.

### `[g, q]` must stay an internal node

A trie node carrying a terminal binding resolves as `Bound` before the walk
descends. So a depth-2 `gq` → "arm operator-pending" binding **kills every
longer chord under it** — `gqq`, `gqap`, `gqi(` — and does it silently, since
a direct trie lookup still answers `Bound` for all of them. Only a real
keystroke walks the prefix and discovers it.

The operator-pending cross-product is what wires `gq{motion}`; `[g, q]` gets
no terminal of its own. `gq_and_gw_stay_internal_nodes_so_their_longer_chords_survive`
pins it, and doubles as the guard for `magit-blame-mode`'s `gq` (stop
blaming): that shadow is safe because a MajorMode layer resolves before
Builtin *and* Builtin leaves the node un-terminated.

See `a-bound-prefix-kills-its-longer-chords`.

> **Paramount goals:** protects #3. The grammar is the public command API, and
> `gq`/`gw` remain in it with vim's meaning; what is dropped is a distinction
> vim itself documents as cursor placement and nothing else.
> **Heuristic #2 (paramount, not other editors):** the argument is that the two
> verbs have no semantic difference to preserve. Zed's identical choice is
> corroboration, not the reason.

### The `gq` conflict, and why it is fine

`magit-blame-mode` already binds `gq` → `magit-blame-quit`, with a comment
predicting exactly this: *"vim's `gq` is the format operator, inert in a
read-only buffer"*. A magit blame buffer is read-only, the binding sits at
`KeymapLayer::MajorMode`, and mode layers resolve before `Builtin` — so the
shadow is correct and deliberate. It gets a test rather than a change.

## 4. The reflow engine

A pure function of `(text, textwidth, comment leader, indent unit)`. No I/O,
no syntax tree, no allocation proportional to the buffer.

### 4.1 Paragraph boundaries

Within the operator's range, a new paragraph begins at:

- a **blank line** — preserved verbatim, never merged across;
- a **leader-only line** (`//` alone inside a `//` block) — the comment
  equivalent of a blank line, and the thing that makes doc comments with
  multiple paragraphs survive `gqaC`;
- a **change of leader** (`//` → `///`);
- a **change of indent**, except a list item's hanging indent (§4.3);
- a **list marker** (§4.3).

### 4.2 The leader is read from the lines, not from the language table

`CommentSyntax::line` gives `//` for Rust — but Rust comments are written
`///` and `//!`, and reflowing a `//!` block with `//` as the leader would
turn continuation lines into `// text`, silently changing an inner doc comment
into an outer one.

So: **the paragraph's leader is the longest common prefix of its lines,
restricted to whitespace plus the characters of `CommentSyntax::line`.**

- `///`-block → LCP is `///`
- `//!`-block → LCP is `//!`
- mixed block → degrades to `//`, which is the honest answer
- `#`-languages → `#`, `##` handled by the same rule with no special case
- markdown / plain (`line: None`) → the leader is the indent alone

One rule, no per-language table of comment decorations, and it extends to any
language a plugin declares a leader for (LG.3).

### 4.3 Lists and hanging indent

A first line may carry a list marker after its leader — `- `, `* `, `+ `,
`1. `, `1) `. Continuation lines indent to the marker's **text column**, not
to the marker:

```
- the first line of a bullet that runs past the
  configured width wraps to here, not to column 0
```

Vim needs `formatoptions+=n` plus a `formatlistpat` regex for this; every
modern rewrap implementation does it unconditionally. Given org and markdown
are first-class here, so do we.

### 4.4 Measure and breaking

- Width is **display columns**, via `unicode-width` (already a workspace
  dependency). Bytes would mis-wrap every CJK and emoji line; chars would
  mis-wrap tabs.
- Greedy fill: append words while the result fits `textwidth`.
- **A word longer than the available width goes on its own line and
  overflows.** Never hard-split a word — matches vim, Emacs `fill-paragraph`,
  and Rewrap. A long URL in a comment stays clickable.
- Trailing whitespace is dropped; interior runs collapse to one space.
- Sentence spacing is not preserved (vim's `fo+=2`, Emacs' `sentence-end-double-space`
  are §9 deferrals).

### 4.5 What it refuses to touch

Inside a fenced code block (```` ``` ````/`~~~` in markdown, `#+begin_…` in
org), reflow is a no-op for the fenced lines: their line breaks are content.
Detected lexically over the range, not from the tree — the operator must work
on a buffer with no parse.

## 5. LSP does not get the format verbs by default

The question that prompted this fragment: when a server is attached, should
`=` / `gq` delegate to it?

**No, and the failure would be silent.** LSP has exactly one range
operation — `textDocument/rangeFormatting`. There is **no reflow request and
no indent-only request** in the protocol. Delegating means asking a
reformatter to do a job it has no API for:

- `gqap` on a Rust doc comment → rust-analyzer → rustfmt → **nothing
  happens**. `wrap_comments` is `false` by default and nightly-only.
- `gqap` on a Markdown paragraph → prettier → **nothing happens**. `proseWrap`
  defaults to `"preserve"`.
- `=ap` → `rangeFormatting` → the range is *reformatted*; line breaks move.
  §7 of `auto-indent.md` is what that violates.

So `gq` would no-op in precisely the cases `gq` exists for. This is the
standing complaint about `set formatexpr=v:lua.vim.lsp.formatexpr()`, and it
is why **conform.nvim — the de-facto modern Neovim formatting plugin —
defaults `lsp_format = "never"`.**

> **Heuristic #2 (paramount, not other editors):** the argument is a protocol
> fact — the requests do not exist. conform.nvim's default is corroboration.

**But the instinct is right about something else:** there is no *operator* for
"reformat this range", only the `:format` ex-command. Vim users reach for `gq`
because it is the only operator-shaped formatting verb available, and are then
disappointed by it. The answer is a third verb (§7), not an overloaded second.

## 6. Intent-keyed provider chains

This supersedes `auto-indent.md` §8's hardcoded cascade and §7's hardcoded
"native only". Both **conclusions** survive as defaults; neither survives as a
hardcode.

### 6.1 The model

Three **intents**. Each resolves an ordered chain of typed **providers**; the
first that is available and returns a result wins.

| intent | default chain | driven by |
|---|---|---|
| `indent` | `[native]` | `=` |
| `reflow` | `[native]` | `gq` / `gw` |
| `reformat` | `[lsp, <per-language table>]` | `:format`, `g=`, format-on-save |

Providers are typed values, not strings:

- `native` — the tree-sitter indent engine (`indent`) or §4's engine (`reflow`)
- `lsp` — `textDocument/formatting` or `rangeFormatting`
- a registered **external spec** — `lattice_format::FormatterSpec`, the
  existing per-language table plus anything the user names
- `plugin:<id>` — a WASM plugin's registered provider

Every intent is a per-buffer option, so `:setlocal` and per-major
`Mode::options()` defaults work with no new machinery.

### 6.2 Why this rather than `formatprg` / `equalprg`

The whole field has converged on ordered typed provider lists, per language:

| | shape |
|---|---|
| Vim / Neovim | `equalprg`, `formatprg`, `formatexpr`, `indentexpr` — four stringly options, fixed precedence |
| conform.nvim | `formatters_by_ft`, `stop_after_first`, `lsp_format = never｜fallback｜prefer` |
| Zed | `formatter:` — `auto｜language_server｜{external}｜{code_actions}`, or a list, per language |
| Helix | `[language.formatter] command/args` + `auto-format`; LSP when unconfigured |

`formatprg` is the 1991 version of this with one slot and no types. Lattice
already has the *pieces* — `FormatterSpec`, the per-language table, the LSP
client — wired together by a two-rung `if` in `do_format_request`. Making the
order data rather than Rust is what lets "use prettier for markdown but the
server for TypeScript" be config instead of a patch.

**"LSP should drive my reflow" becomes one line**, for the user who wants it
and knows what their server does:

```toml
[format.markdown]
reflow = ["lsp", "native"]
```

Which is the flexibility asked for, without making it a default that silently
does nothing.

### 6.3 Migration

- `equalprg` is **deleted**. Zero consumers; it was ⛔ deferred and never
  lit up.
- `formatprg` has three consumers and is **retired into `format.reformat`**,
  retaining a deprecation-shim alias for one release so an existing config
  keeps working with a `:messages` note.
- `formatonsave` keeps its name and meaning; it runs the `reformat` chain.

### 6.4 Chains are expressible, not policed

A user may put `lsp` in the `indent` chain. It will reformat rather than
reindent, and `=` will stop being safe to press casually — which is their call
to make, documented at the option. The design's job is to make the safe thing
the default and the unsafe thing possible, not to make it unreachable.
Paramount #2 is the reason: a policy the host enforces is a policy a plugin
cannot extend.

## 7. `g=` — the reformat operator

`operator:reformat`: the operator form of `:format`, resolving the `reformat`
chain over the operator's range (LSP `rangeFormatting`, or an external filter
fed the range).

`g=` is free — every `g`-prefixed builtin chord was enumerated before
choosing. It is **not** a vim chord and does not pretend to be; it fills the
gap that makes people misuse `gq`, and it is named for its kinship with `=`
rather than against vim's grammar.

Async: the result lands through `SubsystemBoot::inbound::<T>` and applies as a
**minimal edit set** (`lattice_diff` hunks → `Edit`s), exactly as `:format`
already does. A whole-range replace would destroy cursor, marks and folds and
repaint the viewport — a UX veto (`auto-indent.md` §8).

## 8. `textwidth` and `autowrap`

Two typed options replace `textwidth` plus six `formatoptions` letters
(`t c a q n j`):

```
textwidth = 80              # the column. Used by reflow AND auto-wrap,
                            # so the two can never disagree.
autowrap  = comments        # off | comments | all
```

`autowrap` gates **wrapping while typing** only; `textwidth` is always the
measure, so `gq` has a target even where auto-wrap is off. Per-major defaults
ride `Mode::options()`:

- code majors → `comments` (a long string literal is not wrapped; a long
  comment is)
- markdown / org / text / gitcommit → `all`

### Naming

`wrap` is already taken for **soft/display** wrap and is not touched. The
distinction is soft-wrap-is-display versus auto-wrap-inserts-newlines, and the
two options are orthogonal — a buffer may soft-wrap at the window edge while
hard-wrapping at 80.

Emacs' `auto-fill-mode` is the same feature under a name that describes a
1980s implementation ("filling") rather than the effect. Vim has no toggle at
all — it is `formatoptions` letters, which is why nobody remembers them.
`autowrap` says what it does.

### Turning it off

`:setlocal autowrap=off` in the buffer, or `autowrap=off` globally for
languages whose major does not contribute the option.

A **global** `:set autowrap=off` does not reach a markdown, text or
commit buffer, because a major mode's `options()` sits above global
config in the resolution stack (`buffer-local-options.md` §3) and those
three set `all`. That is vim's ftplugin behaviour exactly, and the
per-buffer escape hatch works here where vim needs an autocmd. It is a
real trade and it is made deliberately: the alternative is prose majors
NOT setting a default, which means auto-wrap does nothing out of the box
in the file types most likely to want it.

### Why an option and not a minor mode

It owns no keymap, no lifecycle subscription, no decoration provider, and no
buffer. It is a behaviour flag on the insert path, which is precisely what
`electricindent` (IN.6) already is — same shape, same seam, same per-major
override mechanism. A mode that is really a bool is a mode in name only.

## 9. Auto-wrap is on the keystroke path

When an inserted character pushes the cursor past `textwidth`, break at the
last break point at or before the column and carry the remainder to a new line
with the paragraph's leader and hanging indent.

- **One undo unit with the keystroke**, as vim does. Undoing a typed character
  must not leave the break behind.
- **The comment test is lexical, never a tree query.** `autowrap = comments`
  asks "does this line's first non-blank start with the language's comment
  leader" — a prefix compare on the current line. A tree-sitter query per
  keystroke would be more accurate (it would know a `//` inside a string
  literal is not a comment) and would put a parse on the typing path.
  Paramount #1 is not negotiable for accuracy that costs a frame; the
  inaccuracy is a `//` inside a string, which is rare and harmless.
- Cost is O(current line), no allocation beyond the one edit, and it is
  benched with the other keystroke-path work.

## 10. Error handling

- No `CommentSyntax` for the language → reflow uses the indent as the leader.
  Prose reflow still works; nothing errors.
- `textwidth` narrower than the leader + indent → no break point exists;
  the paragraph is left untouched rather than producing degenerate one-word
  lines.
- An external provider that fails, exits non-zero or times out → log, skip to
  the next rung of the chain, and if the chain is exhausted, report what was
  tried by name. Never a panic, never a silent no-op.
- Format-on-save never blocks the write (`auto-indent.md` §8).

## 11. Rejected alternatives

**One operator, intent chosen by content under the range.** Genuinely one
verb — `=ap` reflows a comment, reindents code. Rejected: the same chord does
different things two lines apart with nothing visible to distinguish them, a
range spanning both does both, and `=` regains the power to move line breaks
that §7 removed on merit.

**`=` absorbs reflow; drop `gq`/`gw`.** Smallest surface, same §7 cost, and it
breaks the verb for the users most likely to press it on a whole file.

**Keep `gq` and `gw` distinct.** Preserves a cursor-placement difference at
the cost of the confusion that prompted this design. Zed's merge is the
precedent; no report of anyone missing it.

**LSP as the default provider for `reflow` / `indent`.** §5 — the requests do
not exist, so the result is a silent no-op in the cases the verbs exist for.

**A single chain shared by all three verbs.** This is what "LSP overrides the
format operator" means literally. It cannot express the one thing the
distinction is for: that a reformatter is right for `:format` and wrong for
`=`.

**Keep `formatprg` / `equalprg` as strings.** §6.2 — one slot, no types, fixed
precedence, and every extension is a host patch.

**Hard-split words longer than `textwidth`.** No editor does this; it breaks
URLs and long identifiers, and the overflow is more readable than the split.

## 12. Paramount-goal alignment

> **UX (higher court):** every verb's effect stays bounded and predictable.
> No keystroke silently does nothing (§5), none gains the power to rewrite
> line breaks unasked (§6.1 defaults), and formatter output applies as a
> minimal diff so no unedited line repaints. Auto-wrap changes only the line
> being typed on.
>
> **#1 Performance:** the reflow engine is a pure text function with no I/O
> and no parse. The only keystroke-path addition is a prefix compare plus an
> O(line) break-point scan, benched. Every external provider runs on
> `spawn_blocking`.
>
> **#2 Extensibility:** providers are a typed open set — `plugin:<id>` is a
> first-class chain entry, so a WASM plugin supplies formatting for a language
> without a host change. The chain being data rather than a Rust `if` is what
> makes that possible.
>
> **#3 Vim modal editing:** `gq` / `gw` are real grammar operators composing
> with every motion and text object; doubled forms behave as vim's do; `=`
> keeps its indent-only meaning by default. `g=` is a deliberate addition,
> named as one.
>
> **#4 Asynchronicity:** provider results land through the inbound primitive,
> never a bare tick callback, and are asserted visible without a further
> keystroke.

## 13. Deferred, named

- **`formatexpr` equivalent** — a plugin-supplied *reflow* engine per
  language, as a `plugin:<id>` entry in the `reflow` chain. The chain shape
  admits it; nothing implements the guest side yet.
- **`colorcolumn`** — rendering `textwidth` as a rule. A renderer concern, and
  the natural companion once `textwidth` exists.
- **Sentence-aware filling** — vim's `fo+=2`, Emacs' double-space convention.
- **`fo+=a`** — continuous automatic paragraph reflow as you type, Emacs'
  `refill-mode`. Distinct from `autowrap`, which only breaks the current line.
- **Block-comment reflow** (`/* … */`) — `CommentSyntax::block` is carried but
  unused; v1 reflows line-comment blocks and prose.
- **Reflow inside injected languages** — a fenced code block in markdown is
  skipped (§4.5) rather than reflowed by the inner language's rules.
