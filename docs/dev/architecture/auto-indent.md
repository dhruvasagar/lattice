# `auto-indent` — indentation from tree-sitter, formatting from external tools

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Sequencing lives in the slice plan
> ([`../operations/slice-plans/auto-indent.md`](../operations/slice-plans/auto-indent.md),
> IN.0–IN.11).
>
> **Status: design, not yet implemented.** Opened 2026-08-15.
>
> Sibling fragments: [`plugin-treesitter-seam.md`](plugin-treesitter-seam.md)
> (the tree snapshot + query surface this computes over),
> [`buffer-local-options.md`](buffer-local-options.md) (the `:setlocal`
> resolution stack the indent options ride),
> [`lsp-architecture.md`](lsp-architecture.md) (`textDocument/formatting`,
> `rangeFormatting`, `onTypeFormatting` — already implemented, unconsumed by
> indentation), [`fold-architecture.md`](fold-architecture.md) (`foldmethod`,
> whose cascade shape `indentmethod` follows).

## 1. Where we start from

Nothing indents. Concretely:

| Surface | Today |
|---|---|
| `o` / `O` / `<CR>` / `cc` | Splice a bare `\n`. The new line starts at **column 0** — not even vim's `autoindent` (copy the previous line's indent). `dispatch.rs:19283` |
| `>` / `<` / `<C-t>` / `<C-d>` / blockwise `>` `<` | Work, but against a hardcoded `const INDENT_UNIT: &str = "    "` (`dispatch.rs:19411`) |
| `shiftwidth`, `expandtab`, `softtabstop` | **Do not exist.** Only `tabstop` is registered, and it means *display width of a tab byte* |
| `=` (reindent operator) | Does not exist. `builtins.rs` has `indent_left` / `indent_right` only |
| `indents.scm` | Does not exist. `folds` / `symbols` / `textobjects` / `highlights` do, per language, embedded via `include_str!` |
| LSP formatting | `formatting`, `range_formatting`, **`on_type_formatting`** all implemented in `lattice-lsp/src/features.rs` with capability probes; reachable only through `:lsp-format` / `:lsp-format-range` |
| External formatters | No `formatprg` / `equalprg` equivalent anywhere |

So this fragment is not a refinement of an existing indent story. It builds one.

## 2. The reframing — three surfaces, three latency budgets

"Auto-indent" reads like one feature. It is three, and the reason they cannot
share a source is paramount goal #1.

| # | Surface | Fires on | Budget | Viable source |
|---|---|---|---|---|
| 1 | **Predictive indent** — what column a newly-created *empty* line starts at | `<CR>`, `o`, `O`, `cc`, `S` | Keystroke path | Tree-sitter only |
| 2 | **Electric reindent** — typing `}` / `end` / `else` snaps the current line | Every Insert keystroke | Keystroke path | Tree-sitter, + LSP on-type as an additive layer |
| 3 | **Reindent existing text** — `=`, `==`, `=ap`, `:format`, format-on-save | User-initiated | Async, ~seconds | LSP → external tool → tree-sitter |

Only surface 3 can lean on external tools. An LSP round-trip cannot sit on the
keystroke path, and a process spawn cannot sit there twice over. **This is the
whole reason the tree-sitter indent engine is unavoidable**: the stated
preference to not own per-language indentation logic is satisfiable only for
surface 3, and only surfaces 1 and 2 are what a user means when they say "the
editor doesn't indent."

The engine is the mitigation, not a contradiction of that preference. It is
**language-agnostic** — roughly 500 lines of tree-walking that never names a
language. Every per-language fact lives in `indents.scm` data files written
against a capture vocabulary Helix and nvim-treesitter both established. We own
an evaluator; we do not own nineteen indentation algorithms.

## 3. The indent-unit model

Seven typed options, all buffer-local-capable through the existing `:setlocal`
resolution stack (see [`buffer-local-options.md`](buffer-local-options.md) §3):

| Option | Type | Default | Meaning |
|---|---|---|---|
| `shiftwidth` | int `1..=32` | `4` | Columns per indent level |
| `expandtab` | bool | `true` | Render one level as spaces vs. a tab byte |
| `indentmethod` | enum | `syntax` | `none` \| `keep` \| `syntax` |
| `electricindent` | bool | `true` | Re-indent the current line on a closing token |
| `equalprg` | string | `""` | External *indent* filter for `=`; empty ⇒ tree-sitter |
| `formatprg` | string | `""` | External *formatter* for `:format`; empty ⇒ default table |
| `formatonsave` | bool | `false` | Run the `:format` cascade before `:w` |

`shiftwidth = 4` / `expandtab = true` are not new opinions —
`site/content/cheatsheet.md:210-211` already documents exactly these defaults
for options that were never registered. This makes the docs true.

### `tabstop` is not the indent unit

`tabstop` (already registered, default 4 — lattice house style; vim's
historical default is 8) stays precisely what it is: **the display width of a
literal tab byte**. It is a rendering property. `shiftwidth` is an editing
property. Conflating them is a common editor bug — it makes "change my indent
size" silently reflow every file that contains a hard tab, including files the
user did not edit. They stay separate from the first commit.

### `indentmethod` is a cascade, and that is on merit

```
syntax  → tree-sitter indents.scm    ─ falls back to ─┐
keep    → previous line's indent (vim's autoindent)  ─┤
none    → column 0                  ←────────────────┘
```

This mirrors `foldmethod`'s shape, and the justification is the property, not
the resemblance: both are *a structural source that can be unavailable at the
moment it is asked* — no query for this language, a parse that has not landed,
a grammar that failed to load. A cascade with a named floor means the failure
is a graceful degradation to documented vim behaviour rather than a silent
wrong answer, and it gives a user whose language has a bad query a one-word
escape hatch. `keep` is not a stub: it is a real, useful setting that some
users prefer, so the fallback path is exercised by choice, not only by failure.

### Per-language defaults ride an existing seam

`Mode::options() -> OptionOverrideSet` (layer 4 of the buffer-local stack) is
already how a major mode contributes option values. The `go` major mode
contributes `expandtab = false`; `python` confirms `shiftwidth = 4`; a future
`make` mode contributes tabs. **No new mechanism.** The buffer-local design
built this seam and nothing has yet used it for indentation.

### What this deletes

`const INDENT_UNIT: &str = "    "` (`dispatch.rs:19411`) and its peers. `>`,
`<`, `<C-t>`, `<C-d>` and blockwise-visual `>` / `<` become `shiftwidth`- and
`expandtab`-aware. That is a standalone user-visible fix that lands before a
line of tree-sitter code exists (IN.0).

## 4. The engine

### 4.1 Dialect

Helix's capture vocabulary, **hand-written** queries. v1 evaluates:

| Capture | Effect |
|---|---|
| `@indent` | The node's children are one level deeper |
| `@indent.always` | As `@indent`, but does not collapse with a sibling `@indent` on the same line |
| `@outdent` | The node's own line is one level shallower |
| `@outdent.always` | As `@outdent`, without collapsing |
| `@extend` | The node's indent extends past its end (braceless `if` bodies) |
| `@extend.prevent-once` | Cancels the innermost `@extend` |
| `#set! "scope" "all"\|"tail"` | Whether the node's own start line is included |

Hand-written rather than vendored, for two reasons. Licensing: lattice is MIT;
Helix's queries are MPL-2.0 (file-scoped copyleft — legal to carry, but those
files would remain MPL inside an MIT tree and need their own headers and a
NOTICE). Precedent: `queries/rust/textobjects.scm` and its siblings were
already written *following* the nvim-treesitter/Helix capture convention
without vendoring, and that has held up.

**`@align` is deferred.** It aligns continuation lines to an opening
delimiter's *column* rather than to an indent level, which forces the engine to
carry a column anchor alongside a level and interacts badly with
`expandtab = false` (a column anchor is not expressible in tab units). It is a
refinement; its absence means aligned-to-paren argument lists indent by one
level instead. Named here so it reads as a decision, not an oversight.

### 4.2 Algorithm

```
indent_for_line(snapshot, rope, line, unit) -> Option<IndentLevel>
```

Find the node covering the target position; walk the ancestor chain to the
root; accumulate each ancestor's capture contribution, scoped by whether the
target line falls inside that node's line span. A pure function of
`(tree, rope, line, IndentUnit)` — no host state, no async, no I/O, trivially
unit-testable against a hand-built tree.

## 5. Staleness — the bounded reparse

**This is the hardest part of the design and the one most likely to be got
wrong quietly.**

`lattice-syntax` publishes snapshots through `ArcSwap`. On an edit,
`try_apply_intermediate` (`syntax.rs:410`) shifts the cached tree's byte ranges
cheaply; the actual reparse runs off-thread in `reparse_with_cached_tree`. So
at the instant `<CR>` is pressed, the tree reflects the last **completed**
parse. Type `fn f() {` and hit Enter, and the tree has no block node yet — the
one node whose existence determines the answer.

Helix does not have this problem because it parses synchronously. lattice
deliberately does not, and that is not negotiable.

Indent computation runs on the **editor actor thread**, not the UI thread, so a
parse there is not a "no UI-thread work" violation. It is still inside the
keystroke→glyph budget, so it needs a ceiling:

```
snapshot.text_version == buffer.text_version   → query directly
buffer len <= indent-reparse budget            → synchronous incremental reparse
                                                 on the actor thread, seeded
                                                 from the cached tree, then query
otherwise                                      → lexical bridge; debug! once
```

The budget is a **measured** number, set from the IN.2 bench and recorded in
`benchmarks.md`, not a guessed constant. Both branches carry tests, including
one that pins the budget to `0` so the lexical path is exercised
deterministically rather than only on someone's large file.

The lexical bridge is the previous line's indent plus an opener/closer scan.

> **Corrected at IN.1.** An earlier revision of this section claimed the bridge
> was "exactly what `indentmethod=keep` promises". It is not, and §3 said
> otherwise — `keep` is documented there as vim's `autoindent`, a **pure
> copy**. Vim separates `autoindent` from `smartindent` for a real reason: the
> brace rule misfires in a language where `{` is not a block opener, and `keep`
> is what a user picks when they want the dumbest predictable thing. The two
> are now distinct:
>
> - **`keep`** — copy the previous non-blank line's indent. No scan.
> - **`syntax`'s fallback** — copy, plus one level if the previous line leaves
>   a bracket unclosed, minus one if the target line opens with a closer. The
>   opener/closer sets are **per-language**, and languages with no bracket
>   notion (plain text, markdown) have empty sets, at which point the bridge
>   degrades to `keep` on its own rather than by special case.
>
> They still share `lattice-indent`'s `lexical.rs` and the same
> previous-line-scan primitive, so neither is dead weight; they are two
> policies over one mechanism.

> **UX (higher court):** the degraded path lands one level off after typing an
> opener on a very large file; the next keystroke or `=` corrects it. No
> flicker, no change to unedited content, no dropped keystroke.
> **Paramount goals:** protects #1 (the parse is budgeted *and* benched, and
> the fallback is O(line)); costs nothing in #3, since the fallback is vim's
> documented `autoindent` behaviour.
> **Heuristic #1 (long-term fit):** the rejected alternative — always reparse —
> is strictly easier to write. It was rejected because its failure mode is
> invisible until a large file is opened, which is the worst kind of latency
> bug to ship.

## 6. Electric reindent

After an Insert-mode character lands, if it is in the language's electric set,
recompute the current line's indent and rewrite **only its leading
whitespace**, only if it differs. One edit, one line, inside the same undo
group as the typed character.

The electric set is derived from the language's `@outdent` captures plus a
small per-language keyword table for word-shaped closers (`end`, `else`,
`elif`, `when`, `esac`).

> The UX contract permits this: only the edited line changes visibly, and it
> changes synchronously with the keystroke that caused it.

### LSP `onTypeFormatting` is additive, not primary

`textDocument/onTypeFormatting` already exists in `features.rs:646` with a
capability probe exposing the server's advertised trigger characters. Server
coverage is uneven and varies by version — some major servers advertise none at
all, and several that do advertise omit `}`, the character that matters most.
The design does not depend on any specific server's set: the trigger characters
are **probed from the capability at runtime**.

It is therefore layered *on top of* the tree-sitter path, for the advertised
trigger characters our electric set does **not** already cover — a server that
re-indents a method chain on `.` adds something tree-sitter's dedent captures
do not. Two hard constraints:

1. **Opt-in**, off by default (`lsp.on-type-formatting`). It is an async
   round-trip on the typing path.
2. **Returned edits are filtered to the current line.** The protocol permits a
   server to return edits anywhere in the document; applying those would be a
   pixel change to content the user did not edit, which the UX contract vetoes
   outright. Out-of-line edits are dropped with a `debug!`.

## 7. `=` is indent-only — and that is why external tools do *not* back it

Vim's `=` adjusts leading whitespace and nothing else; `equalprg` is specified
as an indent-only filter. LSP `rangeFormatting` and every external formatter
(rustfmt, prettier, black, gofmt) **reformat**: they move line breaks, adjust
spacing, insert trailing commas.

Binding `=ap` to rustfmt would mean "rewrite this paragraph", not "reindent
it". `=` would stop being the surgical tool it is in vim, and would become
unsafe to press casually. Vim itself keeps the two on separate verbs —
`=`/`equalprg` for indent, `gq`/`formatprg` for reflow — and that separation is
correct rather than historical.

So:

- `=`, `==`, `=ap`, visual `=` → tree-sitter indent engine, or `equalprg` if
  set. Leading whitespace only. One undo unit for the whole range.
- `:format` / format-on-save → the full cascade in §8.

Because indent comes from the tree rather than from the preceding line's actual
text, reindenting a range has no order dependency between lines. The tree must
be fresh, but `=` is user-initiated, so it takes an unbounded synchronous
catch-up parse — §5's budget applies to the keystroke path only.

> **Paramount goals:** protects #3 (strict vim semantics; the grammar is the
> public command API and `=` has a defined meaning in it).
> **Heuristic #2 (paramount, not other editors):** the argument is not "vim
> does it this way" — it is that a range operator whose effect is unbounded
> rewriting cannot be composed with motions safely, which is what makes the
> vim grammar work at all.

## 8. `:format` — the external-tool cascade

```
:format            whole buffer
:[range]format     a line range

  LSP server attached with formatting capability  → textDocument/formatting
  formatprg set, or a default-table entry on PATH → external process
  neither                                         → error naming what was tried
```

`:format` looks like it violates the dashed-and-namespaced ex-command rule
("no generic-name aliases — `format`, `rename`, `complete`"). It does not, and
the reason is exactly the rule's own rationale: that rule exists because a
generic name *implies it should work regardless of LSP* while being hard-wired
to an LSP-only path. `:format` **is** the LSP-independent cascade. `:lsp-format`
remains the LSP-only command, unchanged.

### The default formatter table

Per-language `FormatterSpec { argv, stdin, filename_arg }`, each **probed on
PATH** and silently skipped when absent: rustfmt, prettier, black/ruff, gofmt,
clang-format, stylua, shfmt, taplo. The table is a starting point, not a
contract — formatter CLIs change flags across versions, so `formatprg`
overrides it entirely and every entry is one line to correct.

### Applying the result is a minimal edit set, not a replace

A formatter returns a whole new file. Splicing that over the buffer would
destroy cursor position, marks, folds, and every renderer fast path, and would
show as a full-viewport repaint — a UX veto.

Instead: `lattice_diff::compute_diff(&[old, new], algorithm)` produces
line-granular hunks (`compute.rs:98`), and each hunk becomes one `Edit`. Line
granularity is the right resolution for a formatter, and `lattice-diff` is
already the in-tree engine for exactly this.

### Format-on-save must not lose the save

`:w` with `formatonsave = true` sequences format → apply edits → write. On
formatter failure, non-zero exit, or timeout (budget ~2s), it **logs and writes
unformatted**. A save that silently does not happen because a formatter hung is
a far worse failure than an unformatted save.

The formatted result lands asynchronously and therefore goes through
`SubsystemBoot::inbound::<T>` — not a bare `TickCallback`, which would leave
the result invisible until the user's next keystroke. Tested the way it fails:
the assertion is that the result is visible *without* dispatching another
action.

## 9. Crate placement

**One new crate, not two.** The indent value goes in `lattice-core`, the
engine into the existing `lattice-syntax`, and only the external-tool runner
earns a crate of its own.

**`lattice-core::indent`** (IN.0, landed) — `IndentUnit { width,
expand_tabs, tabstop }` with `columns_of` / `render` / `shift` /
`reindented_prefix`, plus the `IndentMethod` enum.

The placement is load-bearing rather than tidiness: `lattice-syntax` depends on
`lattice-grammar`, so a value type owned above `lattice-syntax` and consumed by
the `>` / `<` operators *in* `lattice-grammar` is a **dependency cycle**.
`lattice-core` is the floor both sides already stand on, and where the sibling
`FoldMethod` lives for exactly the same reason.

**`lattice-syntax::indent`** (IN.1+) — pure, synchronous, no I/O, no async, no
host state. The engine over that value: the lexical bridge (IN.1), then the
`indents.scm` evaluator (IN.2).

> **Corrected during IN.1.** This section originally specified a dedicated
> `lattice-indent` crate, justified by "`syntax.rs` is already 119 KB". That
> was a bad argument — it describes one *file*, not the crate, and
> `lattice-syntax` already has eleven modules. `indent` belongs beside
> `text_objects.rs` and `motions.rs`, which are the same shape: computation
> over the parse tree, driven by `.scm` files this crate already owns and
> embeds. A separate crate needs a stronger reason than module count, and there
> wasn't one.
>
> Not `lattice-grammar` either, despite `>` / `<` living there: its
> `Cargo.toml` states that it "stays tree-sitter-agnostic", and IN.2's engine
> needs the tree — so the engine would have to move out again one slice later.
> `lattice-core` is out for the same reason (it must not depend on
> `lattice-syntax`), which is exactly why the *value* and the *engine* are
> split across the two.

**`lattice-format`** — the external-tool side.

```
spec.rs      FormatterSpec + the per-Lang default table
runner.rs    spawn_blocking + timeout + stderr capture
apply.rs     formatter output → minimal Edit set via lattice-diff
```

The `.scm` files stay in `lattice-syntax/queries/<lang>/` beside their
siblings, embedded with `include_str!` and re-exported — the engine does not
own the query *files*, only their evaluation.

The engine does not live *in* `lattice-syntax` even though the queries do.
`lattice-syntax` owns parsing and publishing the tree; `syntax.rs` is already
119 KB, and this is a subsystem with an algorithm, per-language golden tests,
and a benchmark, which will grow. Keeping it out is the long-term-fit call
(heuristic #1), not a size complaint.

Neither crate is a mode. Indentation is core editing behaviour that applies in
every buffer — `=` is a grammar operator alongside `>` and `<`, and predictive
indent is Insert-mode behaviour. Making it a mode would be the abstraction-for-
its-own-sake failure heuristic #1 names. Per-language *policy* is contributed
by the existing major modes through `Mode::options()`; per-language *knowledge*
is the query files.

## 10. Error handling

Every path degrades; none panics.

| Failure | Behaviour |
|---|---|
| No `indents.scm` for the language | Fall back to lexical bridge. `debug!` once per language, not per keystroke |
| `indents.scm` fails to compile | Same, plus one `warn!` naming the query and the tree-sitter error |
| Snapshot stale, over budget | Lexical bridge, `debug!` |
| Formatter binary absent | `:format` errors naming what was tried; format-on-save skips after one `info!` |
| Formatter non-zero exit | Surface stderr in a notification. **Do not apply edits** |
| Formatter timeout | Kill the process, `warn!`, do not apply, and (on save) write unformatted |
| LSP on-type edits outside the current line | Dropped, `debug!` |

Per the diagnostic-logging rule: everything on a per-keystroke path is `debug!`.
`info!` is reserved for the one-shot user-actionable cases above.

## 11. Rejected alternatives

**Vendor Helix's `indents.scm` (MPL-2.0).** Instant coverage, battle-tested.
Rejected on licensing friction inside an MIT tree plus coupling to Helix's
exact engine semantics; the existing in-tree precedent is to follow the capture
convention without vendoring.

**Vendor nvim-treesitter's (Apache-2.0).** Friendlier licence, but its dialect's
semantics live in the Lua runtime, so adopting it means reverse-engineering
behaviour from Lua rather than implementing a specified dialect.

**Ship the engine with no bundled queries.** Zero licensing exposure, maximally
extensible — and the editor auto-indents nothing out of the box, which is a bad
first frame.

**`=` cascades to the best available formatter.** Covered in §7: it breaks the
operator+motion composition that makes the vim grammar work.

**LSP `onTypeFormatting` as the primary electric source.** Covered in §6: uneven
server coverage for the character that matters, plus a protocol that permits
document-wide edits on a typing keystroke.

**Always reparse synchronously for predictive indent.** Covered in §5: simpler
and always correct, with a frame-miss that is invisible until a large file is
opened.

**Read `.editorconfig`; sniff indentation from file content.** Both deliberately
out of scope for v1 — see §13.

## 12. Paramount-goal alignment

> **UX (higher court):** every keystroke-path source is synchronous; every
> asynchronous source (LSP on-type, formatters) either lands on the edited line
> only or is a user-initiated whole-buffer operation. Formatter output applies
> as a minimal diff so no unedited line repaints. No path can drop or delay the
> typed character.
>
> **#1 Performance:** the only work added to the keystroke path is a tree walk
> (bounded by ancestor depth) and, at most, a *budgeted* incremental reparse
> with a measured ceiling and a bench that guards it. No I/O, no process spawn,
> no LSP round-trip.
>
> **#2 Extensibility:** per-language knowledge is data, not code. The plugin
> tree-sitter seam already publishes `compile-query` / `run-query`, so a plugin
> can supply or override indentation for a language without touching the host.
>
> **#3 Vim modal editing:** `=` is added as a real grammar operator composing
> with every motion and text object; it keeps vim's indent-only meaning;
> `equalprg` / `formatprg` keep their vim roles; `indentmethod=keep` is vim's
> `autoindent`.
>
> **#4 Asynchronicity:** formatters run on `spawn_blocking`, never on the actor
> or UI thread, and their results reach the screen through the inbound
> primitive rather than a bare tick callback.

## 13. Deferred, named

- **`@align`** — §4.1. Aligned-to-paren argument lists indent one level instead.
- **`.editorconfig`** — the cross-editor standard for indent width, and
  `design.md`'s deferred list already tracks it as item 31. It is the natural
  second source for §3's options and should slot in as a layer without
  redesign.
- **Indent detection from file content** — overriding config when a foreign
  file disagrees. Good UX, but a heuristic we would own, and it interacts with
  `.editorconfig` — both or neither.
- **`softtabstop`** — `<Tab>` / `<BS>` operating on virtual indent stops.
- **`gq` / `formatexpr`** — text reflow is a separate verb and a separate
  feature.
- **Indent guides** — a rendering concern, not this fragment.
