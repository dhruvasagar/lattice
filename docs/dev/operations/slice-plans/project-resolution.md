# Project resolution — slice plan

> **Status: 🚧 ACTIVE (2026-08-22).** PR.1–PR.6 ✅; PR.7 has its own spec.
> The native track is complete;
> PR.7 has its own spec. Sequencing companion to the design fragment
> [`../../architecture/project-resolution.md`](../../architecture/project-resolution.md),
> which owns *what* and *why*. This file owns *when + in what order +
> status*. User docs: `docs/user/project.md` (PR.2).

A **core** concept (`lattice-core`), not a crate and not a plugin. The
project is a pure function of a path with a cache in front; everything
one *does* with a project lives in a plugin above the WIT seam (PR.7).

Land each slice green and commit it on its own — the slice boundaries
are the review and bisect boundaries.

## Ordering rationale

The mechanism lands before any behaviour changes (PR.1–PR.2), then each
consumer flips in its own slice. That is for **reviewability and
bisect**, not risk-avoidance: pre-v1, drastic changes are acceptable
when they follow from the design. But "terminal now roots elsewhere" and
"a resolver exists" are different claims, and a bisect should be able to
land between them.

PR.6 (the WIT seam) deliberately comes *after* the native consumers are
converted. Converting them is what tells us which action seams the
plugin actually needs; committing public WIT surface before that would
be guessing, and WIT is the surface hardest to change later
(design.md §14).

## Slices

### PR.1 — core: the resolver ✅ (2026-08-21)

`lattice-core::project`: `Project`, `ProjectKind`, the `ProjectResolver`
trait, the `MarkerResolver` impl, the directory-keyed cache.

- `for_path` is total (`-> Project`, never `Option`) — §3 of the design
  explains why that signature *is* the design.
- Marker list is a **constructor parameter**, not a core-registered
  option: `lattice-config` depends on `lattice-core`, so core cannot
  name the option type.
- Zero new dependencies on `lattice-core`. No `gix` (see design §4).

*Nothing consumes it yet.* No behaviour changes.

**`:project-root` moved to PR.2.** It needs the host-registered handle
to read, and the host wiring is PR.2 — so keeping it here would have
meant a slice that touches three crates to ship an inspector for a
resolver nothing has yet constructed. PR.1 is now purely `lattice-core`.

**The totality test paid for itself immediately.** `for_path` walked a
relative path up to the *empty* path, and
`Path::new("").join(".git").exists()` tests the **process's** working
directory — so any relative path silently rooted wherever the process
was started, returning an empty root while appearing to work. Fixed by
absolutising against pwd in `start_dir`, plus an `is_absolute` refusal
in `marker_in` as belt-and-braces. This is the same
silently-resolve-against-the-wrong-base failure `magit/workdir.rs` was
written to prevent, which is a good sign the property was worth
asserting rather than a lucky catch.

*Tests:* 18 — resolution table over tempdirs (ordinary repo, repo-in-repo
→ innermost, submodule `.git`-as-file, nested markers → innermost,
directory argument, not-yet-saved path, no marker → pwd); marker
priority within a directory; custom marker set; cache population across
every directory the walk passed; `invalidate` after a mid-session
`git init`; `set_pwd` re-points pwd answers but not marker-rooted ones;
`Send + Sync` under eight concurrent threads; the totality property; and
the relative-path regression above, verified to fail without the fix.

### PR.2 — `ActionContext::project()` + retire `workspace_root_from_cwd` ✅ (2026-08-21)

`lattice-mode`: `ActionContext::project()`, composing core's `for_path`
with `BufferStore::path_for`, pwd for pathless buffers.

`lattice-host`: construct the resolver from the
`project.root-markers` typed option (new, in `lattice-config`),
register the handle, and add `:project-root` (moved here from PR.1 —
it needs a registered handle to read).

Delete `Editor::workspace_root_from_cwd`. Its three call sites —
`lattice-ui-tui/src/runtime.rs:81`, `lattice-ui-gpui/src/lib.rs:465`,
`lattice-ui-gpui/src/window.rs:5084` — collapse to one core call. This
is the renderer-independence half of the goal: today each renderer
derives a workspace root independently.

**Cross-renderer:** both peers in the same patch, per the standing rule.

**`project()` is a method, not a field** — so no dispatch site changes
and the host answers nothing it is not asked.

**The config-ordering knot, recorded because it reads as laziness
otherwise.** The persistent-config loader finds `.lattice/config.toml`
*by resolving a project root*, so the resolver must exist before the
config that configures it has been read. It is built with the defaults
and re-pointed by an `OptionChanged` subscription — TOML at startup and
`:set` later take the same path. That is why `set_markers` sits on the
trait beside `set_pwd` rather than the markers being fixed at
construction.

`Effect::PrintProjectRoot` is classified in both renderers in the same
patch (verified with `--features gui`) and is host-only at the WIT
boundary — a plugin reads the root through the `project` import (PR.6)
rather than echoing to the host's message line.

*Tests:* 14 — 6 host wiring (service under the handle ALIAS not the
concrete type, whose failure is silent; `:cd` re-points the pwd
fallback; a marker-rooted buffer ignores `:cd`; `:project-root` resolves
by the name a user types and names the marker) + 7 option (round-trip,
empty-set refused, default sourced from core rather than restated) + 1
core (`set_markers` re-points and drops stale answers).

*Doc:* `docs/user/project.md` + nav entry + site sync — scoped to what
PR.2 ships. Terminal and compilation are NOT converted yet, so the page
does not claim they root at the project.

### PR.3 — terminal ✅ (2026-08-21)

`dispatch.rs:25045` — `dirname(active file)` → project root. The
presenting bug.

Already-open terminals are unaffected structurally: `SpawnConfig.cwd`
reaches `Command::cwd` at spawn (`spawner.rs:129`), so a running shell's
cwd is the OS's state.

**There is no `terminal.cwd` option.** The plan said this slice would
respect one; it does not exist — `terminal.shell` / `terminal.cwd` are
named only in a `do_terminal_spawn` doc comment as a future T4 plan.
The stale comment is corrected rather than the option invented.

*Tests:* 2 — a terminal roots at the project and **not** at the file's
directory (the `assert_ne` is the half that fails on the old
behaviour; equality alone would pass on any code that happened to
return the repo root), and two buffers in two checkouts yield two
different roots. Both assert the value fed to `SpawnConfig.cwd` rather
than spawning a PTY: the spawn is `Command::cwd`, which is the OS's
job, and a real PTY in a unit test buys only flake.

### PR.4 — compilation, search, picker, ACP/MCP ✅ (2026-08-21)

- **compilation** — `:compile` defaults to the project root;
  `:recompile` reuses the root captured at run start, stored beside
  `last_cmdline` in `RunState` (design §2: mechanism lives in the
  subsystem it serves).
- **multibuffer search** — `providers/search.rs:114`.
- **picker file source** — `picker_sources.rs:476` and `:607`. Takes a
  root argument; `lattice-picker` reaches neither `lattice-mode` nor the
  host.
- **ACP / MCP** — `acp/supervisor.rs:786`, `mcp/install.rs:77`.

**Two things this slice found.**

*A fifth root notion.* `PickerContext.workspace_root` was
`dirname(active file)` — the same answer `:terminal` had, in a field
already named "workspace root". The file picker therefore listed only
the directory you were in, which the source's own comment already
called out as behaving "unintuitively for projects spread across many
subdirectories". Fixed at `picker_workspace_root_path` so both picker
sources inherit it, rather than each growing a resolver lookup.

*A boot-ordering bug in PR.2.* The resolver was registered at
`editor_boot.rs:1674`, AFTER the subsystem installs at 569–641 — while
PR.2's own comment claimed it was "ahead of every subsystem install".
Nothing caught it because every consumer so far looks the handle up
lazily at action time, so the editor worked and the ordering was wrong
only for a subsystem not yet written. Moved ahead of the installs and
pinned by position, since the symptom (a `None` lookup that degrades
silently) would otherwise appear far from the cause.

`:recompile` reuses the directory captured at the last real run rather
than re-resolving: by the time it fires the active buffer is
`*compilation*`, which has no path, so re-resolving would rebuild the
wrong project while looking like it worked. Stored beside
`last_cmdline` in `RunState` — same lifetime, same owner.

ACP and MCP take the **cwd-anchored** `root_from_cwd()` rather than a
per-buffer root: both are process-scoped and established once, so there
is no buffer to resolve from. One agent per checkout would be a
different feature, not this line.

*Tests:* 2 — `:recompile` reuses the first run's directory even when
handed a different one (the `*compilation*`-is-pathless case), and the
boot-order pin.

### PR.5 — magit delegates its cwd fallback ✅ (2026-08-21)

`workdir.rs::magit_workdir`'s cwd fallback delegates to the resolver.

Steps 1 and 2 of magit's resolution stay exactly as they are — a magit
buffer acting on the repository it is already showing is magit-specific
and documented in `magit-repo-scoping.md`. `RepoScopes` stays.
`gix::discover` stays, because magit needs the `Repository` object
regardless and is the one consumer for which
marker-walk-vs-gix divergence is load-bearing.

Not a half-migration: the split is the documented design, per
`documented-splits-are-not-half-migrations`.

**The bug this actually fixes.** Step 3 was `Repository::discover(".")`
— the **process's** working directory. `:cd` sets
`editor.current_dir` and never calls `set_current_dir`, so after
`:cd /other/repo` a fresh `C-x g` still opened the repository the editor
was *launched* in. Step 3 now discovers from the resolver's answer.

`start` is where discovery *begins*, not the answer: magit needs a git
worktree specifically and a project root need not be one, so `gix` still
walks up and still returns `None` outside a repository.

**Option (b): the resolver lives on `RepoScopes`.** That handle is
already carried to all ~15 `workdir_or_cwd` call sites, so threading a
second one alongside it would spend fifteen edits restating what the
type already is. It widens `RepoScopes` from "which repository each
buffer acts on" to "the context magit resolves repositories in" — which
is what `repo_for_trigger`'s three-step resolution has always been.

*Tests:* 1 — the fallback discovers from the directory it is handed. The
`assert_ne` against `magit_workdir()` is the half that fails without the
fix: the suite always runs inside a repository, so an equality-only
assertion would pass on the broken version.

### PR.6 — the WIT seam ✅ (2026-08-22)

`wit/project.wit` — a host **import**, `root-for-buffer` /
`root-for-path`, root resolution only. Host sink, linker wiring in every
world (like `logging`), fixture guest, host test.

`option<project-info>` at the boundary although the native signature is
total — and the two `none`s differ: `root-for-buffer` returns it for an
id the host never issued (untrusted input), `root-for-path` only when no
resolver is wired. A buffer that *exists* always resolves, pathless ones
as `kind = pwd`, so a guest can tell "no such buffer" from "not in a
project".

`project-info` carries the deciding `marker` as well as the root,
because "why is my root here" is the question that follows "where is
it" — the same reason `:project-root` reports it.

**Imported by the eleven ASYNC worlds, not by `error-parser`.** That
world instantiates against the host's sync linker, shared with
`grammar`, which is the keystroke path — and resolution can walk the
filesystem on a cache miss. Wiring it there would put a directory walk
one guest call away from a keystroke (paramount #1). An error-parser
wanting the project root is a real want and needs a sync-safe answer
handed in, not this seam. The refusal is recorded in `error-parser.wit`
itself so the next person does not "fix" the omission.

`PluginState` carries one `ProjectCtx { resolver, buffers }` rather than
two Options that could disagree about whether the seam is wired, stamped
in `new_store` — every store, not per-spawn-path like `log_ctx`, because
resolution needs no plugin id and so there is no path that can forget it.

*Tests:* 3, through a real `wasm32-wasip2` guest that calls both funcs
from `activate` and reports each answer back via `logging.log` (the base
world's `activate` returns nothing). A buffer resolves to its repository
root with the marker attached; an id the host never issued is `none`;
a path with no project is `pwd` with an empty marker. Host-side unit
tests of the conversion cannot prove any of this — what is under test is
that a real component reaches the import and the linker satisfies it.

### PR.7 — the project.el-style plugin ⛔ (own spec)

Switch, project list + persistence, `project-buffers`, the `C-x p`
prefix keymap, multi-project actions.

Blocked on seams that do not exist yet and are **out of scope for this
plan**: `spawn-terminal-payload` carries no `cwd`,
`open-picker-payload` carries no root, and no compilation effect exists
at all. Those payload extensions get designed against real usage once
PR.3–PR.5 have shown which roots each action actually needs.

Gets its own design fragment and slice plan. Listed here only so the
dependency is visible.

## Status table

| Slice | Status |
|---|---|
| PR.1 core resolver | ✅ |
| PR.2 `ActionContext::project()` + retire `workspace_root_from_cwd` | ✅ |
| PR.3 terminal | ✅ |
| PR.4 compilation / search / picker / ACP | ✅ |
| PR.5 magit cwd fallback | ✅ |
| PR.6 WIT seam | ✅ |
| PR.7 plugin | ⛔ own spec |
