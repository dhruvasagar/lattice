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
| OR.4 | `org.roam-directory` and the indexer | 📝 |
| OR.5 | the picker offers to create what it could not find | 📝 |
| OR.6 | `:org-roam-find-node` | 📝 |
| OR.7 | `:org-roam-insert-node` | 📝 |
| OR.8 | `id:` resolves — `<CR>` jumps, `:org-roam-id-create` mints | 📝 |
| OR.9 | the backlinks view | 📝 |
| OR.10 | dailies | 📝 |
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

### OR.4 — the index 📝

**Deps:** OR.1, OR.2.

`org.roam-directory` (unset by default), the walk, the parse, the extraction,
and the four key families in `org-roam.md` §4.2. The async event seam is the
sole writer.

**Extraction is tree-native**, per the rule OT.x established: file-level and
headline-level `:ID:` drawers, `#+title:`, `#+filetags:`, `:ROAM_ALIASES:`,
`:ROAM_REFS:`, and outgoing `[[id:…]]` links. Structure from the tree —
otherwise an `:ID:` written inside a `#+BEGIN_SRC` example becomes a phantom
node, which is the same class of bug OT.3's `a_headline_inside_a_source_block_is_not_a_row`
pins for the agenda.

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
through two headline levels; case-insensitive id matching; an `:ID:` inside a
`#+BEGIN_SRC` block **not** becoming a node (paired with its text-path twin, so
the difference is documented rather than only its good half); a malformed file
skipped with the scan continuing; a duplicate `:ID:` keeping the first and
warning with both paths; a node deleted from a file disappearing from `nodes`,
from `n/<id>` and from every `b/<id>` that named it; case-insensitive `#+TITLE:`
/ `#+title:` / `#+Filetags:`; boot rescan catching a change made while the
editor was closed.

### OR.5 — the picker offers to create what it could not find 📝

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
no matches; always last regardless of query and candidate set; the query
crossing verbatim including spaces and non-ASCII; a source that sets no label
behaving exactly as today (the regression guard for every existing picker);
`accept` receiving `create` and routing to the source.

### OR.6 — `:org-roam-find-node` 📝

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

**Tests:** finding by title; by alias; a headline node reachable and landing on
its line rather than the file's first; the generation check rebuilding the cache
when the index moved and **not** rebuilding when it did not; create producing a
node the next find-node can find; roam inert with the directory unset saying so
rather than showing an empty picker.

### OR.7 — `:org-roam-insert-node` 📝

**Deps:** OR.6.

The same picker; on accept, insert `[[id:<ID>][<Title>]]` at the cursor.
Create-on-no-match creates the node *and* inserts the link to it.

**The insert is one edit, not two.** Creating-then-inserting as separate effects
would mean a failed insert leaves an orphan node with nothing pointing at it,
and the ordering asymmetry `apply_write_to_file` already reasons about applies
here for the same reason.

**Tests:** insert at the cursor mid-line; at end of line; into an empty buffer;
the description matching the node's title at insert time; create-and-insert
producing a link that OR.8's `<CR>` then follows (the round-trip, in a real
editor); cancelling the picker leaving the buffer untouched.

### OR.8 — `id:` resolves 📝

**Deps:** OR.4, OL.1.

`<CR>` on `[[id:…]]` reads `n/<id>` and jumps — file and line, so a headline
node lands on its headline. `:org-roam-id-create` mints an `:ID:` for the
headline at point through OR.3 and writes the property drawer.

**This runs on the keystroke path**, so the lookup is one exact-key `get` and
one generation compare, never a scan of `nodes`. That is why §4.2 keeps `n/<id>`
as a separate key at all — deserializing a 90 KB blob to answer one question is
not a thing to do while someone is holding a key down.

**Test it through the real dispatch gate.** The failure this plan most expects is
the one OC.10 and OT.4 both hit: a seam wired end to end that answers nothing,
because the gate synthesises a context no host test goes through. So the test
presses `<CR>` in a real editor over a real indexed corpus, not against a
hand-built `GrammarEnv`.

**Bench (required):** `<CR>` id resolution, against the grammar-action budget. If
it does not fit, the design is wrong and this is where that becomes visible.

**Tests:** following to a file node; to a headline node, landing on the line;
an unknown id naming the id and distinguishing itself from "no directory
configured" and from "index not built"; case-differing id still resolving;
`id-create` on a headline that already has an `:ID:` being a no-op rather than a
second drawer; `id-create` making the headline findable in the picker after the
index catches up — without an intervening keypress.

### OR.9 — the backlinks view 📝

**Deps:** OR.4.

`:org-roam-backlinks` opens a multibuffer provider over `b/<id>` — one excerpt
per linking node, showing the line the link sits on with its headline as
context. Progress and completion go in the **headerline**, per the standing rule
for async buffers.

Refresh rides `MultibufferExcerptsReady`, which has a wake already wired
(`boot.wake_on_event`). Named explicitly because the alternative — a bare
`TickCallback` — is the bug class that has been re-introduced repeatedly and
whose symptom reads as a rendering fault.

**Tests:** backlinks for a node with several; with none (an honest empty view,
not an error); a backlink from a headline node attributed to the headline rather
than the file; the view refreshing when the index moves **without a keypress**;
excerpt line numbers correct after an edit above the link.

### OR.10 — dailies 📝

**Deps:** OR.4.

`:org-roam-dailies-today` / `-yesterday` / `-tomorrow` / `-goto-date` over
`org.roam-dailies-directory` (default `daily`, relative to the roam directory),
`YYYY-MM-DD.org` — the corpus's existing convention, which is org-roam's.

Dates come from the guest's existing clock path (OC.4's
`local-utc-offset-seconds` plus WASI, as `clock.rs` already does). **Local, not
UTC** — a journal entry filed under yesterday because the user is east of
Greenwich is exactly the midnight-anchor bug OT.3b's `generation` exists to
prevent, in a different costume.

**Tests:** today creating when absent and opening when present; yesterday and
tomorrow crossing a month boundary and a year boundary; `-goto-date` with an
explicit date; the local-vs-UTC case pinned with an offset that changes the day.

### OR.11 — templates, and the one thing capture cannot do 📝

**Deps:** OR.6.

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
