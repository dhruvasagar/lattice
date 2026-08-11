# Error list

Authoritative design for Lattice's **error list**: a single,
editor-wide, navigable list of source locations (`file : line : col`
+ message + severity) that the user steps through with `:next-error` /
`:previous-error` / `]qq` / `[qq`. This is vim's **quickfix** list,
rebuilt as a **core substrate** decoupled from any producer or buffer —
the user-facing name is "error list"; the vim `:c*` command family and
the `q`-chords are preserved as aliases for muscle memory.

Companion to `compilation-mode.md` (the first producer) and
`design.md` §5.1.1 (position history — a sibling core navigation
substrate). Sequencing lives in
`../operations/slice-plans/compilation-mode.md` (slices CM.2, CM.7, CM.8)
and, for the multi-producer work of §3.1–§3.3,
`../operations/slice-plans/error-list-producers.md` (EP series).

## 1. It is core state, not mode-owned

The error list is read by **generic host dispatch** — the `:next-error`
family and the `]qq` / `[qq` chords — uniformly, regardless of which
buffer is focused or what produced the list. By the substrate-vs-mode
rule (uniform-host consumer ⟹ core, not mode) it is **core `Editor`
state**, shaped like `position_history`:

```
ErrorEntry { path: PathBuf, line: u32 /*0-based*/, col: u32 /*0-based*/,
             severity: ErrorSeverity, message: String }
ErrorList  { entries: Vec<ErrorEntry>, index: usize }
ErrorSeverity { Error, Warning, Info, Note }
```

`ErrorEntry` + `ErrorSeverity` live in **`lattice-protocol`** (so
producers *below* `lattice-host` can construct them and they can ride
inside a typed `AppEffect`); `ErrorList` lives on `Editor` in
`lattice-host` (the `error_list` field) with `set_error_list(entries)`
(replace + reset index) and `error_list() -> &ErrorList`.

This placement is load-bearing: a mode-private list (owned by
`compilation-mode`) would (a) vanish when the producing buffer closes,
and (b) block any *other* producer — project search, future tools —
from ever feeding the same navigation. Core state lets many producers
share one list and one set of motions.

## 2. Navigation is buffer-independent (the vim model)

Once populated, the list is walked from **anywhere** — the producing
buffer (e.g. [`*compilation*`](compilation-mode.md)) need not be
focused or even open, and closing it does **not** clear the list (only
a new run's replace does). This deliberately follows vim, not emacs
(where next-error lives on the compilation buffer). It falls out of §1
for free: the state is on `Editor`, the commands are generic.

### Commands

Readable, emacs-style canonical names lead; vim `:c*` spellings are
aliases (`lattice-host::excommand`). Each steps via the canonical
`Editor::jump_to_file_line_col(path, line, col)` — which records the
jump in position history (§5.1.1):

| Command | Vim alias | Chord | `ErrorTarget` |
|---------|-----------|-------|---------------|
| `:first-error` | `:cfirst` `:cr` | `[Q` | `First` |
| `:previous-error` | `:cprev` `:cp` | `[qq` | `Prev` (wraps) |
| `:previous-error-file` | `:cprevfile` `:cpf` | `[qf` | `PrevFile` (wraps) |
| `:next-error` | `:cnext` `:cn` | `]qq` | `Next` (wraps) |
| `:next-error-file` | `:cnextfile` `:cnf` | `]qf` | `NextFile` (wraps) |
| `:last-error` | `:clast` | `]Q` | `Last` |
| `:error [N]` | `:cc [N]` | | `Jump(Option<usize>)` (1-based) |

`NextFile` / `PrevFile` land on the first entry of the next / previous
**file** — a "file" is a maximal run of consecutive entries sharing a
path (producer output groups a file's locations together). Both
directions land on the *first* entry of the target file (deliberately
unlike vim's `:cpfile`, which lands on the *last* of the previous
file — first-of-file lets a following `:next-error` walk it
top-to-bottom).

**Command vs. chord letters differ by design** — as in vim itself
(command `:cnext`, unimpaired chord `]q`). The commands lead with the
emacs `next-error` vocabulary; the chords keep vim-unimpaired's `q`
(the universally-known quickfix chord): **`[`** backward / **`]`**
forward, doubled **`qq`** one entry, **`qf`** a whole file, capital
**`Q`** the extremes. `[q` / `]q` are therefore prefixes (not bound
directly). `e`/`E` were *not* chosen for the chords — they are taken by
`lattice-multibuffer`'s excerpt / file-boundary navigation. All chords
are **Builtin** grammar (universal navigation over a core substrate,
like `<C-o>` / `<C-i>` over the jump ring), firing in any buffer.

Navigation flows through `AppEffect::ErrorNav { target }` → the host
`do_error_nav` handler; the ex-commands are thin front-ends emitting
that effect.

### No diagnostic fallback

An empty error list echoes `no error list` for **every** target —
error-list commands touch only the error list (vim's `E42: No Errors`).
An earlier empty-list fallback-to-diagnostics was removed (CM.7) as a
boundary blur: one command must not silently mean two different things
depending on whether the list happens to be populated.

This holds unchanged now that the language server *is* a producer
(§3.2), and it is what makes that reversal safe. `:cnext` always walks
the error list — never "the error list, unless it is empty, in which
case the current buffer's diagnostics". Diagnostics keep their own
dedicated navigation: `[d` / `]d` (current-file, mode-owned by
`lsp-diagnostics-mode`) and the `:diagnostics` picker. Being a
*producer of* the shared list and being *reachable through* a separate
per-buffer motion are orthogonal; CM.7 rejected conflating the
motions, not sharing the list.

## 3. Producers feed it over a native seam

A producer builds `Vec<ErrorEntry>` and hands it to `set_error_list`
**tagged with its own source** (§3.1). Producers below `lattice-host`
(e.g. `lattice-compilation`) deliver off-thread via the native
`InboundBus → AppEffect::SetErrorList { source, entries }` seam (the
host arm calls `set_error_list`) — the same transport pattern LSP uses
for its own async host-state updates, **not** any plugin path.

The first producer is [compilation mode](compilation-mode.md), whose
four built-in parsers (cargo/rustc, gnu-style, a `file:line:col`
catch-all, and a Rust test/`panic!` matcher) turn any CLI tool's
`file:line:col` output into entries — see
[`compilation-mode.md`](compilation-mode.md) §5 for the parser detail.
Note that **both** the compiler's stderr *and* the process's stdout are
parsed, so `cargo test` panics (which print `thread '…' panicked at
path:line:col` on stdout) populate the list alongside compiler and
linter diagnostics. The second is the language server (§3.2). The list
is producer-agnostic by construction: project search (and other tools)
can feed the identical list later with zero change to the navigation
above.

### 3.1 Sources are tagged; a write replaces one source's slice

`set_error_list(entries)` originally replaced the **whole** list. With
more than one producer that is a clobber: the language server
republishes on every edit-debounce, so a live diagnostic feed would
overwrite a compile run's entries *while the user is walking them*.

Entries therefore carry a **source tag**, and a write replaces only
that source's slice:

```
ErrorSource { Compilation, Lsp }        // in lattice-protocol
AppEffect::SetErrorList { source, entries }
ErrorList { slices: Vec<(ErrorSource, Vec<ErrorEntry>)>, index: usize }
```

A producer never sees the other slices; it hands over its own full set
and the list splices it in. `ErrorSource` is a small closed enum today
— a plugin-producer variant lands with the plugin path (§6), not
speculatively.

**Slices concatenate in a fixed source order (`Compilation`, then
`Lsp`); order *within* a slice is the producer's own.** Producer order
is information — rustc emits the root cause before the cascading
errors it caused — and sorting the merged list by path would destroy
it.

§2's file-grouping rule ("maximal run of consecutive entries sharing a
path") applies to the **concatenation**, not per slice, which makes the
cost smaller than it first appears. When both producers flag the same
file their entries land *adjacent* across the slice boundary, so they
form **one** file group and `:cnextfile` does not double-visit.

The double-visit is real but narrower: it happens when a path is
**non-contiguous** in the flat view — e.g. compilation reports
`same.rs` then `other.rs`, and the language server also reports
`same.rs`, giving three groups. That is accepted. Both tests are
pinned in `error_list.rs` so the boundary between the two cases stays
explicit.

### 3.2 The language server is a producer, under user policy

`lattice-lsp` subscribes to its own `publishDiagnostics` broadcast,
maps `DiagnosticSeverity` onto `ErrorSeverity` (the mapping the
`ErrorSeverity` doc-comment always anticipated: *"producers map their
own severity onto this small set"*), and publishes the workspace set
through the same `InboundBus → AppEffect::SetErrorList` seam
compilation uses. All of it lives in `lattice-lsp`; the host learns
nothing about LSP.

Whether that feed is live is **user policy**, not a design bet:

| | |
|---|---|
| **Option** | `lsp.diagnostics-to-error-list` — bool, default `true` |
| **Command** | `:lsp-diagnostics-to-error-list` — one-shot snapshot into the `Lsp` slice |

- `true` — every publish refreshes the `Lsp` slice (debounced, §3.3).
  The command still works, as a forced refresh after a server restart.
- `false` — publishes do not touch the list. The diagnostics cache
  still updates, so `[d` / `]d`, inline end-of-line text and the
  signcolumn are unaffected. The command pulls a snapshot on demand.
- Toggling `true → false` **stops the feed; it does not clear the
  slice.** Turning the option off must never destroy what the user is
  currently reading.
- Toggling `false → true` takes a snapshot immediately rather than
  waiting for the next edit, so the list matches the setting at once.
  Toggle-on and the command are one function with two callers.

The option lives in the `lsp` group, not `diagnostics`: that group is
*presentation* (`ui.diagnostics.inline`, `…inline-min-severity`), and
this is producer behaviour. It also keeps the option namespace
symmetric with the command's.

**Scope, stated honestly:** this surfaces what servers have
*published*, which is not a workspace scan. rust-analyzer publishes
workspace-wide after a check; other servers publish only for open
files. The command echoes the entry count so the user is not misled
into reading an empty result as a clean tree.

### 3.2b References are a third producer, opt-in

Reference sites are not errors — no severity, no diagnosis. But the
error list is a *navigable set of source locations*, and fifteen call
sites is exactly that. Vim's quickfix culture has always treated the
list as the universal result sink (`:cexpr`, `:grep`, `:cfile`), and
§3.1's tagged slices mean references can join without touching anyone
else's entries.

| | |
|---|---|
| **Option** | `lsp.references-to-error-list` — bool, default **`false`** |
| **Command** | `:lsp-references-to-error-list` — query at the cursor, push the result |

**Default off, unlike diagnostics' default on.** Diagnostics *are*
errors and belong in a list called the error list; turning them on
changes nothing about what the list means. References would: someone
walking compile errors with `]qq` should not have that set silently
grow every time they look up a symbol. Opt-in is the honest default for
a producer whose entries are not problems.

**The manual command runs the query; it does not snapshot.** Unlike
diagnostics, there is no standing "current references" state to pull
from — a references result exists only as the answer to a query. So the
command is a **third terminus** on the drain §17 already routes:

	gr                            → picker
	:lsp-references               → multibuffer
	:lsp-references-to-error-list → error list

The option is orthogonal to the terminus: when on, *any* references
query also pushes to the `References` slice, whatever surface it was
headed for. Severity is `Info` — the list wants one, and a reference is
informational, not a problem.

Each query is an `ErrorWrite::NewRun`: a fresh question deserves a
fresh answer at the top of the list, unlike the diagnostics feed's
continuous refresh.

**This reverses §17's rejection**, which read: *"references aren't
errors, have no severity, and hijacking `]qq` for them would collide
with a live compile list."* Two of those three still hold and are
answered rather than dismissed — the severity is chosen explicitly
above, and the collision is what EP.1's per-source slices removed: a
references push replaces only the `References` slice, so a compile run
survives it. What remains is the taste question of whether they belong
in that list at all, and the option is the answer: the user decides,
and the default says no.

### 3.3 Two properties that make a live feed safe

Without both of these, a default-on feed is worse than no feed.

**Coalesce.** `publishDiagnostics` arrives per-URI at edit-debounce
rate. The subscriber keeps the workspace map and pushes a rebuilt
`Vec<ErrorEntry>` on a short idle debounce (~250ms) — never one push
per notification. Per-keystroke `Vec` rebuilds crossing the inbound
seam are exactly the background churn paramount goal #1 forbids.

**Re-anchor the index.** `ErrorList::set` reset `index` to 0, which is
correct for a fresh compile run and wrong for a refresh: the user
walking entry 7 would be thrown back to entry 1 every time they typed.
A slice write re-points the index at the **same entry** — matched on
`(path, message)`, tolerant of line drift — and falls back, in order,
to the first entry of the same path at-or-after the old line, then to
0. Producer-initiated replacement (a new compile run) keeps the reset.

## 4. Three views of one list

The entries are worked three ways, all reading the same `ErrorList`:

- **Step** — `:next-error` / `]qq` … (§2), one entry at a time.
- **Pick** — `:error-list` / `:cl` opens a **fuzzy picker** of the flat
  list (`Editor::do_list_errors`, host-side), reusing the shared
  `PickerSource::LspLocations` + `JumpToLspLocation` accept path — the
  exact mechanism `:diagnostics` (`do_list_diagnostics`) uses, rows
  sourced from `error_list().entries()` instead of the diagnostics layer.
- **Group** — `:problems` (`*problems*`) renders the entries as a
  grouped, editable [multibuffer](multibuffer-views.md) — one excerpt
  per entry under each file's header, edits propagating to source. See
  `compilation-mode.md` §4 for the provider.

The picker and problems-view are the browse surfaces; the `:next-error`
family is the step surface. All three are views of one list.

## 5. Paramount-goal alignment

- **#1 Performance.** Navigation is O(1) list indexing + the existing
  (benched) `jump_to_file_line_col` path; population is off-thread
  (producer side). No UI-thread work. The live diagnostic feed is
  coalesced on an idle debounce (§3.3) so a fast typist does not drive
  `Vec` rebuilds across the inbound seam.
- **#2 Extensibility.** The tagged-slice write (§3.1) is what a
  third-party producer needs: the plugin boundary currently hard-
  refuses `AppEffect::SetErrorList` partly because an untagged write
  cannot be scoped to its author. Tagging is the precondition for
  lifting that.
- **#3 Everything-is-a-buffer.** The list is generic core state (like
  the jump ring); the `*problems*` view is a plain `Multibuffer`
  Document — no kind-branching.
- **#4 Asynchronicity.** The producer reaches the screen over
  `InboundBus`, whose `send` bakes in the `async_landed` wake — so a
  diagnostic republish repaints without waiting for a keystroke. This
  is mandatory, not stylistic: a bare `TickCallback` here would
  reproduce the "it only updates when I press something" bug class
  `boot-composition.md` §3 exists to design out.

## 6. Rejected alternatives

- **List owned by `compilation-mode`.** Rejected: consumer is generic
  host dispatch, so it is core state (§1); a mode-private list dies
  with its buffer and monopolises the navigation for one producer.
- **List tied to the producing buffer (the emacs model).** Rejected:
  breaks the vim contract that you can `:next-error` from anywhere after
  the buffer is gone — and Lattice's producers (compilation today,
  search later) are many-to-one against the list.
- **Entry type in `lattice-host`.** Rejected: producers below the host
  must construct entries and they ride inside a typed `AppEffect`, so
  the type belongs in `lattice-protocol` (below both).
- **`:d*` diagnostic vocabulary / `e`-prefix chords.** Rejected: kept
  diagnostics on their existing `[d`/`]d` + `:diagnostics` (no parallel
  list), and kept the `q`-chords since `e`/`E` collide with multibuffer
  excerpt navigation and `q` is the recognized quickfix chord.
- **Sorting the merged list by `(path, line)`.** Rejected (§3.1):
  reads tidier, but destroys producer order, and producer order carries
  meaning — rustc emits the root cause ahead of the errors it cascades
  into. Slices concatenate instead; the duplicate-file visit that costs
  is accepted.
- **A `DiagnosticsProvider` multibuffer** (catalogue entry A.4 in
  `slice-plans/multibuffer-providers.md`). **Struck, not built.**
  It would stand up a second editable diagnostics surface beside
  `*problems*`, which already exists and already groups by file. Making
  the language server an `ErrorList` producer (§3.2) yields the
  grouped view, the picker, and the whole `:next-error` family at once.
- **Per-source filters (`:problems lsp`, `:problems compile`).**
  Deferred, not rejected — plausible once two sources are routinely
  live, but speculative before anyone has run merged lists in anger.

### Reversed: "LSP diagnostics are deliberately not a producer"

Recorded by CM.7 and reversed 2026-08-10 on merit (heuristic #1). The
original reasoning was a mode-ownership boundary — but the producer
code lives in `lattice-lsp`, publishes `ErrorEntry` through the
protocol floor, and teaches the host nothing about LSP, so the
boundary is intact. The stated principle it was protecting — no
state-dependent command meaning — is preserved verbatim (§2). What
remained was a carve-out contradicting §3's own "producer-agnostic by
construction", which cost the user the grouped `*problems*` view,
the `:error-list` picker, and cross-file `:next-error` over
diagnostics for no compensating gain.
