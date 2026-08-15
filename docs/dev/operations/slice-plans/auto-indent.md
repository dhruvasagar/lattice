# `auto-indent` — slice plan (IN.0–IN.11)

> Sequencing for [`docs/dev/architecture/auto-indent.md`](../../architecture/auto-indent.md).
> That fragment owns the *what* and *why*; this file owns the *when* and *in
> what order*. Opened 2026-08-15.

## Status

| Slice | Title | Status |
|---|---|---|
| IN.0 | Indent-unit options; retire the hardcoded `INDENT_UNIT` | 📝 |
| IN.1 | `lattice-indent` crate — `IndentUnit` + lexical bridge; `indentmethod=none\|keep` | 📝 |
| IN.2 | Query engine + `rust/indents.scm` + the bounded-reparse staleness path | 📝 |
| IN.3 | `indents.scm` — brace family (9 languages) | 📝 |
| IN.4 | `indents.scm` — indent-sensitive + scripting (4 languages) | 📝 |
| IN.5 | `indents.scm` — data + markup (5 languages) | 📝 |
| IN.6 | Electric reindent | 📝 |
| IN.7 | `=` — the reindent operator | 📝 |
| IN.8 | `lattice-format` + `:format` cascade + minimal-edit application | 📝 |
| IN.9 | Format-on-save; `formatprg` / `equalprg` | 📝 |
| IN.10 | LSP `onTypeFormatting` — the additive layer | 📝 |
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

**IN.2 carries the staleness decision and its bench**, and sets the reparse
budget from measurement. This is the slice with genuine unknowns. It ships one
language (rust) precisely so the engine's semantics settle against real code
before eighteen more query files are written against them.

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

## IN.0 — indent-unit options; retire `INDENT_UNIT` 📝

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

**`lattice-indent` is created in this slice**, containing only `unit.rs`
(`IndentUnit { style: Spaces(n) | Tabs, tabstop }`, `render`, `measure`).
Creating it here rather than putting `IndentUnit` in `lattice-core` and moving
it at IN.1 avoids a move commit for no gain.

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

## IN.1 — `lattice-indent`; lexical bridge; `indentmethod=keep` 📝

**Depends on:** IN.0. **New crate:** `lattice-indent` (or fills out the shell
IN.0 created).

Contents: `unit.rs` (from IN.0), `lexical.rs`.

`lexical.rs` computes the previous non-blank line's indent, plus one level if
that line ends in an unclosed opener, minus one if the target line begins with
a closer. Language-agnostic core plus a tiny per-language opener/closer table.
**This is the same function that becomes `syntax`'s fallback in IN.2** — it is
not scaffolding.

Wire the three predictive call sites through one helper:

```rust
Editor::indent_for_new_line(after_line: u32) -> String
```

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

## IN.2 — query engine; `rust/indents.scm`; the staleness path 📝

**Depends on:** IN.1. The largest slice, and the one with real unknowns.

`query.rs` — compile and cache `indents.scm` per `Lang`, following
`registry.rs`'s existing `include_str!` pattern. `engine.rs` — the ancestor-walk
evaluator for `@indent`, `@indent.always`, `@outdent`, `@outdent.always`,
`@extend`, `@extend.prevent-once`, and `#set! "scope"`. No `@align` (design §4.1).

`queries/rust/indents.scm` — one language, so the dialect's semantics settle
against real code before eighteen more files are written against them.

**The staleness path** (design §5), in `lattice-host`:

```
snapshot fresh                → query
stale, under the byte budget  → sync incremental reparse on the actor thread
stale, over budget            → lexical bridge, debug! once
```

**Benches (`benchmarks.md`):**

1. `indent_for_line` p50/p99 on a 2300-line Rust file — must be far under one
   frame
2. the bounded sync reparse, swept over file size — **this bench sets the
   budget constant**; the number in the code cites this bench
3. `<CR>` keystroke→glyph with `indentmethod=syntax` vs `none` — the regression
   guard

**Tests:** golden `(source, cursor, expected column)` fixtures for Rust —
nested blocks, match arms, closures, method chains, `where` clauses, string
literals, comments; the stale-snapshot case with the reparse landing; the same
case with the budget pinned to `0` so the lexical fallback is deterministic
rather than incidental; a missing query falls back without a panic; a
deliberately malformed `indents.scm` warns once and falls back.

---

## IN.3 — `indents.scm`, brace family 📝

**Depends on:** IN.2.

`c`, `cpp`, `java`, `javascript`, `typescript`, `tsx`, `go`, `css`, `json`.
One query shape reused with node-name swaps. Golden fixtures per language,
same harness as IN.2's.

Watch: `switch`/`case` (a language-by-language convention call — state the
choice per language in the query's header comment); Go's tab convention (the
mode default is IN.11, not here); TSX's JSX children; `json`'s trivial but
worth-having-for-free case.

---

## IN.4 — `indents.scm`, indent-sensitive + scripting 📝

**Depends on:** IN.2. Independent of IN.3.

`python`, `ruby`, `lua`, `bash`. Each needs real per-language study: Python's
colon-block plus continuation lines and bracket continuations; Ruby's
`do`/`end`, `if`/`end` and modifier forms; Lua's `then`/`do`/`end`; Bash's
`case`/`esac`, heredocs, `if`/`fi`.

Heredocs and multi-line strings are the shared hazard: indentation inside them
must be left alone entirely, not merely computed differently.

---

## IN.5 — `indents.scm`, data + markup 📝

**Depends on:** IN.2. Independent of IN.3/IN.4.

`yaml`, `toml`, `html`, `sql`, `markdown`.

This is where tree-sitter indentation is weakest and where the honest answer for
some languages may be **"the lexical bridge is better here"** — YAML block
scalars (`|`, `>`) carry indentation as *content*, and getting that wrong
corrupts data rather than merely looking untidy. HTML has void elements and
inline-vs-block distinctions the grammar does not draw for us. Markdown's
indentation is list-structure, and lists nest by content width, not by a fixed
unit.

**A language may legitimately ship no query out of this slice**, with the
reason recorded in the design fragment's deferred list. That is a finding, not
a failure — and it is the reason this group is sequenced last.

---

## IN.6 — electric reindent 📝

**Depends on:** IN.2.

After an Insert-mode character lands, if it is in the language's electric set,
recompute the line's indent and rewrite **only its leading whitespace**, only
when it differs, inside the same undo group as the typed character. Gated on
`electricindent`, registered in IN.0 and honoured from here.

Electric set = the language's `@outdent` captures + a small keyword table
(`end`, `else`, `elif`, `when`, `esac`).

**Tests:** typing `}` in an over-indented Rust line snaps it back; **only the
current line's bytes change** (assert the rest of the rope is byte-identical —
this is the UX contract, tested directly); `electricindent=false` disables it;
typing `}` inside a string or comment does nothing; the typed character plus
the reindent undo as one unit.

---

## IN.7 — `=`, the reindent operator 📝

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

**Tests:** `=ap` over a scrambled function restores expected indentation; `=`
changes no non-whitespace byte (assert directly); one `u` undoes the whole
range; `=` in a language with no query falls back to lexical without error;
`=` in Visual mode over a partial selection reindents whole lines only.

---

## IN.8 — `lattice-format`; `:format`; minimal-edit application 📝

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

## IN.9 — format-on-save; `formatprg` / `equalprg` 📝

**Depends on:** IN.8.

`formatonsave` sequences format → apply → write in `:w`. `formatprg` and
`equalprg` as buffer-local string overrides.

**The rule with its own tests: a failing formatter must never lose a save.**
Formatter fails, exits non-zero, or times out ⇒ log and write unformatted.
Assert the file on disk is correct in every one of those cases.

**Tests:** save with a fake formatter writes formatted content; save with a
failing formatter writes **unformatted** content and the file exists; save with
a hanging formatter writes after the timeout; `formatprg` overrides the default
table; `formatonsave=false` (the default) writes with no subprocess spawned at
all.

---

## IN.10 — LSP `onTypeFormatting`, additive 📝

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
