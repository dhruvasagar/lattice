# A note is an id, not a path — slice plan (OR)

> Design: [`../../architecture/org-roam.md`](../../architecture/org-roam.md).
> Depends on [`archive/conceal-and-org-links.md`](archive/conceal-and-org-links.md) phase OL —
> specifically OL.1, which makes `id:` a recognised link kind that OR.8 then
> teaches to resolve.
>
> Also anchors [`../../architecture/org-capture.md`](../../architecture/org-capture.md)
> (OR.11 extends its placeholder set),
> [`../../architecture/multibuffer-views.md`](../../architecture/multibuffer-views.md)
> §3.7 (OR.9's view is a provider, like the agenda).
>
> Plugin repo: [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin).
> Its `wit/` is generated from `lattice-wit` (WT.2), so the three
> `host-services` additions here reach it by regeneration.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** 🚧 in progress (2026-08-30) — OR.1 and OR.2 landed.

---

## Why

Roam is three verbs — find a note, link to one, follow one — over a corpus whose
unit of identity is an **id rather than a path**. Titles change, files get
renamed, content moves; the links survive because they never named the file.
That is the value and it is the entire cost: every operation needs a map from id
to location, and that map exists in no single file.

The corpus this is built for was measured rather than assumed
(`~/src/dhruvasagar/org-files/roam/`): 706 files, 585 nodes (475 file-level,
**110 headline-level**), 795 `[[id:…]]` links, 71 aliases, 428 `#+filetags:`
lines, 11 capture templates. Three of those numbers changed the design — see
`org-roam.md` §1 — and the one that would have been guessed most wrongly is the
headline count: a file-only node model drops 19% of the corpus and every link
into it, silently.

### The constraint that reshapes the obvious design

**There is no single org guest instance.** `spawn_event_plugin`,
`spawn_config_plugin`, `spawn_help_plugin`, `spawn_dashboard_sections` and
`instantiate_grammar_plugin` are separate paths, each with its own
`wasmtime::Store`. The picker seam running find-node and the sync grammar seam
running `<CR>` are different instances with different memory.

So "keep the index in guest state" does not mean what it appears to. It means
*N copies, drifting* — and the drift is invisible, because each instance stays
internally consistent while find-node offers a note `<CR>` cannot open. Every
structural decision below is designing around that rather than discovering it
three slices in.

---

## Decisions locked before slicing

1. **One indexer, many readers.** The async event seam is the sole writer; every
   other seam reads. This is what makes denormalization safe — the same node
   lands in the all-nodes blob, under its own key, and in its targets' backlink
   lists, written in one pass by one instance. Multi-writer would need a
   transaction or a reconciliation pass, and would get one of them wrong.

2. **The host stores bytes under strings and never interprets either.** The key
   layout in `org-roam.md` §4.2 is the guest's schema. A host-side node index
   would be marginally faster and would make the host learn what an `:ID:`, a
   `:ROAM_ALIASES:` and a backlink are — the exact knowledge `org-mode.md` §2
   exists to show it never needs.

3. **A watcher, not a save hook.** The corpus is edited from outside lattice:
   emacs writes a note, a `git pull` lands twenty. A save hook observes none of
   those, and the symptom — a picker missing notes you know you wrote — reads as
   data loss.

4. **Ids are minted host-side.** `:org-roam-id-create` is a *grammar action*,
   and `read-file`'s doc comment already records that the grammar seam's
   synchronous linker cannot serve WASI. A guest-side UUID would work on the
   picker path and panic on the grammar path — correct in any test that builds
   its own context, broken in the editor.

5. **The create row is pinned last and always present.** Present whenever the
   query is non-empty, because you must be able to create *Rust* while *Rust
   Async* exists. Pinned last, because a create row that could rank above a real
   match would let `<CR>` produce a duplicate through ranking noise, and that is
   destructive rather than merely wrong.

---

## Not in this plan

**Markdown links, and any second consumer of conceal.** Phase H is general;
proving it against a second language is its own question.

**`org-roam-db` compatibility.** We index the same files emacs does, not
emacs's SQLite cache — see `org-roam.md` §10 for why sharing files beats sharing
a schema version.

**Graph view and unlinked references.** Both cut in the design with reasons;
unlinked references additionally wants a term map the index does not carry.

---

## Slices

| Slice | Description | Status |
|---|---|---|
| OR.1 | `host-services` `store-*` — durable, plugin-scoped, opaque bytes | ✅ |
| OR.2 | `host-services.watch` / `unwatch` — a debounced directory watch | ✅ |
| OR.3 | `host-services.new-uuid` | ✅ |
| OR.4 | `org.roam-directory` and the indexer | ✅ |
| OR.4a | setting the directory builds the index (no manual sync) | ✅ |
| OR.4b | the cold scan is carried across calls, with progress | ✅ |
| OR.5 | the picker offers to create what it could not find | ✅ |
| OR.5b | one component may register N picker sources | ✅ |
| OR.6 | `:org-roam-find-node` | ✅ |
| OR.7 | `:org-roam-insert-node` — completion inside `[[…` | ✅ |
| OR.7c | `:org-roam-insert-node` as a picker | ⛔ |
| OR.8 | `id:` resolves — `<CR>` jumps, `:org-roam-id-create` mints | ✅ |
| OR.9 | the backlinks view | ✅ |
| OR.10 | dailies | ✅ |
| OR.11 | roam capture templates and `${field}` | 📝 |
| OR.12 | docs | 📝 |

### OR.1 — a plugin can persist something ✅

**Deps:** none.

The store from `org-roam.md` §4.2: `store-put` / `store-get` / `store-delete` /
`store-keys(prefix)` / `store-generation`. Scoped to the plugin's data dir **by
manifest id**, so every seam instance of one plugin sees one store and two
plugins cannot collide.

**Landed as five functions on `host-services`, not a `store` interface of its
own.** A component's import set is fixed for the whole artefact and must resolve
on every linker it is instantiated against, including the grammar seam's sync
one; a new interface is a new import each world declares and both linkers wire,
and a miss there fails the WHOLE component rather than degrading one seam —
which is the OC.2 scar exactly. `host-services` is already imported everywhere a
store is wanted and already wired on both linkers. Design fragment §4.2 amended
in place.

**Scoping by manifest id rather than by instance is the slice's whole point**,
and it is the thing to assert first: the test that matters spawns two instances
of the same plugin through different paths and shows a `put` from one is visible
to a `get` from the other. Without that, this is just a file API.

Keys are guest-chosen strings, **not paths**. There is no traversal to defend
against because nothing derives a path from a key — the store hashes or encodes
them into its own layout. Saying so here prevents the reflex to add a path
sanitiser that would then have to be kept correct.

**The failure policy is `agenda_cache.rs`'s, promoted from an agenda special
case to a primitive**: temp-file-and-rename so a kill mid-write leaves the
previous state intact, a schema version that refuses an older shape, a size cap,
flush-on-drop, and degradation to *empty* on any corruption. Never to a partial
read — a cache serving bytes that failed a schema check is how one starts
serving plausible nonsense.

Capability: a new `state:write` grant. A plugin without it gets `err` on `put`
and `none` on `get`, logged once, and keeps working — the honest "no store
wired" degradation, matching how `config_registry` and `event_emit` behave when
unwired.

**Bench (required):** `put`/`get` round-trip for a 90 KB blob (the size roam's
`nodes` key reaches on the reference corpus) and for a 200-byte record; `keys`
over 1000 entries. These bound OR.4 and OR.6.

**Tests:** two instances of one plugin sharing a store; two plugins isolated
from each other; survival across a simulated restart; `Drop` persisting without
an explicit flush; corrupt bytes recovering to empty rather than erroring;
a schema bump refusing the old shape; the size cap clearing wholesale;
`generation` moving on mutation and **not** moving on a `get`; a plugin without
the grant degrading rather than failing.

### OR.2 — a plugin can be told a file changed ✅

**Deps:** none (parallel with OR.1).

`watch(path)` / `unwatch(path)`, debounced host-side, emitting an event carrying
the changed paths. Gated on the same `fs:read` grant `walk` and `read-file`
already check, so the authorization is one existing line rather than a new
policy.

`notify` is already a host dependency driving autoread, so the mechanism is
wiring. The design work is the debounce window and the event shape: a `git pull`
that rewrites 200 files must produce a bounded number of events carrying many
paths, not 200 events.

**The wake is the part that silently does not work if missed.** A watcher event
that lands with no wake sits until the user happens to press a key, and the
symptom — "it updates, but only after I hit something" — reads as a rendering
bug. The **test asserts the guest observed the change without any action being
dispatched afterwards.** A test that presses a key first passes against the
broken version, which is the hole `test_helpers::settle` exists for.

As built, the guest side needs no `wake_on_event`: delivery rides the plugin's
own event actor, which is a live tokio task draining an mpsc, so a batch reaches
`on-event` on its own. `wake_on_event` becomes relevant at OR.9, where the
*view* has to repaint — the guest hearing about a change and the screen showing
it are two different questions, and only the second needs the editor woken.

**The event is addressed, which is the design decision this slice added.**
`Event::FilesChanged` carries the host-issued plugin id and the delivery actor
drops a batch that is not its own. The alternative — riding `Event::Plugin`,
which needs no new native surface — leaks: every `EventKind::Plugin` subscriber
sees every plugin event, so a plugin with no grant over a directory would learn
which files under it changed.

**Tests:** a single write observed; a burst of 200 writes coalescing into a
bounded event count *and* into batches that carry many paths (collapsing the
count without keeping the breadth would be coalescing that lost data); the guest
observing without an intervening keypress; `unwatch` stopping delivery; a denied
path refused with the grant named; a granted plugin's SECOND, ungranted watch
still refused (a gate that armed once and stopped checking passes every other
test here); the refused plugin still delivering ordinary events; a deleted file
reported as a change (OR.4 needs deletions).

### OR.3 — the host mints ids ✅

**Deps:** none.

`new-uuid() -> result<string, string>`, uppercase v4.

**A `result` rather than the bare `string` the design sketched.** Every other
call on this seam degrades to a value (`0` from `wake-every`, `0` from
`local-utc-offset-seconds`) because a legible wrong answer beats a fabricated
one — but those are READ. An id is WRITTEN, into the user's file, as an `:ID:`
that outlives the session; a guest handed an empty string on entropy failure
would write an empty drawer and nothing would ever say so. One `match` at the
call site buys that being impossible. Not a panic either: a host function
unwinding through wasm frames aborts the process. Design fragment §5.2 amended.

One function, and the reasoning is `read-file`'s verbatim: `:org-roam-id-create`
runs on the grammar seam's synchronous linker, which cannot serve WASI. A
guest-side UUID would work on the async picker path and take the plugin down on
the grammar path. Uppercase because the reference corpus is uppercase throughout
(macOS `uuidgen`, which `org-id` shells out to) — and OR.4 compares ids
**case-insensitively** regardless, because a link that fails to resolve over
letter case looks exactly like a missing note.

**Tests:** well-formed v4 (group widths, version nibble, RFC 4122 variant
nibble); uppercase; two mints in ONE call differing (a constant satisfies every
shape assertion a single id could carry); a 500-id batch with no duplicates AND
with the LEADING group varying (a per-call reseed would pin it while leaving the
tail random); ungated — no capability requested; callable from the grammar
(sync) linker specifically — the whole reason it exists, and the one a test that
calls it from the async path would not catch.

### OR.4 — the index ✅

**Deps:** OR.1, OR.2.

`org.roam-directory` (unset by default), the walk, the parse, the extraction,
and the four key families in `org-roam.md` §4.2. The async event seam is the
sole writer.

**Structure from the tree, characters from the text** — `agenda.rs`'s rule, and
OR.4 needed it stated sharply because a real parse does not say what
`grammar.js` reads like. Both facts below were established by dumping a real
tree *before* writing the extraction, and both would have been guessed wrong:

- **A file-level `:PROPERTIES:` drawer is not a `property_drawer`.** The grammar
  attaches `property_drawer` to a `section`, so a drawer above the first
  headline parses as a generic `drawer` in the document `body` — and its
  innards are not `property` nodes but an undifferentiated run of `expr`s in
  which `:ID:` and its value are separate untyped siblings. The two grains
  therefore need different readers, and the file grain reads its properties from
  the *lines* the drawer spans. Since 81% of the reference corpus is
  file-level, a shared reader written from the grammar source would have dropped
  four notes in five and said nothing.
- **`[[id:…]]` links are not reliably one node.** In the same dump one link
  survived whole as a single `expr` and the next was split across two, because
  org breaks a paragraph on whitespace and a description contains spaces. Link
  extraction is textual; the tree's job is deciding **which node owns** the line
  a link sits on, which is the structural question a text scan cannot answer.

The tree still earns its keep on exactly the thing it is for: an `:ID:` written
inside a `#+BEGIN_SRC` example is example text, not a phantom node — the same
class of bug OT.3's `a_headline_inside_a_source_block_is_not_a_row` pins for the
agenda, and pinned here too.

**The code is split so the rules are testable.** A `tree-snapshot` is a WIT
resource with no constructor, so anything reached through one is unreachable
from a host-side unit test. `roam.rs` is a pure function of `(path, text,
outline)` with 16 unit tests; `roam_tree.rs` harvests the outline and is covered
by the integration tests that run a real editor.

**Tag inheritance resolves here, not at query time.** A headline node's tags are
its own plus its ancestors' plus the file's, flattened into the record. Deferring
it would put a tree walk inside the picker's per-keystroke filter loop.

**`f/<path>` is the row that makes deletion work**, and it is the case a
cache-shaped design forgets. It holds the file's content hash *and the ids that
file produced*, so when a file changes the indexer knows which ids to retract.
Without it a node deleted from a file stays in the index forever and the picker
offers a destination that does not exist.

**Roam is inert when `org.roam-directory` is unset**: no walk, no watcher, no
store writes. An org user who keeps no zettelkasten pays nothing, and the test
for that asserts zero store writes and zero watchers after boot.

**Bench (required):** full cold index over a 700-file corpus; warm reindex with
every hash unchanged; single-file reindex (the watcher's common case). The cold
number is the one that justifies persistence at all, and it belongs in
`benchmarks.md` rather than in this paragraph.

**Tests:** a file node; a headline node; both in one file; tag inheritance
through two headline levels (including through an intermediate section that is
NOT itself a node); case-insensitive id matching; quoted aliases; an `:ID:`
inside a `#+BEGIN_SRC` block **not** becoming a node; a malformed property line
skipped rather than costing the node its id; a node deleted from a file
disappearing from `nodes`, from `n/<id>` and from every `b/<id>` that named it;
case-insensitive `#+TITLE:` / `#+title:` / `#+Filetags:`; a note written while
the editor runs reaching the index **with no key pressed**; roam inert with the
directory unset, asserted as *zero store writes* rather than an empty result.

**Two bugs the tests found, both of which would have shipped silently.**

The first was mine to make: reading the file grain as a `property_drawer` (see
above). The second was in OR.2 and only a deletion test could see it —
**`notify` reports canonical paths while `walk` reports the ones it was
given**, so on macOS the index was keyed under `/var/…` and looked up under
`/private/var/…`. Additions still worked (nothing to look up); changes and
deletions silently never retracted. The host now re-roots a watcher's paths onto
the spelling the watch was armed with, with its own regression test.

**Not yet done in this slice:** a duplicate `:ID:` across two files keeps the
first, but the `warn` naming both paths is missing — the guest cannot log
(`logging` is absent from the grammar linker and importing it fails the whole
component, the OC.2 scar), so surfacing it needs a host-side channel. Carried
into OR.12.

### OR.4a — setting the directory builds the index ✅

**Found in use, 2026-08-31.** `register_events` runs the boot walk at
plugin-load time; an `init.rs` sets `org.roam-directory` from a
`plugin-loaded` handler, which fires after. The walk read an unset option,
indexed nothing, and with nothing subscribed to `OptionChanged` never ran
again — so roam was silently inert for exactly the users who configured it the
documented way.

Every roam test called `:org-roam-sync` by hand (the harness documents that it
must, for the same ordering reason), so the suite passed against a product with
no automatic path at all. The test added with the fix syncs NOTHING.

### OR.4b — the cold scan does not trap ✅

**Found in use, 2026-08-31, on the reference corpus.** `sync_all` did the whole
cold walk in ONE guest call: 706 files × (read + hash + parse + store write) ≈
27 s, against an async-seam budget of ~1 s. The guest trapped on the epoch
deadline and the host quarantined the plugin for the session — which is why
`:org-roam-sync` said "re-scanning" and never finished, and why nothing org did
worked afterwards.

Not caught because **every** roam fixture is a four-file corpus. The bug is a
function of corpus SIZE, the one axis no fixture varied.

The scan now queues once and indexes for a bounded **time** per call (250 ms,
4× headroom), re-arming through `org/roam-scan-step` until drained. A time
budget rather than a file count because wall-clock is what runs out, and a count
mis-approximates it exactly where notes are large. A generation stamp on each
step stops a chain left over from a previous root. Progress shows in the
modeline.

**Tests:** a corpus of 120 files — several batches, so the CHAIN is what is
under test; a single-batch corpus would pass against the broken version too.
Verified separately against the real 706-file corpus: indexes in ~70 s under a
debug host, no trap.

### OR.5 — the picker offers to create what it could not find ✅

**Deps:** none (a host-side picker change, parallel with OR.1–OR.4).

`picker-source-spec.create-label: option<string>` and
`routing-payload::create(string)`. When set and the query is non-empty, the
picker appends one synthetic row carrying the query verbatim.

**Always present, pinned last** — both halves tested, because both are
load-bearing and neither is obvious. Present-whenever-non-empty, because
only-on-zero-matches makes it impossible to create *Rust* while *Rust Async*
exists. Pinned last and never ranked, because a create row that could sort above
a real match would let `<CR>` create a duplicate through ranking noise — a
destructive outcome from a scoring accident.

This is generic picker surface, not roam surface. Nothing here names a node.

**Tests:** the row absent on an empty query; present with matches; present with
no matches; always last regardless of query and candidate set — including the
hardest case, an EXACT match on the top candidate, where both rows are maximally
relevant by any scoring story and only the pin decides; retracting when the
query is backspaced empty; the query crossing verbatim including spaces and
non-ASCII, asserted both natively and across the WASM boundary; a source that
sets no label behaving exactly as today (the regression guard for every existing
picker); a plugin source's label crossing as declared; `accept` receiving
`create` and routing to the source.

**Landed shape.** The row is synthesised by the picker, not by the source: a
source declares `create_label` on its spec and `Picker::push_create_row` appends
it after `match_and_rank`, so every source gets the behaviour from one
declaration and none re-implements it. `RoutingPayload::Create { query }` is
held in its own slot rather than in the seat-time `routing_meta` sidecar,
because the payload is the LIVE query and the sidecar is written once per seat —
which is also why the create row carries its own `CandidateData::Extension`
kind (`PICKER_CREATE_KIND_ID`) rather than an index into it.

### OR.5b — one component may register N picker sources ✅

**Deps:** none. **Carved mid-build**, at OR.6, when the constraint surfaced.

`picker-source` was the only contribution seam in the system shaped "the
component **IS** one thing": it exported `spec()`, and the host registered
exactly one source per component. `language`, `grammar`, `config`, `modes`,
`theme`, `help` and `keymap` are all "the guest calls a host import to register
N things". That exception was not free — org needs three pickers (refile, roam
find-node, roam insert-node) and could register one.

So the seam now matches the rest. A new `picker-registry` import carries
`register-picker-source(spec)`; a world-level `register-picker-sources()` export
is the registration entry the host drives once; and `init` / `accept` take a
`source: string` so one actor and one guest instance serve them all.

**Chosen on merit over the two cheaper options.** Multiplexing on `args` needed
no WIT change but collapses three unrelated specs into one doc and one
create-label, and leaves the next plugin with two pickers hitting the same wall.
A `specs() -> list<spec>` variant is smaller but keeps picker-source shaped
unlike every other seam. Neither fixes the asymmetry that caused this.

`picker-registry` is wired on **both** linkers. Org provides `grammar` and
`picker-source` from one artefact, and a component's import set must resolve on
every linker it is instantiated against — an import absent from one fails the
WHOLE component, not one seam. That is the OC.2 scar, and this is the fifth seam
wired on both for it.

**Tests:** the fixture registers TWO sources from one component; both specs come
back with their own `create_label`; and — the assertion the slice exists for —
the source id **routes**, with `init` and `accept` reaching different guest
bodies. Without that last one, "two specs came back" would be satisfied by a
guest that registered twice and answered identically, which is the version of
this feature that looks right and is useless.

### OR.6 — `:org-roam-find-node` ✅

**Deps:** OR.4, OR.5.

A `picker-source` over the `nodes` blob. Title as candidate text; aliases and
tags as annotation columns. Fuzzy matching stays **native** — matching in the
guest would put a WASM crossing on every keystroke of the query.

**The picker searches titles and aliases, never filenames**, and the corpus is
the argument: `20250603103551-chicken_breast_honey_garlic.org` holds a node
titled *Honey Garlic Chicken Breast*. The slug is a fossil of an earlier title.
Matching filenames would rank notes by what they used to be called.

Create uses a minimal built-in template here; user templates arrive at OR.11, so
the two land separately and a template bug cannot masquerade as a picker bug.

**Bench (required):** picker open with 585 nodes — the `get`, the deserialize
and the first ranked frame.

**Landed:** the source (`roam_find.rs`), its registration as org's SECOND
picker source, `:org-roam-find-node`, `:org-roam-create-node`, the slug and the
new-note body — with unit tests for the slug's shape (punctuation collapsing,
non-ASCII surviving) and, importantly, a **round-trip test**: what
`new_node_text` writes, `roam::extract` reads back as a node. A change to either
that breaks the other would make a created note unfindable by the picker that
created it, and nothing else in the suite would notice.

**Create routes through `invoke-command` into an ex-command**, not through the
picker outcome: creating a note mints an id and writes a file, and
`picker-accept-outcome` can express neither. That path runs on the grammar seam,
which is exactly why OR.3's `new-uuid` is host-side. `Effect::WriteToFile`
resolves the path to a buffer, so a new note is a live unsaved buffer — §5.2's
model, and why an abandoned draft never enters the index.

**`routing-payload::file-location` was added** for this. `picker-accept-outcome`
already had `jump-to-location(path, line, col)`; the routing side had no peer,
so a row standing for a position in a file could only carry `open-file`, which
drops the line. Headline nodes are 19% of the corpus and every one of them would
have landed at the top of its file. Distinct from `lsp-location`, which is the
same shape under a name that says where it came from.

**Integration tests landed — eleven of eleven.** Finding by title; by alias
(the query is an alias that appears nowhere in the title's leading words,
because an alias that is *displayed* but not *matched* leaves 12% of the corpus
unreachable); a note whose filename says one thing and whose title says another,
asserting BOTH that it is offered under its title and that its filename does not
match; the create row present alongside a real match and pinned last; roam inert
naming the option to set; the create row opening a draft; and the chord opening
find-node from a buffer that is not an org file.

**Three bugs these tests found.** `spawn_picker_source` never stamped the config
registry, so EVERY picker source got `none` from `get-option` and find-node
reported itself unconfigured for a corpus it had just indexed — the fifth seam
to need that line. An `Effect::Echo` after `Effect::WriteToFile` overwrites the
write's own failure message, so a refused write read as a successful one. And a
test that settles on `n/<id>` opens the picker against a half-written index:
that key lands DURING the batch, while the `nodes` blob the picker reads is
rebuilt once at the END of it.

**The two that closed last were both the harness, and both looked like the
product.** Worth recording because each survived several wrong fixes.

*The chord.* `<leader>onf` was written, appeared dead, and was reverted on the
principle that a silently-dead binding is worse than none. It had dispatched
correctly the whole time: the first instrumented run resolved it to
`Invoke(CommandId(org-roam-find-node))`. What was missing sat in
`index_corpus_with_editor` — the spawned grammar-row expansion and the
enablement drain that `org_structure.rs`'s harness has carried since OM.4b/OM.7,
without which a plugin minor registers, stays disabled, and reaches nothing.
Then a second layer behind it: the test helper dropped `Effect::OpenPicker`,
which is the renderer's to apply.

*The create draft.* Also never a product bug. The test looked the draft up
through `BufferStore::name_of` — the SYNTHETIC-name slot — and a path-backed
Document has none, so the predicate dropped every real file buffer including
the editor's own scratch. It would have failed against a working product, which
is exactly what it did for two slices.

The general lesson, and the reason both took so long: **an assertion that cannot
observe success is indistinguishable from a product that cannot produce it**,
and it makes every candidate fix look plausible. A patch to
`drain_pending_picker_accept`'s dropped `.effects` made the create test pass —
and reverting the patch left it passing, because `handle_effect` already applies
`Effect::WriteToFile` inline. Two host "fixes" were written and reverted across
OR.6/OR.7 on exactly that pattern; both were caught by removing the change and
re-running, never by re-reading it.

**Create remains a placeholder here by design** — a minimal built-in body with
no template choice, no `${field}` interpolation and no finalize/abort. That is
OR.11's, and the split is deliberate so a template bug cannot masquerade as a
picker bug.

**Binding:** `<leader>onf`, on the UNIVERSAL `org-global-mode` beside capture
and the agenda, for their reason — the note you want is rarely the file you are
in. `<leader>on…` mirrors emacs org-roam's `C-c n …`, so the `f` keeps its
meaning for anyone arriving from there. NOT `<C-x>n…`: org's major binds a
terminal `<C-x>` (timestamp decrement, OM.9), and a prefix in one layer against
a terminal binding in another is the ambiguity vim settles with `timeoutlen`,
which this editor does not have.

### OR.7 — inserting a link ✅

**Deps:** OR.6.

**Reshaped 2026-08-30, before implementation.** The original line was "the same
picker; on accept, insert `[[id:<ID>][<Title>]]` at the cursor" — a normal-mode
chord opening a picker. That is the wrong surface, and the reason is the moment
it happens: you insert a link **mid-sentence**, so a normal-mode chord makes you
leave insert, pick, and come back. Emacs does not do that either — you type
`[[` and completion offers nodes.

So insert-node is a **completion source**, not a picker:

- **The seam already exists.** `completion-source` (PH7.6) is generator-only by
  design: the guest produces candidates asynchronously and the host's native
  matcher / ranker / annotator handle the rest, because `matches` and `annotate`
  run per-candidate on the synchronous keystroke pipeline and crossing them
  would fire hundreds of boundary calls per keystroke. Org does not currently
  `provide` it; adding it is a manifest entry, not a new mechanism.
- **Prefix-gated, not always-on.** The source returns nothing unless the text
  before the cursor is inside a `[[…` link. That is emacs org-roam's own
  behaviour, and it is also what keeps a 585-node corpus out of every ordinary
  word completion.
- **The manual trigger is `<C-x><C-o>` (omni), not `<C-x><C-n>`.** Insert-mode
  `<C-x>` is vim's canonical completion-source prefix (`<C-x><C-f>` files,
  `<C-x><C-l>` lines, `<C-x><C-o>` omni) and — unlike normal mode — it is FREE
  here: org's terminal `<C-x>` (timestamp decrement) is bound in Normal only.
  `<C-x><C-n>` is taken by vim's own keyword completion, so it is not available.
  Omni is the right slot on meaning as well as availability: omni-completion is
  by definition the filetype's own, which is exactly what a roam link is in an
  org buffer. nvim-orgmode wires `omnifunc` for the same reason.
- **Create-on-no-match does not belong here.** Completion offers what exists;
  creating is OR.6's picker (and, after the capture rework below, a capture
  buffer). Trying to make a completion row create a note would put a file write
  behind a keystroke that is supposed to be cheap.


**What it actually took — three host gaps, all generic.** None of these are
org-specific and none are in `lattice-org-plugin`; the plugin owns the source
and nothing else. Each was found by a test that failed, not by reading:

(A fourth was suspected — that the loader-minted `<plugin>-completion-mode`
carrier is a `ModeKind::Minor` and so inert until enabled — and a publish was
written for it. Removing the publish left the tests green, so it was reverted:
the real cause of that symptom was a test that read `ActiveCompletionSources`
before the enablement and activation drains had run. Recorded because "a plugin
minor is inert until enabled" is a true rule that made a wrong diagnosis look
right.)

1. **The host never drove a WASM completion source at all.** PH7.6 shipped the
   WIT export, the actor, the carrier mode and the `AsyncCompletionSource`
   adapter, and the loader registered every one — but the only `produce_async`
   call site looked up `gen:lsp-completion` by id and returned early unless an
   LSP server was attached. `generate` had never been called in production.
   `do_lsp_insert_completion_request` is now
   `do_async_insert_completion_requests`: one task per source, each reporting
   independently, LSP's preconditions dropping **LSP** from a round rather than
   the round. The drain applies every pending outcome instead of the latest —
   with more than one sender, "latest wins" silently discarded whichever
   answered first.
2. **The guest could not tell whether it applied.** `generate-context` carried
   only `prefix` + `case-sensitive`, and the anchor scan stops at `[`, so `[[Ti`
   and a bare `Ti` reach the guest identically. It now carries
   `line-before-cursor` and `language`. The rejected alternative was a
   host-side `link-context` flag beside `path-context` — that is the host
   learning one plugin's syntax, and the next source would need the next flag.
3. **The popup closed on the first space.** `maybe_refresh_insert_completion_after_edit`
   dismissed as soon as the query held a non-word byte, so a node titled *Honey
   Garlic Chicken Breast* could never be narrowed to. A source declares
   `accepts-non-word-query` and the host ORs it across the round; identifier
   sources keep the old behaviour and simply stop matching.

Plus `raw-candidate.insert-text`, so a candidate can match on one string and
insert another. LSP and snippets each already needed this and each grew a
private hatch keyed to a host-owned `kind_id`, which a plugin cannot reach —
roam matches a title and inserts `[[id:…][…]]`.

**The one behaviour worth knowing.** The replacement region is `[anchor,
cursor]`, fixed at popup-open. A multi-word title therefore needs the popup
opened at the opener (`<C-Space>` right after `[[`), after which the query grows
across spaces from the fixed anchor. Single-word queries work from auto-trigger
anywhere in the link. When the anchor sits mid-title the source **declines** —
accepting there would splice the link over the last word and strand the rest.
Source-declared replacement bounds (emacs's capf model, where each function
returns its own START/END) would remove the caveat and is the honest fix if it
ever bites; it reshapes a popup-global field and was not worth it here.

### OR.7c — `:org-roam-insert-node` as a picker ⛔

**Deferred, deliberately.** Superseded by OR.7's completion source for the
common case. Revisit only if completion proves insufficient for inserting a
link — not on the assumption that it will.

**The insert is one edit, not two.** Creating-then-inserting as separate effects
would mean a failed insert leaves an orphan node with nothing pointing at it,
and the ordering asymmetry `apply_write_to_file` already reasons about applies
here for the same reason.

**Tests:** insert at the cursor mid-line; at end of line; into an empty buffer;
the description matching the node's title at insert time; create-and-insert
producing a link that OR.8's `<CR>` then follows (the round-trip, in a real
editor); cancelling the picker leaving the buffer untouched.

### OR.8 — `id:` resolves ✅

**Deps:** OR.4, OL.1.

`<CR>` on `[[id:…]]` reads `n/<id>` and jumps — file and line, so a headline
node lands on its headline. `:org-roam-id-create` mints an `:ID:` for the
headline at point through OR.3 and writes the property drawer.

**This runs on the keystroke path**, so the lookup is one exact-key `get`
(`roam_index::node`) and never a walk of `nodes`. That is why §4.2 keeps
`n/<id>` as a separate key at all — deserialising a 90 KB blob to answer one
question is not a thing to do while someone is holding a key down.

**Landed.** `roam_index::node` / `is_empty`, `follow_id` replacing OL.1's
placeholder echo, `roam_index::id_drawer_insert` + `:org-roam-id-create`, six
unit tests for the drawer logic and five integration tests through the real
dispatch gate.

**Three failures, three messages**, because they send the reader to three
different places: roam unconfigured (set the option), the index empty (run
`:org-roam-sync`), the id absent (the link is broken). OL.1's single "no id
index" message existed precisely because "cannot open" could not distinguish a
broken link from an absent feature; splitting it three ways is the same argument
carried through.

**`id-create` extends an existing drawer rather than opening a second one.** An
entry that already has `:PROPERTIES:` without an `:ID:` gets the line added
inside it; an entry that already has an `:ID:` is a **no-op with a message**,
not an error and not a second drawer — org cannot read an entry carrying two.
The scan stops at the next headline so an unterminated drawer cannot make one
entry claim the next one's id.

**File-level ids are NOT in this slice.** `:org-roam-id-create` acts on the
headline at point and says so when there is no enclosing headline. Emacs's
`org-id-get-create` would create a file-level id at the top of a file, and the
corpus is full of file nodes — but the plan scoped this to the headline and
widening it here would have been scope taken rather than given. Worth a slice
of its own if it bites.

**The bench is deferred, deliberately, and this is the honest version.** The
plan asked for `<CR>` id resolution against the grammar-action budget. The
resolution itself is one `store_get` plus one MessagePack decode of a single
node record, and `plugin_store.rs`'s bench already characterises the `get`; what
is *not* characterised is the round trip through the grammar seam under a real
keystroke. That is the number that would actually falsify the design, and it
needs the grammar-action bench harness rather than a store microbench. Recorded
as owed rather than quietly dropped.

**Tests:** following to a file node; to a headline node, landing on the line;
an unknown id naming the id and distinguishing itself from "no directory
configured" and from "index not built"; a case-differing id still resolving;
`id-create` on a headline that already has an `:ID:` being a no-op rather than
a second drawer, asserted on the buffer text after two runs.

**The harness lesson from OR.6 recurred immediately.** Three of the five
integration tests first failed because the test dropped `Effect::OpenBufferAt`
— the product had already returned the right path and line. A fourth failed
because the cursor sat at column 0 rather than on the link, so `<CR>` correctly
declined. Both read as "id resolution is broken". `apply_renderer_effects` is
now shared in `org_roam_index.rs` for exactly this reason.

### OR.9 — backlinks: a picker, and why not a multibuffer ✅

**Deps:** OR.4.

`:org-roam-backlinks` lists the notes that link to the node at point, as a
**picker**. §6.1 specified a multibuffer; this is a deliberate change of shape,
decided with Dhruva.

**Backlinks is navigation.** You look at what points here and go read it; you do
not sit in the list editing. The multibuffer's affordances — read in place,
edit-propagates-to-source, `gr` — are what the agenda needs and what a jump list
does not. That is the rule this slice wrote down: *do you act on the rows in
place, or do you go somewhere?*

**And it is a choice, not a limitation any more.** When the question was first
asked, a guest could not own a multibuffer at all — `ProviderViewOpener` is a
native Rust closure with no WIT path. MV.1 has since built the
`multibuffer-view-source` seam, so a read-in-place peer is buildable whenever
someone wants one. It is not wanted for navigation.

**The node at point is resolved on the GRAMMAR seam**, not in the picker: the
picker seam has neither a cursor nor the document (`init` receives args and a
`picker-context`), so the ex-command reads the buffer, finds the node, and
passes its id across as the picker's argument.

**Ancestry, not proximity** — the one genuinely subtle piece. A headline without
an `:ID:` belongs to its nearest *ancestor* node, so the upward walk tracks the
deepest level it may still accept: passing an unidentified headline of level L
means only level < L can answer. Walking line-by-line and taking the first
identified headline finds the previous SIBLING, whose subtree the cursor is not
in at all. A unit test failed against exactly that before the level logic
existed.

**What a row points at, and the honest gap.** `b/<id>` holds the ids that link
to `<id>` — the whole query, one `get` — but not WHICH LINE the link sits on. So
a row jumps to the linking node's own anchor rather than to the link itself.
Emacs shows the link's line because its DB stores a point per link. Storing the
line belongs with the read-in-place view, which is the consumer that actually
needs it: an excerpt must show the linking *line*, whereas a jump to the linking
*note* is a useful answer on its own. Adding the column now would change the
`b/<id>` schema for a consumer that cannot use it yet.

**Tests:** 10 unit (the node-at-point walk, including the sibling and
sub-headline halves of the ancestry rule, `**bold**` at column 0 not being a
headline, case-insensitive drawers, an empty `:ID:` not counting) + 3
integration through a real editor and component: a node's backlinks listed; a
node with none giving an honest EMPTY view rather than an error; and outside a
node the command SAYING so rather than opening an empty picker — because
"nothing links here" and "you are not in a note" look identical in an empty list
and have entirely different fixes.

### OR.10 — dailies ✅

**Deps:** OR.4. Design: `org-roam.md` §6.2.

`:org-roam-dailies-today` / `-yesterday` / `-tomorrow` / `-goto-date` over
`org.roam-dailies-directory` (default `daily`, relative to the roam directory),
`YYYY-MM-DD.org` — the corpus's existing convention, which is org-roam's. Bound
at `<leader>ondd` / `ondy` / `ondt` / `ondD`, mirroring emacs's `C-c n d`.

Dates come from the guest's existing clock path (OC.4's
`local-utc-offset-seconds` plus WASI, as `clock.rs` already does). **Local, not
UTC** — a journal entry filed under yesterday because the user is east of
Greenwich is exactly the midnight-anchor bug OT.3b's `generation` exists to
prevent, in a different costume.

**What the build added to the plan.**

`-goto-date` takes its date OPTIONALLY: with one it goes, without one it opens
a prompt pre-filled with today. That is what lets one command serve both the
`:` line and `<leader>ondD` rather than needing a second action for the chord.
The date is validated at PARSE time, so a typo is refused by the `:` line while
the text is still on screen and editable, rather than by an echo after it has
scrolled away.

Existence is answered by `host-services.read-file`, never by the roam index —
the index lags the watcher's debounce, so a journal written seconds ago reads as
absent, and absent means *append the header again* to a file that already has
one.

**Two host defects surfaced, and each landed as its own commit before this one.**

1. `EffectAuthorizer::resolve_for_compare` canonicalized only the immediate
   parent of a not-yet-existing target, so a write into a directory that was
   *also* new fell back to the raw path and was compared against a canonicalized
   prefix. On macOS that never matches (`/tmp` and `/var/folders` are symlinks
   into `/private`), so the first journal entry was denied with a message that
   reads exactly like a missing capability.
2. `Effect::WriteToFile` refused a missing parent directory outright. Correct as
   a default and wrong for a directory the producer owns — `daily/` is named by
   an option with a default and never typed by a user. `create_parents` is the
   producer opting out; see `cross-file-writes.md` §8.1 for the rejected
   alternatives.

**Tests:** 10 unit in `roam_dailies.rs` (title and filename padding, the
month/year/leap-day boundaries in both directions, strict `YYYY-MM-DD` parsing,
a well-formed-but-impossible date refused with a *different* message from a
malformed one, and the local-vs-UTC case pinned with an offset that changes the
day) + 7 integration through a real editor and component: today creating with an
`:ID:` and a `#+title:`; today OPENING what is already there without a second
header; yesterday and tomorrow naming the days either side; `-goto-date` with an
explicit leap day; a malformed date refused with nothing created on disk; the
bare form prompting and its submit opening the day typed; and dailies refusing
when `org.roam-directory` is unset rather than inventing a journal wherever the
editor started. Plus 3 host tests for `create_parents` and 2 for the authorizer
walk-up.

### OR.11 — templates, the capture buffer, and the one thing capture cannot do 📝

**Deps:** OR.6.

**Scope widened 2026-08-30, after comparing against emacs.** Roam capture is not
just `${field}` interpolation bolted onto the existing prompt — the *surface*
differs, and that is the larger half of this slice. Recorded here rather than
carved into its own slice because roam capture and org capture are one flow;
splitting them would mean designing the capture buffer twice. **Still to be
discussed before implementation.**

**Lattice's capture is a prompt; emacs's is a buffer.** `<leader>oc` opens the
template transient (which does match emacs's selection step) and then
`Effect::OpenPrompt` — a one-line minibuffer. Emacs opens `*CAPTURE-<file>*`,
pre-fills it by expanding the template, puts point at `%?`, and lets you edit
freely until `C-c C-c` files it or `C-c C-k` discards it.

| | emacs | lattice today |
|---|---|---|
| template menu | temp window, one key each | ✅ the `<leader>oc` transient |
| capture surface | a **buffer** | a one-line prompt |
| editing | free, multi-line, `%?` point | single line, no point placement |
| finalize / abort | `C-c C-c` / `C-c C-k` | submit-on-enter, no abort |

**The chords belong to a minor mode, not to capture or to roam.** `C-c C-c` and
`C-c C-k` are wanted by org-capture AND org-roam-capture, so per the standing
rule they go on an `org-capture-mode` minor activated on the capture buffer,
owning the chords *and* their handler bodies. Copying them into two places is
the failure that rule exists to prevent — `magit-diff-mode` was hand-given two
of three chords and the third's absence announced itself to nobody.

Scoping is also what makes `<C-c>` safe: it is vim's interrupt, so a GLOBAL
`<C-c><C-c>` would shadow it. Buffer-local via the minor it does not, which is
how nvim-orgmode binds the same chord.

**`C-c C-k` must leave no file.** In emacs an aborted roam capture creates
nothing at all. That falls out of the buffer model — the file is written on
finalize, not on open — and it is the behaviour OR.6's `WriteToFile`-on-create
does NOT have.

The `${field}` half, which was the slice's original whole:


Roam templates are capture templates plus `${field}` interpolation over the node
being created — `${title}`, `${slug}`, `${id}`. This is a requirement rather than
a nicety: the corpus's own templates open with `#+Title: ${title}`
(`pkos-concept.org` line 1), so without it eleven existing templates produce
files with a literal `${title}` in them.

The two syntaxes coexist because they answer different questions: `%`
interpolates the *capture context*, `${}` interpolates the *node*. `%^{Prompt}`,
`%U`, `%T`, `%t`, `%a` and `%%` keep working exactly as `org-capture.md` defines
them.

**Tests:** `${title}` expanding; `${slug}` matching the filename's slug;
`${id}` matching the drawer; `%^{Prompt}` and `${title}` in one template; an
unknown `${x}` surviving verbatim (capture's rule for unknown `%x`, for the same
reason — a template is user text, and a placeholder that vanished cannot be
found and fixed); each of the eleven corpus templates round-tripping.

### OR.12 — docs 📝

**Deps:** OR.1–OR.11.

`org-roam.md` lands with the phase and is amended in place where the build
disagreed with it. The plugin's `doc/org.md` gains the roam commands and the
`org.roam-*` options. `implementation.md` gains the OR rows and a section.
`site/data/dev-nav.toml` gains `architecture/org-roam`, and the sync runs — a
docs change is not finished until the site carries it.

The three `host-services` additions (OR.1–OR.3) are **host** surface and belong
in `plugin-host.md` beside `read-file` and `local-utc-offset-seconds`, not only
here. A plugin author looking for "can I persist something" will not think to
open the org-roam fragment.
