# Org-roam

> **Where the code is.** Everything this page describes is implemented in
> [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), a **separate repository**. It
> is a WASM Component plugin: nothing here is compiled into the editor, and
> lattice has no `BufferKind::Org`, no `Lang::Org` arm and no `Editor::`
> method for any of it. What lives in *this* tree is the seams the plugin
> contributes through — see [`plugin-host.md`](plugin-host.md).

Status: design fragment (2026-08-29). Slice plan:
`../operations/slice-plans/org-roam.md`.

Builds on [`org-mode.md`](org-mode.md) §7 (links — `id:` is recognised there and
resolved here), [`conceal.md`](conceal.md) (an `id:` link is unreadable until it
renders), [`org-capture.md`](org-capture.md) (roam templates extend capture's),
[`multibuffer-views.md`](multibuffer-views.md) §3.7 (views generally; backlinks
turned out to want a picker rather than one — §6.1),
[`plugin-host.md`](plugin-host.md) (`picker-source`, `completion-source`,
`host-services`).

A note is a **node**: a title, an id, and its links to other nodes. Roam is
three verbs over that — *find one*, *link to one*, *follow one* — plus the view
that answers the question the links exist for: **what points here?**

## 1. Why this exists, and what it is not

Org already has an agenda, and the agenda is not this. The agenda asks *what do
I have to do and when*; roam asks *what do I know and how does it connect*. They
run over overlapping files and share nothing else — different index, different
query, different view.

What makes roam a design problem rather than a feature is that its unit of
identity is an **id, not a path**. A note's title changes, its file gets
renamed, its content moves to another file; the links keep working because they
never named the file. That is the whole value proposition, and it is also the
whole cost: every operation needs a map from id to location, and that map is not
in any one file.

The corpus this is designed against is real and was measured, not assumed —
`~/src/dhruvasagar/org-files/roam/`:

| | |
|---|---|
| org files | 706 (60,825 lines) |
| file-level `:ID:` | 475 |
| headline-level `:ID:` | 110 |
| `[[id:…]]` links | 795 |
| `:ROAM_ALIASES:` | 71 |
| `:ROAM_REFS:` | 1 |
| `#+filetags:` | 428 |
| dailies (`daily/YYYY-MM-DD.org`) | 8 |
| capture templates | 11 |

Three things in that table set requirements the design would otherwise have
guessed wrong.

**Headline nodes are 19% of the corpus.** A file-only node model would silently
drop 110 nodes and every link into them. §3 is why both grains exist.

**Aliases are used and refs are not.** Alias support has to be real — 71 nodes
are findable under a name that is not their title. Refs get built because they
cost one field once aliases exist, not because the corpus demands them.

**A filename is a fossil.** `20250603103551-chicken_breast_honey_garlic.org`
holds a node titled *Honey Garlic Chicken Breast*. The slug was minted from an
earlier title and never moved. This is not a defect — it is the id model working
— but it means **the picker must search titles and aliases, never filenames**,
because the filename is evidence of what the note used to be called.

## 2. The constraint that shapes everything

**There is no single org guest instance.** `spawn_event_plugin`,
`spawn_config_plugin`, `spawn_help_plugin`, `spawn_dashboard_sections` and
`instantiate_grammar_plugin` are separate paths, each building its own
`wasmtime::Store`. The picker seam that runs find-node and the synchronous
grammar seam that runs `<CR>` are **different instances with different memory**.

This is not incidental. It is what §5.5 of `design.md` means by crash isolation,
and it is correct. But it means the obvious design — "the guest holds the index"
— does not say what it appears to say. It says *N copies, drifting*, and the
drift is invisible: find-node would show a note that `<CR>` cannot open, and
nothing would report an error, because each instance is internally consistent.

Everything in §4 follows from designing around that rather than discovering it.

## 3. What a node is

Both grains, because the corpus has both.

- A **file node** is a file whose top-level property drawer carries an `:ID:`.
  Its title is `#+title:`; its tags are `#+filetags:`.
- A **headline node** is any headline whose property drawer carries an `:ID:`.
  Its title is the headline text with the TODO keyword, priority and tag list
  stripped; its tags are its own plus those inherited from the file and from
  ancestor headlines.

```
node {
    id:      string,        // the :ID: verbatim
    title:   string,
    aliases: list<string>,  // :ROAM_ALIASES:
    tags:    list<string>,  // resolved, inheritance already applied
    refs:    list<string>,  // :ROAM_REFS:
    file:    string,
    line:    u32,           // 0 for a file node
    level:   u32,           // 0 for a file node, else headline depth
}
```

**Tag inheritance is resolved at index time, not at query time.** A headline
node's tags are the union of its own, its ancestors' and the file's, flattened
when the record is written. The alternative — storing them separately and
resolving per query — would put a tree walk inside the picker's filter loop, and
the picker filters on every keystroke.

**Ids are compared case-insensitively and written uppercase.** The corpus is
uppercase throughout (macOS `uuidgen`, which is what `org-id` shells out to),
but org itself is not consistent about case across platforms, and a link that
fails to resolve because of a case difference is the worst possible failure: it
looks like a missing note.

## 4. The index

### 4.1 One indexer, many readers

The **async event seam is the sole writer**. It is woken by the watcher, reads
the files that changed, parses them, and writes records. Every other seam —
picker, grammar, multibuffer — only reads.

Single-writer is what makes the rest of this section simple. It means records
can be **denormalized freely**, because there is no second writer to disagree
with: the same node appears in the all-nodes blob, under its own key, and in its
targets' backlink lists, and all three are written in one pass by one instance.
A multi-writer design would need either a transaction or a reconciliation pass,
and would get one of them wrong.

### 4.2 The store

Five new capability-gated calls on `host-services`. **The host stores bytes
under strings and never interprets either.**

```wit
/// Durable, plugin-scoped key/value. Scoped to the plugin's own data dir by
/// manifest id, so every seam instance of a plugin sees ONE store and two
/// plugins cannot collide. Keys are guest-chosen strings, not paths — there
/// is no traversal to defend against.
store-put:        func(key: string, value: list<u8>) -> result<_, string>;
store-get:        func(key: string) -> option<list<u8>>;
store-delete:     func(key: string) -> result<_, string>;
/// Keys carrying `prefix`, sorted. `""` lists everything.
store-keys:       func(prefix: string) -> list<string>;
/// Bumped on every successful mutation. A reader compares it against
/// what it last built from and rebuilds when it moved.
store-generation: func() -> u64;
```

**Five functions on `host-services` rather than a `store` interface of their
own**, which is what OR.1 landed after weighing the naming against the failure
mode. A component's import set is fixed for the whole artefact and must resolve
on *every* linker it is instantiated against, including the grammar seam's sync
one; a new interface is a new import each world must declare and both linkers
must wire, and a miss there does not degrade one seam — it fails the whole
component at instantiation. That is not hypothetical: OC.2 added a single
`logging::log` call and the entire org plugin stopped loading. `host-services`
is already imported by every world that would want a store and already wired on
both linkers, so the prefix buys structural impossibility where `store.put`
would have bought a nicer name.

The gate is a new `state:write` capability rather than `fs:write`. The two
answer different questions: `fs:write:<prefix>` is *reach* — which of the user's
files a plugin may alter — and a plugin that persists an index needs none of it.
Folding them together would make "remember something between restarts" require a
grant over the user's documents.

Roam's key layout — **the guest's schema, invisible to the host**:

| Key | Value | Read by |
|---|---|---|
| `nodes` | every node record, one blob | the picker, once per open |
| `n/<id>` | one node record | `<CR>`, one exact lookup |
| `b/<id>` | ids linking *to* `<id>` | the backlinks view |
| `f/<path>` | that file's content hash + the ids it produced | the indexer, to skip unchanged files and to retract deleted ones |

`nodes` is a single ~90 KB blob for this corpus, rewritten whenever anything
changes. That is deliberate: find-node is one `get`, not 585. `n/<id>` exists
separately because `<CR>` must not deserialize 90 KB to answer one question, and
it is on the keystroke path.

**`f/<path>` is what makes deletion work**, which is the case a cache-shaped
design forgets. When a file changes, the indexer needs to know which ids it
*used* to contain so it can retract the ones that are gone. Without that row, a
node deleted from a file stays in the index forever and the picker offers a
destination that no longer exists.

**The failure policy is `agenda_cache.rs`'s, promoted from a special case to a
primitive**: crash-safe temp-file-and-rename, a schema version that refuses to
read an older shape, a size cap, and degradation to *empty* on any corruption —
never to a wrong answer. That policy was written once for the agenda and would
otherwise be written again, slightly differently, by every guest that wants to
persist anything.

### 4.3 The watcher

```wit
watch:   func(path: string) -> result<_, string>;
unwatch: func(path: string) -> result<_, string>;
```

Debounced host-side, emitting an event carrying the changed paths. Gated on the
same `fs:read` grant `walk` and `read-file` already use — the check is one line
because the mechanism exists.

Delivery is the `files-changed` arm of the WIT `event`, through the
`events.subscribe` a plugin already uses. It is **addressed**, not broadcast:
`Event::FilesChanged` carries the host-issued plugin id and the delivery actor
drops any batch that is not its own. The bus is a broadcast and a watch is a
capability — without that, a plugin holding `fs:read` over one directory would
learn what changed under another plugin's watched directory, which it holds no
grant over. The id is dropped on projection, because by the time a guest sees a
batch it is always the guest's own.

A watch lives on the guest's own `PluginState`, so unload, quarantine and the
actor's channel closing each stop it with no teardown wiring to forget.

`notify` is already a host dependency and already drives autoread, so this is
wiring rather than invention. It is the right mechanism rather than a
save-triggered reindex because **the corpus is edited from outside lattice**:
emacs writes a note, a `git pull` lands twenty, a sync daemon rewrites a
directory. A save hook sees none of those, and the symptom — a picker missing
notes you know you wrote — reads as data loss.

### 4.4 Freshness, and what a reader does

The indexer bumps `generation` when it finishes a batch. A reader caches what it
built and the generation it built from; on each use it compares, and rebuilds
only when the number moved. The comparison is one host call returning a `u64`,
which is cheap enough to do on every picker open and every `<CR>`.

**Boot does a full walk.** Not because the store is untrusted, but because
lattice was not running while the corpus changed, and the watcher cannot report
what it did not observe. The walk reads 706 files (~10–50 µs each, warm) and
parses only those whose content hash moved — the same shape OT.3b established,
where the read is the cheap end and the parse is what the hash protects.

### 4.5 Why the index is not host-side

The alternative — a host service that knows about nodes, aliases and backlinks —
is faster in the sense that nothing crosses the boundary. It is rejected because
it makes the host learn what an `:ID:`, a `:ROAM_ALIASES:` and a backlink are,
which is exactly the knowledge the org plugin exists to demonstrate the host
never needs (`org-mode.md` §2). The speed argument does not survive contact with
the numbers either: the gap is microseconds, on paths that open a picker or
follow a link.

## 5. The picker, and creating what is not there

Find-node and insert-node are `picker-source` seams over the `nodes` blob.
Fuzzy matching stays **native** — the picker already ranks candidates, and doing
it in the guest would put a WASM crossing on every keystroke of a query.

Candidates carry the title as their text, with aliases and tags as annotation
columns so a search over "the name I remember" works whether or not it is the
title.

### 5.1 Create-on-no-match

The generic half. `picker-source-spec` grows:

```wit
/// When set, the picker offers one synthetic row whenever the query is
/// non-empty. `%s` in the label is replaced by the query.
create-label: option<string>,
```

**The picker synthesises the row; the source only declares it.** `create_label`
is read at seat time and `Picker::push_create_row` appends the row after
`match_and_rank`, so every source gets the behaviour from one declaration and
none of them re-implements it. The payload is held in its own slot rather than
in the seat-time `routing_meta` sidecar, because it carries the *live* query
while the sidecar is written once per seat — which is also why the create row
gets its own candidate kind rather than an index into it.

and `routing-payload` grows `create(string)` carrying the query verbatim. The
source's `accept` receives it and does whatever creation means for that source.

Two decisions in that sentence are not obvious.

**The row appears whenever the query is non-empty — not only when nothing
matches.** Offering it only on zero matches would make it impossible to create a
node called *Rust* while a node called *Rust Async* exists, which is precisely
when you most want to: the general note is the one you write after the specific
one.

**The row is pinned last, never ranked.** If create could sort above a real
match, then `<CR>` on a query that has a match would sometimes create a
duplicate — a destructive outcome caused by ranking noise. Pinned last means
`<CR>` never creates by accident; creating is always a deliberate `<C-n>` past
the real answers.

### 5.2 Creating a node

Create takes the query as the title, expands a roam capture template, writes
`YYYYMMDDHHMMSS-<slug>.org` into `org.roam-directory`, and opens it.

The file is written through `Effect::WriteToFile`, which resolves the path to a
buffer — so a new node exists as an **unsaved buffer** until the user saves it.
That is not a gap; it is org-roam-capture's own model, where a capture is a
draft you finalize. The watcher indexes it when it lands on disk, which means an
abandoned draft never enters the index, which is the correct outcome.

**Ids are minted by the host.** `host-services.new-uuid() -> result<string,
string>`, uppercase.
This is one function and it exists for exactly the reason `read-file` does:
`:org-roam-id-create` on the headline at point is a *grammar action*, and
`read-file`'s own doc comment records that the grammar seam's synchronous linker
cannot serve WASI — "async seams (pickers, completion) are unaffected and may
keep using WASI directly", but this path is not one of them. Minting an id in
the guest would work on the picker path and panic on the grammar path, which is
the worst kind of seam: correct in the test that built its own context.

It returns a `result` rather than the bare `string` first sketched here, and that
is the one place on this seam where the difference is load-bearing. Its
neighbours degrade to `0` when they cannot answer, on the argument that a legible
wrong answer beats a fabricated one — but those values are *read*. An id is
*written*, into the user's own file, as an `:ID:` that outlives the session and
every other tool's view of that note. A guest handed an empty string on entropy
failure would write an empty drawer and nothing would ever say so.

## 6. The surfaces

| Command | What |
|---|---|
| `:org-roam-find-node` (`<leader>onf`) | picker over all nodes; create-on-no-match; opens the node |
| completion inside `[[…` | org-roam nodes as an insert-mode completion source — match a title, insert `[[id:…][Title]]` |
| `:org-roam-id-create` | mint an `:ID:` for the headline at point, making it a node |
| `:org-roam-backlinks` | a picker over what points here — navigation, so a jump list rather than a view |
| `:org-roam-dailies-today` / `-yesterday` / `-tomorrow` / `-goto-date` | the journal |
| `:org-roam-sync` | force a full rescan — the escape hatch when the watcher missed something |
| `<CR>` on an `[[id:…]]` link | resolves through `n/<id>` and jumps — file **and** line |
| `:org-roam-id-create` | give the headline at point an `:ID:`, making it a node |

Naming follows the standing rule: dashed, namespaced, no collapsed forms and no
generic-name aliases. `:org-roam-find-node` does not also register `:find-node`.

**Inserting a link is completion, not a command.** You insert a link
mid-sentence, so a normal-mode chord that opens a picker means leaving Insert,
picking, and coming back — and emacs does not ask that either: you type `[[` and
completion offers nodes. Org contributes a `completion-source` (PH7.6) rather
than a second picker, gated on the cursor being inside an unclosed `[[`, which
is also what keeps a 500-node corpus out of ordinary word completion. The
picker form (`:org-roam-insert-node`) is deferred, not dropped — see the slice
plan.

The gate is the **guest's** to decide, and deliberately so. The host hands over
`line-before-cursor` and `language`; it does not know what `[[` means and must
not learn. The alternative — a host-side `link-context` flag beside
`path-context` — puts one plugin's syntax in the editor and guarantees the next
source needs the next flag.

### 6.1 The backlinks view

**A picker, decided against the multibuffer this section first specified.**

The rule that decides it: *do you act on the rows in place, or do you go
somewhere?* Backlinks is navigation — you look at what points here and go read
it. The multibuffer's affordances (read in place, edit-propagates-to-source,
`gr` refresh) are what the agenda needs, and the agenda needs them because it is
where you change TODO states and reschedule. A jump list is what navigation
wants.

This is not a limitation being accepted. When the question was first asked a
guest could not own a multibuffer at all, and that gap is now closed — see
[`plugin-multibuffer-views.md`](plugin-multibuffer-views.md). A read-in-place
backlinks peer is buildable whenever someone wants one; it is not wanted for
navigating.

**The one thing the index does not yet hold** is which LINE a link sits on.
`b/<id>` answers "which nodes link here" in one `get`; it does not say where in
them. So a row jumps to the linking node's anchor rather than to the link.
Emacs shows the line because its database stores a point per link. Storing it
belongs with the read-in-place view — the consumer that actually needs it, since
an excerpt must show the linking *line* whereas a jump to the linking *note* is
useful on its own.

### 6.2 Templates, and the one thing capture cannot do

Roam templates are capture templates (`org-capture.md`) with one addition the
existing placeholder set cannot express: **`${field}` interpolation over the node
being created**. The corpus's templates use it in their first line —
`pkos-concept.org` opens `#+Title: ${title}` — so this is a requirement, not a
nicety.

`${title}`, `${slug}` and `${id}` expand from the node; `%^{Prompt}`, `%U`,
`%T`, `%t`, `%a` and `%%` keep working exactly as capture defines them. The two
syntaxes coexist because they answer different questions: `%` interpolates the
*capture context*, `${}` interpolates the *node*.

**Keyword parsing is case-insensitive.** The corpus contains `#+TITLE:`,
`#+title:`, `#+Filetags:` and `#+filetags:`, all written by org itself at
different times. A case-sensitive parser would index half the corpus.

## 7. Configuration

| Option | Default | What |
|---|---|---|
| `org.roam-directory` | unset | the corpus root; roam is inert until set |
| `org.roam-dailies-directory` | `daily` | relative to the roam directory |
| `org.roam-capture-templates` | one built-in | keyed templates, capture's shape |

Separate from `org.agenda-files` on purpose. The agenda wants files with TODOs
and dates; roam wants files with ids; for a working setup they are different
sets, and one option serving both would force them to be the same.

**Roam is inert when `org.roam-directory` is unset** — no walk, no watcher, no
store writes, and `<CR>` on an `id:` link says the directory is not configured.
An org user who does not keep a zettelkasten pays nothing for this feature
existing.

## 8. Failure behaviour

Every path degrades the way the seam it rides already does.

- **A malformed org file during a scan** is skipped with a `debug` log and the
  scan continues — `error-parser`'s rule, because one bad file must not fail the
  index.
- **A duplicate `:ID:` across two files** keeps the first indexed and logs the
  collision at `warn` naming both paths. Emacs' `org-id` behaves the same way,
  and silently preferring one would make a real corpus problem invisible.
- **An `id:` link with no matching node** reports that the node is unknown and
  names the id. Distinguishable from "no index configured" (§7) and from "no
  index built yet", because the three have different fixes.
- **A corrupt or version-mismatched store** is discarded wholesale and rebuilt.
  A partial rebuild from bytes that failed a schema check is how a cache starts
  serving plausible nonsense.
- **A watcher that fails to register** logs at `warn` and roam falls back to
  index-on-boot plus `:org-roam-sync`. Degraded, honest, still usable — as
  opposed to appearing to work and going stale.
- **A trap mid-scan** quarantines the plugin and leaves the index holding what
  it had. Partial-and-honest beats empty-and-silent.
- **Diagnostics are `debug!`**, never `info!`. A watcher over 706 files during a
  `git pull` would flood `*messages*`.

## 9. Paramount-goal alignment

**#1 Performance.** Nothing roam does runs per frame or on the UI thread. The
one keystroke-path operation is `<CR>` resolving an id, which is a single
exact-key `get` on the sync grammar linker where `host-services` is already
wired. Indexing is off-thread by construction (§4.1), fuzzy matching is native
(§5), and tag inheritance is resolved at index time so it is not in the picker's
filter loop (§3).

**#2 Extensibility.** The host gains two primitives — a byte store and a
directory watch — and neither names org, a node, or an id. The schema in §4.2 is
entirely guest-side; the host sees strings and bytes. This is the same test
`org-mode.md` §2 set and passed, applied to a harder case, because roam wants
*persistence* and persistence is where hosts usually start learning schemas.

**#3 Vim modal editing.** Roam adds ex-commands and one `<CR>` binding that
declines when there is nothing to follow (`org-mode.md` §7.2). No motion, no
operator and no text object changes.

**#4 Asynchronicity.** The indexer is a woken task, not a poll. Results reach
the screen through an event with a wake already wired (§6.1) rather than through
a bare tick callback, which is the specific bug class
`docs/dev/architecture/boot-composition.md` §3 designs out and which has been
re-introduced by reaching for `tick_callback` directly.

## 10. Scope

**In:** find-node, insert-node, `id:` following, id-create-at-point, the
backlinks view, dailies, roam capture templates, aliases, tags, refs.

**Out, as cuts rather than omissions:**

- **Graph view.** Needs a layout engine and a canvas surface, and the question
  people actually open the graph to answer — *what connects to this?* — is the
  backlinks view's question. Revisit if backlinks prove insufficient rather than
  on the assumption that they will.
- **Unlinked references** — every mention of a node's title that is *not* a
  link. A real feature and a full-text scan of the corpus per query; it wants
  its own slice after the index proves out, and probably wants the index to
  carry a term map it does not carry today.
- **`org-roam-db` compatibility.** We index the same *files* emacs does, not
  emacs's SQLite cache. Sharing the DB would couple lattice to org-roam's schema
  version and to its migration timing; sharing files couples us to org, which is
  the durable thing. The cost is that both tools index independently, which is
  cheap and correct.
- **org-roam-protocol** (capture from a browser). Needs a URL handler and an
  external-invocation path that has no equivalent here yet.

**Deferred, not cut:** node-level `:ROAM_REFS:` search (`:org-roam-ref-find`) —
the field is indexed from the start because it costs one column beside aliases,
but the corpus has one ref in it and a picker over one row is not worth a
command yet.
