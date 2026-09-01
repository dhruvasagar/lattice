# Org agenda as a dashboard — slice plan

> **Status: Active.** Opened 2026-08-31. Implements
> [`org-agenda.md`](../../architecture/org-agenda.md), which extends
> [`org-mode.md`](../../architecture/org-mode.md) §6.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Spans two repos. Slices marked **(plugin)** land in
`~/src/dhruvasagar/lattice-org-plugin`; the rest in `lattice`. Two slices are
cross-repo and say so.

Depends on [`refreshable-views.md`](refreshable-views.md) RV.1 (`gr` comes from
the shared minor). Catalogue entry: the agenda in
[`multibuffer-providers.md`](multibuffer-providers.md).

## Status

| Slice | Title | Status |
|---|---|---|
| **Phase 0 — the scan is quadratic** | | |
| OA.0a | The tree walk stops being O(n²) — a host fix, not a guest one | ✅ |
| OA.0b | The epoch budget is spent by the clock, not by callbacks | ✅ |
| OA.0c | A refresh keeps the old rows until the new ones arrive | ✅ |
| OA.0d | A configured directory means its files, not its subtree | ✅ |
| **Phase 1 — correctness before cosmetics** | | |
| OA.1 | Agenda rows are one line **(plugin)** | ✅ |
| OA.2 | Title-run header grouping; the `[untitled]` rows go | ✅ |
| OA.3 | ~~Refresh repopulates the view~~ — **not a defect**, see below | ⛔ |
| OA.4 | `<Tab>` / `<S-Tab>` cycle agenda blocks **(plugin)** | ✅ |
| OA.4b | `<Tab>` is declared once, on a shared `foldable-view-mode` | ✅ |
| OA.4c | A one-line excerpt is not a fold | ✅ |
| **Phase 2 — the view looks like an agenda** | | |
| OA.5 | `display-span` spans on the scan result (WIT seam) | ✅ |
| OA.6 | Org emits agenda spans; keyword/priority/tag colour **(plugin)** | ✅ |
| OA.7 | Header cells carry the block's own shape | ✅ |
| OA.7b | Conceal resolves per excerpt, so it works in a multibuffer | ✅ |
| **Phase 3 — the query language** | | |
| OA.8 | `Row` carries tags and properties **(plugin)** | 📝 |
| OA.9 | Match-expression parser **(plugin)** | 📝 |
| OA.10 | `Filter` gains `match`; sections honour it **(plugin)** | 📝 |
| **Phase 4 — custom commands** | | |
| OA.11 | `org.agenda-custom-commands` parse + fallback **(plugin)** | 📝 |
| OA.12 | The dispatcher transient **(plugin)** | 📝 |
| OA.13 | `<leader>oa` / `C-c a` open the dispatcher **(plugin)** | 📝 |
| **Phase 5 — layered display modes** | | |
| OA.14 | A second virtual-row provider on one view (spike) | 📝 |
| OA.15 | `org-agenda-log-mode` | 📝 |
| OA.16 | `org-agenda-clockreport-mode` + `cr` | 📝 |
| OA.17 | `org-agenda-timeline-mode` | 📝 |
| OA.18 | The `gD` view-mode dispatch transient | 📝 |

Phases 3–4 are independent of phase 2 and can interleave. Phase 5 depends on
OA.14 proving the pattern, and on nothing else.

---

## Phase 0 — the scan is quadratic

The reported bug ("agenda on refresh breaks") is here, not in refresh. Measured
against the real plugin, one guest `scan` call, debug build — **before** OA.0a,
kept because the shape is the diagnosis:

| lines | bytes | scan time | after OA.0a | rows |
|---|---|---|---|---|
| 202 | 2.1 KB | 0.18 s | 0.10 s | 1 |
| 402 | 4.2 KB | 0.47 s | 0.11 s | 1 |
| 802 | 8.5 KB | 1.65 s | 0.13 s | 1 |
| 1602 | 17 KB | 6.67 s | 0.18 s | 1 |
| 3202 | 34 KB | 28.9 s | 0.28 s | 1 |

Doubling the file quadrupled the time. The row count is constant at 1, so it
was never output-driven — it was the walk itself. A 34 KB org file, an ordinary
size, cost 29 seconds; a corpus of them was not a slow agenda, it was an agenda
that never arrived.

Three defects compound, and each is separately worth fixing:

1. ✅ The tree walk is O(n²) in fan-out — **host-side, fixed in OA.0a**. This
   was the whole of the reported bug.
2. **The armed epoch deadline does not trip.** A scan measured at 6.8 s ran
   against a ~1 s budget and was never interrupted. So nothing bounds a runaway
   guest call — a plugin-host defect well beyond the agenda, and still open.
3. `AgendaActor` is single-consumer, so one slow `scan` blocks every later call
   on that plugin, including a refresh's `begin()`. Measured: a refresh's
   `begin()` waited 10 s behind ONE in-flight scan. OA.0a removes the *cause*
   of that particular stall without changing the *property* — a slow guest call
   still blocks the actor, which is what (2) has to bound.

`open_scan_view` clearing the view before spawning is what makes this visible
on refresh and invisible on first open — on first open you have no rows yet, so
a slow scan reads as "loading"; on refresh it reads as "broken".

**This phase precedes everything.** OA.8 extends `Row` and the scan; doing that
on top of a quadratic walk makes the quadratic worse and buries the cause.

### OA.0a — The tree walk stops being O(n²) ✅

**Not a plugin slice.** The plan called it one; it was wrong. The quadratic is
entirely in the host's node API (`lattice-plugin-host/src/tree_resource.rs`) and
reproduces with no WASM in the picture at all. The guest's own code was
innocent — `scan_tree` precomputes its line index and `row_for_section` reads it
in O(1), exactly as the plan guessed it might.

`NodeResource` is a path of child indices from the root, re-resolved on every
accessor. The last step is `Node::child(i)`, and tree-sitter walks the sibling
list to reach `i`, so resolving the i-th child is **O(i)**. The module header
claimed "resolution is O(depth)", which is what let this sit. `named_child`
compounded it by rescanning the child list from zero per call. A pass over one
node's k children was O(k²).

The fix memoises two things per resource: the node's own kind/range/flags, and
one `TreeCursor` pass over its children indexed **both** ways, so `named_child`
is O(1) rather than a filter-and-count. An immutable snapshot cannot go stale,
so the caches need no invalidation. **No WIT change and no guest change** — the
seam's API is byte-identical, which is why this landed without touching the org
repo at all.

Measured: the host walk went 91.3 ms → 1.43 ms at 800 children (64×) and from
quadratic to 2.0× per doubling. End to end, one guest `scan` of a 34 KB org
file went **28.9 s → 280 ms (103×)**.

**Tests:** `named_child_indexes_past_anonymous_children` (the named-ordinal →
all-children index mapping, which is the one thing a cache could silently get
wrong) and `cached_answers_match_a_fresh_resolve` (every accessor agrees with an
uncached resource on the same path). 302 existing plugin-host tests unchanged.

**Bench:** `benches/tree_walk.rs`, swept across fan-out — a single size cannot
tell linear from quadratic. Results and the wider lesson in
[`benchmarks.md`](../benchmarks.md).

**Follow-up left open.** `tests/org_agenda_hang.rs::scan_time_versus_size` in
the org repo is still an `#[ignore]`d printing probe. It should become an
assertion so the ratchet holds from the org side too, but the authoritative
ratchet is now the bench.

### OA.0b — The epoch budget is spent by the clock, not by callbacks ✅

Every piece was wired and none of it was the re-arm rule: the ticker thread
runs, the agenda task arms per call, and `PluginBudget::default()` really is a
~1 s deadline. The defect was in the **accounting**.

`arm_store` sets a one-tick deadline and re-arms from a callback so the
cancellation token can be polled each tick, and the callback enforced
`budget.epoch_deadline` by counting **its own firings**. That equals elapsed
milliseconds only while the guest is executing guest code continuously — one
checkpoint crossed per tick, one firing per tick. A guest that calls host
imports in a loop is suspended for most of its wall clock, and wasmtime reports
an exceeded deadline **once** on re-entry however many ticks passed. So the
budget stretched by the ratio of host time to guest time, and a call could run
arbitrarily long while spending almost none of it. A tree-walking guest is
exactly that shape, which is why OA.0a's 6.8 s scan sailed past a 1 s deadline.

Now measured against an `Instant` taken at arm time, so the unit the type has
always documented is the unit it enforces.

**The trade-off, recorded rather than discovered later.** The new accounting is
stricter: time the OS spends descheduling the thread counts against the call,
where a firing count quietly forgave it. That is the right measure for what
this guards — the editor was stalled either way — but the sync grammar budget
(50 ms) is the one place a false trip would be user-visible, degrading a plugin
motion to a no-op with a warn. If that ever shows up under load, raise
`PluginBudget::grammar()`'s deadline; do not restore firing counts. Fuel is the
primary bound on that path by design and is unaffected.

**Tests.** The decision is split into `epoch_budget_exceeded` and pinned
directly: the budget is spent by the clock, the boundary is inclusive (so a
zero budget means "immediately", not "never"), and an unarmed call is never
tripped by time.

**What is NOT covered, and why.** There is no end-to-end test of a guest that
blocks in a host import, because no fixture provides one — `spin.wat` and
`busy.wat` are both pure guest loops, which is precisely the case that always
worked. Building one means a bespoke component plus a deliberately-slow host
import. The original evidence is the 6.8 s agenda scan, and it is no longer
reproducible from org because OA.0a made that scan fast. So the mechanism is
established by reading and pinned by unit test, not by an integration test —
stated plainly because "the tests pass" would otherwise imply more than it
should.

**Do not reuse the org scan as the vehicle.** Both tests in
`org_agenda_refresh_stall.rs` pass, and one of them —
`a_single_guest_scan_stays_inside_its_epoch_budget` — passed *before* this
slice too, for the wrong reason: OA.0a made the scan fast enough not to need
interrupting. It measures org, not the deadline.

### OA.0c — A refresh keeps the old rows until the new ones arrive ✅

The view is no longer cleared at open; `append_sorted` replaces rather than
appends, and `finish_empty` clears explicitly. The scan turned out not to be
progressive at all — it collects every file, sorts once and writes in one
terminal call — so the blank window was the entire scan rather than a gap at
its start.

**A regression it caught, worth carrying forward.** Building the source map and
handing it to `replace_excerpts` is NOT equivalent to `add_source`: only
`add_source` derives the per-excerpt `SyntaxHandle` from the path. The first
cut left every agenda row uncoloured while every test about rows, refresh and
folding stayed green — `agenda_rows_carry_per_excerpt_syntax_handles`, left
behind by AH.1 after the identical silent-uncoloured-agenda bug, is what
failed. **`replace_excerpts` is an incomplete peer of `add_source` and will
bite the next provider that reaches for it.** Not fixed here; it is a substrate
slice of its own.

Also unfixed and now written down: `replace_excerpts` does not re-baseline
`state.source_syntax`, so a dropped source's handle outlives it. Pre-existing —
the old clear-then-append path had the same hole.

**Tests**, each verified against its own half: the keep-rows test blocks the
source mid-scan and sees 0 rows on the old behaviour; the clears-when-empty
test sees a stale row when `finish_empty` does not clear.

### OA.0d — A configured directory means its files, not its subtree ✅

`walk_candidates` uses `ignore::Walk`, which recurses. Emacs does not: a
directory in `org-agenda-files` is expanded with `directory-files`, one level,
filtered by `org-agenda-file-regexp`. So an org directory with `roam/`,
`journal/` or `archive/` beneath it currently pulls all of them into every
scan.

**This is a correctness fix that happens to cut the corpus — it is not a fix
for the quadratic.** Recording that here because the two will be tempting to
conflate: OA.0d makes the file set the size the user asked for, OA.0a makes
each file cheap. Landing only OA.0d would leave a 34 KB file costing 29
seconds.

Read the directory one level rather than walking it. No new configuration:
`org.agenda-files` is already newline-separated with multiple entries, so a
user who wants a subdirectory lists it — which is exactly how emacs users do
it, and why emacs never needed a recursion flag either.

Not a per-source declaration in the WIT, deliberately. That was the first
instinct — recursion depth looks like "a fact about this source's file set",
the same category `roots` is in. But `WasmScannedExcerptSource` is the only
production implementor of the trait (every other is a test or bench fake), so a
`recursive()` knob would be an abstraction with exactly one caller and one
possible answer. Non-recursive is also not org-specific policy: it is the
ordinary meaning of a configured root, and recursion is the surprising default
nobody asked for. **If a second source ever wants the subtree, it declares it
then** — and that is the point at which the knob earns its place.

Keep `ignore`'s hidden-file and `.gitignore` filtering for the single level, so
`.git` and ignored files stay out.

Implemented as `ignore::WalkBuilder::max_depth(Some(1))` rather than
`read_dir`, so the hidden-file and `.gitignore` filtering the recursive walk
was doing right still applies at the single level.

**Tests:** a root with `a.org` and `sub/b.org` scans `a.org` only and listing
`sub` explicitly picks up both (verified to fail on the recursive walk); a
dotfile is still skipped; and a file named directly is still taken whatever its
depth, since only the DIRECTORY expansion changed.

User-facing, so `doc/org.md` says it: a directory is one level, list a
subdirectory to include it, and that is why there is no recursion setting.

**Separate observation, not this slice.** The host walks the *union* of every
source's claimed extensions, and org claims `org_archive` alongside `org`. So
archive files are scanned into the agenda today. Emacs's directory expansion
matches `\.org\'` only, and archived entries are archived precisely so they
stop appearing. Worth deciding deliberately rather than inheriting — but it is
a semantics question for org, not a walk question, so it does not ride here.

---

## Phase 1 — correctness before cosmetics

The view should stop lying before it starts looking good. Every slice here is
small, and three of them are bugs.

### OA.1 — Agenda rows are one line **(plugin)** ✅

`end_line` runs out to the planning line so `SCHEDULED:` shows under the
headline. Set `end_line = line` at both construction sites (the tree scan and
the text fallback).

One field, zero host work — `compose_snapshot` copies exactly
`start_line..=end_line` with no padding, and the agenda never opted into the
context-lines setting that only the search provider has.

Three existing tests asserted the old two-line shape and were updated rather
than worked around — they pinned it deliberately, so changing them IS the
slice. The new test asserts on the COMPOSED view as well as the excerpts,
because excerpt bounds and what the user reads are different things and only
the second is the promise.

**A trap for the next guest change here:** `tests/org_agenda.rs` loads the
prebuilt component from `target/wasm32-wasip2/release/`, so a source edit is
invisible until `cargo build --release --target wasm32-wasip2`. The first run
of this slice reported eleven passes against a stale wasm.

### OA.2 — Title-run header grouping; the `[untitled]` rows go ✅

`compose_header_rows` dedups on `excerpt.source`; the agenda needs dedup on the
header *title run*. A date group interleaves files, so today each file change
inside a group emits a spurious `[untitled]` header.

Substrate fix, not an agenda fix — the agenda is the first provider to want
title-run grouping, and the existing source-run behaviour must keep working for
search (`header_rows_dedupe_consecutive_same_source` and its two siblings pin
it). So this is a grouping *choice* alongside `FoldGrouping`, not a change of
the default.

`compose_header_rows` takes a `FoldGrouping` and the header provider carries
it, set from the same `create_multibuffer_view` argument that decides folding.
Reusing that enum rather than minting a second one is not economy: the fold and
the header MUST agree, or a fold spans a different range than the header run
above it and either swallows a visible header or strands one over a closed
fold. One declaration, read twice.

**A blanket switch would have broken the references view**, which is why this
is a parameter. `lattice-lsp`'s references provider titles each excerpt with
its LINE NUMBER, so consecutive excerpts from one file carry *different*
titles; title-runs would give it a header per reference instead of per file.
Search is the opposite — every excerpt of a file carries the same path string —
which is what lets one rule serve both conventions once the choice is
explicit.

**Tests:** the specifying test is un-ignored and passes; the three source-run
tests are unchanged and green; two new ones pin the halves of the title-run
rule that are easy to get wrong — equal titles CONTINUE a run (search's shape,
without which `HeaderRuns` would be agenda-only), and a leading empty title
emits nothing rather than `[untitled]`.

### OA.3 — Refresh repopulates the view ⛔ not a defect

Diagnosed and closed. **The refresh mechanism is correct.** Four end-to-end
tests drive the real plugin, open the agenda, press `gr` and get their rows
back — including after an edit in the agenda, with roots from the option, and
on a second consecutive `gr`.

"Refresh does not load anything back" is Phase 0 seen from the user's chair.
The refresh empties the view and then queues behind a scan that takes tens of
seconds, so what you observe is an agenda that never comes back. Kept here
rather than deleted, because the symptom is the one that gets reported and the
next person will look for it under this name.

### OA.4 — `<Tab>` / `<S-Tab>` cycle agenda blocks **(plugin)** ✅

Bind both on `org-agenda-mode`, whose `ActivationPolicy::Manual` scopes them to
agenda views. `<Tab>` → cycle the block at the cursor, `<S-Tab>` → global
cycle.

The host half is buffer-agnostic already: `CycleFoldAtCursor` reads the
buffer's folds and works on any buffer that has them, which the agenda does
(`foldlevel=0` plus `FoldGrouping::HeaderRuns`). The guest's `org-cycle` body
is *not* reusable — it requires an org tree-sitter tree and matches org node
kinds, neither of which exists in a multibuffer. So this binds the app effects
directly rather than routing through `org-cycle`.

Two new actions rather than reusing `org-cycle`, for the reason the slice
predicted: the guest body needs an org tree and matches org node kinds, and a
multibuffer has neither. They emit the fold effects directly.

No `Declined` fallback — falling through to the global `<Tab>` (jump-list
forward) would move the user out of a read-only view they are reading. The test
asserts the jump list did not move, because fold count alone cannot tell
"cycled a block" from "did the global thing". Verified to fail without the
binding (6 closed folds → 6).

---

## Phase 2 — the view looks like an agenda

### OA.5 — `display-span` spans on the scan result (WIT seam) ✅

`entry` gains `spans: list<display-span>` — **per row**, not per view. A guest
cannot know where its row lands until every other file's rows have been
interleaved by the host's sort, so offsets are relative to the row's own line
and the host does the two translations it alone can: line-relative offsets stay
put, and the row goes to its COMPOSED index.

Reused rather than invented. `display-span` already names a style by string
slot and resolves host-side through the path a `highlights.scm` capture takes —
its own doc names `org.todo.WAITING` as the motivating case — so a plugin's
theme elements reach the row with the colourscheme applied and no colour
crosses the ABI.

**Spans are validated, not trusted**, and dropped PER SPAN: `display-span`'s
contract is that one bad run must not cost a row its others, and a row that
vanished because its colour was wrong is a far worse failure than one that
renders plain.

**Cached with the row.** A cache hit skips the guest call entirely, so spans
left out of the on-disk form would make a WARM agenda render uncoloured while a
cold one rendered correctly — a difference nothing else in the system would
explain. `serde(default)` so a cache written before this field still loads.

**Tests:** the guest fixture emits a span and the host asserts it crosses with
its slot NAME intact; a bad span is dropped without losing the row; spans
survive the cache round trip; and the composed-row translation is a pure
function with its own tests, over a layout where a row's composed index differs
from its index within its file — because both translations fail INVISIBLY, the
rows still render, just wrongly.

Nothing emits spans yet, which is OA.6. Empty is the ordinary case and means
"say nothing about colour": the grammar's own highlighting shows, unchanged.

### OA.6 — Org emits agenda spans **(plugin)** 📝

Keyword, priority, tags, date. Slots resolve against org's already-registered
`org.todo.*` theme elements.

Watch the drain-order trap that already bit this area once: `config` and `theme`
drain at the same rank with a stable sort, so a theme registration that needs
`org.todo-keywords` must not be ordered before the config that supplies it.

**Test:** assert resolved styles, not span offsets — a span that lands on the
right bytes with the wrong slot is the failure mode here.

### OA.7 — Header cells carry the block's own shape 📝

Section headers today are one flat line of cells at `height: 1`. `VirtualRow`
supports multi-line, per-cell background and per-column font scale. Give the
block header a shape of its own (count badge, dimmed date suffix).

Generic: search, diff and references all inherit it. Keep the change in
`header_cells` rather than in the agenda.

### OA.4c — A one-line excerpt is not a fold ✅

Fallout from OA.1, found in use rather than by test: "`<Tab>` doesn't work
cleanly in org-agenda, only `<S-Tab>` seems to work."

Two fold sources are registered per view — one fold per excerpt (M.7), one per
group (AF.1). One-line rows made the per-excerpt fold `start == end`, which can
hide nothing, *and* `innermost_fold_idx` prefers the greatest `start_line` — so
the degenerate fold shadowed the group fold containing the row. `<Tab>` toggled
it and nothing moved. `<S-Tab>` sets every fold at once, so it was unaffected,
which is exactly how the report reads.

**The test that should have caught this passed on the broken code**, and that
is the part worth remembering. It asserted "the number of closed folds
changed" — true when a degenerate fold toggles invisibly, and true again when
`CycleFoldAtCursor` falls back to a global cycle. Both are the failure. It now
asserts WHICH fold changed and that only it did, over two multi-row blocks,
because a one-row group folds to nothing and proves neither.

**Left open deliberately:** a single-row GROUP fold is degenerate for the same
reason. Filtering it breaks three tests that pin single-line group folds,
including the invariant that the header and file-boundary providers agree on a
file-grouped layout. That is a shared-substrate question rather than this bug.

### OA.7b — Conceal resolves per excerpt ✅

Reported alongside OA.4c: conceal does not work in the agenda, so an org link
in a headline shows its raw brackets there while the same line conceals
correctly in its own file.

Structural, not a wiring miss. Every `conceal_rules_for` call in
`cells_worker.rs` takes `pane.syntax_handle` — the buffer's ONE language — and
a multibuffer has no single language: its rows are coloured through per-excerpt
handles, which is what K.4.7 added for highlighting. Highlighting has an
explicit multibuffer branch; conceal has none, so it silently resolves to no
rules.

K.4.7's shape, applied to conceal. An excerpt carries its grammar NAME — a
`&'static str`, so nothing allocates and `lattice-cells` needs no idea what a
conceal rule is — and the worker resolves rules once per DISTINCT language in
the view. An agenda over one org corpus has one language and a thousand
excerpts, so resolving per excerpt would take the registry lock a thousand
times for one answer.

Empty for every ordinary buffer, so the per-row lookup finds nothing and the
row falls through to the pane's rules exactly as before — the hot path is
unchanged where it is hot.

**The incremental rebuild path gets it too**, and its own comment says why: a
link that renders raw only on the line you just touched would be the most
visible possible version of this bug.

A row belonging to NO excerpt — a header's virtual row sits outside them —
resolves to nothing rather than to a neighbour's grammar, which is pinned.

---

## Phase 3 — the query language

### OA.8 — `Row` carries tags and properties **(plugin)** 📝

`Row` is `{ line, end_line, date, priority, keyword }` — no tags, no
properties, and the scan extracts neither. This is the precondition for
everything in phases 3 and 4, and the reason custom commands are not a small
item.

Both scan paths need it: the tree walk and the text fallback. The fallback's
known defects (planning line assumed to be the next line, phantom headlines
inside `BEGIN_SRC`) are deliberate and stay.

**Test:** tags on the headline, inherited tags from ancestors, and a
`:PROPERTIES:` drawer. Tag inheritance is a real decision — org inherits by
default; state which way this goes in the slice's commit.

### OA.9 — Match-expression parser **(plugin)** 📝

`+`/`-` conjunction, `|` alternation, `/` TODO section with `!` and explicit
keywords, property equality. Parsed to **data**, not a closure, so it can come
from a config file.

**Test:** the six shapes in the design's §7 table, plus malformed input
returning an error rather than a match-nothing expression — silently matching
nothing is indistinguishable from a correct empty result, which is the failure
this whole area guards against.

### OA.10 — `Filter` gains `match`; sections honour it **(plugin)** 📝

Extend `Filter` with `match: Option<MatchExpr>` and evaluate it in
`Section::admits`. Extend the `org.agenda-sections` TOML with `match`.

**Test:** a section with a match takes only matching rows; a section without one
behaves exactly as before (the existing default-section tests must not move).

---

## Phase 4 — custom commands

### OA.11 — `org.agenda-custom-commands` parse + fallback **(plugin)** 📝

A TOML-string option shaped like `org.agenda-sections`, with its failure model
inherited whole: malformed ⇒ fall back with the notice ridden onto the first
section title; one bad entry ⇒ skipped and named.

**Test:** mirror `agenda_sections`' own tests, including that a broken set costs
you your layout and never your rows.

### OA.12 — The dispatcher transient **(plugin)** 📝

Rows from the parsed custom commands, keyed as configured. Branch inside org's
existing `transient_source::build` on a new `args` discriminator —
`transient-source::id()` is one per guest, so this is not a second source.

Guest transients get `Action` / `Argument` / `Dismiss` rows only; no submenus,
no flags, no live preview. Drill-down is re-opening the same source with
different `args`, as capture already does.

**Test:** a configured command appears with its key; opening the menu with no
configuration still lists the built-in agenda.

### OA.13 — `<leader>oa` / `C-c a` open the dispatcher **(plugin)** 📝

A deliberate behaviour change: those chords open the default agenda today. Land
it separately from OA.12 so it can be reverted alone if it turns out to annoy.

---

## Phase 5 — layered display modes

### OA.14 — A second virtual-row provider on one view (spike) 📝

Before writing three display modes, prove one non-agenda provider can register
alongside the multibuffer's two and have its rows survive a refresh.
`register_virtual_row_provider` dedups by `ProviderId`, so the risk is
lifecycle (does a provider registered by a minor survive the view rebuild that
refresh performs?), not capability.

**If it does not survive**, phase 5 needs a re-registration hook and that is
better discovered here than three slices in.

### OA.15 — `org-agenda-log-mode` 📝
### OA.16 — `org-agenda-clockreport-mode` + `cr` 📝
### OA.17 — `org-agenda-timeline-mode` 📝

One shape, three times: a manual minor activated on the agenda view, owning its
keymap, its toggle and one virtual-row provider registered in `on_activate`.

These are display-only by design (design §3) — the cursor cannot rest on their
rows. If that proves intolerable in use, the deferred synthetic-source work in
design §9 is the escape, and it is a WIT seam, not a tweak.

OA.16 needs clock data on `Row`, which OA.8 should carry if this phase is
expected; otherwise it extends `Row` again.

### OA.18 — The `gD` view-mode dispatch transient 📝

Toggles for OA.15–17 plus span (day/week). Same guest transient constraints as
OA.12.

---

## Cross-renderer note

OA.7 and phase 5 touch virtual rows, which both renderers paint. Per the
lockstep rule, any `VirtualRow` field or `VirtualRowKind` variant added here
updates `lattice-ui-gpui` in the same patch. End-of-slice check:

```
grep -rn "VirtualRowKind::<NewVariant>" crates/lattice-ui-gpui/ --include="*.rs"
```

An empty grep means GPUI was missed.
