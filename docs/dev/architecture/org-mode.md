# Org-mode as a plugin

**Status: built** (2026-08-26). Archive, refile and capture — the three
verbs blocked longest — closed last, on three primitives the epic turned
out to need rather than one: `Effect::WriteToFile`
([`cross-file-writes.md`](cross-file-writes.md)) to carry the text,
`document.path()` to name a file beside your own, and a picker source
able to invoke an ACTION rather than an ex-line, since an ex-command
receives no document handle and refile has to read the subtree at the
cursor after choosing where it goes. Sequencing, per-slice outcomes and
the amendments this document records:
[`../operations/slice-plans/archive/org-mode.md`](../operations/slice-plans/archive/org-mode.md).
Ledger entry: [`../operations/implementation.md`](../operations/implementation.md)
§"Org-mode as a plugin".

Builds on [`plugin-languages.md`](plugin-languages.md) (the `language` seam,
whose first consumer is the same plugin) and
[`plugin-host.md`](plugin-host.md) (`grammar`, `modes`, `config`,
`picker-source`). The agenda's host half is a multibuffer provider —
[`multibuffer-views.md`](multibuffer-views.md) §3.7 for the provider shape and
§3.7a for the provider-view seam the agenda triggers through.

**Two sections carry amendments made during the build**, each marked in place
rather than rewritten over: §4.2 (the view carries two minors, not one) and
§6.2 (`extensions()`, and `group` as a key rather than a label). Where this
document and the code disagree, the code and the slice plan are what happened.

## 1. What was already true

*Written before the build; kept because it is the starting position the rest
of this document argues from.*

[`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin) already contributed org **the language**: a
tree-sitter grammar compiled to wasm, per-level headline highlights, folds
over sections and blocks and drawers, and its own `:help` page. It rode
the `language` and `help` seams and needed nothing from the host.

Visibility cycling is native and predates the plugin: `z<Space>` cycles a
heading FOLDED → CHILDREN → SUBTREE and `z<Tab>` cycles the buffer
OVERVIEW → CONTENTS → SHOW-ALL (`AppEffect::CycleFoldAtCursor` /
`CycleFoldsGlobal`, whose doc comments name org). Org folds through the
ordinary fold pipeline, so `za` / `zR` / `zM` work with no org-specific
code anywhere.

What was missing was **editing**: promotion, subtree motion, TODO workflow,
tables, agenda. That is org-mode the *mode*, and this fragment is its design.

**All of it now ships** except the two slices that need to write to a file
other than the buffer's own (§10). The plugin rides seven seams; the host
gained four generic changes and learned nothing about headlines.

## 2. The thesis

> Org-mode is a plugin. Not "mostly a plugin with a few host hooks" — a
> plugin. The host learns nothing about headlines.

That is the claim under test. **It held** — asserted at OM.2, again at OM.A3,
and greppable: no `BufferKind::Org`, no `Lang::Org`, no `Editor::do_org_*`, no
`Action::Org*`. Concretely it means: no `BufferKind::Org`,
no `Lang::Org`, no `Editor::do_org_*`, no `Action::OrgPromote`, no org
branch in a renderer. The acid test from CLAUDE.md applies verbatim — a
provider landing should require **zero** `Editor::` method additions and
**zero** new variants in the host's `Action` enum — and org is the
hardest case yet to put it to, because org wants more of an editor than
any plugin before it.

Two things make the claim plausible rather than aspirational.

**The grammar seam already carries the right context.** `apply-action`
receives `borrow<document>` *and* `option<borrow<tree-snapshot>>` — the
same buffer's point-in-time parse tree, acquired the same instant so
their versions agree. The org plugin ships the grammar, so the tree it
walks is a tree it defined. Promote-a-subtree is: read the tree, compute
an edit, return `Effect::Edits`. The host mediates nothing.

**`Effect::Declined` makes context-sensitive chords composable.** A guest
action that returns `[declined]` did not consume the chord; the
dispatcher re-resolves as if that action's keymap layer were not there.
This is what lets several modes bind the same key and let context sort it
out (§4.3), which is in turn what makes the mode decomposition of §4 real
rather than cosmetic.

## 3. What has to change host-side

Three things, all of them finishing a path the codebase already
designates.

### 3.1 Majors over the `modes` seam

`wit/modes.wit` declares `mode-kind::major` and the host rejects it:

```
"register-mode skipped: only minor modes are supported in PH7.11a
 (majors are Phase 8)"                       — mode_host.rs:148
```

Org needs a major. A minor cannot serve: `ActivationPolicy` offers
`manual` / `global` / `universal` / `majors(list)`, and none of those
means "buffers whose language is org". Universal would fire org's chords
in every buffer in the editor.

### 3.2 A language index on `ModeRegistry`

Even with `major` accepted, nothing would activate it.
`resolve_major_mode` (`lattice-host/src/modes.rs:419`) resolves a
`Document` buffer through `lattice_syntax::major_mode_id_for_lang`, and
that function reads:

```rust
// A plugin language's major mode is contributed through the
// `modes` seam by the plugin that owns it — the mode owns its
// full surface, so the host does not synthesise one here.
Lang::Plugin(_) => None,
```

The route is designated and closed. Opening a `.org` file today lands in
`text-mode`.

The fix mirrors machinery that already exists rather than inventing any.
`ModeRegistry` indexes majors by buffer kind at register-time, and
`Mode::target_buffer_kind`'s doc comment already promises the property we
want:

> Adding a new kind-bound major requires zero host-side hand edits —
> register the mode and the index picks it up.

So: a **language index** beside the kind index. `Mode::target_language()
-> Option<String>` beside `target_buffer_kind`, `find_major_for_lang`
beside `find_major_for_kind`, populated the same way at the same time.
`resolve_major_mode` consults it before falling through to `text-mode`.

This is deliberately not org-shaped. It is the general answer to "a
plugin contributed a language; which major owns it", and the native
language majors (`rust-mode`, `markdown-mode`, …) can migrate onto it
later, collapsing `major_mode_id_for_lang`'s hand-written match. That
migration is **not** in scope here — naming it as the eventual shape is,
so the index is not built as a plugin-only side door.

### 3.3 `mode-declaration.target-language`

The WIT record gains one optional field. A major declaring no target
language is manual-activation only, which keeps the field honest for
majors bound to something other than a language later.

### 3.4 Drain order, which is a gate

`mode-keymap-binding` resolves `command` against the `CommandRegistry`
**at registration**. Org binds `<leader>ol` to `action:org-demote`, which
org itself registers through the `grammar` seam. So for a single plugin
the loader must drain `grammar` before `modes`, or every org binding
skips — logged, but silently as far as the user is concerned. This is
checked first (slice OM.0) because everything downstream assumes it.

## 4. Mode decomposition

The plugin owns its functionality through **four modes**, each owning its
full surface — keymap *and* handler bodies, per the standing rule. A mode
that publishes data while the host binds its chords would be a
half-migration and is the failure mode this decomposition exists to
prevent.

| Mode | Kind | Activation | Owns |
|---|---|---|---|
| `org-mode` | major | `target-language = "org"` | headline motions, `ih`/`ah`/`ir`/`ar` text objects, promote/demote, subtree move, meta-return, toggle heading, archive, links, refile, capture, `<Tab>` on a headline |
| `org-todo-mode` | minor | `majors = ["org-mode"]` | TODO keyword cycling, priority, tags, checkboxes + statistics cookies, timestamps |
| `org-table-mode` | minor | `majors = ["org-mode"]` | alignment, cell and row motion, row/column insert and move |
| `org-agenda-mode` | minor | manual — the provider activates it on the view, named by the source's `view-mode` export | TODO change from the agenda |

### 4.1 Why these four and not one, or ten

The test is not "is this feature self-contained" — most are. It is
whether another major would want the behaviour, per the
minor-mode-over-duplication rule.

- **Tables** are the clearest yes. A markdown buffer wants the same
  `<Tab>`-aligns-and-advances editing, and the day that lands,
  `org-table-mode`'s activation policy grows a major rather than
  markdown growing a copied keymap. The mode is named `org-table-mode`
  today because its syntax is org's; generalising means renaming, not
  restructuring.
- **TODO workflow** groups the surface that operates on a headline's
  *metadata* rather than its structure — and checkboxes (`- [ ]`) exist
  in markdown too. Same argument, one step weaker.
- **Agenda** is a different buffer with a different keymap; putting its
  chords on `org-mode` would fire them in ordinary org files.
- Everything else stays on `org-mode`. Minting `org-link-mode` and
  `org-timestamp-mode` would be modes-per-feature, which is the same
  error as crates-per-feature.

### 4.2 The agenda view carries TWO minors, and the split is not arbitrary

The agenda view is a multibuffer. `multibuffer-mode` is its major
(`target_buffer_kind = Multibuffer`), and a provider contributes a
**minor** activated on the view — `ProjectSearchMode` is `ModeKind::Minor`
and the search provider activates it with `activate_minor_by_id`
(`providers/search.rs:912`).

**Amended at OM.A3.** This section originally gave `gr` refresh and
jump-to-source to `org-agenda-mode` along with the TODO change. The
plugin cannot have the first two, and the reason is structural rather
than a matter of taste: refreshing the agenda means re-running the
**host's** walk, which is `AppEffect::OpenProviderView` — and that
effect's plugin surface is deliberately withheld
(`boundary_app_effect.rs`) pending the capability model for which
providers a plugin may trigger. A plugin `gr` could bind the chord and
not do the work.

It is also the better split on merit. Refreshing a host-built view is
host machinery, and the second agenda-source plugin — the markdown TODO
scanner the whole `extensions()` design exists for — inherits `gr`
rather than re-deriving it. Re-derivation is the copied-keymap failure
the minor-mode rule forbids, one layer up.

So the view carries two minors and each owns its full surface:

- **`agenda-view-mode`** (native, `lattice-multibuffer`, beside the
  provider — the `ProjectSearchMode` shape verbatim): `gr` through
  `refreshable-view-mode`'s cascade, with the refresh body in the same
  crate. Jump-to-source comes free from `MultibufferMode`.
- **`org-agenda-mode`** (the plugin's): the TODO chords and their handler
  bodies, on org's own rows.

Neither is a half-migration: each holds both the binding and the body of
what it claims.

**How the host activates a mode it cannot name.** No `ActivationPolicy`
can express "the buffer this provider just built" —
`majors(["multibuffer-mode"])` would fire org's chords in project-search
results and magit diffs. So the `agenda-source` world gained one export,
`view-mode: func() -> option<string>`: the source names a minor, and the
provider activates it on the view. The host learns a mode id and never
learns what its chords do. This is the ABI addition §"Why the agenda is
last" reserved the right to make once, informed by what org turned out to
need.

### 4.3 The decline chain

`<Tab>` is bound by two org modes and one builtin. Minor layers rank
above major layers, and `org-table-mode` is active in every org buffer,
so its binding is reached first:

```
<Tab>  →  org-table-mode : in a table?    align + next cell
                            else          [declined]
       →  org-mode       : on a headline? cycle (AppEffect::CycleFoldAtCursor)
                            else          [declined]
       →  Builtin        : jump-list forward
```

Two hops. If `Declined` did not chain past more than one layer, the
decomposition would collapse — one mode would have to own every
`<Tab>` meaning, and `org-table-mode` would stop being separable. So the
chain is a **tested property**, not an assumed one (OM.5). `<C-a>` /
`<C-x>` decline the same way past `org-todo-mode` to the builtin
increment.

The cost is honest and benched: every `<Tab>` in an org buffer costs a
guest round-trip even when it does nothing. It is budgeted under the
existing grammar gate (§8).

## 5. The keymap

### 5.1 Convention, and where lattice's dispatcher refuses it

The standing UX rule says lead with cross-editor convention, and the
precedent is `magit-keys-follow-evil-magit`: follow the **vim
community's port**, not the emacs original. The org analogue of
evil-collection-magit is **nvim-orgmode**, and it is the baseline.

Several of its chords cannot be expressed here, and the reason is
structural rather than incidental. `KeymapTrie::lookup` returns `Bound`
the moment the walk lands on a node carrying a terminal binding
(`trie.rs:157`). `>`, `<` and `c` are each a *terminal* Normal binding to
an operator (`keymap_normal.rs:909-920`); vim's doubled forms are
operator-pending bindings (`keymap_normal.rs:1146-1154`), not two-key
paths in Normal. And `binding-mode` in the WIT deliberately excludes
operator-pending — *"internal grammar states, not plugin-bindable."*

So `>>`, `<<`, `>s`, `<s`, `cit` and `ciT` would be **dead bindings**:
`>` / `<` / `c` fire first, every time.

Three ways out were considered.

- **Shadow the operators** — bind `<`, `>`, `c` as terminal actions at
  the org layer. Rejected: org buffers would lose the indent and change
  operators outright. No `ciw`, no `>ap`. That trades a paramount goal
  (#3, strict vim semantics) for muscle memory in one filetype.
- **Open operator-pending to plugins** — lift the `binding-mode`
  exclusion. Rejected for now: it exposes an internal grammar state the
  WIT closes on purpose, and `cit` would additionally need a text object
  named `t`, which org has no claim to.
- **Move them into `<leader>o`, and add text objects** — chosen.

### 5.2 The set

Reachable nvim-orgmode chords are kept verbatim:

```
]]  [[         next / prev headline
g{             parent headline            (native zp also works)
<Tab>          cycle subtree              (native z<Space> also works)
<S-Tab>        global cycle               (native z<Tab> also works)
<C-Space>      toggle checkbox
<C-a> <C-x>    timestamp component up / down
<leader>oa     agenda          <leader>oc  capture
<leader>or     refile          <leader>oo  open link at point
<leader>oK oJ  move subtree up / down
<leader>o$     archive subtree <leader>o,  priority
<leader>o'     edit src block
<leader><CR>   meta-return
```

The unreachable ones move into the same prefix, using evil-org's
directional letters so the mnemonic survives:

```
<leader>oh  ol   promote / demote headline      (nvim: << >>)
<leader>oH  oL   promote / demote subtree       (nvim: <s >s)
<leader>ot  oT   TODO cycle forward / back      (nvim: cit ciT)
<leader>o:       set tags                       (nvim: <leader>ot)
```

One deviation beyond necessity: nvim-orgmode's `<leader>ot` is *tags*.
TODO cycling is the more frequent verb and `t` the stronger mnemonic for
it, so tags move to `<leader>o:` — which reads as `:tag:`. Documented in
`:help org` so a nvim-orgmode user is told rather than surprised.

`<Tab>`, `<S-Tab>`, `<C-Space>`, `<C-a>` and `<C-x>` shadow native
bindings **inside org buffers only**. That is not new: `lattice-magit`
already binds `<Tab>` / `<S-Tab>` / `]]` / `[[` mode-locally.

### 5.3 Text objects, which are the better half of the trade

The chords that could not be transplanted have a more vim-idiomatic
replacement than the `<leader>o` slots they landed in. Org registers text
objects through `grammar`'s `register-text-object`:

```
ih  ah    headline (inner / around)
ir  ar    subtree  (inner / around)
```

(Corrected during OM.4: this fragment first said `is`/`as` for subtree, but
`s` is already vim's **sentence** object and org has no business shadowing
it. nvim-orgmode itself uses `ir`/`ar` — following the convention we already
chose fixes the collision rather than creating one.)

and the **ordinary operators compose** — `dar` deletes a subtree, `yah`
yanks a headline, `>as` indents one, `gcas` comments one. No org-specific
chord is involved in any of those. This is paramount goal #3 working as
designed: the grammar is the public API, and a plugin extends the
vocabulary rather than bolting a parallel command set beside it.

## 6. The agenda

### 6.1 It is a multibuffer, literally

`Excerpt { source: BufferId, start_line, end_line, header }` is what an
agenda row is. The agenda is excerpts of headline lines drawn from many
files — which is the search provider's shape with a different predicate.

Taking that seriously buys, from machinery that already ships and is
tested: jump-to-source, **edit-propagates-to-source**, headerline async
status, stale-source handling, and refresh. The second of those is the
one that decided it. Org's agenda is a place you change TODO states and
reschedule from, and those edits hit the file. An agenda you can only
read is a lesser feature wearing the name.

The grouping question — agenda groups by *date across files*, multibuffer
headers are per-file — resolves without touching the excerpt model:
`view.append_excerpts` is insertion-ordered with the provider choosing
the order, `ExcerptHeader.title` is a free string, and an empty title
renders no header row. A date group is therefore "title on the first
excerpt, `""` on the rest".

### 6.2 The seam follows `error-parser`

The host must read each file anyway to build the source `Document`
(`providers/search.rs:657-690`: `spawn_blocking` read, `DocumentBuilder`,
`spawn_document`, `view.add_source`). So it reads once and hands the text
over, rather than the guest reading it a second time through WASI.

```wit
interface agenda-source {
    /// One agenda row the guest recognised in a file.
    record entry {
        /// 0-based line of the headline, `error-parser`'s convention.
        line: u32,
        /// Last 0-based line of the excerpt, inclusive.
        end-line: u32,
        /// Grouping KEY. Rows that sort adjacently and share a key render
        /// under one header — how a date group shows one header for N
        /// rows drawn from N files.
        group: string,
        /// The header title, used when this row starts a group.
        label: string,
        /// Host stable-sorts across files on this. The guest owns what
        /// it means (an epoch day, a priority rank).
        sort-key: s64,
    }
}

world agenda-source-plugin {
    import agenda-source;
    import logging;
    import project;

    /// File extensions this source wants offered, without the dot.
    /// Called once at load. See "the host does not know what an org
    /// file is", below.
    export extensions: func() -> list<string>;
    /// AF.1: the paths to scan — each a FILE or a DIRECTORY. Called PER
    /// SCAN, unlike `extensions`: this comes from user configuration
    /// (org reads `org.agenda-files`) and has to follow a `:set`.
    /// Empty = "no opinion", and the host scans the project root as
    /// before. The world imports `config` so a source can answer it.
    export roots: func() -> list<string>;
    /// Drop per-scan state. Called before the first file of a scan.
    export begin: func();
    /// Scan one file; return its agenda rows.
    export scan: func(path: string, text: string) -> result<list<entry>, string>;
}
```

Host: walk (bounded, `fs:read`-gated), read off-thread, `scan` per file,
stable-sort by `sort-key`, append excerpts, publish
`MultibufferExcerptsReady`, drive the headerline. Guest: everything org —
which headlines are agenda-worthy, TODO / `SCHEDULED:` / `DEADLINE:`
parsing, date arithmetic, grouping, ordering.

The guest touches no filesystem: no WASI preopens, no `walk` capability.

**`group` is a key, not a label** — the amendment OM.A1 made to the shape
first sketched here, where the two fields were redundant and `group` was
documented as "empty = same as the previous entry". A guest cannot know
which of its rows will land first once every other file's rows are
interleaved by the sort, so it cannot decide which one carries the header.
The host compares keys *after* sorting and titles the first row of each
run; the rest get an empty `ExcerptHeader.title`, which renders no header
row. §6.1's grouping mechanism is unchanged — only who decides.

**The host does not know what an org file is.** The sketch above once said
the walk was "`.org` only", which contradicts §11's own claim that every
host change here is generic. `extensions` is what fixes it: the source
declares what it wants offered, resolved once at load and cached beside
the producer, so the walk's per-file test is a string compare. A markdown
TODO scanner then appears in the same view with no host change at all.

Two alternatives were rejected. **Offer every project file to every
source** — one boundary crossing carrying the full text of every file in
the tree, which is precisely the producer-critical-path cost §8 warns
about. **Resolve the extensions from the plugin's `language` seam** (the
`PluginLangRegistry` already indexes `by_extension`) — it would make an
agenda source *require* a language seam when the two are independent
contributions.

**One bad file must not fail the agenda**, so `scan` returns a `result`
and an `err` skips that file with a `debug` log while the walk continues —
`error-parser`'s rule, because it is the same failure class. `begin`
failing is different: that source's per-scan state is then unknown, so it
is dropped from *this scan* and the others carry on.

### 6.3 Rejected alternatives

- **A generic plugin-`view` seam** extending `dashboard`'s
  rows-and-spans fragment. Read-only plus links; acting *from* the
  agenda would need a separate write path, so org's core agenda verb
  would be re-derived rather than inherited.
- **Mode lifecycle + owner-write** — unblock `on-activate` for plugin
  modes and give a mode a write handle to its own buffer. The most
  general answer and the most faithful to mode ownership, but it is two
  new mechanisms, and the agenda would then hand-roll grouping,
  jump-to-source and refresh that multibuffer already has.
- **A picker-source** — ships today with zero host work, and is honestly
  goto-TODO rather than an agenda: no date-grouped view, no acting from
  it.

The rejected options are not wrong so much as differently scoped; if the
`view` seam or mode lifecycle lands for another reason, nothing here
blocks it.

## 7. Links

A link is org's only construct that is simultaneously *markup to be
hidden* and *a thing to be activated*. Both halves are org's, and
neither is roam's — an org file with no roam index in sight still wants
`[[file:diagram.png][the wiring]]` to read as three words and to open
on `<CR>`.

### 7.1 Rendering, which is a host primitive org merely configures

Links render through [`conceal.md`](conceal.md). Org contributes two
rules to the `language` seam and contributes nothing else; the
mechanism, the coordinate maths and the mode scoping are the host's.

```
(\[\[[^]]+\]\[)[^]]+(\]\])     hide [1, 2]   described link
(\[\[)([^]]+)(\]\])            hide [1, 3]   bare link
```

**A bare link keeps its target visible.** `[[https://example.com]]`
renders as `https://example.com`, not as nothing. Emacs draws the same
line, and the reason is not deference: a link whose only text *is* its
target has nothing left to show once the target is hidden, and an
invisible activatable region is worse than visible markup.

**Why patterns and not the parse tree**, which is the question a reader
arriving from §6 will ask, because everything else in this plugin was
migrated *onto* the tree. Two independent answers. `tree-sitter-org`
has no `link` rule — `[[id:X][Title]]` is undifferentiated `expr`
tokens inside `item` or `paragraph`, so there is nothing to capture.
And the tree is absent during a reparse, so tree-driven conceal would
flicker between concealed and raw while the user types: a pixel change
to content they did not edit, which is a standing veto. `links.rs`
already recorded that second reason for its own text scanning, and it
is the same reason here for the same construct.

This is not a retreat from "structure from the tree, characters from
the text". It is that rule applied honestly: a link has no structure in
this grammar, so there is none to read.

### 7.2 Following, and why `<CR>` is safe here

`<CR>` opens the link under the cursor and declines otherwise, through
the same chain §4.3 describes and tests:

```
<CR>  →  org-mode  : on a link?  open it
                     else        [declined]
      →  (nothing, today — see below)
```

**An earlier revision of this section put "Builtin: first non-blank of
the next line" on that last row. That was vim, not lattice.** `<CR>` is
unbound in Normal mode for a Document buffer: `keymap_normal.rs` binds
it only as the `z<CR>` suffix, and `input.rs` routes a bare `<CR>` to
`Action::FollowLink` only for Help, Dashboard, Oil and FileTree
buffers. Lattice has no equivalent of vim's `+` / `<CR>` motion, which
is a real gap in the vim grammar and a separate piece of work from
this one.

So today the decline is observationally a no-op, and a test cannot
distinguish it from `Effect::None` — the same limitation OM.5 records
for `<Tab>`. It is still the right answer for two reasons a test cannot
see: it is the honest one, and org composes for free the day `<CR>`
gains a Document-buffer meaning.

**Two actions, not one, and this is forced rather than chosen.**
`org-open-link` (bound to `<leader>oo`) must answer `Effect::None` on a
miss; `org-follow-link` (bound to `<CR>`) must answer
`Effect::Declined`. The vocabulary has no way to say "it depends", and
the reason they differ is the standing hazard: `Declined` re-runs a
multi-key chord's *trailing key alone*, so declining from `<leader>oo`
would fire bare `o` — "open a line below and enter Insert". A missed
link would start editing the buffer. `<CR>` is a single key, so there
is no trailing key and no hazard.

`<leader>oo` stays. It is the explicit form, it works when the cursor
is not inside the link's span, and removing a working chord to make
room for a new one costs a user's muscle memory for nothing.

The cost is one guest round-trip per `<CR>` in an org buffer even when
the cursor is nowhere near a link — the same honest cost §4.3 already
accepts for `<Tab>`, budgeted under the same grammar gate (§8).

### 7.3 `id:` is recognised before it is resolvable

`Target` gains an `Id` arm. Nothing in org can resolve it: an `:ID:` is
a key into a corpus, and finding the file holding it means an index,
which is [`org-roam.md`](org-roam.md)'s subject.

So org ships `id:` as a **recognised kind that fails honestly** —
`<CR>` on `[[id:6F398E54-…]]` says there is no index rather than
silently doing nothing. That is deliberately not the same as leaving
`id:` unclassified, which would make it fall through to the file
branch and produce "no such file: id:6F398E54-…", an error that blames
the wrong thing and sends the user looking for a file.

## 8. Performance

Paramount goal #1. Org adds guest calls to the keystroke path, and the
budget is the existing grammar gate — **typed call < 500 ns p99,
grammar-extension round-trip < 5 µs p99**, measured at ~340 ns release
(PH7.7d).

Benched:

- Org's `apply-action` round-trip for promote / demote / TODO cycle.
- **The `<Tab>` decline path**, specifically. It is the one org path
  that costs a guest call on keystrokes that do nothing, and it fires
  twice per press through the §4.3 chain. If anything here threatens the
  gate, it is this.
- Agenda scan throughput per file. Off the keystroke path, but on a
  producer's critical path — a guest that blocks in `scan` backs up the
  agenda the way a slow `error-parser` backs up a build.

Parse cost is already recorded and needs no new work: a wasm grammar
parses **2.0× cold, 1.25× incremental** against native, flat across file
size (LG.1, `benchmarks.md`).

Nothing org does runs per frame. The renderer reads folds and highlights
from caches that already exist; `no_per_frame_wasm_guard` continues to
hold.

## 9. Failure behaviour

Every path degrades the way the seam it rides already does — which is the
point of riding them.

- A guest action returning `err` is logged at `debug` and the
  contribution is a no-op. The buffer is untouched.
- A fuel or epoch trap is caught, the plugin quarantines, and the chord
  no-ops. Never a hang on the keystroke path.
- A malformed org file during a scan is skipped with a `debug` log and
  the scan continues. **One bad file must not fail the agenda** —
  `error-parser`'s rule, because it is the same failure class.
- A trap mid-scan quarantines the plugin and leaves the agenda showing
  what it collected, with the headerline saying it stopped.
  Partial-and-honest beats empty-and-silent.
- An unparseable chord or unknown command in a mode declaration skips
  that one binding, logged — already `mode-keymap-binding`'s contract,
  and the reason §3.4 is a gate.
- Unloading org removes the language, the grammar contributions, all
  four modes, the keymap layers, the help topics and the agenda
  provider. The multi-seam teardown fix (`PluginTeardown::seam_ids`) is
  what makes that true, and it was found by this plugin.
- Diagnostics are `debug!`, never `info!`. A per-`<Tab>` decline at
  held-key rates would flood `*messages*`.

## 10. Scope

**In:** structure editing, text objects, TODO workflow, priority, tags,
checkboxes with statistics cookies, timestamps, links, refile, capture,
agenda, tables (alignment, cell/row motion, row and column insert and
move).

**Out, as cuts rather than omissions:** export backends, babel / source
execution, table formulas, column view, org-roam.

**Was blocked on a host primitive, now built:** archive, refile and
capture all move text into a file other than the buffer's own, and no
effect could. [`cross-file-writes.md`](cross-file-writes.md) is the
answer — a host-mediated `write-to-file` effect gated on `fs:write`.
Two more were needed and neither was foreseen: `document.path()`
(OM.6b.0), because an archive's target is derived from the source file's
own name; and a picker source able to invoke an ACTION with typed args
(OM.11.0), because refile picks a target and only THEN reads the subtree
at the cursor — and the ex-line route a picker had is closed to actions
and hands an ex-command no document. All three ship.

**Clocking ships** (OC.1–OC.11). It was deferred here for needing
"persistent 'currently clocked' state and a modeline contribution", and
the first half of that turned out to be wrong in a useful way: **there is
no persistent state.** An unterminated `CLOCK: [start]` with no `--end`
*is* a running clock, so the buffer is the whole record — clock-out and
clock-cancel re-derive their target structurally, and clocking out works
on a clock started before the editor was last opened. Guest state exists
only to feed the modeline and to remember the last clocked entry for
`:org-clock-goto` / `:org-clock-resume`, and losing it costs a segment,
never a fact.

The modeline half was real, and it closed a gap rather than consuming
one: plugins had no way to contribute a modeline element at all, so
clocking landed ML.6 (the `ui` seam) on the way past.

What clocking exposed is worth more than the feature. Three seams
promised something they could not deliver, each invisible from the guest
side: a grammar action could call `emit-event` and be silently dropped
(OC.1); a plugin could not be woken at all, so anything periodic waited
for a keystroke (OC.2); and an ex-command was handed no cursor and no
buffer id while still being offered an `apply-edit` effect it had no way
to construct (OC.10). Each is the same shape — a seam wired end to end
that answers nothing — and none was reachable by a test that built its
own context.

`<leader>o'` ships as a narrow
to the block body (`AppEffect::NarrowLines`); a true indirect buffer in
the block's own major mode is post-v1.

## 11. Paramount-goal alignment

**#1 Performance.** Every org path is either off the keystroke path
(agenda) or inside the measured grammar budget (actions). The decline
chain is the one new per-keystroke cost and it is benched by name. No
per-frame work is added.

**#2 Extensibility.** This is the goal org exists to test. The editor
learns headlines, TODO keywords, agenda scheduling and table alignment
without a line of org in the host. The three host changes (§3) are all
*generic* — a language index, a WIT field, a lifted restriction — and
none names org.

**#3 Vim modal editing.** Org extends the grammar rather than escaping
it: text objects that compose with every existing operator, motions that
compose with counts and operators, and a keymap that refuses to shadow
`c` / `<` / `>` even though that would have been the easy way to match
nvim-orgmode's chords.

**#4 Asynchronicity.** The agenda scan is off-thread by construction —
the host reads with `spawn_blocking` and the guest runs in the seam's
async actor task. Results reach the screen through
`MultibufferExcerptsReady`, an event with a wake already wired
(`boot.wake_on_event`), so no keypress is needed to see them.
