# Project commands — choosing the project, then the verb

> **Where the code is.** A **bundled in-tree plugin**, `plugins/project/`, in the
> shape `auto-pair` and `treesitter-context` already have. Nothing here is
> compiled into the editor: there is no `Editor::` method, no host `Action`
> variant, and no host concept of "a project command". What lives in the tree
> besides the plugin is three small generic seams (§9).

**Status:** ✅ built (PC.1–PC.8). Builds on
[`project-resolution.md`](project-resolution.md), which already answers *where
is the project* and explicitly leaves the rest to a plugin. Slice plan:
[`../operations/slice-plans/project-commands.md`](../operations/slice-plans/project-commands.md).

## 1. The gap

Every project-aware surface in lattice resolves its root **implicitly, from the
buffer you are standing in**. `:files`, `:terminal`, `:compile`, `:search` and
magit all root themselves that way, and `project-resolution.md` is the design
that made them agree on one answer.

That is right for the common case and has no answer for the other one: *I have a
file open in project A and I want to open a file in project B.* Today the only
route is `:e` with a hand-typed path — which is the one thing a fuzzy file
picker exists to avoid, and it is worst exactly when you need it most, because
you do not remember the layout of the project you are switching to.

The missing verb is **choose the project first**. Emacs has had it for years as
`project.el`'s `C-x p` map, and the shape it settled on is the one worth taking:
a project picker, and then — via `project-switch-commands` — a menu of what to
*do* in the project you just chose.

## 2. What already exists, so none of it gets rebuilt

This design is mostly wiring, and that is the point. The inventory matters
because the tempting version of this feature reimplements three things that are
already here:

| Piece | Already provides it |
|---|---|
| "where is the project for this buffer/path" | `wit/project.wit` — `root-for-buffer`, `root-for-path` |
| a file picker **rooted anywhere** | the native `files` source already reads `args[0]` as its root |
| a directory browser rooted anywhere | `Effect::OpenOil(Option<String>)` takes a path |
| a keyed menu built by a guest | `Effect::OpenTransient` (`plugin-transients.md`) |
| picker → command → effect routing | `roam_insert`'s accept → `InvokeCommand` → ex-command → `OpenPicker` |
| persistent per-plugin state | `host-services.store-*`, gated on `state:write` |
| "a file was opened" | `Event::DocumentOpened`, carrying `id` and `path` |
| per-repository magit buffers | `magit-repo-scoping.md` — `*magit:status:<repo>*` already coexist |

`project.wit`'s own header wrote this design's boundary before it existed:

> A `project.el`-style plugin therefore READS the root here and acts through the
> ordinary effect seams; it does not supply the root. […] Deliberately just
> "where is the project". No file listing, no project list, no switching — those
> are the plugin's job, and a host seam that grew them would be re-implementing
> the plugin inside the host.

So the host gains **no** notion of a project list, a project command, or a
switch menu. That sentence is load-bearing in §7, where it decides the
extensibility mechanism.

## 3. Why a plugin, and which one

**Heuristic #6 — the crate boundary.** A new *crate* would need a dependency
surface it carves out. This has none: it depends on the project seam, the picker
seam, the transient seam and the store, all of which exist. It is a plugin
because `project.wit` says the behaviour belongs on the guest side of that line,
not because it is big.

Bundled and in-tree (`plugins/project/`) rather than a separate repository,
because it is a **core verb** — "open a file in another project" is not an
optional extension the way an org implementation is, and a user should not have
to install anything to get it. `auto-pair` is the precedent: bundled ⇒
pre-granted, `default_modes` makes it live out of the box.

**Capabilities: `state:write`, and nothing else.** No `fs:` grant. The plugin
never reads the filesystem — the host resolves roots, the native pickers do the
walking, and the store holds the list. That is a real design constraint and it
shapes §5's pruning decision.

## 4. The known-projects list

**Remembered on visit**, which is `project.el`'s own model
(`project-remember-project` writing `project-list-file`):

- subscribe to `Event::DocumentOpened`;
- `project::root-for-buffer(id)` — skipping `kind = pwd`, which means *not in a
  project* and must not enter a list of projects;
- insert into a set persisted under one store key.

Stored as **one key holding the whole list**, not a key per project. The list is
read whole every time (the picker shows all of it) and written whole on change;
a key per project would buy nothing and cost a `store-keys` scan on every open.

```
key    "projects"
value  one absolute path per line, most-recently-visited FIRST
```

**The format is the ordering, and that replaced a counter.** This section
originally specified `msgpack(Vec<Remembered { root, last_visited_seq }>)`. The
counter existed for exactly one purpose — ordering the picker so that switching
back and forth between two projects does not require typing — and a list is
already ordered, so it was a second encoding of what the container encodes for
free. Remembering a project *moves* it to the front rather than appending, which
is the whole of the ordering rule.

Dropping the counter drops the codec with it. A bundled guest should not pull
serde and rmp into a wasm artifact to persist a list of paths, and a line format
is inspectable in a hex dump when something is wrong. A format change can then
only produce a line that does not parse — never a schema-skew failure.

Two consequences, both deliberate:

- **A path containing a newline is refused rather than escaped.** Legal on unix,
  pathological in practice. Escaping would put a decoder in the one place this
  is meant to stay simple; declining to remember that one project, and *saying
  so*, is the honest failure.
- **The list is bounded** (256). Not a policy — the picker is fuzzy-matched, so
  length costs nothing to use — but an unbounded store value grows forever in a
  long-lived config directory. Dropping happens at the back, so the entry lost is
  always the least-recently-visited.

A corrupt store decodes lossily to whatever survives. Refusing to load would
turn one bad byte into "the feature is gone" with no way to see why.

### The cold-start hole, named

Remembered-on-visit **cannot reach a project you have never opened** — which is
the motivating case verbatim ("I want to open a file in project B"). A fresh
install's list is empty, and the first thing the feature does is show nothing.

So it ships with a seed: **`:project-remember [dir]`**, and a *Remember a
project…* row in the picker when the list is short or the query matches nothing.
`project.el` has the same escape hatch (`project-remember-projects-under`), for
the same reason. Without it the feature's answer to its own motivating example
is "open a file there the hard way first", which is the friction it exists to
remove.

`:project-forget [dir]` is its peer. A project directory that has been deleted
or moved is not pruned automatically — with no `fs:` grant the plugin cannot
tell, and the honest failure is the one the native picker already gives:
`files: no files under <path>`. Guessing that a root is dead and silently
dropping it would lose a project that was merely on an unmounted volume.

## 5. The project picker

A plugin `picker-source`, `id = "projects"`, `live = false` (the list cannot
change while the picker is open). One row per remembered project: the basename
as the display, the full path as the annotation — `magit-repo-scoping.md` §3.1's
rule, and for its reason, that *"names are read far more often than they are
parsed"* and two checkouts can share a basename.

Ordered most-recently-visited first.

An **empty list no longer refuses to open.** It used to answer
`project: no projects remembered yet — open a file in one, or :project-remember <dir>`
on the `roam_find` precedent, that *"'no notes' and 'roam is not configured'
look identical in an empty picker and have entirely different fixes."* That
reasoning holds only for a picker with nothing to offer. This one has the
`… (choose a dir)` row below, and a fresh install is exactly when you need it —
refusing to open put the escape hatch behind the wall it exists to get through.
The picker opens with the row alone, which says "nothing remembered" by showing
nothing remembered, and hands you the fix in the same breath.

### The `… (choose a dir)` row

`project.el`'s own escape hatch, and the same label. Pinned last, always
present — not gated on the query, because the empty list is the case it serves.

```
:project-switch  →  projects picker
                      lattice
                      lattice-org-plugin
                    ▸ … (choose a dir)
                          │ <CR>
                          ▼
                    dir-pick                      ← native source, §9 H5
                      Choose a directory: ~/src/dh▊
                    ▸ ~/src/dhruvasagar/
                      ~/src/dharma/
                          <C-l> descend · <C-h> up · <CR> choose
                          │
                          ▼
                    remembered, then the switch-commands menu (§7)
```

**One hop, not two.** The chosen directory is remembered and goes straight on
to §7's menu — `project.el`'s `project-switch-project` does not return you to
the project list to confirm a directory you just chose, and neither does this.
The row is a way *into* the same flow, not a detour beside it.

**The listing is incremental, not a walk.** `dir-pick` lists the children of
the directory the query names, filtered by the basename it ends with — the
`gen:directories` model, which is what `read-directory-name` does. The
alternative considered and rejected was `walk_files_for_picker` with
directories instead of files: it has no depth cap and a flat 5000-entry
ceiling, so pointed at `~` it stops somewhere arbitrary inside `~/Library` and
the project you wanted may simply not be in the list. Incremental has no
ceiling, reaches any depth, and opens instantly.

**`<C-l>` descends, not `<Tab>`.** `<Tab>` is `PickerSelectNext` in every
picker (`input.rs`), and `<S-Tab>` its peer. Giving one picker a `<Tab>` that
means something different from all the others is the inconsistency the
convention rule exists to prevent, and changing it everywhere to suit a
directory browser is worse. `<C-l>` / `<C-h>` are free, and are what ranger,
lf, nnn and vifm use for descend / ascend — the tools this surface most
resembles. `<CR>` keeps the meaning it has in every other picker: take the
selected row.

**Choosing a non-project is not refused here.** `project_of_path` resolves the
*containing* project, so choosing `~/src/lattice/crates` remembers `lattice`.
A directory with no root marker above it is the one real refusal, and it is
reported by the existing `project: `…` is not inside a project` path rather
than by the picker declining to show the row — the plugin holds no `fs:` grant
and cannot know what is a project until it asks the host.

## 6. Two entry points, and the difference matters

`project.el`'s map has both, and conflating them is the easy mistake:

- **`f`, `d`, `g`, `s`, … act on the CURRENT project.** `<leader>pf` is "find a
  file in the project I am already in" — a faster `:files`, no picker.
- **`p` chooses a project FIRST**, then shows the switch-commands menu (§7).

Both are wanted. The first is the everyday verb; the second is the one this
design exists for. A design that only had the second would make the common case
pay a project picker it does not need.

### 6bis. `project-buffers` — the same distinction, applied to buffers

`project.el`'s `project-switch-to-buffer`, on `b` under both prefixes and as
the second row of the switch menu. `:b` lists every buffer across every
checkout, which is the right answer for `:b` and the wrong one when you are
inside one project and want the handful of files that belong to it.

A **plugin** picker source, not a native one, because the domain is this
plugin's: it already owns every other project verb, and splitting one of them
into the host is the half-migration the mode-ownership rule exists to prevent.
It needs nothing the plugin does not have — `picker-context` already carries
`buffers` and `workspace-root`, so the source computes its list inside the
`state:write`-only capability boundary §3 drew, with no `fs:` grant and no host
round-trip.

**The filter is component-wise path containment**, and that is a real choice
with a stated cost. The alternative is asking the host to resolve each buffer's
project and comparing roots, which is more correct for symlinked checkouts and
nested repositories — "under this path" and "in this project" are genuinely
different questions there. It is not what this does, because it costs one WIT
round-trip per open buffer on a path that runs synchronously inside a keystroke
(paramount #1), where containment costs a string compare. What the cheap answer
loses is bounded and visible: a buffer whose file sits outside the root but is
morally the project's — a sibling checkout, a generated file under `/tmp` —
does not appear, and `:b` still lists it.

Component-wise rather than `starts_with`, because `~/src/lattice` and
`~/src/lattice-old` share a prefix and are different projects. A buffer from
the second appearing in the first's list is a wrong answer that looks like a
right one, which is the only kind worth writing a test for.

**A buffer with no path is in no project.** Magit status buffers, oil listings,
`*messages*`, help. Including them would put the same rows in every project's
list, which is exactly the `:b` behaviour this source exists to narrow.

It declares `rooted` (picker.md §4.2ter), and it is the source that asked for
that mechanism: a filtered list with no statement of what it was filtered BY
leaves the user reading a short list with no way to know why it is short.

**Not remembered on the way through**, unlike `find-file` / `dired` / `grep` /
`shell`. Those four can land you in a project you have not recorded; this one
can only list buffers that are already open, and opening them is what
remembered the project in the first place (`document-opened`). Recording it
again here would touch the store on a keystroke to write bytes it already
holds.

## 7. `project-switch-commands` — the menu, and how it extends

After the project picker accepts, the plugin emits `Effect::OpenTransient`
carrying the chosen root as an argument (`TR.3a` added exactly this: *"a row
that opens a second menu has no way to say what it opened it FOR"*). The menu's
rows come from a **structured config option**:

```toml
[project]
switch-commands = [
  { key = "f", label = "Find file",   command = "project-find-file" },
  { key = "d", label = "Dired",       command = "project-dired" },
  { key = "g", label = "Find regexp", command = "project-grep" },
  { key = "s", label = "Shell",       command = "project-shell" },
  { key = "v", label = "Magit",       command = "magit-status" },
]
```

A record-valued list option is possible **now** and was not when
[`org-capture.md`](org-capture.md) §2 argued that "no option can hold a record"
and fell back to a TOML blob inside a string. TC.4/TC.5's `ConfigShape` /
`register-structured-option` landed since; this design uses the typed form, so
`:describe-option project.switch-commands` shows a schema rather than a blob.

### The calling convention

**A project command is an ex-command whose first argument is a project root.**
That one sentence is the whole extension contract. The menu invokes
`:<command> <root>`; anything registered under that convention can be a row.

Most built-in verbs are **thin wrappers owned by the plugin**
(`:project-find-file`, `:project-dired`, `:project-grep`, `:project-shell`),
because the underlying commands do not share an argument shape: `files` takes a
root, `grep`'s first argument is its *pattern*, and a terminal takes a cwd
rather than any argument at all. A menu that had to know each one's signature
would be a switch statement over the editor's command surface.

**Magit is the exception, and it is the contract proving itself.** A
`:project-magit` wrapper was specified here and then deleted: PC.3 gave
`:magit-status` an optional path, so it now *is* "an ex-command whose first
argument is a project root" and the row names it directly. Magit keeps owning
what a status buffer is, which repository it acts on, and how two checkouts
sharing a basename are told apart — including its own "Not a git repository."
for a project that is not one, rather than a second opinion from here. A wrapper
could only have re-described that; it also could not have forwarded at all,
since `Effect` has no `invoke-command` arm.

The lesson generalises: **if a command already takes a root, do not wrap it.**
A wrapper is for translating a root into whatever shape the underlying effect
needs, not for putting this plugin's name on someone else's verb.

Recency is refreshed once by `:project-switch-to` when the menu opens, so
individual rows do not each need to remember the project.

### Extensibility: configuration, deliberately not a registry

A third plugin makes itself available by **registering an ex-command that takes
a root**; the user (or the plugin's own default config) adds a row. No host
mechanism is involved.

**Rejected: a host-brokered contributable registry for project commands.**
`contributable-registries.md` establishes that pattern for help topics and
dashboard sections, and it is the obvious thing to reach for. It is wrong here
for a reason already written down: the host would have to learn what a project
command *is*, and `project.wit` refuses exactly that — *"a host seam that grew
them would be re-implementing the plugin inside the host."* The registry pattern
also fits its two existing users because the **owner is a host crate**
(`lattice-help`, `lattice-dashboard`); here the owner is a plugin, so
plugin→plugin contribution would need the host to broker between two guests,
which is a genuinely new mechanism and not a reuse of that one.

The cost, stated: a plugin cannot add a row to your menu *without* a line of
config. That is a feature at this size — an editor where installing something
silently rewrites a menu you keyed by muscle memory is worse than one where you
type five words.

## 8. Keymap

`project-mode`, a minor with `ActivationPolicy::Universal` — the
`org-global-mode` / `magit-global-mode` precedent, and for their reason: the
verbs are global, and a project picker that only worked inside a project would
be useless for the case it exists for. In `default_modes`, or it registers
correctly and stays permanently inert
(`plugin-minors-are-inert-until-enabled`).

Both prefixes, per `project.el` muscle memory *and* the fact that the tribute
layer is optional:

- **`<leader>p`** — the always-live home. `pf` `pd` `pg` `ps` `pv` `pp`.
- **`<C-x>p`** — `project.el`'s own prefix, free in the emacs-keys table
  (which binds `b`, `k`, `o`, `0`–`3` and the `<C-f>`/`<C-s>`/`<C-b>`/`<C-c>`
  pairs — no `p`).

**`<C-x>p` is bound unconditionally, and that is a knowing trade.**

This section first specified gating it on the `emacs-keys` option — "pushed and
popped on `Event::OptionChanged`" — so that `:set noemacs-keys` fully reclaimed
`<C-x>`. **That is not buildable.** Both keymap surfaces a plugin has are
registration-only at load: `keymap.register-binding`, called from
`register-keymap`, and `mode-keymap-binding`, declared statically in
`register-modes`. Neither has an unregister, and there is no runtime push/pop.

So the choice was between three real options and the cost is named rather than
smuggled: **`:set noemacs-keys` no longer fully reclaims `<C-x>`.** It stays
half-alive with one working sub-chord, `p`, which is exactly the promise
`emacs-keys.md` makes and exactly what the gate existed to protect.

Rejected, with reasons:

- **Putting the `p` rows in the host's emacs-keys table.** That layer is already
  host-owned and already option-gated, so the gating would have been correct by
  construction, and it is not a mode-ownership violation — `emacs-keys.md`
  governs that table as host chrome. Declined because it is a host change to buy
  a property the trade above accepts losing.
- **A dynamic keymap seam** (unregister / re-register for plugin keymaps). The
  general fix, and a genuinely new host mechanism with its own lifetime and
  layering questions. Nothing else currently needs it, which is precisely when a
  new mechanism is hardest to design well.
- **Shipping `<leader>p` alone.** Honest, but it drops the `project.el` muscle
  memory that is half the point, and "defer" here had no date attached.

If a dynamic keymap seam ever lands for its own reasons, this is its first
consumer and the gate should come back.

> Every chord here must be **driven in a test**, not read off the keymap.
> `org-capture.md` §6 records `<C-x>o` shipping in a design doc despite being
> unfirable, found only by pressing it.

## 9. Host changes

Three, all small, all generic, none naming a project. Each unblocks exactly one
menu row; `f` and `d` need none.

### H1 · `Effect::OpenPicker` gains a `root`

**This section first specified an `args[1]` root on the `grep` source, and that
cannot work.** `grep` is `live: true`, so it re-queries through
`on_query_changed(&self, ctx, query)` — which receives the query and the context
and **not** the open's args — and a source is a shared `&self` generator with no
per-open state. A root passed as an argument would apply to the first grep and
then silently revert to the workspace root the moment the user typed a
character. A feature that works until you type is worse than one that is absent.

So the root rides the **context**: `Effect::OpenPicker` (and its WIT payload)
gains `root: option<string>`, and the host returns it from
`picker_workspace_root_path` for that open.

Three things follow, and they are why this is the better shape:

- **`grep` needed no change at all.** It already reads `ctx.workspace_root` in
  both `init` and `on_query_changed`, so the override survives every re-query.
- **Every root-sensitive source gets it uniformly**, rather than each inventing
  an argument convention. `files`' existing `args[0]` root stays for
  `:picker files <path>`, but it stops being the mechanism.
- **The precedent is `transient-context.args`** (TR.3a), which exists because
  "a row that opens a second menu has no way to say what it opened it FOR".
  Same problem, same answer: the thing the open was *for* belongs in the
  context, not in a payload the next hop cannot see.

The override is stored beside its picker-scoped peers on `Editor`
(`picker_open_target`, `picker_fill_target`) and set **unconditionally at open,
`None` included**. That is what makes a stale root impossible without a close
hook: opening a picker is the one moment guaranteed to run, whereas every close
path would have to remember to clear it — the discipline `PENDING_QUESTIONS`
already uses for the same class of bug.

### H2 · `spawn-terminal-payload` gains `cwd`

The native spawner **already has the field** — `lattice-terminal`'s
`SpawnConfig { cwd: Option<PathBuf>, … }`, documented as *"`None` = inherit
parent's cwd"*. Only the WIT payload lacks it (`cmd-line`, `env`,
`activate-minor`). This is threading an existing field through the boundary, not
new machinery.

### H3 · `:magit-status <path>`

`:magit-status` "resolves the repository from `buffer_id`" and ignores its
arguments today. Magit already supports **per-repository status buffers**
(`*magit:status:lattice*` and `*magit:status:api*` coexist), so the model is
there; only the explicit entry point is missing.

This is **not** a reversal of a recorded decision.
`magit-repo-scoping.md`'s rejected-alternatives list says:

> **Resolving from cwd but letting `:magit-status <path>` override.** Rejected
> as the *primary* mechanism — it makes the common case (working across two
> checkouts) the one that needs an argument. **Worth having later as an explicit
> form.**

This is that explicit form, and it stays complementary: the implicit resolution
in §2 of that document is untouched, and an argument-less `:magit-status`
behaves exactly as it does now.

### H4 · `FillTarget::Action`, and the `open-picker` field that names it

A guest opening a picker **to answer a question** needs the answer back. Today
`FillCaller` puts it into a surface captured at open — the document, the `:`
line, a prompt, a transient argument, another picker's query. A plugin owns
none of those; what it owns is an action.

So `FillTarget` gains `Action { id }`, and `open-picker-payload` gains the
field that names it. The picked value arrives as that ex-command's first
argument, exactly as `open-prompt-payload`'s `on-submit-action` already works
for prompts. The asymmetry between the two — a guest can be handed a prompt's
answer but not a picker's — is the gap this closes.

**Not an `on-accept-action` that overrides the source.** That was the first
shape considered and it is the wrong one: a source's accept outcome is the
*source's* decision, and a flag on the open that overrode it would be a second
answer to the question `file-pick` already answers by existing — a separate
source whose accept means "supply a value" rather than "act". `FillTarget` is
where "who gets the value" already lives, and it is captured at open for
YR.3's reason, which applies here unchanged: by accept time the picker is
dismissed and the question has a different answer.

**Cost, stated rather than buried.** WIT records have no field defaults, so
this is a boundary change: every guest needs `wit-sync` and a rebuild before it
will instantiate, in-tree and out. That is the price of the field, and it is
paid once.

### H5 · `dir-pick` — a directory peer for `file-pick`

`live = true`, listing the children of the directory the query names, filtered
by the basename it ends with, tilde-expanded. Accept yields
`RoutingPayload::SuppliedValue` → `PickerAcceptOutcome::FillCaller`, which is
`file-pick`'s shape verbatim and for its reason: this source supplies a value
and must not decide what happens to it.

It lands in `lattice-picker`'s `picker_sources.rs` beside `file-pick` rather
than in the plugin, for a reason that is not tidiness: **the project plugin
holds `state:write` and nothing else.** With no `fs:` grant it cannot list a
directory at all, and that constraint is deliberate (§3). A guest-side
directory picker is not a design choice that was passed over; it is
unimplementable.

Two keys come with it. `<C-l>` calls a new
`PickerSourceGenerator::descend(ctx, routing) -> Option<String>` that defaults
to `None`, so every existing picker is unaffected by taking the default;
`Some(query)` replaces the query and re-lists. `<C-h>` needs no hook — it
deletes back to the previous `/`, a pure query edit that is generic over any
path-shaped query.

Both are free in `translate_picker`. `<Tab>` is not: it is `PickerSelectNext`
in every picker, and that is why it is not the descend key (§5).

Being native rather than plugin-local also makes it reachable from the `:`
line for free — an `ArgSpec` declaring `picker: Some("dir-pick")` gets it under
`<C-x><C-o>`, which is what `:project-remember` should declare.

## 10. Failure behaviour

All echo, none panic, and none loses the user's place.

- A remembered root that no longer exists → the native picker's own
  `files: no files under <path>`, plus `:project-forget` to remove it. Not
  auto-pruned (§4).
- A `dir-pick` query naming a directory that cannot be read → an empty list,
  not an error. Half a typed path names nothing yet, and that is the state the
  user is in for most of the keystrokes; erroring on it would mean the picker
  spends its life reporting failure.
- `FillTarget::Action` naming a command that is not registered → the existing
  `report_vanished_caller` shape: say which caller went away rather than
  dropping the value silently. A picked directory that vanishes with no
  message is indistinguishable from a picker that did nothing.
- `root-for-buffer` answering `kind = pwd` → not remembered. "Not in a project"
  is not a project.
- A `switch-commands` row naming a command that is not registered → the row is
  **shown greyed with the reason**, not silently dropped. A missing row is
  invisible; a row that says *"magit-status: no such command"* tells you the
  plugin providing it did not load.
- An empty or malformed `switch-commands` option → fall back to the built-in
  defaults rather than an empty menu. A menu with no rows is
  indistinguishable from a broken chord.

## 11. Paramount-goal alignment

**UX (higher court).** The feature is a UX argument end to end: the implicit
root is right until it is exactly wrong, and today's fallback is typing a path
from memory. The menu follows `project.el` because that muscle memory is the
thing being imported — the UX-convention rule, which says to lead with the
cross-editor convention on surfaces where muscle memory dominates.

**#1 Performance.** Nothing on the typing path. The store read happens on
picker open; `root-for-buffer` runs once per document-open on the plugin's own
task (the seam's doc: *"never the UI or actor thread"*, cached per directory);
the file walk is the native picker's, unchanged.

**#2 Extensibility.** The point of the work. The host learns nothing about
projects beyond resolution — the boundary `project.wit` drew — and the three
seams are each generic (a root for a grep, a cwd for a terminal, a path for a
magit status). A third-party plugin joins the menu by registering an ex-command
with a root argument, no host mechanism involved. The acid test holds: zero
`Editor::` methods, zero new host `Action` variants.

**#3 Vim modal editing.** The verbs are ex-commands and the chords are ordinary
keymap entries on a minor's layer. `<leader>p` is the vim-native home;
`<C-x>p` is the tribute, and it stays honest about being one.

**#4 Asynchronicity.** The remember-on-open handler runs on the event actor's
own task; nothing blocks. The pickers are the native ones, already off-thread.
