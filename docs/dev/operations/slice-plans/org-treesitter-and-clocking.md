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

**Status:** 📝 planned (2026-08-28). Specced, not started.

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
grammar it never consults, and it does not:

> `agenda.rs:111` — "The planning line is the one immediately below the
> headline, and ONLY that one."

Anything between a headline and its `SCHEDULED:` line — a `:PROPERTIES:`
drawer, a blank line, an edit by an external tool or an agent — silently drops
that row from the agenda. The grammar has `field('plan', $.plan)` on the
`section` node and would not care. **This is the divergence class the phase
exists to end**, and it is live today. Files increasingly change under tools
we do not control, which is exactly when a bespoke parser's assumptions rot
without announcing themselves.

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

**D2 — Off-buffer parsing is a missing primitive, not a structural barrier.**
`lattice-syntax/src/syntax.rs:347` is `pub fn parse(&mut self, source: &str)`;
the host can parse arbitrary text with any registered language. An earlier
draft of this plan recorded "no tree off-buffer" as a carve-out for
`agenda.rs`. That was wrong — it stated an absent feature as a constraint —
and OT.2 supplies the primitive instead. **Heuristic #1:** the carve-out was
risk-aversion wearing the costume of a constraint.

**D3 — The agenda's bulk text copy is circular, and dies with the migration.**
`agenda-source.wit` justifies passing whole file text as: "a scan reads EVERY
line, so a handle would cost one boundary crossing per line where one copy
costs one." That holds only *because* the guest hand-parses. With a
host-side parse and `run-query-ranges` returning ranges, neither the text nor
per-line crossings happen. Today `scan(path, text)` copies every project org
file's full text across the WASM boundary — the exact bulk crossing the
`document` handle exists to prevent everywhere else. **Paramount #1**, via
boundary traffic. Note honestly: raw tree-sitter parsing is *slower* than a
line-prefix scan, so the net is not assumed — OT.3 benches it, and the agenda
scan is a producer critical path.

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
| OT.3 | `agenda-source.scan` takes a tree; `agenda.rs` migrates | 📝 |
| OT.4 | `headline.rs` → `(section)` / `(headline)` / `(stars)` | 📝 |
| OT.5 | `timestamp.rs` → `(timestamp)` | 📝 |
| OT.6 | `checkbox.rs` → `(listitem)` / `(checkbox)` | 📝 |
| OT.7 | `table.rs` → `(table)` / `(row)` / `(cell)` | 📝 |
| OT.8 | capture target + refile picker → `parse-file` | 📝 |

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

`scan` currently takes `path: string, text: string`. Both shapes must survive:
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
prefix-matching the line below the headline, **which fixes the `agenda.rs:111`
bug named in Why**. `civil_from_epoch_day`, `sort_key`, `group_key` and
`group_label` are date arithmetic, not parsing, and stay.

**Bench (required, D3):** agenda scan over a synthetic project — wall-clock
and bytes-crossed, before and after. The crossing win is structural; the parse
cost is a regression that must be shown to be paid for. Lands in
`benchmarks.md`.

**Test:** a headline whose `SCHEDULED:` is separated from it by a
`:PROPERTIES:` drawer produces a row (fails on today's code); a malformed file
skips with a `debug` log and the scan continues.

### OT.4 — `headline.rs` on the tree 📝

**Deps:** OT.1.

The big one. `headline_level`, `enclosing_headline`, `subtree_end`,
`next_headline`, `prev_headline`, `parent_headline`, `prev_sibling`,
`next_sibling` become tree walks over `(section)` / `(headline)` / `(stars)`.
`folds.scm` already documents why `(section)` is the load-bearing node: "a
section is a headline plus everything beneath it *including nested sections*",
which is `subtree_end` by construction.

`restar` / `shift_headlines` / `toggle_heading` **stay text rewrites** — the
tree locates the `stars` node, the edit is still an edit. Locating with the
tree and rewriting text is the correct split, not a half-migration.

Every motion, text object and promote/demote action rides this, so its test
surface is the widest in the phase.

### OT.5 — `timestamp.rs` on the tree 📝

`stamp_at` and `first_stamp` become `(timestamp)` lookups. The **civil-date
arithmetic stays**: `weekday` (Zeller) and `epoch_day` (Hinnant) are integer
math, not parsing, and `timestamp.rs:76` records the deliberate reason — a
`chrono` dependency tree in a wasm guest to replace twenty lines of arithmetic.
Nothing about tree-sitter changes that.

### OT.6 — `checkbox.rs` on the tree 📝

`(listitem)` / `(checkbox)`. Statistics cookies (`[2/5]`) are computed from the
sibling listitems the tree yields rather than from a line scan, which is also
what makes a cookie correct when a nested list sits between siblings.

### OT.7 — `table.rs` on the tree 📝

`(table)` / `(row)` / `(cell)`. Cell and row motion, row/column insert and
move. Alignment stays a rendering concern computed from cell extents.

### OT.8 — capture and refile stop hand-parsing 📝

**Deps:** OT.2.

Two remaining off-buffer parsers, same bug class as the agenda:

- capture's `file+headline` target reads through `host-services.read-file` and
  finds the headline to compute an insertion line (`org-capture.md` §4).
- refile's picker source walks project org files listing every headline.

Both move to `parse-file`. Refile's picker can then stop reading files through
WASI entirely. `headline::subtree_end` is shared between them and must stay
shared — `org-capture.md` §4 makes the point that the two computations "cannot
drift apart", and that is more true, not less, once it is a tree walk.

---

## Phase 2 slices — OC

| Slice | Description | Status |
|---|---|---|
| OC.1 | `EventEmitCtx` on the grammar store — finish a half-wired import | 📝 |
| OC.2 | `wake-every` / `cancel-wake` + `on-wake` on the event actor | 📝 |
| OC.3 | `ui.emit-segment` / `ui.clear-segment` — **this is ML.6** | 📝 |
| OC.4 | `host-services.local-utc-offset-seconds` | 📝 |
| OC.5 | `clock.rs` — the drawer primitive, tree-native | 📝 |
| OC.6 | The four actions, the session owner, the modeline segment | 📝 |
| OC.7 | `:clock-in` / `:clock-resume` capture keys | 📝 |
| OC.8 | Docs — design fragment, `doc/org.md`, ledger, site nav | 📝 |

### OC.1 — a grammar action can reach the bus 📝

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

### OC.2 — a plugin can ask to be woken 📝

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

### OC.4 — local wall-clock time 📝

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
