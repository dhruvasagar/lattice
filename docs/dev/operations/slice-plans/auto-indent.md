# `auto-indent` — slice plan (IN.0–IN.11)

> Sequencing for [`docs/dev/architecture/auto-indent.md`](../../architecture/auto-indent.md).
> That fragment owns the *what* and *why*; this file owns the *when* and *in
> what order*. Opened 2026-08-15.

## Status

| Slice | Title | Status |
|---|---|---|
| IN.0 | Indent-unit options; retire the hardcoded `INDENT_UNIT` | ✅ |
| IN.1 | `lattice-syntax::indent` lexical bridge; `indentmethod=none\|keep` | ✅ |
| IN.2 | Query engine + `rust/indents.scm` + the staleness path + benches | ✅ |
| IN.3 | `indents.scm` — brace family (9 languages) | ✅ |
| IN.4 | `indents.scm` — indent-sensitive + scripting (4 languages) | ✅ |
| IN.5 | `indents.scm` — data + markup (3 of 5; 2 declined) | ✅ |
| IN.6 | Electric reindent | ✅ |
| IN.7 | `=` — the reindent operator | ✅ |
| IN.8a | `lattice-format` crate — spec table, runner, minimal-edit application | ✅ |
| IN.8b | `:format` cascade + async landing | ✅ |
| IN.9 | Format-on-save; `formatprg` (`equalprg` ⛔ deferred) | ✅ |
| IN.10 | LSP `onTypeFormatting` — the additive layer | ⛔ dropped |
| IN.11 | Per-language mode defaults; GPUI parity audit; docs + benchmarks | 📝 |

## Shape of the sequence

**IN.0 ships value with zero new machinery.** `>`, `<`, `<C-t>`, `<C-d>` and
blockwise `>` / `<` currently indent by a hardcoded four spaces. Registering
`shiftwidth` / `expandtab` and routing those five sites through them is a
user-visible fix that needs no crate, no tree-sitter, and no new concepts. It
lands first so that everything after it has a real indent unit to render into,
and so the option surface is settled before three consumers depend on it.

**IN.1 and IN.2 are ordered by risk, not by capability.** IN.1 wires the three
predictive-indent call sites (`<CR>`, `o`/`O`, `cc`/`S`) to a lexical bridge and
ships `indentmethod = keep` — vim's `autoindent`, useful on its own. That means
the *plumbing* (where the hook goes, how the newline and its indent form one
edit and one undo unit, how auto-inserted indent is cleaned up) is proven green
before the tree-sitter engine is introduced in IN.2. If IN.2's engine has a
bug, it is isolated to the engine, because the call-site wiring already landed
and already has tests.

**IN.2 carries the staleness decision and its bench.** This is the slice with
genuine unknowns, and it ships one language (rust) precisely so the engine's
semantics settle against real code before eighteen more query files are
written against them. In the event the bench did more than validate: it caught
two perf bugs and **deleted** the reparse branch the plan had specified,
budget constant and all. See the findings under IN.2.

**IN.3–IN.5 are the query files, grouped by shared shape**, not alphabetically.
The brace family (IN.3) is one query shape reused with node-name swaps, so it
goes together and goes first. The indent-sensitive and scripting languages
(IN.4) each need real per-language study. The data and markup group (IN.5) is
last because it is where tree-sitter indentation is genuinely weakest — YAML
block scalars and HTML void elements are the long tail, and the honest outcome
for some of them may be "the lexical bridge is the better answer here", which
is a finding worth having late rather than a blocker held early.

Each of IN.3–IN.5 is independently landable and independently revertable. A
language whose query turns out wrong degrades to the lexical bridge; it does
not break the others.

**IN.6 and IN.7 both consume the engine and are independent of each other.**
Either order works. IN.6 (electric) is on the keystroke path and is the fiddlier
of the two; IN.7 (`=`) is user-initiated and is the one that lets a user repair
whatever the other surfaces get wrong, which is an argument for landing it
early enough to be useful during IN.3–IN.5's query work. Listed in this order
for that reason.

**IN.8–IN.10 are the external-tool half** and touch none of the indent engine.
IN.8 is the substrate (spawn, timeout, minimal-edit application) plus `:format`
as its first consumer. IN.9 adds the save hook, which is the risky part — the
rule that a failing formatter must never lose a save gets its own tests. IN.10
is optional polish and could be dropped without affecting anything before it.

**IN.11 is the convergence slice**: per-language option contributions through
`Mode::options()`, the GPUI parity audit for any `Effect::` variants IN.8/IN.9
introduced, and the docs/benchmark wrap-up.

---

## IN.0 — indent-unit options; retire `INDENT_UNIT` ✅

**Depends on:** nothing. **New crates:** none.

**This slice owns the entire option surface — all seven of design §3.** Not
three now and four later: an option registered across four slices means four
chances for the names, defaults and validators to drift, and `:describe-option`
metadata written four times. The *honoured set* grows slice by slice; the
*declared set* lands once, here.

Register in `lattice-config/src/core_options.rs`:

| Option | Default | Honoured from |
|---|---|---|
| `shiftwidth: i64`, validated `1..=32` (mirror `tabstop`'s validator) | `4` | IN.0 |
| `expandtab: bool` | `true` | IN.0 |
| `indentmethod` enum `none \| keep \| syntax` | `syntax` | `none` IN.0, `keep` IN.1, `syntax` IN.2 |
| `electricindent: bool` | `true` | IN.6 |
| `equalprg: String` | `""` | IN.9 |
| `formatprg: String` | `""` | IN.9 |
| `formatonsave: bool` | `false` | IN.9 |

An option whose honoured-from slice has not landed is inert, and
`indentmethod=syntax` degrades to `none` until IN.1/IN.2 light up the lower
rungs — which is the cascade behaving exactly as designed, not a stub.

**`IndentUnit` lands in `lattice-core`, not in `lattice-indent`.**

The plan originally said the opposite — create `lattice-indent` here holding
only `unit.rs`, "to avoid a move commit". That was a **planning error caught at
execution**: `lattice-syntax` depends on `lattice-grammar`, so a value type in
`lattice-indent` (which depends on `lattice-syntax` from IN.2) that `>` / `<`
in `lattice-grammar` consume is a dependency **cycle**, not a move commit.

`lattice-core` is the floor both sides already stand on, and where the sibling
`FoldMethod` lives for exactly the same reason — `lattice-config` needs it and
cannot depend upward. The *engine* went to `lattice-syntax::indent` at IN.1
(see the note there); `lattice-core::indent` holds the *value*.

Shape as built: `IndentUnit { width, expand_tabs, tabstop }` with
`columns_of` / `render` / `shift` / `reindented_prefix`, plus the
`IndentMethod` `labeled_enum!`.

Route through it, deleting `const INDENT_UNIT: &str = "    "`:

- `dispatch.rs:19411` — `<C-t>` / `<C-d>`
- the `>` / `<` operator sites
- blockwise-visual `>` / `<` per-row indent

**Tests:** `:set shiftwidth=2` then `>>` indents by 2; `:set noexpandtab` then
`>>` inserts a tab; `<<` dedents a mixed tab/space prefix correctly;
`:setlocal shiftwidth=8` affects one buffer only; `2>>` with a count still
collapses to one undo unit (regression guard on existing behaviour).

**Docs:** `site/content/cheatsheet.md:210-211` already claims these defaults —
this slice makes that true rather than adding a claim.

---

## IN.1 — the lexical bridge; `indentmethod=keep` ✅

**Depends on:** IN.0. **New crates: none.**

The engine goes in **`lattice-syntax::indent`**, beside `text_objects.rs` and
`motions.rs` — the same shape (computation over the parse tree, driven by
`.scm` files that crate already owns and embeds).

> **Corrected during execution.** The plan originally specified a dedicated
> `lattice-indent` crate. It was built, then collapsed: the stated
> justification was "`syntax.rs` is already 119 KB", which describes one file
> rather than the crate, and `lattice-syntax` already carries eleven modules. A
> dedicated crate needs a stronger reason than module count. Not
> `lattice-grammar` either — it "stays tree-sitter-agnostic" by its own
> manifest, and IN.2's engine needs the tree.
>
> `lattice-format` (IN.8) still earns its own crate: it spawns processes and
> owns a timeout/stderr policy, which is a genuinely different concern from a
> pure tree walk.

`indent.rs` computes the previous non-blank line's indent, plus one level if
that line ends in an unclosed opener, minus one if the target line begins with
a closer. Language-agnostic core plus a tiny per-language opener/closer table.
**This is the same function that becomes `syntax`'s fallback in IN.2** — it is
not scaffolding.

`keep` and the `syntax` fallback are **two policies over one mechanism**, not
the same behaviour: `keep` is a pure copy (vim's `autoindent`) and does NOT
scan, because the bracket rule misfires in a language where `{` is not a block
opener. The design fragment §5 originally conflated them; corrected there.

Wire the predictive call sites through **two** helpers, not one — the create-a-
line case and the split-a-line case do not share a "previous line":

```rust
Editor::auto_indent_for_new_line(after_line: u32, moved_tail: Option<&str>)  // o / O
Editor::auto_indent_for_split(head: &str, tail: &str)                        // <CR>
```

`<CR>` derives its indent from the **head** (the text before the cursor), not
the whole line. In `foo(a, |b)` the whole line is bracket-balanced while the
head leaves `(` open, so passing the whole line silently fails to indent the
continuation. Found by a test during IN.1; both helpers share one private body.

- `do_open_line_below` (`dispatch.rs:19283`)
- `do_open_line_above` (`dispatch.rs:19305`)
- the Insert-mode `Action::Insert("\n")` path
  (`AppEffect::InsertNewline`, `dispatch.rs:8481`)
- `cc` / `S`

**The newline and its indent must be a single `Edit`**, not two. Two edits
means two entries in the render path and a real risk of the undo grouping
splitting — `do_open_line_below` already opens its undo group before the
newline specifically to avoid that class of bug, and this slice must not
regress it.

**Auto-indent cleanup.** Vim strips auto-inserted indent when the line is left
without typing anything, so `o<Esc>` does not leave trailing whitespace. Track
"this line's indent was auto-inserted and nothing followed it"; strip on
leaving Insert and on a subsequent `<CR>`.

**Tests:** `o` inside an indented block lands at the previous line's column;
`<CR>` mid-line carries indent; `o<Esc>` leaves no trailing whitespace;
`o<Esc>u` restores exactly the original buffer (one undo unit);
`indentmethod=none` restores column-0 behaviour verbatim.

---

## IN.2 — query engine; `rust/indents.scm`; the staleness path ✅

**Depends on:** IN.1. The largest slice, and the one with real unknowns.

The ancestor-walk evaluator for `@indent`, `@indent.always`, `@outdent`,
`@outdent.always` lands in `lattice-syntax::indent` beside the IN.1 bridge.
`@extend` / `@extend.prevent-once` / `@align` are parsed-and-ignored, so a
query written against the fuller vocabulary loads and simply contributes less
(design §4.1).

`queries/rust/indents.scm` — one language, so the dialect's semantics settle
against real code before eighteen more files are written against them.

The query **source** is resolved by language name from
`indent::indents_source` rather than threaded through `build_config`'s
parameter list: that function already carries eight positional arguments and a
`too_many_arguments` lint to match, so a ninth `Option<&str>` repeated across
twenty call sites is the wrong direction. It also keeps the `include_str!`
beside the engine that consumes it.

### Two rules, not one

Predictive and reindent ask different questions and need different bounds:

- **new line at byte `at`** — count `@indent` ancestors with
  `start_byte < at < end_byte`. Both bounds strict; the upper one matters,
  since `at <= end_byte` makes a cursor just past `fn f() {}` indent the next
  line for no reason.
- **existing line `row`** — count `@indent` ancestors with
  `start_row < row <= end_row`, minus `@outdent` nodes starting on `row`. Row-
  based because a block *starting* on the line must not indent it and a closer
  *starting* on it must dedent it.

### 🔍 Finding: the bench deleted the middle branch

The plan specified three branches — fresh / stale-under-budget-reparse /
stale-over-budget-lexical — and a budget constant "set from the bench". Built,
benched, and cut to two:

```
snapshot fresh  → query the tree
otherwise       → lexical bridge, debug! once
```

`indent_reparse` measured **1.9 ms at 16 KB, 7.6 ms at 64 KB, 15.4 ms at
129 KB** on Apple silicon, where `benchmarks.md` warns user hardware runs 2–5×
slower. Any budget generous enough to be useful misses frames. And the branch
would buy nothing anyway: the snapshot is stale exactly *just after an edit*,
when the code is half-typed, and a fresh parse of half-typed code yields an
`ERROR` node and declines — so it would spend milliseconds to return `None`.

Staleness is read from **`reparsed_from_version`**, not `text_version`: the
latter advances on `try_apply_intermediate`'s range-shift with no reparse, so
it reports fresh while the structure is stale.

`choose_indent_source(fresh) -> IndentSource` stays a pure function, and a test
asserts there is no parsing branch — re-adding one is a plausible-looking fix
for a future stale-indent report.

### 🔍 Finding: the query must be scoped, or it is O(file) per keystroke

`indent_query` first benched at 64 µs / 623 µs / 2.57 ms for 80 / 800 / 3200
lines — linear, on the keystroke path, ~30 ms for a 36k-line file. Cause: the
capture map ran over the whole file while only a handful of ancestor nodes are
ever consulted. Scoping to the root child containing the position (found by
binary search, since children are byte-ordered) gives **8.8 / 13.4 / 29.7 µs**
— 87× at 3200 lines — and is sound rather than a trade, because every consulted
node is an ancestor and every ancestor but the root lies inside that child.

### 🔍 Finding: incomplete code defeats the tree entirely

Probed rather than assumed. Parsing `fn f() {\n    x();\n\n` yields exactly one
`ERROR [0..17]` node and **no `block` node at all** — so `@indent` matches
nothing and the engine confidently answers **zero** for a cursor plainly inside
a function body. Answering zero is worse than declining: while typing,
incomplete code is the normal state, not the exceptional one.

Hence `has_error_at_or_before(root, at)`: a parse error at or before the cursor
means the tree has no answer, and the lexical bridge — which handles unclosed
openers correctly by construction — takes over. Bounded to errors *before* the
cursor so a syntax error at the bottom of a file does not disable indentation
at the top.

**This reframes what the tree path is for.** It does not win while typing new
code; it wins on *complete* code — `=` and electric reindent (IN.6/IN.7),
`<CR>` inside already-written code, and string/comment awareness. The
`the_tree_beats_the_lexical_bridge_on_a_brace_in_a_string` test exists so that
value cannot silently disappear.

**Benches (`benchmarks.md` §IN.2):** `indent_query` swept over file size (the
flatness guard — it caught both perf bugs above), `indent_reparse` swept over
size (retained as the negative result that deleted the reparse branch), and
`indent_method` comparing `syntax` / `keep` / `none` on the same call.

The planned third bench — end-to-end `<CR>` keystroke→glyph — was **built and
discarded**: constructing an `Editor` in the setup closure dominated, and it
reported `none` as 3.5× *slower* than `syntax` with 70% spread. A benchmark
whose ranking is backwards is worse than none, because it gets quoted.
`indent_method` measures the one call that differs and is stable.

**Tests:** golden `(source, cursor, expected column)` fixtures for Rust —
nested blocks, match arms, closures, method chains, `where` clauses, string
literals, comments; the stale-snapshot case with the reparse landing; the same
case with the budget pinned to `0` so the lexical fallback is deterministic
rather than incidental; a missing query falls back without a panic; a
deliberately malformed `indents.scm` warns once and falls back.

---

## IN.3 — `indents.scm`, brace family ✅

**Depends on:** IN.2.

`c`, `cpp`, `java`, `javascript`, `typescript`, `tsx`, `go`, `css`, `json`.
One query shape reused with node-name swaps. All nine compiled against their
real grammars on the first attempt — no invalid node kinds — which the
`every_shipped_indents_query_compiles` test now guards. That guard is not
ceremony: an unknown kind does not degrade to "no indent for Go", it fails
`Query::new` and takes the whole registry build for that language down, so
Go buffers would stop parsing entirely.

Three shared tests rather than nine bespoke ones, each sweeping the family:
bodies indent and closers dedent; wrapped argument lists indent their
continuations; a brace inside a string is not an opener (the property that
distinguishes the tree path from the lexical bridge, now checked beyond Rust).

Decisions recorded in the query headers rather than here, since that is where
the next reader will be: `case`/`default` labels are left uncaptured across the
C family, Java and Go (aligning labels with the braces, which is what gofmt
does); `>` is not an `@outdent` in C++ because it is also the greater-than
operator; `template_string` and `jsx_self_closing_element` are deliberately
absent.

**Go's tab convention is not expressed here.** This file says *where* the
levels are; `expandtab` / `shiftwidth` say what one level is made of, and the
per-language contribution lands in IN.11. Keeping those apart is the whole
reason `tabstop` and `shiftwidth` are separate options (design §3).

One test expectation was wrong on the first run — the Java fixture nests a
method inside a class, so its body is at level 2 and its inner closer at 1, not
1 and 0. Fixed by making every expected level explicit in the case table
instead of assuming 1/0; an assertion that hardcoded the common shape would
have been wrong for the right reason and taught nothing.

---

## IN.4 — `indents.scm`, indent-sensitive + scripting ✅

**Depends on:** IN.2. Independent of IN.3.

`python`, `ruby`, `lua`, `bash`.

### 🔍 Finding: capture the CONSTRUCT, not the block

Three of the four needed their queries rewritten after the first run, for one
shared structural reason that the brace family does not have. In Lua, Python
and Ruby the block node **excludes the opener**:

```
if_statement [rows 0..2]
  then       [row 0]
  block      [rows 1..1]   ← starts on the BODY row
  end        [row 2]
```

The engine indents a row when an `@indent` ancestor satisfies
`start_row < row <= end_row`. Capturing `block` therefore indents **nothing** —
its start row *is* the body row. Capturing the enclosing construct
(`if_statement`, rows 0..2) is what makes the body indent and lets the `end`
dedent it back. The brace family is unaffected because `{` sits inside its
block node, so the block starts on the opener's row.

This is a real difference between grammars, not a style choice, and it is the
kind of thing only a per-language test catches — the queries compiled fine.

**Ruby's modifier forms need no special handling**, which was a pleasant
surprise: `return if x` parses to a one-row `if` node, and a one-row construct
can never satisfy `start_row < row <= end_row`. The row rule excludes it
structurally rather than by a rule written for the case, so `if` / `while` can
be captured directly.

### 🔍 The engine now refuses to answer inside a string

Not a query change but a correctness fix the group forced. A Python docstring,
a Bash heredoc, a Rust raw string and a JS template literal all carry leading
whitespace as **data**; applying the enclosing block's structural indent there
edits the string's value, and for `<<-EOF` can change whether the terminator is
recognised. `tree_levels_for_new_line` checks `cursor_in_string_scope` (which
already existed in this crate) and declines, handing off to the lexical bridge,
which copies the previous line — also what vim does inside a string.

Bash needed no rewrite: it has no block node at all (commands are direct
children of `if_statement`), so capturing the statement was right the first
time.

---

## IN.5 — `indents.scm`, data + markup ✅

**Depends on:** IN.2. Independent of IN.3/IN.4.

Shipped: `yaml`, `toml`, `html`. **Declined: `sql`, `markdown`** — the plan
allowed for this and it happened.

- **markdown** nests by CONTENT WIDTH, not by a fixed unit: a nested list item
  aligns under its parent's *text*, which depends on the marker (`-` vs `10.`).
  A `shiftwidth` step is the wrong model and would fight the user. Markdown's
  `BracketSyntax::NONE` already stops a stray brace from indenting prose, so
  the lexical copy is both simpler and closer to right.
- **sql** is parsed by `tree-sitter-sequel`, a deliberately permissive
  multi-dialect grammar, and SQL indentation convention varies more between
  houses than between dialects (leading vs trailing commas, `AND` alignment,
  river style). There is no default worth imposing; `=` plus an `equalprg`
  formatter is the honest answer.

Both are asserted in `markdown_and_sql_deliberately_ship_no_query` so the
absence reads as a decision, and adding one later becomes an act that updates
the test.

### 🔍 Finding: IN.4's heredoc protection did not exist

IN.4's query header and commit message both claimed the engine refused to
answer inside a Bash heredoc. It did not. `cursor_in_string_scope` matches an
**explicit node-kind list**, and that list covered neither `heredoc_body` nor
YAML's `block_scalar` — so the protection was asserted, not implemented. Found
while probing YAML for this slice; fixed here, with a test naming both so a
future edit to that list cannot quietly drop one.

`string_scalar` is deliberately NOT added: YAML wraps every *plain* scalar in
one, so including it would put the engine "inside a string" for essentially all
YAML and disable indentation wholesale. The test pins that too.

Second consumer worth noting: `gen:path` insert-completion reads the same
helper, so path completion now triggers inside heredocs and block scalars.
That is a reasonable place to want it, not a regression.

### Two hazards that turned out not to be hazards

- **HTML void elements** (`<br>`, `<img>`) need no special handling. They parse
  to one-row nodes, and the row rule (`start_row < row <= end_row`) can never
  fire for a one-row node — the same structural exclusion that handles Ruby's
  modifier `if`.
- **TOML tables** were the real judgement call instead: `[table]` spans its
  whole section, so capturing it would indent every key under a flush-left
  header. Not captured; only `array` and `inline_table` are.

---

## IN.6 — electric reindent ✅

**Depends on:** IN.2.

After an Insert-mode character lands, if it is in the language's electric set,
recompute the line's indent and rewrite **only its leading whitespace**, only
when it differs, inside the same undo group as the typed character. Gated on
`electricindent`, registered in IN.0 and honoured from here.

Electric set = bracket closers from `BracketSyntax` + a per-language dedent
keyword table (`end`, `until`, `fi`, `done`, `esac`, `else`, `elif`, `elsif`,
`when`, `rescue`, `ensure`, `except`, `finally`). Read from the same knowledge
the queries encode rather than extracted from the compiled query's literals —
the extraction would be fiddly and would not survive a query that captures a
node rather than a token.

Two triggers, both narrower than "the character is a closer":

- a bracket closer typed when **nothing but whitespace precedes it** on the
  line. Without that guard, the `}` in `let x = Foo { a };` re-indents a line
  the user is in the middle of writing.
- a keystroke that **completes a dedent keyword forming the whole line so
  far** — so `end` fires and `bend`, `append`, `x = end` do not.

### 🔍 Finding: electric reindent must be lexical, and that is forced

The plan assumed the tree would answer this. It cannot, and the reason is
IN.2's finding rather than a new one. At the instant the closer lands, **two**
things are true at once: the published snapshot has not caught up with the
edit, *and* the surrounding code is half-written. So even a fresh synchronous
parse would yield an `ERROR` node with no block structure and the engine would
decline. Asking the tree would cost a parse to learn nothing.

No second algorithm was needed: `electric_columns` reuses IN.1's
`indent_columns_for_new_line` with the **current line passed as `next`** —
"what indent would a new line here get, given what this line starts with" is
exactly the question — plus one extra step down for word closers, which the
bracket scan cannot see.

**Tests:** typing `}` in an over-indented Rust line snaps it back; **only the
current line's bytes change** (assert the rest of the rope is byte-identical —
this is the UX contract, tested directly); `electricindent=false` disables it;
typing `}` inside a string or comment does nothing; the typed character plus
the reindent undo as one unit.

---

## IN.7 — `=`, the reindent operator ✅

**Depends on:** IN.2.

New `reindent: OperatorId` in `lattice-grammar/src/builtins.rs` beside
`indent_left` / `indent_right`, with `=` bound in Normal (operator-pending) and
Visual. `==` for the current line, `=ap` / `=i{` / any motion, counts (`3==`).

Leading whitespace only (design §7). One undo unit for the whole range. Takes
an unbounded synchronous catch-up parse — the §5 budget is a keystroke-path
rule and does not apply to a user-initiated operator.

**`equalprg` is honoured in IN.9, not here.** This slice is tree-sitter-backed
only. Piping a range through an external indent filter needs `runner.rs`'s
spawn/timeout/stderr machinery and `apply.rs`'s minimal-edit application, both
of which land in IN.8 — building a second, smaller subprocess path here and
retiring it two slices later is exactly the churn the sequencing exists to
avoid.

### How the level reaches a tree-sitter-agnostic crate

`=` needs a per-line indent depth, which only the tree can supply, and
`lattice-grammar` must not know about tree-sitter (its own manifest says so).
Solved with the **`ScopeResolver` pattern already in the crate** rather than a
new mechanism: a `trait IndentResolver { fn levels_for_line(&self, u32) ->
Option<i32> }` declared in `lattice-grammar`, implemented host-side over an
`Arc<SyntaxSnapshot>`, injected through `GrammarEnv` / `DispatchEnv`. Nothing
tree-shaped crosses the boundary; the grammar asks a question and the host
answers it.

**The resolver is supplied only when the snapshot matches the buffer.**
Reindenting existing lines against a stale tree would move them to where they
belonged one edit ago — worse than leaving them alone, because it looks like it
worked. `=` is user-initiated, so "press it again" is real recovery; a silently
wrong reindent is not.

**Lines with no answer are skipped, not guessed.** A range spanning a syntax
error, a heredoc or an unsupported language reindents what it understands and
leaves the rest byte-identical. Falling back to the lexical bridge here would
be wrong on purpose: that bridge answers "where would a NEW line go", which is
a different question from "where does this EXISTING line belong", and using it
would drift a whole range toward whatever the first line happened to have.
`equals_without_a_structural_source_is_a_no_op` pins this against SQL.

**`=` in a multibuffer is a no-op**, pending a composed→source resolver of the
shape N.1.5 built for text objects. Recorded rather than silently absent.

**Tests:** `=ap` over a scrambled function restores expected indentation; `=`
changes no non-whitespace byte (assert directly); one `u` undoes the whole
range; `=` in a language with no query falls back to lexical without error;
`=` in Visual mode over a partial selection reindents whole lines only.

---

## IN.8 — `lattice-format`; `:format`; minimal-edit application

**Carved in two during execution.** IN.8a is the crate: pure substrate,
17 tests, no consumer. IN.8b wires `:format` and the async landing. The split
is along a real seam — everything in 8a is testable without a runtime, a host
or an editor, and everything in 8b is host plumbing — and it keeps a large
slice from sitting uncommitted while the second half is built.

### IN.8a — the crate ✅

**Depends on:** IN.0 (nothing else). Independent of the whole engine half —
**this slice can run in parallel with IN.3–IN.7.**

New crate `lattice-format`: `spec.rs` (the per-`Lang` default table, PATH-probed),
`runner.rs` (`spawn_blocking` + timeout + stderr capture), `apply.rs`
(formatter output → minimal `Edit` set via `lattice_diff::compute_diff`,
`compute.rs:98`).

`:format` / `:[range]format` in `lattice-host`, cascading LSP → external →
error naming what was tried. `:lsp-format` is untouched.

Results land asynchronously and therefore go through
`SubsystemBoot::inbound::<T>`, **not** a bare `TickCallback`.

**Tests:** all formatter tests use a **fake formatter** — a script written into
a tempdir and put on `PATH` — so CI never depends on rustfmt/prettier being
installed. Cover: successful format applies a minimal edit set and the cursor,
marks and folds survive; formatting an already-formatted buffer produces
**zero** edits (the idempotence guard that catches a whole-buffer replace
sneaking in); non-zero exit surfaces stderr and applies nothing; a hanging
formatter is killed at the timeout and applies nothing; a missing binary
errors naming the command; **the result is visible without dispatching another
action** (the inbound-primitive assertion).

---

## IN.9 — format-on-save; `formatprg` ✅

**Depends on:** IN.8.

`formatonsave` sequences format → apply → write in `:w`. `formatprg` was
already honoured by `:format` (IN.8b) and is reused here unchanged.

**Synchronous, deliberately**, and the second reason is decisive:

1. The save path already blocks on an LSP round-trip
   (`run_will_save_wait_until_blocking`), so this is not a new kind of stall,
   and `:w` is an explicit ex-command rather than the keystroke path.
2. An async format-then-write lets `:wq` quit **before the write lands**. That
   is data loss, and no amount of responsiveness pays for it.

**LSP is not a rung here**, unlike `:format`'s cascade. Servers already get
their chance through `textDocument/willSaveWaitUntil`, which `save_blocking`
fires and waits on — that IS the LSP format-on-save mechanism and predates this
plan. Running `textDocument/formatting` as well would apply two servers'
opinions to one save.

### ⛔ `equalprg` deferred

`=` dispatches inside `lattice-grammar`, which cannot spawn processes, and the
operator's range is resolved *inside* that dispatch — so piping the range
through an external filter needs a range-filter seam that does not exist. The
`IndentResolver` trait added at IN.7 is the wrong shape for it: it answers
"how deep is line N", not "rewrite this text".

Inventing that seam at the tail of a long slice is how bad abstractions get
built, and the payoff is narrow: tree-sitter covers 17 of 19 languages, so
`equalprg` would serve mainly the two that ship no query (sql, markdown) — for
which `:format` with a `formatprg` already works today. Deferred with the seam
named rather than half-built.

**The rule with its own tests: a failing formatter must never lose a save.**
Formatter fails, exits non-zero, or times out ⇒ log and write unformatted.
Assert the file on disk is correct in every one of those cases.

**Tests:** save with a fake formatter writes formatted content; save with a
failing formatter writes **unformatted** content and the file exists; save with
a hanging formatter writes after the timeout; `formatprg` overrides the default
table; `formatonsave=false` (the default) writes with no subprocess spawned at
all.

---

## IN.10 — LSP `onTypeFormatting`, additive ⛔ DROPPED

**Dropped 2026-08-16 (Dhruva).** The plan already marked it optional; the
evidence gathered since turned "optional" into "not worth it".

IN.6 established that electric reindent has to be lexical, because at the
instant a closer lands the snapshot is stale *and* the code is half-written, so
no parse can answer. LSP on-type formatting faces that same wall and adds three
problems of its own: an async round-trip on the typing path; uneven server
coverage for the character that actually matters (`}`); and a protocol that
permits the server to return edits **anywhere in the document**, which the UX
contract vetoes outright and which would need filtering to the current line
regardless.

So it would be a second, slower, less reliable path to an answer the lexical
bridge already gives synchronously. The one thing it could add — a server's
opinion on trigger characters our electric set does not cover, such as
rust-analyzer's `.` for method chains — is not worth the machinery.

Not deferred, dropped. If it comes back it should be re-argued from scratch
rather than resumed from this plan.

**Depends on:** IN.6. Optional; droppable without affecting anything prior.

Opt-in via `lsp.on-type-formatting`, default off. Trigger characters are probed
from the server capability (`capabilities.rs`) at runtime, and only those **not
already covered by IN.6's electric set** are subscribed.

**Returned edits are filtered to the current line**; out-of-line edits are
dropped with a `debug!`. This is a hard constraint, not a nicety — the protocol
permits document-wide edits and applying them on a typing keystroke would be a
pixel change to unedited content.

**Tests:** a fake server advertising `.` triggers on `.` and not on `}`; an
edit returned for another line is dropped and the rope is unchanged there;
the option defaults to off and no request is sent.

---

## IN.11 — mode defaults; GPUI parity; docs + benchmarks 📝

**Depends on:** everything.

- Per-language option contributions through `Mode::options() -> OptionOverrideSet`:
  `go` → `expandtab=false`; confirm `python`; any language whose community
  convention differs from 4/spaces.
- **GPUI parity audit.** IN.8/IN.9 may add `Effect::` variants; every one needs
  its arm in `lattice-ui-gpui`'s effect classifier. Shortcut:
  `grep -rn "Effect::Format" crates/lattice-ui-gpui/ --include="*.rs"` —
  an empty result means GPUI was missed.
- `implementation.md` ledger row; `benchmarks.md` final numbers;
  `:describe-option` metadata for all seven options; help-buffer coverage for
  `=` and `:format`.
- Update the design fragment's deferred list with anything IN.5 could not do.

---

## Cross-cutting rules for every slice here

- **One slice, one commit.** The exception clause applies only where a slice
  cannot compile without its neighbour; none is expected here, and IN.0's
  option-surface question is called out explicitly so it does not become one.
- **`scripts/precommit.sh <touched-crate>...` before committing**, not after.
- Diagnostic logging on any per-keystroke path is `debug!`, never `info!`.
- Any slice touching `lattice-ui-tui` updates `lattice-ui-gpui` in the same
  patch.
- Formatter tests never depend on a real formatter being installed.
