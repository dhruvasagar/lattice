# Slice plan — project commands

Design: [`../../architecture/project-commands.md`](../../architecture/project-commands.md).
Builds on [`../../architecture/project-resolution.md`](../../architecture/project-resolution.md)
(PR.6's `wit/project.wit`), which is already shipped.

**Un-archived 2026-09-10.** PC.1–PC.8 are complete and stay so; PC.9–PC.13 are
new work on the same feature (design §5's `… (choose a dir)` row), so the plan
comes back to active rather than a second plan being started beside it.

PC.1–PC.3 are host seams in **this** tree, each unblocking exactly one menu row.
PC.4–PC.8 are the bundled plugin at `plugins/project/`. The plugin's first three
slices need **no** host change, so PC.4–PC.6 can land before PC.1–PC.3 and the
feature is useful at PC.6.

| Slice | Where | What | Status |
|---|---|---|---|
| PC.4 | plugin | Scaffold, remembered-projects list, `:project-remember` / `-forget` | ✅ |
| PC.5 | plugin | The `projects` picker + `:project-find-file` / `:project-dired` | ✅ |
| PC.6 | plugin | `project-switch-commands` menu + keymap on both prefixes | ✅ |
| PC.1 | lattice | `Effect::OpenPicker` gains a `root` | ✅ |
| PC.2 | lattice | `spawn-terminal-payload` gains `cwd` | ✅ |
| PC.3 | lattice | `:magit-status <path>` | ✅ |
| PC.7 | plugin | `:project-grep` / `:project-shell` rows (magit needs no wrapper) | ✅ |
| PC.8 | both | `:help project`, core-plugins row, hot-path fix | ✅ |
| PC.9 | lattice | `dir-pick` — the incremental directory source | ✅ |
| PC.10 | lattice | `descend` hook + `<C-l>` / `<C-h>` | ✅ |
| PC.11 | lattice | `FillTarget::Action` + the `open-picker` field | ✅ |
| PC.12 | plugin | The `… (choose a dir)` row, end to end | ✅ |
| PC.13 | both | Docs, and `:project-remember`'s missing completion | ✅ |
| PC.14 | lattice | The row did nothing — two host seams dropped its effects | ✅ |
| PP.1 | lattice | `dir-pick` shows where it is, and offers `../` | ✅ |
| PP.2 | both | A rooted picker names the root it is operating on | ✅ |
| PB.1 | plugin | `project-buffers` — one project's open buffers | ✅ |
| PP.3 | lattice | `<CR>` on `../` navigates, it does not choose the parent | ✅ |
| PP.5 | lattice | `<Tab>` drills into a directory, and keeps drilling | ✅ |
| PK.1 | plugin | Every switch-menu verb has a direct chord | ✅ |
| PD.1 | both | `<C-d>` forgets the selected project | ✅ |
| PP.4 | plugin | Any folder is a project — the marker refusal is gone | ✅ |

**Deliberate ordering.** The plugin leads. PC.4–PC.6 prove the whole shape —
list, picker, menu, keymap — against the two verbs that need nothing from the
host, so the host seams are cut against a working consumer rather than
speculatively. That is the opposite of the usual "seams first" order and it is
chosen for that reason.

**PC.9–PC.13 reverse that order, and for a matching reason.** The `…
(choose a dir)` row (design §5) is *only* the composition of three host pieces
— none of them has a plugin-side half worth landing first, and the plugin slice
is a handful of rows once they exist. Cutting the seams first here is not a
change of principle; it is the same principle reading the other way, because
this time the consumer is trivial and the mechanism is not.

PC.9 and PC.10 are independently useful and independently landable: `dir-pick`
alone gives `:project-remember <C-x><C-o>` a real directory picker on the `:`
line. PC.11 is the only one that touches the boundary.

---

## PC.4 ✅ — Scaffold and the remembered list

`plugins/project/` in `auto-pair`'s shape: bundled ⇒ pre-granted, `plugin.toml`
with `id = "project"`, `capabilities = ["state:write"]`, **no `fs:` grant**
(design §3 — the plugin never touches the filesystem, and the absence is what
forces §4's honest no-auto-pruning).

`provides = ["grammar", "events"]` — it grows with the slices: PC.5 adds
`picker-source`, PC.6 adds `modes`, `config` and `transient-source`, PC.8 adds
`help`.

- Subscribe `Event::DocumentOpened` → `project::root-for-buffer(id)` → skip
  `kind = pwd` → insert → `store-put` under one key, `"projects"`.
- One absolute path per line, most-recently-visited first; remembering MOVES a
  project to the front. See the corrections below — the counter this originally
  specified is gone.
- `:project-remember [dir]` (default: the current buffer's project) and
  `:project-forget [dir]`.

**Landed.** `plugins/project/` (grammar + events, `state:write` only),
`wit/project-plugin.wit`, registered in `xtask`'s `CORE_PLUGINS` and in
`lattice-plugin-host/build.rs` as `PROJECT_PLUGIN_WASM`. 9 guest unit tests +
3 host integration tests.

**Two corrections the build forced, both recorded in the design:**
- the store format dropped msgpack and the `last_visited_seq` counter for a
  line-oriented one — the counter re-encoded an ordering the list already has,
  and a bundled guest should not pull serde+rmp to persist paths (design §4);
- ~~`root-for-buffer` uses the buffer store's **`name_for` as its existence
  oracle** and short-circuits on `None` before ever consulting `path_for`. A
  test stub answering `None` there makes every resolution return `none` and the
  plugin silently remember nothing — which is how the integration test first
  failed, and why the stub now carries a comment saying so.~~

  **This "correction" was the bug, and it was recorded here as a lesson for
  ten weeks.** `name_for` is the *synthetic*-name slot: it is `None` for every
  buffer opened from a file, so the oracle refused exactly the buffers a user
  edits and the plugin remembered nothing in the real editor —
  `:project-switch` answered "no projects remembered yet" however long lattice
  had been running. The integration test failing with an honest `None` was the
  bug reporting itself; making the stub answer `Some("the-buffer")`, a state
  production cannot produce, silenced it. Fixed 2026-09-10:
  `BufferStore::contains_buffer` is the oracle, the host registry answers it
  from its map, and the stub answers `None` like a real file buffer. See
  `project-resolution.md` §6.

**Tests.**
- opening a file in a project remembers its root exactly once, and re-opening
  does not duplicate it;
- a buffer resolving `kind = pwd` is **not** remembered — "not in a project" is
  not a project;
- re-visiting an older project moves it to the front of the order;
- `:project-remember` seeds a project **never opened**, which is the cold-start
  hole design §4 names and the motivating example verbatim;
- `:project-forget` removes it;
- the list survives a plugin reload (it is in the store, not in guest memory).

## PC.5 ✅ — The picker and the two free verbs

`picker-source` `id = "projects"`, `live = false`. Basename as display, full
path as annotation (`magit-repo-scoping.md` §3.1's rule).

Accept → `PickerAcceptOutcome::InvokeCommand` → the plugin's own ex-command →
effect. The `roam_insert` routing precedent, used a third time rather than
invented.

- `:project-find-file <root>` → `Effect::OpenPicker { source: "files", args: [root] }`.
  Free because the native source already reads `args[0]` as its root.
- `:project-dired <root>` → `Effect::OpenOil(Some(root))`.

**Landed.** `picker.rs` (the `projects` source), plus `:project-switch`,
`:project-find-file [root]` and `:project-dired [root]`. `provides` gained
`picker-source`.

**The sharpest test is cross-seam.** The list is written on the EVENTS seam and
read on the PICKER seam — two guest instances of one component, where a
`thread_local` written in one is invisible in the other. It survives only
because the list lives in the host-side store, which is why the design put it
there; a test that populated and read on one seam would pass against a
guest-memory implementation.

**A harness trap worth recording:** re-spawning the event seam per file and
re-pointing `set_project_context` each time remembered only the FIRST project.
The realistic shape — one host, one resolver, a buffer store mapping distinct
ids to distinct files — is both simpler and what the editor actually does.

**Tests.**
- ~~an empty list yields a picker that **says** it is empty rather than
  rendering as an empty list (the `roam_find` rule)~~ — **reversed by PC.12.**
  The rule is about a picker with nothing to OFFER; once `… (choose a dir)` is
  always present, refusing to open put the escape hatch behind the wall it
  exists to get through. The test now asserts the picker opens on that one row;
- accepting a row opens the files picker rooted at **that** project while the
  active buffer belongs to a **different** one — the whole feature in one
  assertion, and the one a same-project test would pass without proving;
- two remembered projects sharing a basename are distinguishable in the picker.

## PC.6 ✅ — The switch-commands menu and the keymap

Structured option `project.switch-commands` as a list of
`{ key, label, command }` records via TC.4/TC.5 `ConfigShape` — the typed form,
not `org-capture.md` §2's string-of-TOML workaround, which predates that
machinery.

Accept from the projects picker → `Effect::OpenTransient` carrying the root as
an argument (TR.3a).

Keymap on `project-mode`, a `Universal` minor, in `default_modes`.
`<leader>p` always; `<C-x>p` **only while `emacs-keys` is on**, pushed and
popped on `Event::OptionChanged`.

**Landed.** `switch.rs` (the structured option + menu rows), `project-mode` as
a `universal` minor in `default_modes`, `:project-switch-to` as the picker's
second hop, and the transient itself.

**A host change this forced, and it is a latent gap rather than a local fix.**
The plugin would not load at all: `root-for-buffer has the wrong type /
function implementation is missing`. The `project` seam was never wired into the
**sync grammar linker**, so ANY component that both provides `grammar` and
imports `project` fails instantiation entirely. PC.4/PC.5 passed only because
they spawn async seams. Wired alongside the existing TC.6 / CR.3 / LG.3c entries
that exist for exactly this reason — and `project.wit` already promised it:
"Sync, and available in every world."

**`reverse_entries` is the wrong oracle for a multi-prefix binding** — it holds
ONE path per command, so a command bound under two prefixes looks singly-bound.
`layer_bindings` answers "what did THIS layer bind", which is the question. The
first version of this test failed against correct code for that reason.

**Deliberately gating on stale state:** the option decision (`<C-x>p` bound
unconditionally) is asserted here, so if a dynamic keymap seam ever lands this
test fails and points at the gate that should come back.

**Tests.**
- **Press every chord.** `<leader>pf`, `<leader>pp`, `<C-x>pf`, `<C-x>pp` are
  driven, not read off the keymap — `org-capture.md` §6's `<C-x>o` shipped in a
  design doc and could never have fired.
- `:set noemacs-keys` → `<C-x>p` stops resolving and `<leader>p` still does;
  turning it back on restores it. This is the promise `emacs-keys.md` makes and
  the one an unconditional binding would quietly break.
- `<leader>pf` acts on the **current** project with no picker; `<leader>pp`
  shows the project picker first (design §6 — the two entry points are different
  verbs and conflating them makes the common case pay for the rare one).
- a row naming an unregistered command renders **greyed with the reason**, not
  dropped — a missing row is invisible, a labelled one tells you what did not
  load;
- an empty / malformed option falls back to the built-in defaults rather than an
  empty menu.

---

## PC.1 ✅ — `Effect::OpenPicker` gains a `root`

**Re-scoped during the build, and the original plan could not have worked.**
It said "optional `args[1]` on the `grep` source". `grep` is `live: true`, so it
re-queries through `on_query_changed(&self, ctx, query)` — which sees the query
and the context and NOT the open's args — and a source is a shared `&self`
generator with no per-open state. An argument-borne root would have applied to
the first grep and silently reverted on the next keystroke: a feature that works
until you type.

So the root rides the **context** instead. `Effect::OpenPicker` and its WIT
payload gain `root: option<string>`; the host returns it from
`picker_workspace_root_path`. Consequences:

- **`grep` needed no change at all** — it already reads `ctx.workspace_root` in
  both `init` and `on_query_changed`.
- Every root-sensitive source is served uniformly. `files`' `args[0]` root stays
  for `:picker files <path>` but stops being the mechanism, and PC.5's
  `:project-find-file` was switched over to the context root.
- Stored beside its picker-scoped peers on `Editor` and set **unconditionally at
  open, `None` included** — which is what makes a stale root impossible without
  a close hook.

**Tests.** `picker_root_override.rs`: an explicit root wins verbatim; no
override resolves from the buffer exactly as before; and opening without a root
CLEARS a previous override (the assertion that pins the unconditional write).

**Pre-existing failures verified by stashing:** `lattice-ui-tui`'s
`q_on_magit_status_buries_it_and_never_quits_the_editor` fails identically on
clean HEAD.

## PC.2 ✅ — `spawn-terminal-payload` gains `cwd`

`lattice-terminal`'s `SpawnConfig.cwd: Option<PathBuf>` **already exists** and
is documented as "`None` = inherit parent's cwd". This threads it through the
WIT boundary: payload field, `boundary_effect.rs` both directions, the native
`Effect::SpawnTerminal` variant, and **both** renderers' effect arms.

**Audit.** `grep -rn "cwd" crates/lattice-ui-gpui/ --include="*.rs"` around the
spawn-terminal arm — an empty grep means GPUI was missed.

**Tests.** A terminal spawned with an explicit cwd starts there; `None` is
unchanged.

## PC.3 ✅ — `:magit-status <path>`

The **explicit form** `magit-repo-scoping.md` deferred rather than rejected:
"Worth having later as an explicit form." Per-repository status buffers already
coexist (`*magit:status:<repo>*`), so this is an entry point, not a model
change.

**Tests.** `:magit-status <path>` opens that repository's status buffer while
the active buffer is in a different repo, and the two buffers coexist;
argument-less `:magit-status` resolves from the buffer exactly as before — the
assertion that keeps this complementary rather than a reversal.

## PC.7 ✅ — The remaining rows

`:project-grep <root>` and `:project-shell <root>` as thin wrappers — the
underlying commands do not share an argument shape (`grep`'s `args[0]` is its
PATTERN, and a terminal takes a cwd rather than any argument at all).

**`:project-magit` was dropped, and that is the extension contract working.**
The plan specified it as a third wrapper, and it existed only because
`:magit-status` ignored its arguments. PC.3 gave it an optional path, so it now
IS "an ex-command whose first argument is a project root" and the default menu
row names `magit-status` directly. Magit keeps owning what a status buffer is
and how two checkouts sharing a basename are told apart — including its own
"Not a git repository." rather than a second opinion from the project plugin. A
test pins the row so a tidy-looking wrapper is not reintroduced.

`Effect` has no `invoke-command` arm, which is what surfaced this: a wrapper
could not have forwarded to magit anyway without one.

Recency is refreshed by `:project-switch-to` when the menu opens, so the
individual rows do not each need to remember.

## PC.8 ✅ — Docs, and the one hot path

- `:help project` via the plugin's own embedded `doc/project.md`, so the manual
  travels with the plugin and never enters lattice's embedded-doc budget.
- A row in `docs/user/core-plugins.md` — **not** a new `docs/user/` page. That
  file is the core-plugin index and both existing core plugins are a row there
  pointing at their own `:help`; a second full page would duplicate the plugin's
  manual and the two would drift. No `nav.toml` work follows, because
  `core-plugins.md` is already routed. (The design fragment and this plan live
  under `docs/dev/` and are correctly absent from nav.)

**The bench became a fix.** The plan said to bench the list read at 1/50/500 and
to "assert it is a no-op when the root is already the most recent". It was not a
no-op: `remember` moved the root to the front unconditionally and the caller
wrote the store every time — once per file opened, storing bytes the store
already held. `remember` now reports whether the list actually changed and the
caller writes only then. Measuring an O(n) decode over a list bounded at 256 was
never going to be the interesting number; the unconditional write was.

---

## PC.9 ✅ — `dir-pick`, the incremental directory source

Design: [`project-commands.md` §9 H5](../../architecture/project-commands.md).

`DirPickSource` in `crates/lattice-picker/src/picker_sources.rs`, beside
`FilePickSource` and modelled on it: `live = true`, accept yields
`RoutingPayload::SuppliedValue` → `PickerAcceptOutcome::FillCaller`.

`on_query_changed` splits the query at its last `/` into a directory and a
basename prefix, expands `~`, and lists that directory's subdirectories
matching the prefix. **Not `walk_files_for_picker`** — that walk has no depth
cap and a flat 5000-entry ceiling, so pointed at `~` it truncates somewhere
arbitrary. The logic wanted is `gen:directories`' `fs_entries(ctx, false)`;
factor it out of `lattice-completion/src/builtins/generators.rs` if it can be
shared without dragging the completion engine's `GenerateContext` into
`lattice-picker`, and copy the ~20 lines with a pointer if it cannot. Do not
take a dependency between the two crates to save the duplication.

Rows carry the absolute path with a trailing `/`, so what the source shows is
what descending would produce.

**Tests.** An empty query lists `~`'s children; a partial basename filters; a
query naming an unreadable directory yields an empty list rather than an
error (design §10 — half a typed path names nothing yet, and that is most of
the keystrokes); `~` expands; accept yields `FillCaller` and never
`OpenFile`, which is the mistake `file-pick`'s own comment warns about.

**Bench.** `on_query_changed` against a directory with 1/50/5000 entries. The
number that matters is that it is a single `read_dir` and not a walk — a
regression to a recursive shape would show here and nowhere else.

## PC.10 ✅ — `descend`, and the two keys

`PickerSourceGenerator::descend(&self, ctx, routing) -> SourceResult<Option<String>>`,
defaulting to `None`. `Action::PickerDescend` calls it: `Some(query)` replaces
the picker's query and re-runs `on_query_changed`; `None` is a no-op, which is
what every existing source inherits by taking the default.

`Action::PickerQueryUpOneComponent` needs no hook — it deletes back to the
character before the previous `/`. Generic over any path-shaped query, so it
lives entirely in the picker.

Bound in `translate_picker` (`crates/lattice-host/src/input.rs`): `<C-l>`
descend, `<C-h>` up. Both are currently unbound there; the ctrl arm holds
`c n p u s v t q r` and neither `l` nor `h`.

**`<Tab>` is not touched.** It is `PickerSelectNext` in every picker and
`<S-Tab>` its peer — see design §5 for why that decides the key.

**Tests.** `<C-l>` on a `dir-pick` row replaces the query and re-lists; `<C-l>`
in the `files` picker does nothing at all (the default-`None` path, which is
the one that could silently break every other picker); `<C-h>` walks up one
component and stops at the root rather than emptying the query; `<C-h>` on a
query with no `/` clears it.

## PC.11 ✅ — `FillTarget::Action` and the boundary field

Design: [`project-commands.md` §9 H4](../../architecture/project-commands.md).

`FillTarget::Action { id: CommandId }` in `lattice-picker/src/outcome.rs`, and
its arm in `Editor::fill_captured_target` — dispatch the command with the
value as its first argument. A command that is not registered reports through
`report_vanished_caller`, not silence (design §10).

`open-picker-payload` in `wit/types.wit` gains the field naming the action,
mirrored in `boundary_effect.rs` and `Effect::OpenPicker`. Capture the target
at open, in the `Effect::OpenPicker` arm, and roll it back if the picker does
not open — the exact shape `do_open_arg_picker` already uses, including the
rollback, which exists there because a refused open would otherwise leave a
target for the next unrelated `FillCaller` to consume.

**This is a boundary change.** WIT records have no field defaults, so every
guest needs `wit-sync` + rebuild before it will instantiate: `plugins/*` via
`cargo xtask build-core-plugins`, and out-of-tree guests (the org plugin) by
hand. Say so in the commit message.

**Tests.** A guest-opened picker whose accept routes to its own ex-command with
the picked value as `Args::String`; an unregistered action reports rather than
drops; a dismissed fill-picker leaves no target behind (the YR.6 hole, in its
new variant); a picker opened with no action still `FillCaller`s to the
surface targets exactly as before.

## PC.12 ✅ — The row, end to end

`plugins/project/src/picker.rs`:

- `init` appends the `… (choose a dir)` sentinel row, pinned last and present
  even when the query is empty — that is what the create-row mechanism cannot
  do, and why this is a plain candidate rather than `create_label`.
- The empty-list `Err` goes. The picker opens with the sentinel alone (design
  §5): refusing to open put the escape hatch behind the wall it exists to get
  through.
- The row routes `InvokeCommand("project-choose-dir")`.

`plugins/project/src/lib.rs`:

- `project-choose-dir` → `Effect::OpenPicker { source: "dir-pick", … }` naming
  `project-remember-and-switch` as its fill action.
- `project-remember-and-switch <dir>` → `project_of_path`, `remember_root`,
  then `InvokeCommand("project-switch-to", root)`. **One hop** — design §5:
  `project.el` does not return you to the project list to confirm a directory
  you just chose.

**Tests.** Guest-side: the sentinel row is present with an empty query and
still present with a query that matches nothing; its routing names the command.
Host-side integration through the real component: choosing a directory
remembers it AND opens the switch-commands menu — assert both, because
remembering without the menu and the menu without remembering are each half the
feature and each looks fine alone. A directory with no root marker above it
echoes the existing refusal and remembers nothing.

## PC.13 ✅ — Docs, and the completion that was never wired

- `:project-remember`'s `dir` argument declares `completion: Some("gen:directories")`
  and `picker: Some("dir-pick")`. It has carried `completion: None` since PC.4,
  so `:project-remember <Tab>` has never completed a path — a gap PC.9 makes
  free to close, and `<C-x><C-o>` comes with it.
- `:help project` gains the row and the two keys.
- `docs/dev/architecture/picker.md` gains `dir-pick`, the `descend` hook and
  `FillTarget::Action` — the picker crate's own reference, which is where the
  next person looks for "is there a source that does X".
- Site sync (`site/scripts/sync-docs.sh`); no `nav.toml` work, both pages are
  already routed.

**What each of PC.9–PC.13 actually did, where it differed from the plan.**

- **PC.9** landed `dir-pick` and `path_entries` (the shared listing, extracted
  from `gen:directories` rather than copied). The bench found a `stat` per
  entry — `metadata()` on every entry to answer `is_dir` and read a size only a
  file listing shows — on a path that runs SYNCHRONOUSLY on the actor thread
  inside a keystroke. `file_type()` answers `is_dir` out of the `read_dir`
  buffer; symlinks still pay the stat because `file_type()` reports the link.
  50 subdirs 182 µs → 117 µs, 5000 → 15.2 ms → 7.4 ms, and `gen:files` /
  `gen:directories` took the same halving for free. The first measurement of
  this was taken beside a `cargo check` and reported the two small cases as
  90% and 24% REGRESSIONS — recorded in `benchmarks.md` because a number that
  moves the wrong way has to be re-run alone before it is believed.

- **PC.10** added `ascend` as a HOOK, which the plan said would not be needed.
  The plan reasoned that "delete back through the previous `/`" is generic over
  any path-shaped query. True — but *live* is not *path-shaped*, and `grep` is
  live, so a generic `<C-h>` would have truncated a grep pattern at a slash.

- **PC.11** collapsed both peers' inlined `Effect::OpenPicker` bodies into
  `Editor::open_picker_for_effect` rather than adding the capture and its
  rollback twice. **The mechanical pass that added `fill_action` to 37
  construction sites put `None` on the wit→native arm** — the one direction
  where the field IS the feature. Caught by review, not by a test; the boundary
  round-trip now carries a populated value, which the test list's own note
  already said was the only way to tell a carried field from a dropped one.

- **PC.12** reversed two recorded decisions and both reversals are argued in
  place: the create row (which remembers rather than creates, so the original
  objection does not reach it) and the empty-list `err`.

- **PC.13** found `:project-remember`'s `completion` and `picker` both `None`
  since PC.4 — a command whose entire argument is a directory, offering nothing
  when asked for one. Nothing justified it; they were simply never wired.

---

## PC.14 ✅ — the row did nothing, on both of its hops

Reported as *"`project-switch` has the option `… (choose a dir)` but selecting
it doesn't do anything"*, and PC.12's tests all passed: the guest emitted every
effect it was supposed to. **Nothing applied them.**

Two host seams produce effects with no renderer to hand them back to, and both
dropped the ones this flow needs:

1. `Editor::drain_pending_picker_accept`. A plugin picker source's accept is
   always async (`accept_async`, PH7.4c.2), so the commit happens a tick after
   the keystroke, off any `DispatchOutcome` a renderer will see. It forwarded
   three variants by name and `warn!`ed the rest into the log.
   `Effect::OpenPicker` was not one of the three, so accepting the row closed
   the projects picker and opened nothing.
2. `FillTarget::Action` (PC.11's own arm). It discarded `out.effects` under a
   comment asserting `apply_effect_host` had already applied them — true of
   most effects, and false of exactly the ones a plugin reaches for.
   `project-remember-and-switch` returns `Effect::OpenTransient`, so **even with
   hop 1 fixed**, choosing a directory would have remembered the project and
   opened no menu.

Fixed by one `Editor::apply_off_renderer_effect`, called by both. The allowlist
is unchanged in kind — it gained `OpenPicker` and lost its duplicate — but it
now has a single place to extend, because a second copy is a second place to
forget, and this list has silently killed four features now: `OpenTransient`
(OR.11b), `OpenBufferAt` (OR.16), `ApplyEdit` (OR.7c) and `OpenPicker` here.
The structural fix OR.16's report describes — these paths returning `Effect`s
so callers apply them through the renderer's own `apply_effect_app_arms` — is
still open and still costs a `Vec<Effect>` threaded through
`run_tick_pending`'s return.

**Why the tests missed it.** Every PC.12 test stopped at a seam boundary. The
plugin tests asserted the guest returns the right effect; PC.11's host test
asserted the fill command *runs*, with a fixture returning `Effect::None` — the
one return value that cannot tell an applied effect from a dropped one. Neither
side was wrong; the gap was between them, which is where this class of bug
always is.

**Tests.**
- `lattice-host/tests/choose_a_dir_reaches_its_picker.rs` — the sub-picker
  opens, it opens *with its fill target captured*, and the picked directory
  opens the menu. Driven through `do_picker_accept` + the async drain, not the
  sync return: a test on the sync path passes on the broken code, because there
  the renderer applies the effects.
- `lattice-plugin-host/tests/project_plugin_picker.rs` gains the guest half —
  `:project-choose-dir` returns `OpenPicker { dir-pick, fill_action }`. The
  effect being right and the host applying it are different claims, and for a
  while only the first was true.

---

## PP.1 ✅ — `dir-pick` shows where it is, and offers `../`

Two halves of one complaint: the picker listing a directory was the one surface
that could not say which directory.

- **The query opens on the start directory.** A third source hook,
  `initial_query(args)`, peer to PC.10's `descend` / `ascend`, defaulting to
  `None` (the host's existing rule — a live source's first argument seeds the
  query). The hook exists rather than the host reading `args[0]` because the
  default start belongs to the source (home, not the workspace) and because the
  argument needs normalising into a *listing* prefix: `path_entries("/tmp")`
  lists `/`'s children beginning `tmp`, where `path_entries("/tmp/")` lists
  what is inside. `:picker dir-pick /tmp` seeded the un-slashed form and had
  been walking into that since PC.9.
- **A `../` row**, first, whenever the query names a whole directory that
  exists. An ordinary row whose text is the parent's path, so `<C-l>` descends
  into it and `<CR>` supplies it with no special-casing anywhere.
- It shares `parent_of` with `ascend`, which fixed `<C-h>` at `~/` — it used to
  clear the query, which re-listed `~/`: a key that visibly did nothing. Home
  is the one case where the query's own spelling cannot name its parent, so
  that answer is absolute.

Suppressed for a filtered query (it would be the one row in the set that is not
a match) and for a path that resolves to nothing (a lone `../` there suggests
the path resolved). The `is_dir` stat is paid only when the listing came back
empty.

**Tests.** `lattice-picker`'s `dir_pick_tests` gain the row, the row/key
agreement, home's absolute parent, the root's refusal and the seeded query;
`lattice-host/tests/picker_descend.rs` drives `<C-l>` on `../` through the real
keystroke path and pins the opening query. Two existing tests changed shape
rather than meaning — both took `rows.first()` and now say which row they mean.

## PP.2 ✅ — a rooted picker names the root it is operating on

`files> ` reads the same in every checkout and so do its rows, so with two
projects open nothing on screen says which one answered.

`PickerSourceSpec.rooted` declares it; the host resolves at seat time and the
prompt reads `files ~/src/lattice> `. A **declaration, not an inference** — the
host resolves a root for every open, so it could show one everywhere and must
not: `buffers` spans every open project, `commands` is registry-wide, `lines`
is one buffer. See `picker.md` §4.2ter for the full argument and the per-source
verdicts.

Declared by `files`, `file-pick`, `grep`, every magit source. Declined by
`dir-pick` (its query is the answer) and by `projects` (its rows *are* roots,
and it lists all of them). The LSP pickers set `Picker::root_label` directly —
they are seated by hand and have no spec — for the workspace-scoped ones
(definitions, references, symbols, workspace-symbols, the two hierarchies), not
the cursor-local ones and not `:diagnostics` / `:clist`, which span every
attached server by their own definition.

`lattice_core::home::contract_tilde` is new: `expand_tilde`'s inverse, display
only, component-wise so a sibling sharing home's prefix cannot contract into a
path that does not exist.

**Cost, stated.** `picker-source-spec` crosses WIT, so every guest needs
`wit-sync` and a rebuild. The boundary round-trip and the guest fixture both
carry `rooted: true` against a `false` peer — PC.11's `fill-action` shipped
through exactly this hole, where a mechanical field-add wrote the default on
the one arm where the value is the feature.

**Tests.** `lattice-host/tests/a_rooted_picker_names_its_root.rs` (the root is
the one that was walked; two projects give two prompts; `buffers` / `commands`
/ `marks` / `registers` name nothing; `dir-pick` leaves it to its query; the
label is contracted) and a painted-frame test in `lattice-ui-tui`'s `render`
module — a prompt that carries the root in its model and never paints it is
indistinguishable, to the user, from the feature not existing.

## PB.1 ✅ — `project-buffers`

`project.el`'s `project-switch-to-buffer`: `b` under both prefixes, second row
of the switch menu, `:project-buffers [dir]`. A plugin picker source, because
`picker-context` already carries `buffers` and `workspace-root` so it computes
inside the plugin's `state:write`-only boundary with no host round-trip.

Design and the filter's stated cost: `project-commands.md` §6bis.

**Tests.** `projects.rs` pins the prefix trap (`lattice` vs `lattice-old`), the
root-contains-itself case and the empty root; `picker.rs` pins the filter, the
pathless-buffer exclusion, the searchable directory, the active-buffer sink and
the accept; `lattice-plugin-host/tests/project_plugin_picker.rs` drives all of
it through the real guest seam. `connect_picker` now resolves a source BY ID —
with two sources registered, `.next()` would silently start testing whichever
one registration emitted first.

## PP.3 ✅ — `<CR>` on `../` navigates

Reported as `WARN project: `/Users/` is not inside a project` while navigating
up. The warn was correct for what happened: `<CR>` on `../` at `~/` *chose*
`/Users` as a project, and the project flow refused it. PP.1 shipped that
reading deliberately — `../` is an ordinary row, so `<CR>` supplies its path
like every other row — and it is wrong in the way that only shows up in use.
`../` reads as a verb, and every file browser there is (netrw, oil, ranger, lf,
telescope-file-browser) treats `<CR>` on `..` as *go up*. UX-convention rule:
muscle memory is the dominant cost on a surface like this one.

A fourth source hook, `accept_navigates(ctx, candidate) -> Option<String>`,
default `None`. `Some(query)` means *this row is a signpost, not a
destination*: the host replaces the query and re-lists, the picker stays open,
and nothing is accepted — no outcome resolved, no MRU recorded, no
`PickerAccepted` published, because none of that happened.

Distinct from `descend`, which it resembles: `descend` answers for every row
that CONTAINS things, this answers for rows that are not things at all. In
`dir-pick` every other row is a directory you might be choosing, so `<C-l>`
answers for all of them and this answers only for `../`.

Checked in `do_picker_accept` before any of the three accept paths, and
native-only — a `WasmPickerSource` takes the `None` default, so the WIT
boundary is untouched and no guest needs rebuilding for this.

**Tests.** `picker_descend.rs` gains the navigation (picker still open, query
moved, no effects) and its guard — `<CR>` on a CHILD still chooses it, because
a fix that made every row navigate would turn `dir-pick` into a browser that
can never answer the question it was opened to answer.

## PP.4 ✅ — any folder is a project

Design §5 used to call a directory with no root marker above it "the one real
refusal", reasoning that the plugin holds no `fs:` grant and cannot know what a
project is without asking the host. True, and not the same claim: the host
answers *"is there a marker above this"*, and that had been standing in for
*"is this a project"* without ever being it.

A directory of notes, a scratch tree, a vendored drop, anything not yet
`git init`-ed — all are projects if you want to work in them, and every verb
this plugin has works rooted at a plain directory. The refusal bought nothing
and cost the whole flow: browsing to a folder and being told it does not count
is the picker declining to do the one thing it was opened to do.

**Resolution stays a preference, not a gate.** A marker above the path still
wins, so `:project-remember .` inside a checkout still names the checkout and a
path to a FILE still names its project rather than storing a file as a project.
Only the unresolvable case changed: it used to refuse, and now takes the path.

The automatic path is untouched and the distinction is who asked —
`root-for-buffer` answering `kind = pwd` on `document-opened` is still not
remembered, because nobody named it and the cwd standing in would put `~` at
the top of the list forever.

**Tests.** `project_plugin_picker.rs` reverses
`a_directory_that_is_not_a_project_is_refused_with_a_reason` into
`a_directory_with_no_root_marker_is_a_project_because_you_chose_it`, and adds
the guard the reversal needs: a marker above the path still resolves to the
checkout, driven with a FILE path — the case verbatim storage would mangle
worst. A fix that took the path verbatim in every case passes the first and
quietly breaks the second.

## PP.5 ✅ — `<Tab>` drills in

Reported twice: *"I can't drill down"*, then *"at `dir-pick> /Users/dh` I can
only choose `dhruva/`"*. `<C-l>` was wired correctly the whole time — a TUI
test through the real crossterm → translate → `App::apply` → host seam proves
it — and the key being pressed was `<Tab>`, which with one matching row
wrapped select-next onto the row already selected and did nothing visible.

**The first diagnosis was wrong and worth recording.** `picker_descend.rs`
drives `Editor::dispatch_chord`, and on that evidence `<C-l>` was reported as
working end to end. It is not the path a terminal keypress takes — crossterm
event, `crate::input::translate`, `App::apply`'s action match, then the host —
and a report that a key does nothing in the running editor is exactly a failure
in one of those seams. `descend_through_the_tui` now crosses all of them.

PC.10 rejected `<Tab>` (`picker.md` §4.2bis, reversed in place): it is
`PickerSelectNext` in every picker, and one picker meaning something else by it
is the inconsistency the UX-convention rule prevents. Right rule, wrong
reference — `dir-pick` is modelled on emacs `read-directory-name`, where
`<Tab>` completes and `C-n` / `C-p` move, so `<Tab>` = select-next is ours, not
emacs's.

`Action::PickerDescendOrSelectNext` carries both meanings, because translate
cannot see which source seated the picker and only the dispatcher can ask.
`do_picker_tab` tries `descend` and falls back to select-next **when the query
did not move**: `descend` already IS the depth declaration, so a separate flag
would be a second knob free to disagree with it. `<C-n>` / `<C-p>` and the
arrows are untouched; `<S-Tab>` stays `PickerSelectPrev`.

**Tests.** `descend_through_the_tui` in `lattice-ui-tui` — `<C-l>` / `<C-h>`
through the real seam, `<Tab>` drilling twice ("until I am satisfied" is the
requirement, and one drill that worked with a second that did not is the same
complaint one level deeper), and `<Tab>` still selecting next in `buffers`,
which is the half of PC.10's argument that stays true.

## PK.1 ✅ — every switch-menu verb has a direct chord

§6's two entry points are the same verbs reached two ways, and three of the six
only had the second half: `g`, `s` and `v` were menu rows with no chord, so
they were reachable only by choosing a project you were already standing in.
`project.el` binds all three. The keymap list and `switch.rs`'s defaults are now
the same letters, held together by `every_menu_row_has_a_chord`.

The magit row moved `v` → `m` in a follow-up: `v` is emacs shorthand for
`project-vc-dir` and magit takes that key because it REPLACES that command;
this row replaces nothing, it names `magit-status` outright, so the letter is
worth more as a mnemonic than as a transplant.

`m` binds to another subsystem's command, which is safe only because
`lattice_magit::install` runs at `editor_boot.rs:720` and
`lattice_plugin_loader::install` at `:2058` — a `mode-keymap-binding` resolves
its command name against the `CommandRegistry` AT REGISTRATION and an
unresolvable name is dropped silently. The menu row has a greyed-with-reason
fallback for a missing command; a chord has none.

**It also fixed a test PB.1 broke and crate-scoped gating missed.**
`both_prefixes_are_bound_in_the_modes_own_layer` asserted `== 3` per prefix and
`b` made it 4; the run that would have caught it was `-p lattice-plugin-host`,
not `-p lattice-plugin-loader`. A count is the wrong assertion for a list that
grows — it reported only `4 != 3`, naming neither the letter that arrived nor
the one that should have. It asserts the suffix SET now.

## PD.1 ✅ — `<C-d>` forgets the selected project

`<C-s>` / `<C-v>` / `<C-t>` are cheap because they are host concerns; the host
knows how to open a candidate in a split without asking. Deletion is not — only
the source knows that removing a row from `projects` means forgetting a root,
and that source is a WASM guest.

So the source owns the verb and the host owns only the key.
`PickerSourceSpec.delete_command` names an ex-command; `<C-d>` runs it with the
selected row's routing ARGUMENT and re-lists. Design: `picker.md` §4.2quater.

**A record field, not a new export.** It crosses WIT the way `rooted` and
`create_label` do, so guests rebuild but none has to add a function. The
alternative — a `delete` export symmetric with `accept` — is the more general
shape; it was not taken because naming a command is the routing every plugin
row already uses, so declaring this needs no new seam and no new capability.

**Never a filesystem delete.** `projects` forgets a path; oil and the file tree
own deleting a directory. That is also why there is no confirmation: forgetting
is trivially reversible, and a prompt on a reversible action is friction on the
common path. `project-buffers` declares no verb — removing one of its rows
would be `:bdelete`, a different verb with different consequences.

**Tests.** `lattice-host/tests/picker_delete_row.rs` — the verb runs with the
row's own argument, the picker stays open and re-lists from the store,
repeated presses walk down the list (and a press on an empty one runs nothing
rather than firing with an empty argument, which `:project-forget` would read
as the current buffer's project), the query survives, and a source declaring no
verb is silent. `picker_actor.rs` covers the boundary with a
`Some`/`None` fixture PAIR — a single `None` assertion passes whether or not
the value travels, which is the hole PC.11's `fill-action` shipped through.
