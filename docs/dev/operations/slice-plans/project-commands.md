# Slice plan — project commands

Design: [`../../architecture/project-commands.md`](../../architecture/project-commands.md).
Builds on [`../../architecture/project-resolution.md`](../../architecture/project-resolution.md)
(PR.6's `wit/project.wit`), which is already shipped.

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

**Deliberate ordering.** The plugin leads. PC.4–PC.6 prove the whole shape —
list, picker, menu, keymap — against the two verbs that need nothing from the
host, so the host seams are cut against a working consumer rather than
speculatively. That is the opposite of the usual "seams first" order and it is
chosen for that reason.

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
- `root-for-buffer` uses the buffer store's **`name_for` as its existence
  oracle** and short-circuits on `None` before ever consulting `path_for`. A
  test stub answering `None` there makes every resolution return `none` and the
  plugin silently remember nothing — which is how the integration test first
  failed, and why the stub now carries a comment saying so.

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
- an empty list yields a picker that **says** it is empty rather than rendering
  as an empty list (the `roam_find` rule: "no notes" and "not configured" look
  identical and have different fixes);
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
