# Project commands — choosing the project, then the verb

> **Where the code is.** A **bundled in-tree plugin**, `plugins/project/`, in the
> shape `auto-pair` and `treesitter-context` already have. Nothing here is
> compiled into the editor: there is no `Editor::` method, no host `Action`
> variant, and no host concept of "a project command". What lives in the tree
> besides the plugin is three small generic seams (§9).

**Status:** 📝 planned. Builds on
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

An **empty list says so** rather than rendering as an empty picker:
`project: no projects remembered yet — open a file in one, or :project-remember <dir>`.
The `roam_find` precedent — *"'no notes' and 'roam is not configured' look
identical in an empty picker and have entirely different fixes."*

## 6. Two entry points, and the difference matters

`project.el`'s map has both, and conflating them is the easy mistake:

- **`f`, `d`, `g`, `s`, … act on the CURRENT project.** `<leader>pf` is "find a
  file in the project I am already in" — a faster `:files`, no picker.
- **`p` chooses a project FIRST**, then shows the switch-commands menu (§7).

Both are wanted. The first is the everyday verb; the second is the one this
design exists for. A design that only had the second would make the common case
pay a project picker it does not need.

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
  { key = "v", label = "Magit",       command = "project-magit" },
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

This is why the built-in verbs are **thin wrappers owned by the plugin**
(`:project-find-file`, `:project-dired`, `:project-grep`, `:project-shell`,
`:project-magit`) rather than the menu pointing at `:files` / `:oil` /
`:magit-status` directly. Two reasons:

- the underlying commands do not share an argument shape — `files` takes a root,
  `grep` takes a pattern, `magit-status` takes nothing — and a menu that had to
  know each one's signature would be a switch statement over the editor's
  command surface;
- a wrapper is where "remember that I visited this project" belongs, so
  switching to a project through the menu updates the ordering even when no file
  is opened.

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

**The `<C-x>p` half is gated on the `emacs-keys` option**, pushed and popped on
`Event::OptionChanged`. `emacs-keys.md` promises that *":set noemacs-keys
reclaims `<C-x>`, so a vim purist can have it back"*, and a plugin binding
`<C-x>p` unconditionally would quietly break that promise — `<C-x>` would stay
half-alive with one working sub-chord. The gate keeps mode-ownership whole: the
plugin owns both chords and both handler bodies, and simply declines to register
one of them when the layer it belongs to is off.

> Every chord here must be **driven in a test**, not read off the keymap.
> `org-capture.md` §6 records `<C-x>o` shipping in a design doc despite being
> unfirable, found only by pressing it.

## 9. Host changes

Three, all small, all generic, none naming a project. Each unblocks exactly one
menu row; `f` and `d` need none.

### H1 · the `grep` picker source accepts a root

`files` reads `args[0]` as its root, with the comment *"An explicit `:picker
files <path>` still wins — that is the user saying 'not that project, this
one'."* `grep` reads `args[0]` as its **pattern** and takes its root from
`ctx.workspace_root` with no override.

So `grep` grows an optional `args[1]` root. The asymmetry (root first for
`files`, pattern first for `grep`) is deliberate and stays: each source's
*primary* argument is first, and reordering `grep`'s would break every existing
`:picker grep <pattern>`.

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

## 10. Failure behaviour

All echo, none panic, and none loses the user's place.

- A remembered root that no longer exists → the native picker's own
  `files: no files under <path>`, plus `:project-forget` to remove it. Not
  auto-pruned (§4).
- `root-for-buffer` answering `kind = pwd` → not remembered. "Not in a project"
  is not a project.
- A `switch-commands` row naming a command that is not registered → the row is
  **shown greyed with the reason**, not silently dropped. A missing row is
  invisible; a row that says *"project-magit: no such command"* tells you the
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
