# Project Resolution

Authoritative design for Lattice's **project** concept: the single
question "which project does this buffer belong to", answered in core,
consumed by every operation that is currently rooted at a working
directory.

A project is **not** a workspace, a session, or a mode. It is a derived
property of a path — a pure function with a cache in front of it. That
is the whole of the concept in core; everything one might *do* with a
project (switch, list, find-file-across-projects) lives in a plugin
above this seam.

Companion to `design.md` (§5.1 buffer model, §5.12 configuration) and to
`magit-repo-scoping.md`, which solves the same problem for git
specifically and whose three-step resolution predates this. Sequencing
lives in `../operations/slice-plans/project-resolution.md`.

## 1. The problem: four roots, none of them shared

Before this design, "where am I working" had four independent answers
in the tree, and no two of them agreed:

| Notion | Where | Resolves to |
|---|---|---|
| `Editor.current_dir` | `lattice-host/src/editor.rs` | `:cd` state, seeded from process cwd |
| VCS workdir | `lattice-magit/src/workdir.rs` | innermost git worktree; magit buffer → active file → cwd |
| Language root | `lattice-lsp/src/config.rs` | per-language `root_markers`, outermost workspace |
| Workspace root | `Editor::workspace_root_from_cwd` | walk up from **process cwd** for `.git` / `.lattice` |

Plus bare `std::env::current_dir()` at the seams that had no notion at
all: the multibuffer search provider's root, two picker sources, the ACP
supervisor, MCP `workspace_folders`.

And the terminal, which rooted at neither — `dirname(active file)`, so
`:terminal` with `crates/lattice-host/src/dispatch.rs` open landed in
`crates/lattice-host/src/`. That matches vim (editor cwd) and emacs
(project root) equally badly, and is what prompted this design.

The fourth row is the sharpest illustration: `workspace_root_from_cwd`
already *is* a project-root walk, complete with a `.lattice` marker —
but it is anchored to the process's cwd rather than to a buffer, and
both renderers call it independently at boot. The concept existed; it
was just not a concept anyone could reach.

The cost of this scattering is documented in the tree by the crate that
already fixed it locally. `lattice-magit/src/workdir.rs` opens:

> Every magit mode needs "where is the repository", and every one of
> them spelled it out — eleven copies of `Repository::discover(…)`.
> **This is not tidying.** `gix::discover` takes a *directory*, and
> passing it a file path fails silently […] MG.11 found three sites
> doing exactly that, one of them in `lattice-host`'s auto-head-diff
> subsystem — which meant gutter diff signs had never worked, for any
> file, since they landed.

That is the failure mode this design exists to make unrepresentable,
at every consumer rather than at magit's eleven.

## 2. The model: derived, not stateful

**A project is a pure function of a path.** There is no "current
project" anywhere in core — no mutable session state, nothing to
persist, nothing to fall out of sync, no clear-on-what rules.

Three consequences, all of them wanted:

- **Multiple projects co-exist by construction.** Buffers from three
  checkouts are open at once; each answers with its own project. An
  action in one cannot re-root another, because there is no shared cell
  for it to write.
- **Resolution is a creation-time binding for long-lived surfaces.** A
  terminal spawned in project X stays in X forever — enforced by the OS
  (`Command::cwd` at spawn), not by our bookkeeping. Same for a
  compilation run's `:recompile` root and a magit buffer's workdir.
- **The cache is an optimisation, never a source of truth.** Dropping
  the whole cache changes latency and nothing else.

### Where captured roots live

A pathless buffer that was *born* in a project (terminal, compilation,
magit) records its root at creation. That state deliberately does **not**
live in a central buffer→root map on the Editor: its lifetime matches
each subsystem's own object, so it belongs there —
compilation's beside its existing `last_cmdline` in `RunState`, magit's
in `RepoScopes`, a terminal's in the PTY itself. A central map would be
a second thing to invalidate on buffer close, for no reader that the
owning subsystem does not already have.

## 3. Resolution

```
for_path(p):
    1. marker walk    — from p's directory upward, first directory
                        containing any marker wins
    2. pwd            — Editor.current_dir (seeded from process cwd)
```

Always returns a `Project`. Never `Option<Project>`.

**That signature is the design.** `Option` is what produced magit's
eleven copies: every caller writes its own `.unwrap_or_else(cwd)`, and
one of them writes it wrong. If a consumer cannot express "no project",
it cannot get "no project" wrong.

### Markers

Ordered, first match wins within a directory:

```
.git  .hg  .jj  .lattice  Cargo.toml  go.work  go.mod
package.json  pyproject.toml  flake.nix
```

`.git` first, and `.git` alone is sufficient — no `gix` in core. A
`.git` entry marks a worktree root whether it is a directory (ordinary
clone) or a file (submodule, `git worktree add`), so the marker walk and
`gix::discover` agree in every case a user will meet.

They diverge under `GIT_WORK_TREE` / `GIT_DIR` env overrides,
`core.worktree`, and gix's ceiling-directory handling. This is accepted
and documented rather than papered over: those are rare in an editor
session, and magit — the one consumer for which the distinction is
load-bearing — keeps its own `gix` discovery, because it needs the
`Repository` object anyway.

### Innermost, not outermost

The walk stops at the **first** directory holding a marker. A submodule
is its own project; a crate inside a Cargo workspace is *not*, because
`crates/lattice-host/Cargo.toml` sits under a workspace root whose own
`Cargo.toml` is further up — but the walk stops at the crate. That is
deliberate and differs from LSP, which prefers the **outermost**
`Cargo.toml` because rust-analyzer must see the whole workspace.

The two are different questions and keep different answers. LSP's root
is where a *language server* should be started; a project root is where
*you* are working. `:terminal` in a crate landing in that crate is
right; starting a second rust-analyzer per crate would not be.

## 4. Placement

```
lattice-core     Project, ProjectKind, ProjectResolver (trait),
                 MarkerResolver (impl), the cache
lattice-mode     ActionContext::project()
lattice-config   project.root-markers typed option
lattice-host     constructs the resolver, registers the handle
wit/project.wit  root-for-buffer / root-for-path  (a host IMPORT)
<plugin>         everything one DOES with a project
```

**Core, because it is a core concept.** Every crate already depends on
`lattice-core`; nothing else in the tree is reachable from every
consumer that needs an answer. `lattice-host` cannot host it — no
subsystem crate depends on the host, so magit, terminal, compilation and
multibuffer would each be a dependency cycle away.

**No new crate.** Heuristic #6 asks what dependency surface a crate
carves out and what breaks without it. The answer here is *nothing*:
resolution is `std::path` plus `std::fs::exists`. A crate would group
files, which is what a module is for.

**Core gains zero dependencies.** This is why the marker walk is not
`gix`-backed. Only three crates depend on `lattice-vcs` today (host,
magit, and itself); routing project resolution through it would pull
`gix` — a heavy compile-time and binary-size dependency — behind
`lattice-core`, and therefore into all ~39 crates. For a feature whose
entire job is walking up a directory tree, that trade is not close.

**`ProjectResolver` is a trait, so this is reversible.** If gix-accuracy
ever matters, `lattice-host` registers a different impl; no consumer
changes, because consumers hold the trait.

**The marker list is a constructor parameter, not a core-registered
option.** `lattice-config` depends on `lattice-core`, so core cannot
name the option type. `lattice-host` reads
`project.root-markers` and passes it in — which is also what keeps core
free of a config dependency it would otherwise need for one `Vec<String>`.

## 5. `ActionContext::project()`

The concept "carries across all actions" concretely: every mode action
handler already receives an `ActionContext`, so the project is a field
access rather than a service lookup someone must remember to perform.

```rust
impl ActionContext<'_> {
	/// The project this action is acting in.
	pub fn project(&self) -> Project { … }
}
```

Composed from `ProjectResolver::for_path` and
`BufferStore::path_for(self.buffer_id)`, falling to pwd for a pathless
buffer. `lattice-mode` depends on `lattice-core`, so the type is in
scope with no new edge.

Core's primitive stays `for_path` alone. The buffer→path step lives in
`lattice-mode` because that is where `BufferStore` lives; pushing it
into core would mean core learning about buffer registries to answer a
filesystem question.

## 6. The WIT seam

`wit/project.wit`, a host **import** — what a guest consumes, like
`logging`. Not a contribution seam:

```wit
interface project {
	enum project-kind { marker, pwd }
	record project-info { root: string, kind: project-kind }
	root-for-buffer: func(buffer: u64) -> option<project-info>;
	root-for-path:   func(path: string) -> option<project-info>;
}
```

An import, so **core never depends on a plugin being alive.** Were this
a contribution seam, terminal / compilation / search would each need a
"if the project plugin loaded, ask it, else fall back" branch, and boot
ordering would become load-bearing for correctness rather than for
features — the failure mode that keeps recurring in this codebase.

`option<project-info>` at the boundary, unlike the total native
signature, because a buffer id from a guest is untrusted input: `none`
means "no such buffer", not "no project".

Sync, available in every world. It can do a filesystem walk on a cache
miss, but it runs on the plugin's own store and task — never the UI or
actor thread.

## 7. Consumers

| Consumer | Before | After |
|---|---|---|
| terminal | `dirname(active file)` | project root |
| compilation | caller-supplied `cwd` | project root; `:recompile` reuses the captured one |
| multibuffer search | `current_dir()` | project root |
| picker file source | `current_dir()` | root passed in |
| ACP / MCP | `current_dir()` | project root |
| boot config load | `workspace_root_from_cwd()` ×3 | one core call |
| magit | own 3-step | steps 1–2 unchanged; step 3's cwd fallback delegates |

`lattice-picker` depends on neither `lattice-core`'s consumers nor
`lattice-mode`, so its sources take a root argument rather than
resolving one.

**Magit keeps its own resolution.** Its step 1 — a magit buffer acts on
the repository it is already showing — is genuinely magit-specific and
documented in `magit-repo-scoping.md`; only the cwd fallback delegates.
Flattening the rest would be the half-migration
`documented-splits-are-not-half-migrations` warns about.

**LSP is untouched.** Per §3, its per-language outermost-marker root is
a different question with a different right answer.

## 8. `:cd` is unchanged

`:cd` remains path resolution for `:e` / `:w`, and the pwd that step 2
falls back to. It does **not** become a project override — that would
reintroduce exactly the mutable "current project" that §2 removes.

## 9. Paramount-goal alignment

- **#1 Performance.** Resolution is off every hot path: it fires on
  `:terminal`, `:compile`, picker open — never per keystroke, never per
  frame. The cache is keyed by *directory*, so a project's buffers share
  one entry and the walk runs once per directory per session.
- **#2 Extensibility.** The WIT import gives plugins the same answer
  core has. The marker list is a typed option, so a new ecosystem needs
  a config line, not a release — which is most of what a detector-plugin
  seam would have bought, at none of the cost.
- **#3 Modal editing.** Untouched; no grammar surface.
- **#4 Asynchronicity.** The resolver is `&self` and `Send + Sync`, so
  any thread may ask. Cache writes take a short mutex never held across
  an await.

## 10. Rejected alternatives

**A `lattice-project` crate.** Rejected under heuristic #6: names no
dependency surface it carves out, and nothing breaks structurally
without it. It would group a module's worth of files.

**Placement in `lattice-host`.** Rejected on the dependency graph: no
subsystem crate depends on the host, so the consumers that need this
most could not reach it.

**A sticky "current project" set by an explicit switch.** Rejected in
§2: buys the ability to keep working "in" project X while focused on a
file outside it, at the cost of mutable session state that must be
persisted, invalidated, and reasoned about. The derived model gives
multi-project co-existence for free; the sticky model has to be taught
not to break it.

**A per-buffer project override / pin.** Deferred, not rejected. It
generalises magit's `RepoScopes` and would handle a vendored dependency
one wants rooted at the outer project. Nothing needs it yet, and the
derived model can grow one later without any consumer changing.

**`gix`-backed discovery in core.** Rejected in §3 on the dependency
cost, with the divergence documented and the trait left as the escape
hatch.

**The whole abstraction as a project.el-style WASM plugin.** Rejected on
layering and, decisively, on what the plugin surface can actually drive:
`spawn-terminal-payload` carries no `cwd`, `open-picker-payload` carries
no root, and no compilation effect exists. A plugin could not have
opened a terminal at the project root. Making it able to would mean
adding *more* public WIT surface than the native path needs internally
— and WIT is the surface hardest to change later (design.md §14).

The emacs precedent does not transfer. `project-compile` works by
`let`-binding `default-directory`, which every command reads because it
is dynamically scoped; our consumers read `self.document.path()`
directly. Citing project.el's structure here would be reasoning from
another editor's substrate rather than ours.

**Multi-root workspace (VS Code style).** Deferred. It is design.md
Open Question #4 and entangles session persistence (#27). The derived
model already delivers what multi-root is usually wanted for — several
projects live at once — without the root-set UI, the ambiguity rules, or
the per-root LSP fan-out.

## 11. Testing

- **Resolution table** over tempdirs: ordinary repo; repo nested in
  repo; submodule (`.git` as a file); marker-only tree; nested markers
  (innermost wins); no marker anywhere (pwd); pathless buffer.
- **Cache**: hit/miss by directory, invalidation on `DocumentClosed`,
  `:project-refresh` after a `git init` mid-session.
- **Per consumer**: roots at the project and *not* at cwd — with the
  test process's cwd deliberately set elsewhere, because a test run from
  the workspace root passes on the broken version too.
- **WIT**: round-trip through a fixture guest, including the `none` case
  for an unknown buffer id.
