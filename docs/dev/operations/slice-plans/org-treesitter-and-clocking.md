# Org parses with tree-sitter, then org clocks — slice plan (OT / OC)

> Design: [`../../architecture/org-mode.md`](../../architecture/org-mode.md) (§9 scope,
> where clocking is currently listed as deferred),
> [`../../architecture/org-capture.md`](../../architecture/org-capture.md) §3 (the
> `:clock-in` / `:clock-resume` template keys cut for want of clocking),
> [`../../architecture/plugin-treesitter-seam.md`](../../architecture/plugin-treesitter-seam.md)
> (the TS.1–TS.3 snapshot + query seam this leans on),
> [`../../architecture/modeline.md`](../../architecture/modeline.md) §6 (the plugin row —
> **OC.3 below IS ML.6**, and [`modeline.md`](modeline.md) stays active until it lands).
>
> Plugin repo: [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin).
> Its `wit/` is generated from `lattice-wit` (WT.2), so every WIT change here
> reaches it by regeneration rather than by hand-vendoring.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** 🚧 in progress (2026-08-28). **Phase 1 (OT.1–OT.8) complete ✅.** Phase 2 (OC.1–OC.8) planned.

---

## Why

Two phases, and the first is not what the work started as. The ask was
clocking; the constraint that reshaped it is that **org must parse with
tree-sitter rather than with parsers we wrote ourselves.**

### Phase 1 — org does not use the tree it ships

The plugin registers the org grammar through the `language` seam, ships
`queries/highlights.scm` and `queries/folds.scm`, and proved `#eq?` predicates
work end to end. It then parses org with hand-rolled string scanning
everywhere else. `TreeSnapshot` appears exactly twice in `src/lib.rs`: the
import, and `_tree: Option<&TreeSnapshot>` — an underscore-prefixed parameter
the host threads in and the guest drops on the floor.

`headline.rs` counts `*` characters, `timestamp.rs` byte-scans for `[` / `<`,
`checkbox.rs` and `table.rs` scan lines, `agenda.rs` prefix-matches
`SCHEDULED:`. That is roughly 2,500 lines of parser that must agree with a
grammar it never consults.

**An earlier revision of this plan claimed a specific live bug here and was
wrong.** It read `agenda.rs:111` — "The planning line is the one immediately
below the headline, and ONLY that one" — as a divergence a `:PROPERTIES:`
drawer would trigger. OT.3 tried to reproduce it three times and could not: org's
grammar puts `plan` *before* `property_drawer` in the section rule, so the
planning line genuinely does come first, and org's planning info is a single
line so `DEADLINE:` / `SCHEDULED:` on two lines is not a case either. **The old
line assumption matches org's real grammar.** The claim is retracted here rather
than left to be re-derived.

What a line matcher genuinely cannot do is see **context**. `* TODO ` at the
start of a line inside a `#+BEGIN_SRC` block is example text, not a headline,
and no care in the matcher fixes it because the fact is not on the line — the
text scan invents a phantom agenda row. That is verified in both directions
(`a_headline_inside_a_source_block_is_not_a_row`, and its text-path twin
`the_text_fallback_cannot_tell_a_source_block_from_a_headline`).

So the case for the phase is the one originally made for it: **a bespoke parser
must agree with a grammar it never consults, and over time it will not** —
especially as files change under tools we do not control. That is a forward-looking
argument about a whole class, not a claim that each file is buggy today, and the
plan should not have dressed it as the latter.

The substrate is ready: TS.1 / TS.2 / TS.3 are all ✅, `tree_resource.rs`
implements `compile-query` / `run-query` / `run-query-ranges` with predicates
evaluated **host-side**, and every node the migration needs is named by the
grammar — `section`, `headline`, `stars`, `item`, `list`, `listitem`,
`checkbox`, `table`, `row`, `cell`, `drawer`, `property_drawer`, `plan`,
`timestamp`.

### Phase 2 — clocking

`org-mode.md` §9 defers clocking with a reason: it "needs persistent
'currently clocked' state and a modeline contribution, so it wants its own
slice after the rest lands." The rest has landed. `org-capture.md` §3 cut
`:clock-in` / `:clock-resume` for the same want.

---

## Decisions locked before slicing

Each of these was contested during design and resolved on a named goal or
heuristic, per the heuristic-mapping rule. A slice that finds one of them
wrong should surface the conflict, not quietly re-decide it.

**D1 — The tree is safe for edit actions.** `wit/tree-sitter.wit:12` — a plugin
"acquires the handle alongside the `document` handle from the same dispatch
context (same instant → tree + text versions agree)" — and `wit/grammar.wit:71`
states it for the action path specifically: the tree is "acquired the same
instant as `doc` so their versions agree". A stale tree yielding a stale line
number was the objection; version agreement is a contract, so it does not
arise. `none` means plain-text or parse-pending only.

**OT.7 corrected this.** "Acquired at the same instant" bounds the read against
a concurrent republish; it does not make an off-thread reparse finish, and those
are different claims. The objection was right and the answer was wrong. The host
now gates both plugin gates on `tree_reflects`, so `none` also means
parse-behind-the-buffer — see OT.7.

**D2 — Off-buffer parsing is a missing primitive, not a structural barrier.**
`lattice-syntax/src/syntax.rs:347` is `pub fn parse(&mut self, source: &str)`;
the host can parse arbitrary text with any registered language. An earlier
draft of this plan recorded "no tree off-buffer" as a carve-out for
`agenda.rs`. That was wrong — it stated an absent feature as a constraint —
and OT.2 supplies the primitive instead. **Heuristic #1:** the carve-out was
risk-aversion wearing the costume of a constraint.

**D3 — RETRACTED by its own bench.** This said the agenda's per-file text copy
was a bulk crossing worth removing, and that removing it was a paramount-#1 win.
It required the net be measured rather than assumed, and the measurement killed
it: the copy is **217 ns** against a **1–2 ms** parse, and a tree alone cannot
answer a scanner's questions because the seam exposes no node text. `scan` ends
up taking **text AND tree**, not tree instead of text.

What replaces it: **structure from the tree, characters from the text** — which
governs every remaining OT slice, and means no `node.text()` primitive is needed
anywhere, since each seam already carries text beside its tree (`scan` here,
`borrow<document>` on the grammar seams, `read-file` / WASI beside `parse-file`).

**D4 — The buffer is the clock's durable record; there is no clock-persist.**
An unterminated `CLOCK: [start]` with no `--end` *is* a running clock, so
`clock-out` and `clock-cancel` are pure buffer operations that re-derive their
target structurally and need no session state. Guest state exists only to feed
the modeline and `clock-goto`. After a restart the modeline is empty, the file
is still correct, and clocking out on that entry works. A resumed-on-boot
state file is cut as YAGNI.

**D5 — The clock line's position is derived structurally, never from the
cursor.** The cursor's only role is to identify the enclosing entry. Placement
is: enclosing `section` → skip `plan` → skip `property_drawer` → find-or-create
the `LOGBOOK` `drawer` → insert as its **first** `CLOCK:` line (org's
newest-first convention, which also makes "find the running clock" O(1)). No
enclosing headline is a refusal with an echo, not an invented location.

**D6 — The clock session lives on the events seam, not the grammar seam.**
Grammar's only job is the buffer edit, which genuinely must be synchronous.
The session, the wake and the modeline segment live on org's event actor —
off the keystroke path by construction (**paramount #4**). Modeline updates
ride the **event bus**, the same channel `lattice-lsp::modeline` and
`lattice-ai::mcp::status` publish on, so a plugin segment and a native segment
are indistinguishable downstream. A rejected alternative put the periodic wake
on the grammar store because it needed one fewer host change; that is
blast-radius reasoning and was struck.

**D7 — The guest holds the stopwatch.** The host could own an
`elapsed-since(T)` element and tick it with zero WASM calls. Rejected: it
moves duration semantics into the host. The guest formats its own string once
a minute (**one typed call per minute against a <500ns p99 budget** — negligible
on magnitude), and the general wake seam it needs is the primitive
`design.md` Appendix B already wants for idle hooks. **Heuristic #1.**

---

## Not in this plan

**The superset grammar linker.** `lib.rs:2390–2475` adds `theme`, `help`,
`language`, `dashboard`, `modes` and `picker-source`'s `walk` to the
Reflex-class sync linker, and the comments (TC.6, CR.3, LG.3c, CR.4) all give
the same reason: a multi-seam component's instantiation must satisfy **every**
import its world declares, not only the ones that seam uses. So that store has
become the union of every seam's imports by accident of the Component Model,
with only `logging` deliberately withheld. That is a real plugin-host question,
it is bigger than either phase here, and it gets its own investigation rather
than riding this work.

It does bear on **OC.1** in one way worth recording: `host-services` — and
therefore `emit-event` — is *already* linked into the grammar store
(`lib.rs:2470`). OC.1 is not "grant the keystroke path a new power"; it is
"finish wiring a power it already has and that currently does nothing."

---

## Phase 1 slices — OT

| Slice | Description | Status |
|---|---|---|
| OT.1 | `option<borrow<tree-snapshot>>` on `apply-motion` + `apply-text-object` | ✅ |
| OT.2 | `tree-sitter.parse-file` — the off-buffer parse primitive | ✅ |
| OT.3 | `agenda-source.scan` takes a tree; `agenda.rs` migrates | ✅ |
| OT.3b | **Persistent** result cache — survives restarts | ✅ |
| OT.4 | `headline.rs` → `(section)` / `(headline)` / `(stars)` | ✅ |
| OT.5 | the plan's date from `(plan (entry))`; stepping stays text | ✅ |
| OT.6 | `checkbox.rs` → `(listitem)` / `(checkbox)` | ✅ |
| OT.7 | `table.rs` → `(table)`; the shared `tree.rs`; the staleness gate | ✅ |
| OT.8 | capture target + refile picker → `parse-file` | ✅ |

### OT.1 — motions and text objects get a tree ✅ (2026-08-28)

**Landed smaller than specced, for a reason worth recording.** This slice
predicted native plumbing, quoting `grammar.wit`: "the native `MotionContext`
carries a `ScopeResolver` rather than a `SyntaxSnapshot`, so there is no tree
handle to mint here without changing the native context." Half right.
**`GrammarEnv::syntax` already carried the type-erased snapshot on every
dispatch** (`registry.rs:305`) — `execute_action` cloned it into
`ActionContext`, and its own doc-comment said "motions / text-objects /
operators ignore it." So the native cost was two borrowed fields and five
assignments, not a plumbing project. Borrowed rather than cloned because
motions fire on every `j`: a native motion pays nothing, and only a plugin
motion that mints the resource pays the `Arc` bump.

The trampoline's mint is now `resolve_tree_snapshot`, shared by all three
seams. It was three conditions duplicated per seam — capability grant, downcast,
and a parse check — and one copy quietly dropping one is exactly the class of
bug the seam cannot afford.

**A latent bug fell out.** `lattice-cli`'s scaffold template still declared
`apply_motion(_c, _ctx)` / `apply_text_object(_c, _ctx)` — never updated for
`doc` when OM.4 / OM.4b added it. `lattice plugin new` emitted a plugin that
could not compile (`E0050`). Fixed here because this slice touches both
signatures anyway.

**Tests:** four in `tree_seam.rs` (granted + ungranted × motion + text object),
against `multiseam-guest` contributions that answer **from the tree alone** — so
a regression to `none` fails them loudly rather than returning a plausible
position. Verified by mutation: forcing both seams back to `none` fails exactly
the two granted tests with the guest's own "got no tree" message, and leaves the
ungranted pair passing.

**Blast radius, as predicted and all mechanical:** `plugins/auto-pair`,
`plugins/treesitter-context`, both fixtures, the scaffold, the projection bench,
and the two `boundary_grammar.rs` unit tests. Note the failure mode found while
doing it — an out-of-date guest fixture builds as a **warning**, not an error,
and its tests then silently *skip*. `cargo check` looked clean while four guests
were broken.

**Original plan text follows.**

**Design:** `plugin-treesitter-seam.md` §7; `grammar.wit:83–87` pre-authorises
this exact slice — "no motion has yet needed one. **When one does, that is the
slice that adds it.**"

Today `apply-action` takes `option<borrow<tree-snapshot>>` and
`apply-motion` / `apply-text-object` take only `borrow<document>`. Org's
headline navigation (`]]` / `[[` / `g{`) is motions and `ih` / `ah` / `ir` /
`ar` is text objects — both call into `headline.rs`, so OT.4 is blocked until
they can reach a tree.

Native side first: `MotionContext` and `TextObjectContext` carry a
`ScopeResolver` rather than a `SyntaxSnapshot`, so there is no handle to mint
without changing the native context. Then the WIT signatures, then the
trampoline mint sites — minted alongside `doc` at the same instant, so the
D1 version-agreement guarantee extends unchanged.

**Blast radius, mechanical:** `plugins/auto-pair`, `plugins/treesitter-context`,
the `grammar-guest` and `multiseam-guest` fixtures, `lattice-cli/src/scaffold.rs`
(the new-plugin template), `benches/grammar_roundtrip.rs`,
`benches/grammar_trace_gate.rs`. Each gains an ignored parameter. Org
regenerates its `wit/` per WT.2.

**Test:** a fixture motion that resolves its target through the tree and would
fail with `none`; the existing grammar round-trip benches must not regress.

### OT.2 — `tree-sitter.parse-file` ✅ (2026-08-28)

**Two consumers, not three — the plan miscounted.** It listed OT.3 alongside
OT.8's two, but `agenda-source.wit` is explicit that its guest "touches no
filesystem: no preopens, no `walk` capability", and `parse-file` is `fs:read`
gated. Having the agenda call it would hand a filesystem capability to the one
seam deliberately built without one. So OT.3 keeps the host doing the read and
the parse (which it must do anyway to build the source `Document`) and lends the
guest a borrow; `parse-file` serves OT.8's capture target and refile picker,
both of which are already filesystem-capable.

**Gated twice**, and the second gate is the load-bearing one: `tree-sitter` for
the parse, and the same `fs:read` grant `read-file` enforces for the read — so a
plugin cannot learn the structure of a file it was refused the contents of.

**Cost recorded in the WIT rather than buried.** This is reachable from the sync
grammar linker, whose sibling comment promises reads are "no I/O, no parse — the
tree is already there". `parse-file` is both. `read-file` set the I/O precedent
at OC.5a; this adds a parse on top, so it belongs on explicit user actions and
not in a motion or text object.

**Tests:** four in `tree_seam.rs`. The granted case parses a two-function Rust
file with `GrammarEnv::default()` — *no* `syntax` on the dispatch at all, so the
tree provably did not come from a buffer — and asserts `source_file:2`. Three
denial arms, each isolating one cause: capability withheld (fs granted), path
outside the fs grant (capability granted), and an extension with no language
(both granted, file readable).

**Original plan text follows.**

```wit
/// Parse off-buffer content with a registered language. The host reads and
/// parses; only the path crosses. Capability-gated on `fs:read`, like
/// `read-file`.
parse-file: func(path: string) -> option<tree-snapshot>;
```

One primitive, three consumers (OT.3, OT.8's two), and any future plugin
wanting structure over files nobody opened. `none` when the extension resolves
to no registered language, when the read fails, or when the parse fails —
graceful, never a trap, per `agenda-source.wit`'s "one bad file must not fail
the agenda."

Backed by `lattice-syntax::SyntaxSnapshot::parse` (D2) and handed out through
the existing `tree_resource.rs` resource machinery, so the tree still never
crosses the boundary.

**Test:** a guest parsing a file it has no preopen for; capability refusal
yields `none` rather than a trap; an unregistered extension yields `none`.

### OT.3 — the agenda scans the tree 📝

**Deps:** OT.2.

**Final shape: `scan(path, text, tree: option<borrow<tree-snapshot>>)`** — text
always, tree beside it, mirroring `apply-action(callback, ctx, doc, tree)`.

The first attempt *replaced* text with a `scan-input` variant, on the theory
that the per-file copy was the cost worth removing. Two findings killed it, both
worth keeping:

1. **The copy was never the expense.** 217 ns against a 1–2 ms parse (bench
   above). Removing it optimised the wrong end by four orders of magnitude.
2. **A tree alone cannot answer what a scanner asks.** The `tree-sitter` seam
   exposes node kinds, ranges and navigation — but **no node TEXT**. Nothing had
   ever needed characters (auto-pair reads kinds, treesitter-context reads
   ranges), so the gap was invisible until now. Without the string a guest needs
   one crossing per headline to read a TODO keyword: ~50 µs per file, 200× the
   copy it was avoiding.

So the rule for every OT slice is **structure from the tree, characters from the
text** — and no `node.text()` primitive is needed anywhere, because each seam
already carries text beside its tree: `scan` here, `borrow<document>` on the
grammar seams (OT.4–OT.7), and `read-file` / WASI beside `parse-file` (OT.8).

**Original plan text follows.**

`scan` previously took `path: string, text: string`. Both inputs must survive:
`agenda-source.wit` deliberately keeps a source independent of the `language`
seam ("would make an agenda source *require* a language when the two are
independent contributions"), so a source whose extension has no registered
grammar still needs text.

```wit
variant scan-input {
    tree(borrow<tree-snapshot>),
    text(string),
}
export scan: func(path: string, input: scan-input) -> result<list<entry>, string>;
```

Host supplies `tree` when the file's language is registered, `text` otherwise.
Org handles `tree` and errs on `text` — a loud, logged, single-file skip.

`agenda.rs` then reads `(section)` with its `plan` field rather than
prefix-matching the line below the headline. **It fixes no pre-existing dated-row
bug** — see the retraction in Why — but it stops counting headlines that only
look like headlines, and one real bug DID fall out of writing it: a tree-sitter
node's end position is exclusive and org's `plan` rule swallows its newline, so
a one-line plan reports an end on the following line and every scheduled row's
excerpt was one line too tall. `civil_from_epoch_day`, `sort_key`, `group_key` and
`group_label` are date arithmetic, not parsing, and stay.

**Bench (required, D3) — ran, and it falsified D3's performance premise.**
`benches/agenda_scan_input.rs`, per 400-line file:

| | |
|---|---|
| `parse` (markdown) | 2.20 ms |
| `parse` (rust — simpler, single-pass grammar) | 1.16 ms |
| `text_copy` — what crossing the boundary cost | 0.217 µs |

So parsing costs **~1–2 ms per file** whatever the grammar, against a **217 ns**
copy: the boundary saving D3 called "structural" is real and four orders of
magnitude too small to matter. Parser construction is not the cause — reused and
fresh parsers measure identically, so hoisting it out of the per-file loop saves
nothing.

**OT.3 ships anyway, on the accuracy argument alone**, which was always the
stronger one and which this does not touch: `agenda.rs:111` drops any row whose
`SCHEDULED:` line is separated from its headline, and the tree ends that. The
plan no longer claims a performance win here. What it claims is a correctness
win at a measured price.

**The price is smaller than the first estimate, because the estimate used the
wrong workload.** It scaled to a 200-file project (~300 ms/refresh). A real
agenda works over tens of files, not hundreds — **the large file counts live in
org-roam, not the agenda** — so the true cost is ~20–80 ms per refresh,
off-thread, behind headerline progress. That is acceptable; 300 ms would not
have been.

**OT.3b exists so that "optimise later" is a numbered slice rather than a good
intention.** And org-roam, when it comes, does not want a snapshot cache — it
wants a real index (org-roam itself uses a database for exactly this reason).
OT.3b is the near-term fix for refresh cost; the DB is the eventual answer for
scale, and they are different mechanisms for different problems.

**Test:** verified as a PAIR, so the difference is documented rather than only
its good half — `a_headline_inside_a_source_block_is_not_a_row` (tree path, one
row) beside `the_text_fallback_cannot_tell_a_source_block_from_a_headline` (text
path, two rows, same corpus). Plus: the tree arm proven host-side with `.rs`,
the text fallback proven with an unknown extension, and a malformed file still
skipping with a `debug` log.

### OT.3b — scan results, persisted across restarts ✅ (2026-08-28)

**Deps:** OT.3.

**Not the snapshot cache this plan first specified.** Two things redirected it.

**Tree-sitter trees cannot be serialised** — no `to_bytes` / `from_bytes` on
`Tree` anywhere in the crate. So a cache that survives a restart physically
cannot hold snapshots; it must hold what the scan *derived*. That is also why
org-roam uses a database of extracted nodes rather than of parse trees.

And that is the better layer anyway: caching rows means a hit skips **the parse
and the guest call**, where a snapshot cache would only ever have skipped the
parse. The in-memory snapshot cache written first was deleted as the weaker
version of the same idea, not kept alongside.

**What a hit does not skip: the read.** You cannot know a file is unchanged
without looking at it, and the host reads it upstream regardless. A warm read is
~10–50 µs against the ~2 ms parse, so this is the cheap end — not worth an
`mtime` pre-filter's correctness risk.

**Key: `(generation, path, content-hash)`.** Content hash rather than mtime
because the text is already in hand: hashing costs ~2–5 µs against the ~2 ms it
protects, and it is exactly right — no one-second granularity, no filesystem
that lies, no length collision to paper over.

**`generation` needed one small seam change**, and the plan should record why it
was unavoidable rather than a preference. Cached rows embed presentation
computed against the scan's `today` anchor (`"tomorrow"`, `"overdue by 2
day(s)"`), so serving them under a different anchor renders yesterday's
"tomorrow" as tomorrow — silently wrong at midnight, the exact bug the anchor
exists to prevent. But `today` and the keyword set are *guest-side* state the
host cannot see. So `begin` now returns an opaque `u64` the guest derives from
its own scan-wide state. The host compares two integers and still learns nothing
about dates or keywords (**paramount #2**).

A rejected alternative split `entry` into content-derived and today-derived
halves so the cache could survive midnight too. That buys one avoided daily
rebuild for an ABI change, a per-scan rendering hop, and org's grouping
semantics moving partly host-side — the property this seam exists to protect.
**Heuristic #1**: the cheaper design is also the one that keeps the boundary
honest.

**Failure behaviour, all degrading to "no cache" and never to a wrong answer:**
a missing file, schema-version mismatch, corrupt bytes, an unreadable directory,
a failed write. Written to a temp file and renamed, so a kill mid-write leaves
the previous cache intact rather than a truncated one. Flushed every 64 entries
and on `Drop`, bounding what an unclean exit loses. Capped at 4096 files, then
cleared wholesale — eviction *order* does not matter, because a scan repopulates
exactly what it touches.

Stored under the plugin's own data dir (`plugin_data_dir`), so two plugins
cannot collide and uninstalling one removes what it cached.

**Tests:** seven, asserting on hit/miss counters rather than on timing — a
timing assertion for "did it reparse?" is a flaky test. Hit on unchanged text,
miss on changed text, generation change discarding everything (the midnight
case), survival across a simulated restart, `Drop` persisting without an
explicit flush, corrupt bytes recovering, a schema bump refusing to read the old
shape, and two sources not reading each other's rows.

### OT.4 — `headline.rs` on the tree ✅ (2026-08-28)

**Deps:** OT.1.

The big one, and it landed as specced plus **two links that were not in the
plan because nobody knew they were missing**. Both were found the same way — by
writing the first test that drove a chord through a real editor instead of
calling the walk directly — and both had the same shape: a seam that reads as
wired end to end and delivers `none`.

**What was specced.** `headline_level`, `enclosing_headline`, `subtree_end`,
`next_headline`, `prev_headline`, `parent_headline`, `prev_sibling`,
`next_sibling` are now tree walks over `(section)` / `(headline)` / `(stars)`.
`grammar.js` makes each one short: `section` is `headline, plan?,
property_drawer?, body?, subsection*`, so a section node IS a subtree, its
extent IS `subtree_end`, the parent is the nearest `section` ancestor, and
siblings are sibling nodes. `]]` is the pre-order successor; `[[` from body text
is the enclosing headline itself and from a headline line the pre-order
predecessor.

They live behind one `headline::Headlines` locator holding `Option<&tree>` plus
the line accessor, tree-first with the text logic as the fallback. That shape
replaced a `match tree { … }` that had already been written out three times in
the same function — a fourth site forgetting it would be the only one still
hand-parsing, which is this phase's own bug class reproduced inside the plugin.

`restar` / `shift_headlines` / `toggle_heading` stayed text rewrites as planned.

**Link 1 — the actor path carried no snapshot.** `GrammarEnv::syntax` is filled
on two routes: the host's Action gate, and `dispatch_blocking → DispatchEnv →
actor.rs`. TS.1 wired the first (actions were then the only consumer) and left
the second a hard `syntax: None` with a comment saying why. OT.1 made motions
and text objects consumers — and they come through the second. So the WIT, the
trampoline, the guest and `tree_seam.rs` were all correct and org's `ar` / `]]` /
`g{` received `none` on every keystroke. `tree_seam.rs` missed it by building
`GrammarEnv { syntax: Some(&snapshot) }` by hand: a seam test that supplies its
own context tests everything except the gate. `DispatchEnv` gains a type-erased
`syntax` field; `lattice-runtime` and `lattice-host` each gained a test that
fails on the old code.

**Link 2 — org never asked for the `tree-sitter` capability.** The seam is
capability-gated, and org's manifest declared `provides = ["language", …]` but
no `editor_capabilities`. Registering a grammar is not asking to read one. With
link 1 fixed the trampoline still handed back `none`, silently, because the gate
was doing exactly its job.

Neither link is visible from the guest side, from a unit test, or from a test
that constructs the context it is testing. What found them was one test that
pressed a key in an editor and looked at the buffer.

**Also recorded, not fixed:** `dar` leaves a blank line where the subtree was.
The object's range stops short of the last line's newline, so `d` empties the
lines without closing the gap — unchanged by this slice (the text path leaves
the same blank), and a different question from where a subtree *ends*.
`archive.rs` already works out which line break travels with a subtree and `ar`
does not consult it. Pinned in `dar_inside_a_source_block_does_not_split_the_block`.

Every motion, text object and promote/demote action rides this, so its test
surface is the widest in the phase: five new dispatch-through-the-editor tests
in `org_structure.rs` plus the two host-side gate tests above.

### OT.5 — the plan's date from the tree; the stepping path retracted ✅ (2026-08-28)

**This slice specced more than the grammar can support, and the retraction is
the more useful half.** It said "`stamp_at` and `first_stamp` become
`(timestamp)` lookups". Dumping the parse tree says otherwise:

```
* TODO Task <2026-09-05 Sat>
  DEADLINE: <2026-08-25 Tue> SCHEDULED: <2026-08-20 Thu>
```
```
(section
  headline: (headline stars: (stars) item: (item (expr) (expr) (expr) (expr)))
  plan: (plan (entry name: (entry_name) timestamp: (timestamp date: (date) day: (day)))
              (entry name: (entry_name) timestamp: (timestamp date: (date) day: (day)))))
```

**`timestamp` is a node only inside `(plan (entry))`.** The headline's own stamp
is four undifferentiated `expr` tokens, and one in body text is
`(paragraph (expr) …)`. So the `<C-a>` / `<C-x>` stepping path — `stamp_at`,
`part_at`, `step` — has no node to migrate to. It stays on the text scanner, and
that is not a compromise: with no grammar rule to diverge from, the scanner is
the only parser of the construct rather than a second one. Half-migrating would
make it two.

Two further facts the dump settled, both of which correct comments that were in
the code:

* **A plan is exactly one line.** `plan: seq(repeat1(entry), _eol)`, so several
  entries on one line are several `entry` nodes, and a `SCHEDULED:` written on a
  SECOND line parses as body text. `date_for`'s doc-comment claimed the tree path
  had to scan multiple plan lines; it never did, because the node is one line.
* **Entry order is not precedence.** The text path does
  `line.trim_start().strip_prefix("DEADLINE:")`, which requires the keyword to
  come first — so `SCHEDULED: <+5> DEADLINE: <+1>` files the entry five days
  late. The tree sees two `entry` nodes and has no opinion about their order.

So what landed is the half the grammar supports: `agenda::plan_date` reads
`entry`'s `name` and `timestamp` fields, and the prefix match and bracket scan
are gone from the tree path. `scan_file` keeps both, deliberately — it runs only
when there is no grammar for the file, and fixing a second parser to agree with a
tree it cannot see is what this phase exists to stop doing. The pair is pinned:
`a_deadline_outranks_a_scheduled_written_before_it` (tree) and
`the_text_path_reads_the_first_keyword_on_the_plan_line_not_the_strongest` (text).

The **civil-date arithmetic stays** as specced: `weekday` (Zeller) and
`epoch_day` (Hinnant) are integer math, not parsing, and `timestamp.rs:76`
records the reason — a `chrono` dependency tree in a wasm guest to replace twenty
lines of arithmetic. Nothing about tree-sitter changes that.

### OT.6 — `checkbox.rs` on the tree ✅ (2026-08-28)

`(list)` / `(listitem)` / `(checkbox)`, all real nodes, with a nested list as a
`contents` child of the item that owns it — so "the direct children of this
item" became a walk where `tally` reconstructed it from indentation plus a
lock-on-the-first-child-level rule.

The divergence is OT.4's, one construct along: a `- [ ] example` line inside a
`#+BEGIN_SRC` block is block text and indentation cannot say so. `<C-Space>`
rewrote it, and the enclosing cookie counted it — a one-item list next to a code
sample read `[1/2]`.

**The slice's own claim needed narrowing.** It said cookies "are computed from
the sibling listitems the tree yields". Half true: the *count* is, but the cookie
itself stays text. `* Shopping [1/3]` parses as `item: (item (expr) (expr))` —
the grammar does not model a statistics cookie, the same finding OT.5 recorded
for timestamps.

**What the split has to be, and why.** `Checkboxes` answers only STRUCTURE from
the tree; each box's state is read from the caller's text. That is not stylistic:
the cookie is recomputed against the buffer as it will be AFTER the toggle, and
the tree describes it as it is BEFORE. Reading a tick from the tree would leave
every cookie one keypress behind — the exact bug the `after` overlay exists to
prevent.

**A probe-cost worth recording.** An indented item's `listitem` node starts at
its bullet, so `enclosing` at byte 0 lands in the enclosing `list` and the
ancestor walk finds no item at all. Every indented list in the suite went silent
until the probe moved to the checkbox's own column. `Headlines` is not exposed to
this because a headline starts at column 0 — which is why OT.4 never hit it.

**The fallback's limit is now written down**: a child must be indented more than
its parent and a headline's indent is 0, so a list flush at column 0 under a
headline has no children by the indent rule and its cookie never moves. The tree
has no such problem (a `list` is a child of the section's `body` whatever its
column), and the fallback runs only when there is no grammar for the buffer.

### OT.7 — `table.rs` on the tree ✅ (2026-08-28)

`table: (row | hr)+`, so a `table` node IS the contiguous run `table_bounds`
reconstructs by walking outward while lines still start with `|`. Same answer
for real org; different for a table drawn inside `#+BEGIN_SRC`, which parses as
`block contents:` — `<leader>o|` was realigning example content in a code block.

**Only the bounds moved, and the slice's own wording was wrong about the rest.**
It said "alignment stays a rendering concern computed from cell extents". It is
not a rendering concern at all — alignment REWRITES the table as an edit, so the
cell offsets the caret needs are offsets into a line that is not in the buffer
yet, and no tree describes a buffer that does not exist. `parse_row`, `align`,
`cell_at` and `cell_start` stay text for that reason, not for a rendering one.

#### The staleness gate — the finding that outlives this slice

Running one operation TWICE is what exposed it. `<leader>tr` then `<leader>tc`
(insert a table row, then a column) put the column into the one-row table that
no longer existed, because reparsing is off-thread and the published snapshot
still described the buffer as it was before the first chord's edit.

TS.1's comment says the tree and the document handle are acquired "at the same
instant … so their versions agree". That bounds the read against a concurrent
republish; it does **not** make an off-thread reparse finish. Two different
claims, and only the first was true. D1 in this plan repeats the same conflation
and should be read with this correction.

Both plugin gates now gate on `tree_reflects` — the predicate a few lines away
already used for the `=` operator's indent resolver, for the same reason. A
plugin handed a stale tree does not fail; it resolves real structure at line
numbers that have moved and edits there. `none` is strictly better: it is a
contract every OT.x locator already handles by falling back to its line logic.

**Every slice before this one ran a single chord per assertion**, which is why
five slices of tree migration did not surface it.

#### `src/tree.rs`

The dedup OT.7 made unavoidable: by OT.6 there were three copies of
`last_content_line`, each with its own paragraph explaining the same
exclusive-end off-by-one, and two of `ancestor` / `children_of_kind`. Three
copies of a subtle rule is the shape of a rule about to drift — this phase's own
thesis, reproduced inside the plugin — so they live in one module now.
`tree::enclosing` carries OT.6's lesson in its doc-comment: `byte` is a column
and 0 is not always right, because a node starting after indentation does not
contain column 0.

#### The fallback bug the gate exposed

`checkbox`'s indent walk required a child to be indented MORE than its parent,
and a headline's indent reads as 0 — so a list flush at column 0 under a
headline had no children and its cookie never moved. Ordinary org, invisible
because every existing test indents its lists. Fixed rather than documented: a
headline owns every list beneath it until the next headline, whatever column
they start at. An item parent still requires a deeper indent, which is what
tells its children from its siblings.

### OT.8 — capture and refile stop hand-parsing ✅ (2026-08-28)

**Deps:** OT.2.

Both remaining off-buffer parsers move to `parse-file`, which OT.2 added for
exactly this.

The failure removed is the quiet kind, and it is worth stating precisely because
the obvious guess is wrong. A template naming `headline = "Vocabulary"` matched a
`* Vocabulary` line written as an example inside a `#+BEGIN_SRC` block — and it
did **not** file into the block. `subtree_end` stops at the next real headline,
so the note landed just past `#+END_SRC`, attributed to a heading that does not
exist, and capture reported success. Quieter than filing into the block, and
therefore worse: the note sits where the user has no reason to look. With the
tree the target is absent, and absent already has a contract (append, and warn —
OC.5a). Refile's picker had the same shape one worse: it *offered* such a
headline as a destination, with an insertion line.

**Neither gains reach.** `parse-file` makes the identical grant check
`read-file` does, and refile's paths already came from a grant-checked `walk`.
What refile loses is the WASI read it no longer needs for structure — the text is
still read, because `parse-file` returns structure and no node text and the
picker's labels are headline titles (D3's split, again).

**The sharing the slice asked for, one level up.** It said `headline::subtree_end`
"must stay shared". It is better than that now: both consumers share
`headline::Entry` and one `targets_from` / `resolve_in` body over it, with
`outline` (tree) and `outline_text` (fallback) as interchangeable sources. That
is what let the structure source be swapped underneath without either consumer
changing.

---

## Phase 1 is complete (2026-08-28)

OT.1–OT.8 all ✅. What the phase actually cost and bought, since the plan's own
"Why" was partly wrong when it was written:

- **Two slices were retracted or narrowed by evidence, not by preference.** OT.5
  specced a `(timestamp)` migration for a node the grammar only has inside a
  `plan`; OT.6 and OT.7 each specced a cookie/alignment migration for constructs
  the grammar does not model. Dumping the tree settled all three in minutes,
  where reasoning from the slice text would have produced half-migrations.
- **Three separate links were dead, and none was visible from a unit test.**
  `DispatchEnv` carried no snapshot (OT.4), org's manifest never asked for the
  `tree-sitter` capability (OT.4), and both gates handed over stale trees (OT.7).
  Every one was found by a test that pressed a key in a real editor; every
  existing seam test passed against all three, because they build the context
  they are testing.
- **The divergence the phase was for is real and is one thing:** the grammar
  knows what a line is *inside*. `dar`, `]]`, `<Tab>`, `<C-Space>`, a statistics
  cookie, `<leader>o|`, an agenda row, a capture target and a refile destination
  were each wrong for a headline, checkbox or table written inside a
  `#+BEGIN_SRC` block, and no care in a line matcher fixes any of them.

---

## Phase 2 slices — OC

| Slice | Description | Status |
|---|---|---|
| OC.1 | `EventEmitCtx` on the grammar store — finish a half-wired import | ✅ |
| OC.2 | `wake-every` / `cancel-wake` + `on-wake` on the event actor | ✅ |
| OC.3 | `ui.emit-segment` / `ui.clear-segment` — **this is ML.6** | 📝 |
| OC.4 | `host-services.local-utc-offset-seconds` | ✅ |
| OC.5 | `clock.rs` — the drawer primitive, tree-native | 📝 |
| OC.6 | The four actions, the session owner, the modeline segment | 📝 |
| OC.7 | `:clock-in` / `:clock-resume` capture keys | 📝 |
| OC.8 | Docs — design fragment, `doc/org.md`, ledger, site nav | 📝 |

### OC.1 — a grammar action can reach the bus ✅ (2026-08-28)

**Landed as specced, and the two-line estimate was right** — but the *placement*
matters more than the size, and the plan did not say where. The wiring goes
**before** `call_register_grammar`, not after the registration drain where the
id happened to be allocated: a guest may `register-event` from inside
`register-grammar`, and `spawn_event_plugin` already orders it that way with a
comment saying so. So `alloc_id()` moved up to sit beside the store setup and
the context is stamped there, matching the events path line for line.

**Verified by mutation**, because a passing test proves nothing here on its own:
removing the two lines fails the new test with `Err(Empty)` — the emit went
nowhere — and restoring them passes it. That is the check the slice is for.

Two doc comments were stale and are corrected: `PluginState::event_emit` said
`Some` "only for a plugin spawned onto a bus", which is now two paths, and its
"the host isn't boot-wired into the `Editor`, so this slice is validation-only"
clause has not been true for some time.

**Original plan text follows.**

`EventEmitCtx` is populated in exactly one place, `event_task.rs:249` — the
events store. `host-services` including `emit-event` is nonetheless linked into
the grammar store (`lib.rs:2470`), so a grammar action calling `emit-event`
today takes the `None` arm of `emit_event` (`lib.rs:1001`) and gets a
**warn-and-drop**.

That is the `plugin-gates-hand-guests-throwaway-contexts` shape: a seam that
looks available and silently is not. It is a defect independent of clocking —
any plugin bridging a chord to its own async side hits it — and it is a real
gap that the fix is two lines wide, at `grammar_trampoline.rs:589`, which
already takes `bus: &Arc<EventBus>` for the `Quarantine`.

**Test:** a fixture grammar action emits an event; a native subscriber receives
it. This is the test whose absence let the gap survive.

### OC.2 — a plugin can ask to be woken ✅ (2026-08-28)

**The plan's shape was right and its arithmetic was one input short.** "A wake
rides that existing channel as a second `Delivery` variant" reads as the cheap
option, and it is not the cheap option — putting a wake on the bus channel means
something must hold a sender, and a store-held sender never closes, so the
actor's "end when the last subscription is pruned" property dies for every event
plugin whether it arms a wake or not. **A wake is a future on the actor's own
`FuturesUnordered` instead**, selected against the channel. No sender, no
lifecycle inversion, and the actor's exit condition becomes the honest one:
closed channel **and** nothing armed.

That last clause is a real behaviour change, not bookkeeping. A plugin that
subscribes to nothing and only arms a wake is exactly org's clock between
clock-in and clock-out, and under the old `while let Some(..)` its actor exits
before the first tick. `an_actor_with_a_live_wake_outlives_its_last_subscription`
is that case.

**The timer had to be injected, and this was the slice's one genuine fork.**
`lattice-plugin-host` states twice in its own `Cargo.toml` that it owns no
runtime — `tokio` is a dev-dependency and `futures` was picked over `tokio::sync`
to keep it so. `tokio::time::sleep` inside `wake-every` would have ended that for
one function. Two options were weighed: a host-owned scheduler thread (mirrors
the in-crate `EpochTicker`) versus a `Sleeper` trait object the loader supplies,
with the sleeps living on the actor. The second won on **heuristic #1** — the
wake is a general primitive (`design.md` Appendix B wants it for idle hooks), and
scoping each one to the plugin's actor means cancellation, budget and teardown
are inherited rather than re-implemented. It also spends no thread and needs no
cross-thread hop. Unwired, `wake-every` answers `0`, which is the same honest
degradation `event_emit` and `config_registry` use.

**Blast radius the plan did not list:** every world that declares its own event
exports must now declare `on-wake` too, because a component is instantiated
against the `events-plugin` bindings and those name every export. That is
`init-fixture.wit` + its guest, and `lattice-cli`'s `init` scaffold — the same
scaffold-drift class OT.1 found, caught here before it shipped.

**Also spec'd here, not in the plan:** a `MIN_WAKE_MS` floor of 50 ms. `ms` is a
`u32` the guest chooses, and `wake-every(0)` is a request to re-enter the guest
as fast as the executor allows — a busy loop that would starve the plugin's own
event deliveries, since they share the actor. Clamped and logged rather than
refused, so a slightly-too-eager plugin still works.

**Tests:** four in `wake_seam.rs`, each written the way the seam fails —
**nothing is published on the bus in any of them**, so a wake that only arrives
alongside some other delivery reads as an empty log rather than as a pass. Real
clock, not a paused one: the bug is "nothing ever wakes me", and a fake clock the
test advances by hand is precisely the shape that passes on a broken host.
Verified by mutation twice — making `arm_pending` a no-op fails three of the four
(the fourth is the no-timer-wired arm, which correctly still passes), and making
`cancel-wake` not remove the entry fails the cancel assertion with ten ticks
where three were expected.

**Original plan text follows.**

```wit
wake-every:  func(ms: u32) -> wake-id;
cancel-wake: func(id: wake-id);
```
plus an `on-wake(id)` export on the `events-plugin` world.

`EventActor::run` (`event_task.rs:99`) is a single
`while let Some(delivery) = self.rx.next().await` drain, so a wake rides that
**existing** channel as a second `Delivery` variant. No task-per-plugin, no new
concurrency, and the actor's budget + quarantine handling apply unchanged.

Cancelled en masse on deactivate / quarantine, like subscriptions (`events.wit`
has no `unsubscribe` for the same reason).

**Test:** an armed wake fires without any keystroke; `cancel-wake` stops it;
teardown cancels a live wake; a trapping `on-wake` quarantines without
wedging the actor.

### OC.3 — a plugin can push a modeline segment (ML.6) 📝

**Design:** `modeline.md` §6 (plugin row). **This slice closes ML.6**, which
`modeline.md`'s plan carries as ⛔ deferred-to-the-plugin-phase and which keeps
that plan active.

`wit/ui.wit` is an empty `interface ui {}` today; `wit/types.wit:1615` already
mirrors `ui-zone` and `ui-segment` for the ABI freeze, deferring the emit
producer until "a real plugin that needs more". Clocking is that plugin.

Wired on the **async linker only**, so the modeline is structurally
unreachable from the keystroke path. Publishes an `ElementContent` on the event
bus exactly as `lattice-lsp::modeline` does; the §12 wake forwarder repaints
off-keystroke. Element descriptors are registered by the plugin (zone,
priority), so ownership stays with the mode (**mode-owns-its-surface**).

**Parity:** TUI and GPUI both, same patch, per the cross-renderer rule — though
a plugin element renders through the same path a native one already does, so
the audit is expected to find nothing to change. Confirm rather than assume.

### OC.4 — local wall-clock time ✅ (2026-08-28)

**The plan named the signature and the rejected alternative; what it did not
name is that this is the workspace's FIRST timezone dependency, and that the
absence was deliberate.** `lattice-agent/src/log/ai_log.rs:97` says it in as many
words: it renders UTC because "rendering local time would need a timezone
dependency neither logger carries." That was a good trade for a log line, where
UTC is merely unfamiliar. It is not the same trade for a `CLOCK:` line, which org
*defines* as local wall-clock time — there, UTC is a wrong number written into
the user's file.

`chrono`, `default-features = false, features = ["clock"]`, used for exactly
`Local::now().offset().local_minus_utc()`. **The `time` crate was the obvious
alternative and does not work here:** its `local-offset` deliberately refuses in
a multi-threaded process (soundness of `localtime_r`) unless `unsound_local_offset`
is enabled, so in this editor it would answer UTC every time — a silent wrong
answer, which is worse than no seam. Chrono resolves the zone through
`iana-time-zone`, already in the graph via `wasmtime-wasi`, plus its own TZif
reader.

**Resolved per call, never cached** — and that is not a micro-decision. The
offset changes at a DST boundary and when the user changes their system zone, so
a cache would make an editor left open overnight write clock lines an hour wrong
for the rest of the session: the exact bug class the seam exists to prevent,
reintroduced as an optimisation of a call org makes twice a minute.

**Ungated, unlike its `walk` / `read-file` neighbours.** It names no path and
reaches no resource, and gating it would mean a plugin with no filesystem grant
renders every timestamp in the wrong zone. The test instantiates with
`CapabilitySet::empty()` to pin that.

**Test honesty, stated rather than papered over.** The strong assertion — guest
value equals the host's own `chrono::Local` answer — is *degenerate on a machine
configured to UTC*, where both sides are `0` and a stubbed seam would pass. The
module doc says so. Pinning `TZ` to fix it would mean mutating process-global env
to test a function whose whole job is reading process-global env. The
shape-assertions (a real offset is whole minutes, inside the range zones occupy)
hold everywhere, and the guest also echoes its own `wasi:clocks` reading so the
test can prove the two are independent sources rather than one value copied
twice. Mutation-verified: stubbing the host to `0` fails it here (+05:30).

**Original plan text follows.**

```wit
local-utc-offset-seconds: func() -> s32;
```

`CLOCK: [2026-08-28 Fri 16:02]` is local time. The guest has only
`wasi:clocks` (UTC) — `today_epoch_day()` in `lib.rs:1086` is UTC-derived — and
the host builds `WasiCtxBuilder::new()` with no environment inheritance, so
there is no `TZ` either. Without this every clock line is wrong by the user's
offset.

Offset **at the current instant**, so DST is correct. Rejected: an
`org.utc-offset` option — it makes the user maintain what the OS knows and
breaks twice a year.

Also corrects `%U` / `%T` / `%t` in capture and the agenda's "today" anchor,
which are UTC-derived today and wrong near midnight.

### OC.5 — `clock.rs`, the drawer primitive 📝

**Deps:** OT.4 (entry location), OC.4 (local time).

The plugin has **no drawer support at all** — no `:PROPERTIES:`, no
`:LOGBOOK:`, no `:END:` anywhere in the crate. The nearest thing is
`agenda.rs:301`, which knows only that an *inactive* timestamp is never a row;
that is what keeps logbook and `CLOSED:` lines out of the agenda, and it never
parses a drawer. So this is the plugin's first drawer primitive, and
`:PROPERTIES:` handling and a future `org-log-into-drawer` both reuse it.

Locate / create / insert per **D5**, over `(section)`'s `plan` and
`property_drawer` fields and the `(drawer)` node:

```scheme
(section (drawer (name) @n) @logbook (#eq? @n "LOGBOOK"))
```

Plus `H:MM` duration arithmetic — integer, no `chrono`, following
`timestamp.rs`'s house style (OT.5).

**Test:** insert into an existing drawer; create one where none exists; skip a
plan line; skip a `:PROPERTIES:` drawer; refuse above the first headline;
find the open `CLOCK:` line after a simulated restart with no session state
(**D4**).

### OC.6 — the four actions and the session 📝

**Deps:** OC.1, OC.2, OC.3, OC.5.

| chord | action |
|---|---|
| `<leader>oi` | `org-clock-in` |
| `<leader>oO` | `org-clock-out` |
| `<leader>oq` | `org-clock-cancel` |
| `<leader>oj` | `org-clock-goto` |

Bound at `KeymapLayer::MinorMode`, under org's existing `<leader>o` prefix,
in the mode's own crate — never `Builtin`.

Flow per **D6**: the grammar action does the buffer edit and emits
`org/clock-started`; org's event actor records the session, publishes the first
segment **immediately** (so there is no up-to-60s delay before `◷ 0:00`), and
arms `wake-every(60_000)`; `on-wake` formats and re-pushes. `clock-out` /
`clock-cancel` mirror it into `cancel-wake` + `ui.clear-segment`.

Segment: `Right` zone, `◷ 0:14 Write the clocking slice…`. `◷` is U+25F7,
Geometric Shapes — the BMP fallback palette — with a Nerd Font clock at the
same cell width when `ui.nerd_fonts=on`, per the icon-degradation rule.

`clock-out` operates on the current buffer's entry; `clock-goto` is how you get
there. It does not reach into a closed file — which keeps the whole feature
inside tree-land and is a consequence of the OT phase, not a limitation of it.

**Test (the way it fails):** assert the segment advances **without dispatching
any action** — a test that presses a key first passes on the broken version
too. Assert clock-out works on a fresh session with no recorded clock (**D4**).

### OC.7 — the capture keys 📝

**Deps:** OC.6.

`:clock-in` and `:clock-resume` as `org.capture-templates` keys — the two
`org-capture.md` §3 lists as cut, with "clocking is not built" as the reason.
The reason expires here.

### OC.8 — docs 📝

`org-mode.md` §9 moves clocking out of "Deferred with a reason" into "In", and
gains the tree-sitter statement as a design position rather than an
implementation detail. `org-capture.md` §3 drops the two cut rows. The plugin's
`doc/org.md` and `README.md` seam table gain clocking. `implementation.md`
ledger row; `modeline.md`'s plan marks ML.6 ✅ and becomes archivable if
nothing else in it is open. Site nav + sync per `docs-land-on-the-zola-site-too`.

---

## Sequence

```
OT.1 ─┬─► OT.4 ─► OT.6, OT.7          (headline first: motions/objects ride it)
      │
OT.2 ─┼─► OT.3  (agenda: the bug + the bench)
      └─► OT.8  (capture + refile)
          OT.5  (independent)

          OC.1, OC.2, OC.3, OC.4   (host, independent of each other)
                    │
          OT.4 ─────┴─► OC.5 ─► OC.6 ─► OC.7 ─► OC.8
```

Phase 1's host slices (OT.1, OT.2) gate everything in Phase 1. Phase 2's host
slices (OC.1–OC.4) are mutually independent and can land in any order, or in
parallel with Phase 1's guest migrations. OC.5 is the first slice needing both
phases.

**One slice, one commit**, each fmt-clean, warning-clean and green via
`scripts/precommit.sh <touched-crate>...` before committing. Every non-trivial
slice ships the four artefacts: doc, bench where the path is measurable, tests
covering the failure modes and not only the happy path, and graceful
degradation.

---

## Deliberate cuts

- **No clock-persist state file** (D4). The drawer is the record.
- **No `org-clock-into-drawer` nil / numeric variants.** Always `LOGBOOK`. An
  `org.clock-into-drawer` option can follow if it is missed.
- **No clocktable / `org-clock-report`.** It needs a dynamic-block mechanism
  the plugin does not have; it is a feature, not a gap in this one.
- **No `org-clock-idle-time`, no `org-clock-out-when-done`.**
- **`restar` / `shift_headlines` / `toggle_heading` stay text rewrites** (OT.4).
  Locate with the tree, edit as text.
- **Civil-date arithmetic stays hand-written** (OT.5). It is not parsing.

---

## Cross-references

- Seam contracts: `plugin-treesitter-seam.md` (TS.1–TS.3, all ✅),
  `agenda-source.wit`, `grammar.wit`, `events.wit`, `ui.wit`, `types.wit`.
- Modeline: `modeline.md` §6 + [`modeline.md`](modeline.md) (ML.6 ⛔ → OC.3).
- Wake discipline: `boot-composition.md` §3 and
  `lsp-architecture.md` §12 (the off-keystroke repaint path OC.3 reuses).
- Precedents for a mode-owned modeline element: `lattice-lsp::modeline`,
  `lattice-ai::mcp::status` (`spawn_status_publisher` — wakes on a `Notify`
  **or a deadline**, republishes only on change).
- Cross-file writes: `cross-file-writes.md` (the "host primitive, generic,
  names no org concept" test OT.2 / OC.1–OC.4 are each held to).
