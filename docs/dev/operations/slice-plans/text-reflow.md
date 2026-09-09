# `text-reflow` — slice plan (RF.0–RF.7)

> Sequencing for [`docs/dev/architecture/text-reflow.md`](../../architecture/text-reflow.md).
> That fragment owns the *what* and *why*; this file owns the *when* and *in
> what order*. Opened 2026-09-09 out of `auto-indent.md` §13, which deferred
> `gq` by name, plus the two questions that surfaced when it was picked up:
> whether `formatprg`-shaped options belong here, and whether LSP should take
> over the format verbs.

## Status

| Slice | Title | Status |
|---|---|---|
| RF.0 | The option surface — `textwidth`, `autowrap`, the three chains | ✅ |
| RF.1 | The reflow engine — pure, tested, benched | ✅ |
| RF.2 | `operator:reflow` on `gq` **and** `gw` | ✅ |
| RF.3 | Auto-wrap on the insert path | ✅ |
| RF.4 | Lists, hanging indent, fenced blocks; per-major defaults | ✅ |
| RF.5a | `format.reformat` chain replaces `:format`'s cascade | ✅ |
| RF.5b | `=` / `gq` route through `format.indent` / `format.reflow` | ⛔ |
| RF.6 | `operator:reformat` on `g=` | ⛔ |
| RF.7 | Docs, site sync, benches, parity audit | ✅ |

**NOT ARCHIVABLE.** RF.5b and RF.6 are ⛔ deferred, not dropped — both are
still wanted, and §12 below names what unblocks them.

## Shape of the sequence

**RF.0 declares the whole option surface at once**, following IN.0's precedent
and for the same reason: splitting the declaration across five slices is five
chances for names, defaults, validators and `:describe-option` metadata to
drift. The *honoured* set grows slice by slice; the *declared* set lands once.

**RF.1 before RF.2** so the engine is proven green as a pure function before
an operator, a keymap and an undo unit are wired to it. If the fill algorithm
has a bug it stays in the algorithm, because the operator wiring lands against
an engine that already has tests.

**RF.2 before RF.3** for the inverse reason: `gq` is user-initiated and can
take an unbounded synchronous pass, so it is the low-risk consumer. Auto-wrap
runs on the keystroke path and is the one with a latency budget. Proving the
engine through the safe verb first means RF.3's bench measures wiring, not
algorithm.

**RF.5 is deliberately late.** It is a refactor of working code (`:format`'s
cascade, `formatprg`'s three call sites) with no new user-visible capability
of its own — it exists to make `indent` / `reflow` / `reformat` configurable.
Landing it after the native verbs work means the chain has three real
providers to resolve on day one instead of being an abstraction over one.

**RF.6 after RF.5** because `g=` *is* the `reformat` chain with an operator
range; before RF.5 there is no chain for it to drive.

---

## RF.0 — the option surface ✅

`textwidth: i64 = 80` (validated `> 0`), `autowrap: AutoWrap = Comments`
(`off｜comments｜all`, following `IndentMethod`'s `OptionType` +
`parse_label` + `all()` pattern in `lattice-core`), and the three chain
options `format.indent` / `format.reflow` / `format.reformat`.

**The chain needs a new `OptionType`.** Today's impls are scalars
(`BufferDisplayPreference`, `FoldMethod`, `IndentMethod`, `TablineShow`) —
there is no list-valued option. `ProviderChain` parses a comma-separated
`:set format.reformat=lsp,rustfmt`, which is how vim expresses lists in `:set`
and needs no new config machinery beyond the impl.

Defaults per §6.1. Completion candidates come from the registered provider
names so `:set format.reflow=<Tab>` is useful from the first slice.

Nothing honours the chains yet — RF.5 does. `textwidth` is honoured by RF.2,
`autowrap` by RF.3.

**Tests:** parse/validate/round-trip per option; an unknown provider name is a
rejected value that leaves the previous chain in place (`§4.1` of the config
docs); `:describe-option` renders each.

## RF.1 — the reflow engine ✅

`lattice-grammar/src/reflow.rs`. Pure functions, no I/O, no tree.

Placement (heuristic #6): no new crate — no new dependency surface. Reflow is
a text function over `(text, textwidth, leader, indent)`; the operator that
drives it lives in `builtins.rs` and reads `GrammarEnv`, which already carries
`comment_syntax` (N.1.6) and `indent` (IN.0). The host's insert path (RF.3)
calls the same module, and `lattice-host` already depends on
`lattice-grammar`. `lattice-format` was considered and rejected: that crate is
process spawning, timeouts and diff-derived edits — a different mechanism.

Adds `unicode-width` (already a workspace dependency) for display-column
measure.

Covers §4.1 paragraph splitting, §4.2 leader-as-longest-common-prefix, §4.4
greedy fill. Lists and fences are RF.4.

**Tests:** the `///` and `//!` cases by name (the LCP rule exists for them);
leader-only line as a paragraph separator; a word longer than `textwidth`
overflows rather than splitting; CJK and tabs measured in columns not bytes;
an empty range and a single-word range are no-ops.

**Bench:** fill a 200-line paragraph — the number RF.3's keystroke budget is
read against.

## RF.2 — `operator:reflow` on `gq` and `gw` ✅

Registered beside `operator:reindent` with `blockwise_per_row: false`
(reflow is a whole-line operation, like `=`). Both chords bind to the one
operator; `gqq` / `gww` are the current-line forms. Cursor preserved. One
undo unit for the range.

**Corrected while building:** the plan said `gqw` / `gwq` should also mean
the current line, following Zed. They must not — `gq` is linewise in vim,
so `gqw` already formats the whole line, and binding it as a fixed
current-line chord would spend the `gq{motion}` composition to gain a
second spelling of `gqq`. Also: `[g, q]` must stay an INTERNAL trie node.
A depth-2 terminal there resolves as `Bound` before the walk descends and
silently kills `gqq`, `gqap`, `gqi(`.

**Tests:** `gqap`, `gqaC` (the comment text object, N.1.6), visual `gq`,
`3gqq`, and each doubled form; the cursor is where it started; one `u`
reverses the whole range. Plus the `magit-blame-mode` shadow — `gq` in a blame
buffer still quits blame, because a mode layer resolves before `Builtin`.

## RF.3 — auto-wrap on the insert path ✅

Honours `autowrap` + `textwidth`. Lexical comment detection per §9 — a prefix
compare on the current line, never a tree query on a keystroke. Same undo unit
as the keystroke that triggered it.

**Tests the way it fails:** typing past the column wraps; typing past it
inside a string literal with `autowrap=comments` does **not**; `autowrap=off`
never wraps; the carried remainder gets the leader and hanging indent; one `u`
undoes character *and* break together.

**Bench:** the keystroke-path number, against the §8.2 budget. This is the
slice with a latency claim to defend.

## RF.4 — lists, hanging indent, fenced blocks ✅

§4.3 markers (`-`, `*`, `+`, `1.`, `1)`) with continuation indented to the
text column; §4.5 fence detection so reflow is a no-op inside ```` ``` ````
and `#+begin_…`. Per-major `autowrap` defaults land here
(`markdown｜org｜text｜gitcommit` → `all`, code majors → `comments`) through
`Mode::options()`.

**Tests:** a bullet wraps to its text column, not to column 0; a nested bullet
keeps its depth; an ordered list survives; a fenced block inside a `gqip`
range is untouched; org `#+begin_src` likewise.

## RF.5a — `format.reformat` replaces the hardcoded cascade ✅

The refactor. `do_format_request`'s two-rung `if` becomes a chain resolver;
`FormatterSpec` + the per-language table become chain entries.
`format.indent` and `format.reflow` are declared and default to
`[native]`; ROUTING them to a non-native rung is RF.5b below.

Migration per §6.3: **delete `equalprg`** (zero consumers, ⛔ deferred at
IN.9); **retire `formatprg`** into `format.reformat` with a one-release
deprecation alias that logs to `:messages`; `formatonsave` keeps its name and
runs the `reformat` chain.

Amends `auto-indent.md` §7 and §8 in the same commit — both conclusions
survive as defaults, neither survives as a hardcode, and leaving the fragment
asserting a cascade the code no longer has is how docs rot.

**Tests:** each provider kind resolves; a failing rung falls through to the
next and an exhausted chain names every rung it tried; reordering the chain
changes which formatter wins (the extensibility claim, asserted rather than
asserted-about); a `formatprg` in an existing config still works as the
first rung. Deferred to RF.5b: `reflow = ["lsp","native"]`
actually routes `gq` to the server (the flexibility claim, asserted rather
than asserted-about); a `formatprg` in an existing config still works and says
so once.

## RF.5b — `=` / `gq` route through their own chains ⛔

**Deferred 2026-09-09, and this is what unblocks it.** The chains are
declared, default to `[native]`, and are honoured in the sense that the
native path is what runs. What is missing is delegation: a chain whose
first rung is `lsp` / `external` / `plugin` still runs the native engine.

The obstacle is structural rather than incidental. The operator computes
its range inside the grammar layer and applies its edit there; a
non-native rung needs that range to reach the host, and every route to it
crosses a boundary:

- a new `AppEffect` variant — costs `wit/types.wit`, the
  `boundary_app_effect.rs` mapping, and both renderers' classifiers;
- a new `LspRequest` arm — the cheaper and in-grain option
  (`effect.rs`'s own comment on `ReferencesView` says "a further LSP
  surface adds an arm here, not a host `Action`, not a renderer
  classifier entry"), but still `wit/types.wit` plus the boundary map,
  and it only covers the `lsp` rung.

Neither is hard; both are a different review surface from the rest of
this plan, which is why they were split out rather than folded in. Do
them together with RF.6, which needs the same channel.

Until then the user docs say so plainly rather than implying the chains
are live for all three intents.

## RF.6 — `operator:reformat` on `g=` ⛔

The operator form of `:format` over a range. LSP path uses
`do_lsp_format_request(is_range: true)`, which already exists; the external
path feeds the range to the filter. Result lands through
`SubsystemBoot::inbound::<T>` and applies as minimal edits.

**Tests:** `g=ap` reformats only the paragraph; the result is visible
**without** a further keystroke (the inbound-primitive assertion — a test that
presses a key first passes on the broken version too); cursor and folds
survive; no unedited line is re-emitted.

**Deferred with RF.5b**, and blocked on the same thing: `g=` IS the
`format.reformat` chain with an operator range, so it needs the same
range→host channel. `do_lsp_format_request(is_range: true)` already
exists but takes its range from the visual anchor, so `g=` works in
Visual mode by construction and `g={motion}` is what needs the channel.

## RF.7 — docs, site sync, benches, parity audit ✅

`docs/user/formatting.md` covers all three verbs, `textwidth` /
`autowrap`, the chain syntax, the `:setlocal` escape hatch, and the
replaced settings; added to `site/data/nav.toml` and synced.

Benches landed with their slices rather than here (RF.1's fill, RF.3's
break point) so the numbers were recorded while fresh.

**GPUI parity audit: empty, and structurally so.** No `Effect` variant
was added, no theme element, no renderer match arm — reflow produces
ordinary `Edit`s and `autowrap` produces one more. `grep -rn
"reflow\|autowrap\|textwidth\|AutoWrap\|WrapWidth\|ProviderChain"
crates/lattice-ui-gpui/src/` returns only two pre-existing comments
about image layout. Recorded here because "no GPUI change" that is not
written down is indistinguishable from the parity rule having been
skipped.

User docs for `textwidth`, `autowrap`, the three chains, and the three verbs;
`site/data/nav.toml` + `python3 site/scripts/sync-docs.sh` (a `docs/` change is
unfinished until the site carries it); `benchmarks.md` entries for RF.1 and
RF.3.

**GPUI parity audit.** Expected to be empty — no `Effect` variant, no theme
element, no renderer match arm; reflow produces ordinary edits. The audit
still runs and the result is *recorded*, because "no GPUI change" that is not
written down is indistinguishable from the parity rule having been skipped.

## Risks

**Auto-wrap is on the keystroke path and is the only slice with a latency
claim.** RF.3 ships its bench in the same commit, and the lexical-vs-tree
decision (§9) is the reason it can. If the bench says otherwise, the answer is
to narrow what auto-wrap inspects, not to move it off the keystroke path —
a wrap that lands a frame late is a visible cursor jump.

**RF.5 touches working code with no new capability.** It is the slice most
likely to be judged "not worth it" mid-flight. The concrete win is stated in
§6.2 and should be re-read rather than re-litigated: without it, every new
formatter placement is a host patch.

**`formatprg` deprecation is user-visible.** One release with an alias and a
`:messages` note, not a silent break — an editor that drops a config key
without saying so is indistinguishable from a bug.

**Undo granularity is easy to get subtly wrong** in two places: RF.2's
whole-range unit and RF.3's shared-with-the-keystroke unit. Both get an
explicit `u` test rather than an assertion on the edit batch, because the
batch shape and the user-visible undo step are not the same claim.
