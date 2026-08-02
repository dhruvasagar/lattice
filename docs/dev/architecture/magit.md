# Magit — Git porcelain as a core plugin

**Status:** design fragment. Contracts, data model, mode decomposition, keymap
surface, performance posture, WIT-gap analysis, slice plan. Supersedes
[`vcs-and-magit.md`](../archive/vcs-and-magit.md) (2026-05-31 sketch; archived 2026-07-25
— see header in that file). This fragment owns *what* and *why*. Slice
sequencing + per-slice status lives in
[`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md).

Sibling fragments: [`diff-system.md`](diff-system.md) (diff data model +
subsystem), [`diff-extraction.md`](diff-extraction.md) (diff as modes via
`SubsystemBoot`), [`compilation-mode.md`](compilation-mode.md) (the
synthetic-buffer + process-spawning pattern Magit reuses),
[`host-provider-boundary.md`](host-provider-boundary.md) (the boundary drawing
that keeps magit inverted out of `lattice-host`).

## 1. Why this fragment exists

The diff system (D.1–D.8) is complete: two-way/three-way/side-by-side/inline
overlay, gutter signs, hunk motions and operators, virtual rows, scroll-binding,
fold providers. The VCS data layer (`lattice-vcs` crate) and subsystem
(auto-inline-diff against HEAD, git watcher, `RepositoryEvent`) are designed but
**not implemented** — there is no `lattice-vcs` crate, no `RepositoryWatcher`,
no `GitBaseline`.

This fragment locks in:

1. **The complete Magit view surface** — every buffer view, its mode, its keymap,
   its data dependencies, so the VCS Layer 1+2 implementation can be sized to
   feed Magit's needs from day one (not retrofitted later).
2. **The deliverable path** — whether Magit ships as a WASM component plugin
   (the `vcs-and-magit.md` original intent) or as a native Rust crate through the
   `SubsystemBoot` seam (the pattern proven by diff-extraction, compilation-mode,
   and claude-code). §15 evaluates both against the paramount goals and
   heuristics.
3. **The mode decomposition** — one major mode per view type, `magit-core` as a
   shared minor mode, every chord and action handler owned by the crate that
   defines it. No `Editor::do_magit_*` methods. The acid test.
4. **The `lattice-vcs` vs `lattice-magit` crate split** — §3.1 explains why they
   are separate crates and why merging them would violate the host-provider
   boundary.

When the design is locked and VCS Layers 1+2 are built, Magit lands as the
**first heavy core plugin** — the feature that proves the extension architecture
can host serious, production-grade functionality.

> **UX (higher court):** the UX is Magit's — section-collapsible status buffer,
> transient prefix menus, hunk-at-a-time staging, inline diffs of staged changes
> in the commit buffer, blame annotations alongside code, log with graph. Every
> view is a buffer-backed Document. The UX convention is muscle memory from magit,
> fugitive, and gitsigns; the implementation carries zero new render-path risk
> because the renderer already handles the primitives (folds, virtual rows, gutter
> signs, inline diffs, styled text).

## 2. Paramount-goal alignment

- **#1 Performance.** Every git operation runs on `spawn_blocking`; buffer
  content is generated off-thread and published via `apply_edit_batch_blocking`.
  The renderer sees ordinary Documents with ordinary row streams — zero new
  hot-path branches. Per-keystroke overhead in a magit buffer is identical to
  any other buffer (chord dispatch + mode filter + action handler). No
  per-frame WASM calls.

- **#2 Extensibility.** Magit installs through the same `SubsystemBoot` seam as
  every other core feature (claude-code, terminal, compilation, diff modes).
  A third-party magit-equivalent — built on a different VCS backend (jujutsu,
  sapling, fossil) — can install through the same seam with zero host changes.
  The WASM migration path (§14) is a documented upgrade from native crate to
  component plugin; the mode architecture is identical, only the dispatch
  boundary moves.

- **#3 Extensible vim modal editing.** Every magit chord is registered through
  the `CommandRegistry` and lives at `KeymapLayer::MinorMode(magit-core)` or a
  per-view major-mode layer. Chords compose with vim grammar where applicable
  (e.g. `v` + hunk motions in a diff view = visual selection over hunks).

- **#4 Asynchronicity.** Git process I/O streams through the event bus → tick
  drain → `apply_edit_batch`. The editor actor never blocks on git. Async
  buffer progress (headerline spinner + stage) follows the `*messages*` and
  `*compilation*` precedent.

## 3. Architecture: the three-layer model

```
┌──────────────────────────────────────────────────────────────┐
│  Layer 3 — lattice-magit crate (feature-buffer modes)        │
│    Owns: magit-status-mode, magit-log-mode, magit-commit,    │
│    magit-blame, magit-diff, magit-stash, magit-branch,       │
│    magit-rebase, magit-core (shared minor mode).             │
│    Keymaps at MinorMode(magit-core) + MajorMode(per-view).   │
│    Installs through SubsystemBoot seam.                      │
│    INVERTED OUT of lattice-host (same tier as oil/file-tree).│
│    Deps: lattice-vcs, lattice-diff, lattice-mode,            │
│          lattice-runtime, lattice-core, lattice-protocol,    │
│          lattice-grammar, lattice-keymap.                    │
└────────────────────────┬─────────────────────────────────────┘
                         │ consumes
┌────────────────────────▼─────────────────────────────────────┐
│  Layer 2 — lattice-host::vcs subsystem                       │
│    RepositoryWatcher (tokio fs-watcher on .git/HEAD, index,  │
│    refs/heads/*). Fires RepositoryEvent on event bus.        │
│    Auto-registers DiffSession(GitBaseline(HEAD, path)) on    │
│    DocumentOpened — gutter signs appear immediately.         │
│    Option git.auto-head-diff (default true).                 │
│    CORE — stays in host (same category as LSP/diff).         │
└────────────────────────┬─────────────────────────────────────┘
                         │ consumes
┌────────────────────────▼─────────────────────────────────────┐
│  Layer 1 — lattice-vcs crate (data layer)                    │
│    Repository / GitBlob / Reference / WorkingTree / Index /  │
│    Commit / Branch / Stash. gix dependency isolated.         │
│    Read-API + Write-API — both complete (not stubbed).       │
│    GitBaseline (implements DiffParticipantSource).           │
│    Zero lattice-* dependencies. Leaf crate.                  │
└──────────────────────────────────────────────────────────────┘
```

### 3.1 Why `lattice-vcs` and `lattice-magit` are separate crates

Two crates, not one, because they sit on opposite sides of the host-provider
boundary and serve different consumers with different dependency profiles.

|                                   | `lattice-vcs`                                                                                                      | `lattice-magit`                                                                                                                                |
|-----------------------------------|--------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------|
| **What it is**                    | Pure data layer — git object model + read/write API                                                                | Feature-buffer UI plugin — modes, keymaps, action handlers, synthetic buffers                                                                  |
| **`lattice-*` deps**              | Zero (only `gix`, `ropey`, `smallvec`)                                                                             | ~8 (`lattice-vcs`, `lattice-diff`, `lattice-mode`, `lattice-runtime`, `lattice-core`, `lattice-protocol`, `lattice-grammar`, `lattice-keymap`) |
| **Consumed by**                   | (a) `lattice-host::vcs` subsystem (Layer 2 auto-inline-diff — CORE), (b) `lattice-magit` (Layer 3 views — FEATURE) | The CLI binary (wired through `SubsystemBoot::install`)                                                                                        |
| **Host-provider boundary**        | CORE — host depends on it for auto-inline-diff (same tier as `lattice-diff`)                                       | INVERTED OUT — host must NOT name it (same tier as oil/file-tree)                                                                              |
| **Can host function without it?** | No — auto-gutter-diff-against-HEAD is table-stakes                                                                 | Yes — the editor is fully itself without magit views                                                                                           |
| **Platform deps**                 | Pure data — builds on any target                                                                                   | Process spawning, file watching — needs `std::process`, tokio, event bus                                                                       |

If the two were merged into one `lattice-magit` crate:

- The **host** (`lattice-host::vcs`) would depend on a crate that also pulls
  in mode infrastructure, keymaps, action handlers, and synthetic-buffer
  machinery — all of which are feature-buffer concerns the host should not
  see.
- The **host-provider boundary** would blur: the same crate provides both core
  data (consumed by the host's auto-inline-diff) and feature-buffer UI
  (consumed by the CLI binary's `SubsystemBoot` install). This is exactly the
  kind of "half-migration" the standing rule forbids.
- A **third-party VCS backend** (jujutsu, sapling) would need to reimplement
  the magit UI just to get the data layer — or the data layer would need to
  be extracted later anyway.

The split matches the existing precedent: `lattice-diff` (pure algorithm crate,
zero `lattice-*` deps) vs the diff subsystem mode + overlay providers
(extracted into `lattice-diff` via `SubsystemBoot`). The data layer stays
lean; the UI layer pulls everything it needs.

### 3.2 What changed from the vcs-and-magit.md sketch

- **Write-API is no longer stubbed.** The original sketch proposed stubbing
  `Index::stage_hunk`, `Commit::create`, `Branch::checkout` as
  `unimplemented!("D.7.write — Phase 7+")`. Magit needs stage/unstage/commit/
  branch/checkout/rebase/stash from day one — the write-API ships complete in
  Layer 1.
- **`spawn-process` is NOT gated on a WIT host-service.** Magit calls the git
  CLI on `spawn_blocking` — the same native `std::process::Command` pattern
  compilation-mode uses. No WIT host-service dependency. The lighthouse-designed
  `spawn-process` WIT seam is for WASM plugins; Magit v1 doesn't cross that
  boundary.
- **Rich buffer rendering does not require new WIT primitives.** Magit buffers
  are styled text with folds, virtual rows, and gutter decorations — all of
  which the renderer already supports through the mode-architecture primitives
  (decoration providers, fold overlay service, virtual-row provider registry).
- **Deliverable path is native crate, not WASM.** The original sketch committed
  to WASM-as-first-path. §15 evaluates both against the paramount goals +
  heuristics and recommends native crate for v1, with WASM migration documented
  as a post-v1 upgrade when the host-services surface matures.

## 4. View inventory

Every magit view is a synthetic Document with a major mode. Buffer names follow
the `*magit:<kind>*` convention (consistent with `*messages*`, `*lsp*`,
`*compilation*`).

### 4.1 magit-status (`*magit:status*`)

The primary workhorse. One buffer that shows the state of the current
repository.

**Sections** (top to bottom, each collapsible via fold). "Unpulled
commits" / "Unpushed commits" sections are target design, **not built** —
`SectionKind` has no such variants; the only ahead/behind signal today is
the two counts in the branch status line (below), not a per-commit list:

| Section | Content | Data source |
|---|---|---|
| Staged changes | Files with changes in the index — paths + status only. Diffs loaded on demand via `=`. | `WorkingTree::statuses(repo)` filtered to `Modified`/`Added`/`Conflicted` (staged) |
| Unstaged changes | Files with working-tree changes — paths + status only. Diffs loaded on demand via `=`. | `WorkingTree::statuses(repo)` filtered to `Modified`/`Deleted`/`Unmerged`/`Conflicted` (unstaged) |
| Untracked files | Untracked files, listed | `WorkingTree::statuses(repo)` filtered to `Untracked` |
| Stashes | List of stashes with messages | `Stash::list(repo)` via gix |
| Recent commits | Last N commits with abbreviated SHAs and subjects | `git log --oneline -N` (CLI, not gix) |

A `Conflicted` path appears in BOTH the Staged and Unstaged sections
simultaneously — real double-listing, not a bug (see §7.2's `entry_key`
note on why this matters for expansion state).

**Section rendering:** Each section has a header line (bold, with section name
and item count) followed by its body. Sections are fold regions anchored at the
header line. The fold engine handles `TAB` / `za` / `zM` / `zR` without
magit-specific code.

**Inlined diffs:** Staged and unstaged sections start as file lists with
status labels only. Pressing `=` on a file entry loads and inlines the diff
for that file — a local edit to the buffer, not a full refresh (§6.3 has
the real mechanism: a full-text `git diff` insert with syntax-highlight
spans, not virtual-row deletion blocks or a per-file cache).

**Headerline:** a sticky virtual row above line 0 showing repo name,
branch, upstream tracking (ahead/behind), and dirty counts — ` lattice
main ↑2 ↓1  3 staged  5 unstaged `, or ` lattice  main  clean `. Built
from the same `SectionIndex` the body is formatted from, so it costs no
git call of its own, and re-set by every refresh path. See §4.9 for the
mechanism, which is shared by every magit view.

### 4.2 magit-log (`*magit:log*`)

Commit history with graph, branch and tag decorations.

- Arguments configurable via buffer-local options: `--all`, `--graph`,
  `--decorate`, `-N` (count), path filter.
- Output of `git log --oneline --graph --decorate -50` rendered as styled text.
- Each commit line: abbreviated SHA (dim), graph (styled), refs (colored tags),
  subject (normal).
- `<CR>` on a commit line opens `*magit:commit:<sha>*` showing the full diff.

### 4.3 magit-commit (`*magit:commit*`)

The commit-message editor. Opens from magit-status `c c` or directly via
`:magit-commit`.

- **Top region** (read-only, scrollable): inline diff of staged changes, the
  same render as the "Staged changes" section in magit-status.
- **Bottom region** (editable): the commit message — subject line, blank
  separator, body.
- `C-c C-c` (confirm): calls `Commit::create(repo, message)` → closes buffer →
  refreshes magit-status. `C-c C-k` (abort): closes buffer without committing.
- Amend support: `c a` pre-populates the previous commit message and uses
  `Commit::amend`.

### 4.4 magit-blame — a minor mode, not a view (MG.26b)

**No buffer.** `magit-blame-mode` is a `ModeKind::Minor` with
`ActivationPolicy::Manual`, activated on the buffer already showing the
content being blamed. It contributes one virtual row above each *chunk*
— a run of consecutive lines sharing a commit — carrying
`<sha> <author> <relative-date> <summary>`, magit's `headings` style.

Design:
[`magit-blame.md`](magit-blame.md). What this replaced rendered the
file as text, one `<sha> <author>  <code>` row per line, and that is
precisely why blame lost syntax highlighting: the buffer stopped being
the file, so it had no language and no parser. Every editor checked —
magit, fugitive, Zed, GitLens, JetBrains — annotates the real buffer,
and highlighting survives *because* the file was never replaced.

**Read-only for the duration**, which is what re-frees `<CR>` and `p`;
a minor on an editable buffer cannot take grammar keys. The override
reverts on deactivation, so the file is editable the moment blame
stops. Read-only is per-buffer state any kind can carry, so this needs
no kind-specific branch.

**Direction is mode state, not a buffer name.** Forward and reverse
blame are the same annotations from a different `git blame` invocation.
The old shape carried direction in the buffer's name because a buffer
was the only carrier available, and that forced `p` in a reverse view
to open a *new* buffer rather than walk in place. State has no such
constraint.

**`Effect::ToggleMode` is the activation seam**, and it deliberately
carries only a mode name — the grammar crate must not learn what a
blame direction is. A non-default blame (reverse, a specific revision)
therefore leaves a `BlameRequests` entry keyed by the *buffer name* it
will land on, which `on_activate` consumes. Keyed by name rather than
`BufferId` because the buffer may not exist yet: the reverse path opens
a blob buffer and activates the mode on it in one `Effect::Many`. The
entry is removed on read — leaving it would make the next plain
`:magit-blame` on that buffer silently reverse.

**The result reaches the screen with no keypress, and needs no
`inbound` wiring.** The cells worker polls
`VirtualRowProvider::version()` every tick, so bumping it *is* the
wake. `collect()` hands back rows built once when a blame landed —
re-chunking there would be exactly the UI-thread work paramount goal #1
forbids. Colours *are* resolved in `collect`, one read-lock plus an
`ArcSwap` load, which is what `MagitHeaderline::render` already does
and is what makes `:colorscheme` repaint the headings.

**`VirtualRowKind::Annotation` was added for this** and both renderers
were wired in the same patch. `Generic` carries the diff deletion-block
backdrop in TUI and GPUI alike, so a heading painted with it would read
as a removed line; `Filler` is blank alignment padding; `Sticky` pins
to the top of the pane, and an annotation has to scroll away with the
lines it describes.

**No `q`, though magit binds it.** This mode can be active on a blob
buffer, where `magit-core-mode` is also active and already binds `q` —
two minors binding one chord on one buffer resolves by registration
order, which is not a contract. Blame toggles off the way it toggled
on. Guarded by `the_blame_minor_shares_no_chord_with_magit_core`, which
the ordinary chord guard would not have caught: both chords reach a
registered action and a handler.

`magit-blame-mode` also left `magit-core-mode`'s `Majors` allowlist —
naming a minor there is an entry that can never match.

**Retired with it:** `*magit:blame:<path>*`,
`*magit:blame-reverse:<rev>:<path>*`, the blame *major*,
`blame_styled_spans` and `format_blame_porcelain`'s row formatter. The
porcelain *parser* stays, rewritten in `blame.rs` (MG.26a) to produce
chunks — headings need exactly the (sha, author, time, summary,
line-range) it extracts.

### 4.5 magit-diff (`*magit:diff*`, or path-scoped `*magit:diff:<path>*`)

Target design: a two-pane side-by-side diff (reuses D.4 pane group
machinery), left pane staged/HEAD, right pane working tree, hunk-level
staging including partial-hunk staging from a Visual-mode selection.
**Not built.** The current `MagitDiffMode` is a real, scoped middle
ground, not the target: it populates the buffer with `git diff HEAD`
(staged + unstaged changes combined against HEAD, matching this
section's original "against HEAD" framing) as plain styled text —
one buffer, no panes, no `DiffSession`/`GitBaseline` integration. It
resolves the file at cursor by scanning upward for the nearest
`diff --git a/<path> b/<path>` header (no `x`/discard chord here).
MG.18c added hunk-level `s`/`u` on top of that fallback, but only in
the *scoped* buffers: `git diff HEAD` combines staged and unstaged
changes into single hunks, so a hunk from the bare `*magit:diff*` is
not a patch against either tree and staging it is refused (§7.3).
The earlier stub closed a real bug: the
buffer used to open empty with `s`/`u` declared in the keymap but no
handler of their own, so pressing them silently fired whatever
`magit-status` handler happened to be registered, against
magit-status's captured state rather than this buffer's cursor. The
full side-by-side `DiffSession` design above, and `do`/`dp` hunk
transfer between panes, remain a real follow-up (MG.19), larger than
this pass.

The mode now also backs a path-scoped variant, opened by file-dispatch's
`d` item (§8.8): buffer name `*magit:diff:<path>*` instead of the bare
`*magit:diff*`. `on_activate` parses the optional path back out of the
buffer name (`store.name_for(buffer_id)` stripped of the
`*magit:diff:`/`*` wrapper) and runs `git diff HEAD -- <path>` instead of
the whole-tree `git diff HEAD` — same mode, same `s`/`u` handlers, same
`DiffState`, just a narrower git invocation. The unscoped `:magit-diff`
ex-command is unaffected; it still opens the bare `*magit:diff*` buffer.

### 4.6 magit-stash, magit-branch, magit-rebase

Each is a synthetic Document with its own major mode:

- **magit-stash** (`*magit:stash*`): stash list with apply/pop/drop/create
  operations, one row per stash as `  stash@{N} <message>` — the same row
  shape magit-status renders for its Stashes section, so a stash reads
  identically in both. Every chord in the buffer locates its stash by
  parsing `stash@{N}` back out of the row under the cursor, which is why
  the label is load-bearing rather than decorative: MG.15 found the list
  rendering a bare message and the parser reading a label that was never
  written, leaving `a`/`p`/`d` silently dead. `<CR>` opens §4.8.
- **magit-branch** (`*magit:branch*`): branch list with checkout/delete/merge
  operations. `c` (create) opens a two-step picker-driven wizard (pick base
  branch, then type the new name) — see §12.9.
- **magit-rebase** (`*magit:rebase[:<upstream>]*`): interactive rebase todo
  buffer. The upstream is encoded in the buffer name, mirroring magit-blame's
  file-in-buffer-name pattern (`*magit:rebase*` with no arg falls back to
  resolving `@{upstream}`). The todo is real — built from `git log --reverse
  --format="pick %h %s" <upstream>..HEAD`, not a placeholder. Editable
  pick/reword/squash/fixup/drop list. `C-c C-c` writes the buffer's
  (possibly user-edited) non-comment lines to a temp file and actually
  STARTS the rebase via `git rebase -i <upstream>` with
  `GIT_SEQUENCE_EDITOR="cp <tempfile>"` and `GIT_EDITOR=true` (accepts a
  `reword` step's original message unchanged — no message-edit UI, a known
  limitation). `C-c C-k` runs `git rebase --abort` only if
  `.git/rebase-merge` or `.git/rebase-apply` actually exists, so it can't
  fail against a rebase that was never started.

### 4.6b magit-remote (`*magit:remote*`)

The configured remotes, one row per remote: `  <name><pad>  <fetch-url>`,
with `  (push: <url>)` appended **only when the push URL differs**. `a`
add, `r` rename, `d` remove, `u` set-url, `p` prune. `:magit-remote`, or
`M` on the root dispatch. Chord table: §12.9b.

**Why a buffer and not a transient — the one deliberate divergence from
magit here.** Magit puts remote management on the `M` transient and
renders `remote.<name>.url` / `.pushurl` as *variable rows* inside the
menu. Lattice's transient substrate has no variable-row concept, so a
straight port would leave the URLs invisible and make every operation a
name typed blind — and "what is origin actually pointing at?" is the
most common reason to open remote management at all. Rendering the list
into a Document is what makes it readable, `/`-searchable, `y`-yankable
and `gr`-refreshable: paramount goal #3's everything-is-a-buffer claim
doing real work rather than being observed. It also removes machinery —
because the rows are on screen, `r` / `d` / `u` need no picker to choose
a target, unlike the transient shape which would need one for each.

**Cursor→remote mapping goes through stored state, not the rendered
line.** `RemoteState::entries` holds the `Vec<RemoteEntry>` the last
render was built from, and row `i` is entry `i - 1` (line 0 is the
`Remotes (N)` heading). Re-parsing the line under the cursor — which is
what magit-branch does — would decode the heading as a remote named
`Remotes` and hand it to `git remote remove`. Guarded by
`the_heading_row_maps_to_no_remote` plus a test asserting the renderer's
row order and the mapping agree.

**`a` / `r` / `u` are prompt-backed, and the target rides in the prompt
buffer's name.** A prompt submit fires its `on_submit_action` with the
PROMPT buffer's `buffer_id` (`Editor::do_prompt_line_submit`), so
`state_for` returns `None` there and the finish handler cannot reach the
remote buffer by id. Two consequences, both handled the way
`magit_global_mode`'s file-rename prompt already does: the remote name
is carried in `*magit:remote-{add,rename,set-url}:<name>*`, and the
refresh afterwards goes through `BufferStates::all()` — a service
lookup, which is context-free — rather than through the buffer id. `a`
chains two prompts (name, then URL) in magit's own order.

**`d` does not confirm.** §12.13 reserves ask/execute for
*irrecoverable* operations. Removing a remote drops config and tracking
refs that `a` restores in two prompts, and the URL is on the row in
front of the user at press time; asking here would dilute the prompt
that matters (`Oh`). Magit does not confirm it either.

**`p` is the only network row.** It echoes `pruning <name>…`
synchronously and refreshes when the detached task returns — the same
optimistic-echo shape §9 describes, because there is no synchronous path
back from a detached task. The others are local config edits.

**Fetch / pull / push are deliberately elsewhere.** They are
`magit_global_mode`'s `RemoteOp` — long-running network operations with
their own flag sub-transients on `f` / `F` / `P`. This mode manages
*which remotes exist and where they point*; the split is by lifetime and
by whether flags apply, not by subject matter.

**Known gap:** `u` sets the fetch URL only (`git remote set-url`). A
separately-configured push URL is *shown* but has no editing chord. The
list carrying both columns is what keeps that honest rather than hidden.

### 4.6d magit-submodule (`*magit:submodule*`)

The configured submodules, one row per module:
`  <marker> <short-sha>  <path>[  (<describe>)]`. `a` add, `u` update,
`s` sync, `d` remove. `:magit-submodule`, or `o` on the root dispatch.

**Here magit agrees with us on the shape**, unlike §4.6b:
`magit-list-submodules` opens a `*Modules*` buffer there too, and the
`o` transient carries the *operations* rather than the list. So the
UX-convention rule and paramount goal #3 point the same way, and the
implementation is `magit-remote-mode`'s — stored `entries` for the
cursor mapping, `BufferStates::all()` for the prompt-finish refresh,
prompt-name carriers for the two-step add.

**The marker column is git's, verbatim.** `-` uninitialised, ` ` in
sync, `+` moved off the recorded commit, `U` conflicted — the same
characters `git submodule status` prints, so a row reads identically in
both places. The styler colours `-` and `U` as removals and `+` as an
addition rather than giving the column one flat colour: the ones
needing attention have to be findable by scanning, which is the whole
reason to open this buffer. The headerline says the same thing in
words (`3 submodules  1 uninitialised`) because a bare total would not
convey that any of them need anything.

**`d` asks; §4.6b's `d` does not.** The difference is the §12.13 line
exactly: removing a submodule runs `deinit -f` then `rm -f`, deleting a
working tree that may hold uncommitted work git has no copy of.
Removing a remote drops config you can retype. The confirm carries the
path in a declared argument slot (`CONFIRM_TARGET_ACTIONS`), so the
answer acts on the submodule the *question* named — without the slot
the value lands nowhere and the handler re-derives from the cursor,
which a refresh landing mid-dialog can have moved.

**Magit keys not carried over, and why.** `k` remove is the up-motion,
so removal is `d` — matching branch/stash/remote, where `d` already
means "remove the row under the cursor". Magit's own `d` (unpopulate)
is not offered at all: it is `u`'s inverse, rarely wanted, and putting
it adjacent to a destructive `d` would make the dangerous one easy to
mis-hit. `p` populate and `r` register fold into `u`, which runs
`submodule update --init --recursive` — the command that subsumes both.

**No `<CR>`, and the blocker is architectural rather than an
oversight.** The obvious binding is "open this submodule's own
magit-status", but `workdir::magit_workdir` discovers from the
**process's** current directory: every magit buffer in the editor is
bound to one repository, and there is no way to point a status buffer
at a subdirectory. A chord that opened the superproject's status while
claiming to open the submodule's is worse than no chord. Per-buffer
workdir is its own slice — and the same thing magit's `Z` worktree rows
will need, so it is a shared prerequisite rather than a submodule
detail.

### 4.7 magit-revision (`*magit:commit:<sha>*`)

A read-only `git show --stat -p <sha>` view of a single commit. Opened by
magit-log's `<CR>` and magit-blame's `<CR>`, both of which resolve a sha and
open this buffer rather than duplicating a "show one commit" view per
caller. No mode-specific chords — `q`/`gr`/nav come from `magit-core` (this
mode is in its `ActivationPolicy::Majors` list); `gr` is a harmless no-op
here since a commit's content doesn't change under a fixed sha.

### 4.8 magit-stash-show (`*magit:stash:<n>*`)

A read-only `git stash show -p stash@{n}` view of one stash's patch,
opened by `<CR>` in the stash list. Before MG.15 the stash list was the
only list view with no `<CR>` — the sole exception to the `<CR>` uniformity
rule (§12.3) — and the only way to see a stash's contents was magit-status,
where `<CR>` toggles the patch inline among the other sections.

That inline toggle stays: there a stash is one row among many and
expanding in place keeps the surrounding context. Here the stash IS the
subject, which is the same reasoning that gives a commit both an inline
`=` and a dedicated `*magit:commit:<sha>*`.

Fixed-content, like magit-revision: no mode-specific chords
(`q`/`gr`/nav come from magit-core), and `gr` is a deliberate no-op
because `stash@{n}`'s patch does not change under a fixed index.
Dropping or popping a stash renumbers its *neighbours*, which is why the
buffer name carries the index it was opened at and the stash list — not
this buffer — is what refreshes.

### 4.9 Headerline — every view answers "what am I looking at?"

Each magit view is a slab of git output whose identity lives outside the
text. `*magit:diff*` does not say which scope it diffed; `*magit:blame:
x.rs*` does not say which revision `p` has walked back to; the status
buffer computed branch and ahead/behind on every refresh and displayed
neither. Every view therefore carries one sticky row above line 0,
through `lattice_cells::{Headerline, HeaderlineProvider}` — the same
mechanism `lattice-multibuffer` and `lattice-compilation` use.

**One provider, every view.** There is a single `Headerline` impl
(`lattice_magit::headerline::MagitHeaderline`); the per-view difference
is *data*, a `Vec<Field>` where each `Field` is a string plus a
`FieldStyle` naming its git role (`Sha`, `Branch`, `Ref`, `Author`,
`Alert`, `Label`). No `match buffer_kind`, no impl per view — adding a
view means adding a field-builder function.

**Compact and symbol-led**, not `Head:`-labelled: colour carries the
field identity, so the row stays readable on a narrow split. This
matches the two headerlines lattice already ships rather than Emacs
magit's in-body `Head:` / `Merge:` lines.

| View | Row |
|---|---|
| magit-status | `lattice  main ↑2 ↓1  3 staged  5 unstaged` |
| magit-commit | `main  3 files +120 −18  AMEND` |
| magit-revision | `a1b2c3d  Jane Doe  3 days ago  Fix the thing` |
| magit-file-revision | `src/main.rs  @  a1b2c3d` (or `@  index`) |
| magit-diff | `staged  src/main.rs` |
| magit-log | `HEAD  50 commits  src/main.rs` |
| magit-blame | `src/main.rs  @  a1b2c3d` |
| magit-branch | `main  12 branches` |
| magit-remote | `2 remotes` |
| magit-submodule | `3 submodules  1 uninitialised` |
| magit-status, bisecting | `lattice  a1b2c3d  BISECTING 3 left, ~2 steps  clean` |
| magit-stash | `3 stashes` |
| magit-stash-show | `stash@{2}  WIP on main: fix the thing` |
| magit-rebase | `onto  origin/main  4 commits  REBASE IN PROGRESS` |

**No git call of its own.** Fields are produced by the same blocking
builder that produces the buffer's text — `build_and_format`,
`build_branch_list`, `run_log`, and peers — so a header is a byproduct
of work already done. The two exceptions are honest: magit-revision runs
one `git show -s --format=%h%x00%an%x00%ar%x00%s` (a metadata-only read
next to the patch it already fetches, and `--format` rather than
scraping `git show`'s locale-dependent header), and magit-commit adds
one `rev-parse --abbrev-ref HEAD` inside the `spawn_blocking` that
already runs two git commands.

**No work per tick.** The cells worker polls `version()` every tick and
calls `render()` only when it advanced. `set()` compares before it
bumps, so a `gr` that finds the same branch and the same counts costs
one comparison and no repaint (paramount goal #1). Measured:
`version()` 26ns, `render()` 581ns, an unchanged `set()` 129ns — see
the `headerline` bench.

**Theme-live.** Colours resolve inside `render()`, and the theme's
resolved version folds into the row's own, so `:colorscheme` repaints
the header rather than leaving it on the previous palette. The four git
roles reuse the MG.11 `magit.*` palette (registered as builtins because
`lattice-syntax`'s styled-span table resolves them by builtin id); the
two header-only roles, `magit.headerline.alert` and
`magit.headerline.label`, are registered by the mode itself.

**Lifecycle.** The provider is installed in `on_activate`'s synchronous
prefix, alongside the MG.13 state publish, and renders nothing (`render`
returns `None`) until its first fields land — so a buffer never shows a
half-built row that shifts content down and back up. Teardown rides the
mode's Guard: `HeaderlineRegistration::drop` unregisters.

## 5. Mode decomposition

One crate, two layers of modes: a **shared minor mode** (`magit-core`) active
on every magit buffer, and **per-view major modes** for view-specific chords
and lifecycle.

```
lattice-magit crate
├── magit-global-mode   (MinorMode, ActivationPolicy::Universal)
│   ├── Keymap:  C-x g  → :magit-status
│   │            C-c g  → :magit-dispatch (Effect::OpenTransient)
│   │            C-c f  → :magit-file-dispatch (Effect::OpenTransient)
│   └── ActionHandlers: action:magit-global-* (status/commit/log/branch/
│       stash/rebase/pull/push/file-stage/file-diff/branch-create-finish) —
│       the root dispatch + file-dispatch transients' items (and the
│       branch-create wizard's prompt) fire these, registered ONCE
│       process-wide via a static OnceLock (see §8.8)
│
├── magit-core          (MinorMode, ActivationPolicy::Majors([...9 major
│   │                    modes incl. magit-revision-mode]))
│   ├── Keymap:  gr     → refresh
│   │            q      → close (bury, return to previous buffer)
│   │            ]] / [[→ next/prev section
│   │            ]f / [f→ next/prev file/entry
│   │            ]c / [c→ next/prev hunk
│   │            TAB    → toggle section fold
│   │            S-TAB  → cycle section visibility
│   ├── ActionHandlers: refresh, close, navigation, fold
│   └── Shared: `ActionRegsGuard` — RAII guard every non-status mode below
│       uses to hold its `ActionHandlerRegistration`s (see §5.2)
│
├── magit-status-mode   (MajorMode)
│   ├── Keymap:  s / u / x → stage / unstage / discard file at cursor
│   │            cc / ca    → commit / amend
│   │            =          → toggle inline diff (file-level)
│   │            p          → stage hunk interactively (not supported — see §12.2)
│   │            <CR>       → open/visit at cursor
│   ├── ActionHandlers: stage/unstage/discard at cursor, open views
│   └── Buffer provisioning: ensure *magit:status*, populate, refresh;
│       registers its own `MagitStatusFoldSource` (§7.1)
│
├── magit-commit-mode   (MajorMode)
│   ├── Keymap:  C-c C-c → commit
│   │            C-c C-k → abort
│   ├── ActionHandlers: commit, abort
│   └── Buffer provisioning: ensure *magit:commit*/*magit:amend*, populate
│       staged diff + (amend) prior message
│
├── magit-log-mode      (MajorMode)
│   ├── Keymap:  <CR>   → open *magit:commit:<sha>* (magit-revision-mode)
│   ├── ActionHandlers: open commit, refresh
│   └── Buffer provisioning: ensure *magit:log*, run git log, render
│
├── magit-revision-mode (MajorMode) — new: `git show --stat -p <sha>` for
│   │                    one commit; buffer name carries the sha
│   ├── Keymap:  (none — q/gr/nav inherited from magit-core)
│   └── Buffer provisioning: ensure *magit:commit:<sha>*, render
│
├── magit-blame-mode    (MajorMode)
│   ├── Keymap:  <CR>   → open *magit:commit:<sha>* for blamed line
│   │            p      → re-blame at the parent of the revision shown
│   ├── ActionHandlers: open commit at SHA, re-blame at parent
│   └── Buffer provisioning: ensure *magit:blame:<path>*, render annotations
│       inline (not a gutter column — see §10.2)
│
├── magit-diff-mode     (MajorMode)
│   ├── Keymap:  s / u → stage / unstage file at cursor (no `x`)
│   ├── ActionHandlers: stage/unstage file at cursor, refresh
│   └── Buffer provisioning: `git diff HEAD` (staged+unstaged combined) as
│       plain text — no `DiffSession`, no panes (§4.5)
│
├── magit-stash-mode    (MajorMode)
│   ├── Keymap:  a → apply stash    p → pop stash
│   │            d → drop stash     z → create new stash
│   ├── ActionHandlers: apply/pop/drop/create stash
│   └── Buffer provisioning: ensure *magit:stash*, list stashes
│
├── magit-branch-mode   (MajorMode)
│   ├── Keymap:  <CR>  → checkout branch
│   │            c     → open branch-create wizard (Effect::OpenPicker
│   │                    { source: "magit-branch-pick-base" })
│   │            d     → delete branch
│   │            m     → merge branch
│   ├── ActionHandlers: checkout/delete/merge branch; create's second step
│   │       (`action:magit-branch-create-finish`) is a GLOBAL handler in
│   │       magit-global-mode, not here — it fires against the PROMPT
│   │       buffer opened by the wizard's picker step, not this buffer
│   └── Buffer provisioning: ensure *magit:branch*, list branches
│
└── magit-rebase-mode   (MajorMode)
    ├── Keymap:  C-c C-c → execute rebase
    │            C-c C-k → abort rebase (only if a rebase is in progress)
    │            editable buffer (pick/reword/squash/fixup/drop), a REAL
    │            todo from `git log --reverse` against the resolved upstream
    ├── ActionHandlers: execute/abort rebase
    └── Buffer provisioning: ensure *magit:rebase[:<upstream>]*, populate
        real todo list
```

**Standing-rule check (mode ownership):** every chord (`s`, `u`, `c c`, `TAB`,
etc.) is registered by the mode that owns the buffer it fires on. The
action-handler body lives in the same crate as the keymap registration.
`lattice-magit/src/magit_status_mode.rs` contains both the keymap definition
AND the handler bodies (`crate::actions::register_action_handlers`). No
`Editor::do_magit_stage_hunk` shim exists. The acid test holds. One real
gotcha this surfaced: every non-status mode used to `mem::forget` its
`Vec<ActionHandlerRegistration>` rather than holding it in the mode's
`Guard` — harmless for a single open buffer, but with two buffers of the
same major mode open simultaneously the second's registration silently
replaced the first's (registry is last-write-wins per `CommandId`), so
firing a chord in buffer A could execute buffer B's captured state against
A's cursor. Fixed by a shared `ActionRegsGuard` (`magit_core_mode.rs`) that
all modes but magit-status now hold — magit-status already did this
correctly via its own `MagitStatusGuard`.

### 5.1 Activation policy

- **`magit-core` minor mode**: `ActivationPolicy::Majors(vec![...])` naming
  every magit major mode explicitly (status, commit, diff, log, blame,
  stash, branch, rebase, revision) — not a major-id pattern matcher.
- **`magit-global-mode` minor mode**: `ActivationPolicy::Universal` — active
  on every buffer, of any kind, so `C-x g`/`C-c g`/`C-c f` work from
  anywhere. `on_activate` runs on EVERY buffer activation under this
  policy, not once (see §8.8 for the idempotency gotcha this creates for
  the global action handlers it also registers).
- **Major modes**: activated by id when the buffer is created via
  `ctx.activator.ensure_named_document(name, mode_id, flags)` (an ex-command's
  `apply` closure returning `Effect::OpenSyntheticBuffer { name, mode_id }`,
  resolved by the host the same way every other synthetic buffer is).

### 5.2 Lifecycle — `ModeActivator` + tick drain pattern

Each magit view follows the synthetic-buffer lifecycle proven by `*compilation*`
and `*messages*`, with one simplification from what was originally planned
here: there is no `RepositoryEvent` to subscribe to yet (VCS Layer 2 — the
`RepositoryWatcher` — is still design-only, see §1). Refresh is
trigger-driven, not filesystem-watch-driven:

1. **Trigger** (ex-command): calls `ctx.activator.ensure_named_document(name,
   mode_id, flags)`. The activator creates the buffer, sets the major mode,
   returns `BufferId`.
2. **`on_activate`**: pulls the `DocumentHandle`, runs an initial
   `spawn_blocking` refresh, populates the buffer, and registers this
   buffer's action handlers into a `Vec<ActionHandlerRegistration>` held by
   the mode's `Guard` (see §5's standing-rule check for why this matters).
3. **Refresh task** (`spawn_blocking`): runs the view's git command(s) —
   lightweight for magit-status (`git status --porcelain`-equivalent via
   gitoxide, `git stash list`, `git log --oneline -N`), heavier where the
   view IS the content (magit-diff's `git diff HEAD`, magit-blame's
   `git blame --line-porcelain`). Produces formatted text, applied via
   `apply_edit_batch`.
4. **Refresh trigger, in practice**: `gr` (explicit, every view) and
   post-mutation auto-refresh (stage/unstage/discard/checkout/etc. — see
   §13's async-architecture note) both re-run step 3. There is no
   background watch: an external change to the repo (another terminal's
   `git commit`) is not picked up until the user presses `gr` or triggers a
   mutation from within the buffer.
5. **Guard `Drop`** (deactivation): drops the action-handler registrations
   (unregistering them — see §5's standing-rule check), removes any
   registered fold source (magit-status only, §7.1). Buffer survives frozen
   until `:bd`.

## 6. Data flow — lazy by default

Magit buffers are **lazy**: only the data needed to paint the viewport is
fetched. Expensive operations (diffs, blame, commit details) are deferred
until explicitly invoked by the user. This is the single most important
performance decision in the magit design — Emacs magit slows down on large
repos because it pre-computes every diff on status open; Lattice never does.

### 6.1 Lazy strategy per view

| View | On open / refresh | On demand |
|---|---|---|
| **magit-status** | `git status --porcelain` (file paths + status letters), `git stash list`, `git log --oneline -N` | `=` loads `git diff --cached <path>` / `git diff <path>` per-file |
| **magit-diff** | Nothing (explicitly invoked by user) | Diff loaded on open (the view IS the diff) |
| **magit-log** | `git log --oneline --graph --decorate -N` | `<CR>` loads `git show <sha>` for the commit at cursor |
| **magit-blame** | Nothing (explicitly invoked by user) | Blame loaded on open (the view IS the blame) |
| **magit-commit** | `git diff --cached` (the only exception — reviewing staged changes IS the purpose) | — |

**Why magit-commit is the exception:** The commit buffer exists solely to
review staged changes and write a message. The staged diff IS the content
the user came to see — deferring it would leave an empty buffer. However,
it's scoped to staged changes only (not the full working tree), and the
load is async. There is no progress indicator: the buffer carries a
headerline (§4.9) but it reports what is staged, not load progress —
the row appears with the content, which for a staged diff is fast enough
that a spinner would flash rather than inform.

### 6.2 Status buffer refresh — fast path

Triggered by `gr` or by an auto-refresh after a mutation (stage/unstage/
discard) — NOT by a filesystem watch (§5.2: there is no `RepositoryEvent`
yet). The refresh task runs on `spawn_blocking` (`refresh::build_and_format`):

1. `WorkingTree::statuses(repo)` → list of changed files with status codes
   (gitoxide — fast, no process spawn).
2. `Stash::list(repo)` → stash entries (gitoxide).
3. `git log --oneline -20 --format="%h %s"` → recent commits (CLI, not gix).
4. Format sections as file lists with status labels via `SectionIndex::
   format_buffer_styled`. **No diffs.**
5. `apply_edit_batch` (async, on the current task, after the blocking phase
   completes) replaces buffer content wholesale.
6. Clear `StatusBufferState::expanded` — a full-buffer replace collapses any
   inline expansion, so stale entries there would desync the next `=`/`<CR>`
   press's "already expanded" check from what's actually on screen (§6.3).

The buffer shows file names and statuses only — a list view, not a diff
view. Rendering is O(files), not O(lines-of-diff).

### 6.3 Lazy diff/patch loading — `=` on a file, `<CR>` on a stash, `d` for a dedicated buffer

One shared mechanism (`toggle_expand` in `actions.rs`) backs `=` on a file
entry and `<CR>` on a stash entry — there is no per-kind duplication. The
cursor line is classified first (`classify_line` → `StatusLine::{File,
Stash, Commit}`, §7.2), then:

1. Look up the entry's `entry_key` in `StatusBufferState::expanded:
   HashMap<String, usize>` (key → inserted line count, not a hunk-aware
   cache — there is no `DiffCache` and no per-file `git diff` caching keyed
   by status version. MG.18c parses hunks out of the buffer text at action
   time, §7.3, and caches nothing: a cache would give `]c` one set of hunk
   boundaries and `s` another, with nothing forcing them to agree).
2. If already expanded: delete exactly the recorded line count starting the
   row after the entry (`collapse_range` — the end must land at column 0 of
   the following row, not that row's text length, or the collapse eats the
   next entry's content; see the fix's regression tests in `actions.rs`).
3. If not expanded: run `git diff [--cached] -- <path>` (file) / `git stash
   show -p` (stash) on `spawn_blocking`, apply line-level syntax-highlight
   spans (add/remove/hunk-header/diff-header — `highlight::diff_styled_spans`,
   NOT the diff system's virtual-row deletion-block machinery), insert the
   whole text as one edit below the entry line, and record the inserted
   line count in `expanded`.

Both branches record/clear `expanded` only AFTER the edit has actually
landed (not eagerly before spawning the task) — recording eagerly let a
rapid second toggle race ahead of the edit and compute a delete/insert
range against rows that didn't reflect it yet.

`=` on a section header does NOT currently expand every file in the
section — only single-entry toggle is implemented.

`<CR>` on a **commit** entry does NOT go through `toggle_expand` — it opens
the dedicated `*magit:commit:<sha>*` buffer (`magit-revision-mode`),
matching every other magit view with a per-row SHA (log, blame, rebase).
This changed from an earlier inline-toggle behaviour identical to `=`/stash
— kept for stashes (which have no dedicated "stash detail" mode to open)
but inconsistent for commits, which do.

`d` on a **file** entry (staged or unstaged) is a third path, independent
of `toggle_expand`: it opens a dedicated `magit-diff-mode` buffer scoped to
that file AND that section's baseline (`*magit:diff:staged:<path>*` /
`*magit:diff:unstaged:<path>*` — see `magit_diff_mode::DiffScope`), rather
than inserting the diff inline. Exists alongside `=`, not instead of it —
`=` is the quick in-place look, `d` is for a diff too large to read
comfortably without its own scrollable buffer (and without growing the
status buffer's line count or re-triggering its splice-based inline-
highlight bookkeeping for every subsequent entry).

### 6.4 Expansion tracking — `StatusBufferState::expanded`

The target design's per-file `DiffCache` (parsed hunks, per-file staging,
staleness detection) is **not built**. What exists is much simpler — a
map from entry identity to how many buffer lines its expansion occupies:

```rust
pub struct StatusBufferState {
    // ...
    pub expanded: HashMap<String, usize>,
}
```

Keyed by `entry_key()`, which includes `staged` for `File` entries — a
`Conflicted` path appears in BOTH the Staged and Unstaged sections at once
(§4.1), as two distinct buffer rows that can be independently expanded;
collapsing that distinction into a path-only key would let expanding one
row's diff make `toggle_expand` treat the *other* row as already-expanded
too, corrupting the collapse.

No hunk data is parsed or stored — `s`/`u`/`x` always act at file
granularity (§7.3), regardless of whether the diff is expanded. No
invalidation-on-repo-change exists either, since there's no
`RepositoryEvent`: a full refresh (§6.2) simply clears the whole map,
collapsing every expansion unconditionally.

### 6.5 Process spawning

## 7. Section model and fold integration

Sections are the core UX primitive in magit-status. By default, sections show
**file paths and status labels only** — no diffs are pre-computed. Diffs load
on demand when the user presses `=` on a file entry (§6.3).

```
Staged changes (3)                                    ← header (not foldable — §7.1)
  modified     src/main.rs                             ← file entry (status only)
  modified     src/lib.rs
  ...
```

After pressing `=` on `src/main.rs` (actual current rendering — no
separator lines, no fold triangle; the raw `git diff` text is inserted
directly below the entry with syntax-highlight spans, and the whole
inserted span is what `MagitStatusFoldSource` folds as one unit):

```
Staged changes (3)
  modified     src/main.rs                             ← file entry (state: expanded)
diff --git a/src/main.rs b/src/main.rs
@@ -1,2 +1,2 @@
-old line
+new line
  modified     src/lib.rs                              ← file entry (status only, not expanded)
  ...
```

### 7.1 Section folds

- **Section header → fold range.** Target design; **not built** — there is
  no fold registered for the section headers themselves today, only for
  *expanded entries* (below). `TAB`/`S-TAB` at a section header currently
  have nothing to fold there.
- **Expanded-entry fold → nested fold.** What IS built, and differs from
  the original design: `MagitStatusFoldSource` (`fold_source.rs`) is an
  overlay `FoldSource` — mirroring `lattice_diff::HunkFoldSource`'s shape —
  that recomputes fold ranges live from `StatusBufferState::expanded` plus
  a buffer scan on every `compute_folds()` call, rather than caching stale
  line numbers. For each expanded entry it emits one outer fold (header
  line through the end of the inserted patch) containing one inner fold
  per `@@ ...@@` hunk within it — nesting expressed the same way the
  generic fold engine expresses it everywhere else: range containment.
- **Registered by `MagitStatusMode`, not `magit-core`.** `MagitStatusMode::
  on_activate` registers the source via the generic `FoldOverlayService`
  extension point (`FoldOverlayServiceHandle::add_source`); `MagitStatusGuard::
  drop` removes it — same Drop-based lifecycle `DiffModeGuard` and
  `MultibufferModeGuard` use. Other magit views have no folds at all today.
- **`magit-core`'s `TAB`/`S-TAB` dispatch to the generic fold engine.**
  `Effect::AppAction(ToggleFoldAtCursor)` / `CycleFoldsGlobal` — previously
  these unconditionally returned `None` (dead keys).

### 7.2 Section index (mode-private data)

The section index is an in-memory data structure built during buffer refresh.
It stores file paths and status labels — **not diffs**:

```rust
pub struct SectionIndex {
    pub sections: Vec<Section>,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
}

pub struct Section {
    pub kind: SectionKind,       // Staged, Unstaged, Untracked, Stashes, RecentCommits
    pub header_line: usize,
    pub body_start: usize,
    pub body_end: usize,
    pub entries: Vec<SectionEntry>,
}

pub enum SectionEntry {
    File { path: PathBuf, status: PathStatus },
    Stash { index: usize, message: String },
    Commit { sha: String, subject: String },
    UntrackedFile { path: PathBuf },
}
```

This matches the real `sections.rs` types (the target design's separate
`DiffCache` keyed by `(path, section)` with parsed `Hunk`/`ParsedHunk` data
does not exist — see §6.4 for what actually tracks expansion state).

A second, independent data structure resolves *which* line the cursor is
on back to a `StatusLine` — `classify_line`/`classify_line_text` in
`actions.rs`. It matches the line against `FILE_LABELS` (a fixed list —
`"clean"`, `"modified"`, `"new file"`, `"deleted"`, `"untracked"`,
`"ignored"`, `"unmerged"` — checked as whole-word prefixes) rather than
guessing at word boundaries. This exists because a naive "split the line on
the first space" parse mis-splits the two-word `"new file"` label, taking
`"file"` as the label and the wrong half of the path — a real bug this
replaced. `status_label()` (`sections.rs`) and `FILE_LABELS` (`actions.rs`)
must stay in sync; a unit test in `sections.rs` checks that every rendered
label is a member of `FILE_LABELS` mechanically rather than by inspection.

Both live as helper data in `lattice-magit` (`StatusBufferState` on the
mode's `Guard`-reachable state, not `RefCell`s specifically) — NOT on
`Document` or `Editor`. Consumed only by magit action handlers. This is the
**substrate-helper** pattern (CLAUDE.md substrate-vs-helper rule): consumed
only by the mode's own handlers → lives as helper data in `lattice-magit`,
not as a `Document` trait method.

### 7.3 Staging from sections — hunk, then file

`s`/`u`/`x` resolve **hunk-at-cursor first, file-at-cursor second**
(MG.18c). A cursor inside an expanded diff's hunk body acts on that
hunk; anywhere else `classify_line` resolves the whole
`StatusLine::File`/`Stash`/`Commit` exactly as before, so no
pre-MG.18c behaviour changed.

The hunk is parsed out of the buffer's own text — the same boundaries
§7.5's `]c`/`[c` walk — and turned into a standalone patch piped to
`git apply` (`--cached` to stage, `--cached --reverse` to unstage,
`--reverse` alone to discard from the worktree). There is no index API
for "stage hunk N of path P"; `git add -p` synthesizes a patch too.
The resolution lives in `magit-core-mode`, not per view: a hunk is a
property of diff text, identical in every magit buffer.

Which of the three chords applies is gated on
`MagitView::diff_source(cursor)` — magit-status answers from the
section header above the cursor, magit-diff from its buffer-name
scope. A mismatch is refused with what to press instead. This is not
politeness: `x` on a *staged* hunk would not be refused by git, and
would remove the change from the working tree while leaving it in the
index. Design:
[`magit-hunk-staging.md`](magit-hunk-staging.md).

A Visual-mode **region** narrows the hunk to the selected lines
(MG.18e): the body is rewritten so unselected additions are dropped and
unselected removals become context, with both counts recounted. `s` /
`u` / `x` are bound in Visual mode in the views that offer them, and
`ActionContext::selection` is how the handler sees the region.

`git add -p`-equivalent *interactive* staging remains unsupported
(§12.2's `p` chord explains why: it's genuinely interactive over
stdin, which the TUI's raw-mode input loop already owns — running it
via `Command::output()` would hang the actor waiting on a child that's
also waiting on stdin neither process routes to the other). `git
apply` has none of that: the complete patch is written, the pipe
closed, the child exits.

### 7.4 Stale hunk boundary detection

**Fulfilled by `git apply`'s exact context match** (MG.18c), not by a
separate heuristic. A synthesized patch carries the hunk's full body
including context, and `git apply` validates every context line
against the real target — so a patch built from a buffer that has
drifted from the tree is refused outright rather than applied at a
plausible-looking offset. The failure path is "report and refresh",
never a silent partial write.

### 7.5 Navigation model — three levels, three chord families

The magit-status buffer has a three-level structure:
**sections → files/entries → hunks**. Navigation follows vim-unimpaired
conventions adapted to this hierarchy.

| Level | What you navigate | Chord family | Walks (actual) | Registered by |
|---|---|---|---|---|
| **Sections** | Top-level section headers (Staged, Unstaged, Untracked, Stashes, Recent commits) | `]]` / `[[` | A raw buffer scan for lines matching `sections::is_section_header` (untrimmed-line prefix check against `SECTION_HEADER_PREFIXES`) — NOT `SectionIndex::sections` (that structure isn't retained past the refresh that built it) | `magit-core` |
| **Files/entries** | Any indented, non-blank line | `]f` / `[f` | A raw buffer scan for lines whose UNTRIMMED text starts with `"  "` — every entry line, not "current section" scoped; see below for a real bug this fixed | `magit-core` |
| **Hunks** | `@@ ...@@` / `diff --git` lines anywhere in the buffer | `]c` / `[c` | A raw buffer scan for lines starting with `@@` or `diff --git` (trimmed) — same scan in every magit buffer, magit-status and magit-diff alike | `magit-core` |

None of these three walkers consult `SectionIndex`, `StatusBufferState::
expanded`, or any per-file cache — they are plain buffer-text scans
(`section_headers`/`entry_lines`/`hunk_lines` in `magit_core_mode.rs`),
identical in every magit buffer kind. The target design's
`DiffCache`-aware "only walk expanded files' hunks" behavior for
magit-status, and the `HunkIndex`-backed `DiffSession` navigation for
magit-diff, are **not built** — there is no `DiffSession` or `diff-mode`
minor mode active on any magit buffer; `]c`/`[c` are entirely `magit-core`'s
in every case.

**A real bug this uncovered:** `entry_lines` used to check
`starts_with("  ")` on the line AFTER trimming it — `trim()` strips all
leading whitespace, so a trimmed string can never start with two spaces.
The check was unsatisfiable; `]f`/`[f` never navigated anywhere, on any
magit buffer, from the moment they were written. Fixed by checking the raw
(untrimmed) line and trimming only for the prefix comparisons that follow.

**Multi-file navigation** (`]E`/`[E`, `]e`/`[e`) from the multibuffer system
is inherited when a project-wide-diff multibuffer includes magit excerpts —
zero magit-specific code needed for cross-file jumping. (Aspirational: no
magit view currently participates in a project-wide-diff multibuffer.)

**Section visibility cycling** (`S-TAB`) — target design: a 4-step cycle
from all-expanded down to only-section-headers-visible. **Not built as
described**, since there are no section-level folds to cycle through
(§7.1) — `TAB`/`S-TAB` dispatch to the generic fold engine
(`Effect::AppAction(ToggleFoldAtCursor)` / `CycleFoldsGlobal`) and only
ever have something to act on where `MagitStatusFoldSource` has registered
a fold: an expanded entry's header (outer) or one of its `@@` hunks
(inner). Folding an unexpanded file/section header is currently a no-op —
there's nothing registered there to fold.

## 8. Transient menu design

Magit's "transient" presents groups of options with single-key selection,
toggleable flags, argument inputs, and a live command preview. Rather than
building a separate `PopupMenu` host primitive, the transient uses the
**existing picker** as its rendering and interaction surface — extended
with a transient interaction mode.

### 8.1 Why the picker

The picker already provides everything a transient menu needs as rendering
and interaction substrate:

- **Floating overlay rendering** — TUI + GPUI parity, styled text, scroll
- **Keyboard capture** — `j`/`k` navigation, `q`/`Esc` dismiss, single-key
  chords via the existing key capture path
- **Display preferences** — `PickerDisplay::BottomPopup`, `Floating`,
  `Inline`; the transient can configure its position
- **Preview pane** — already renders above/below the candidate list;
  repurposed as the transient's live command preview
- **Header/footer** — picker title becomes the transient title; footer
  becomes the dismiss/back hint

What the picker needs for transient mode:

1. **Grouped, non-filterable entries** — the transient is a fixed action
   menu, not a searchable candidate list. A `PickerMode::Transient` variant
   tells the picker to render grouped entries with section headers instead
   of a flat filterable list.
2. **Single-key triggers** — each entry carries a key binding; pressing that
   key fires the entry's action without cursor navigation.
3. **Toggleable flags** — entries that represent boolean flags; pressing the
   key toggles the value in-place and updates the preview line.
4. **Argument value entry** — entries that open a minibuffer prompt for a
   value; on confirm, the value is set and control returns to the transient.
5. **Nested transients** — entries that open a new transient, replacing the
   current one. `DEL` or `BS` returns to the parent.

### 8.2 `TransientSpec` — the data model

A transient is a data structure the mode defines and passes to the picker.
The picker renders it and drives the interaction.

```rust
/// Lived in `lattice-picker`, extended for transient support.
pub struct TransientSpec {
    pub title: String,
    pub groups: Vec<TransientGroup>,
    pub preview: Option<Box<PreviewFn>>,
    pub footer: Option<String>,
}

pub struct TransientGroup {
    pub label: String,          // "Actions", "Arguments", "Configure"
    pub items: Vec<TransientItem>,
}

pub struct TransientItem {
    pub key: Vec<KeyChord>,     // ["b"], ["c"], ["C-c", "C-c"]
    pub label: String,          // "checkout"
    pub description: String,    // "Check out a branch"
    pub kind: TransientItemKind,
}

pub enum TransientItemKind {
    Action(ActionId),
    Submenu(TransientSpec),
    Flag { name: String, value: bool },
    Argument { name: String, value: Option<String>, prompt: String },
}

/// Builds the live command preview from current transient state.
/// Called every time a flag toggles or an argument changes.
pub type PreviewFn = dyn Fn(&TransientState) -> String + Send;

/// Accumulated state: flag values and argument values.
/// Keyed by the `name` field from Flag and Argument items.
pub type TransientState = HashMap<String, TransientValue>;

pub enum TransientValue {
    Bool(bool),
    String(String),
}
```

### 8.3 Picker integration

The picker's API gains an `open_transient` method alongside the existing
`open`:

```rust
impl PickerRegistry {
    /// Open a transient menu. Reuses the picker's rendering pipeline but
    /// switches to grouped, non-filterable display with single-key triggers.
    pub fn open_transient(
        &self,
        spec: TransientSpec,
        display: PickerDisplay,  // BottomPopup, Floating, etc.
    ) -> TransientHandle;
}
```

When opened:
1. The picker renders `title` as the picker header.
2. Each `TransientGroup` renders as a section: the group label in bold,
   followed by its items.
3. Each item renders as: `key` (highlighted), `label` (normal), `description`
   (dimmed marginalia).
4. Flag items show their current value (e.g., `[x]` or `[ ]`).
5. Argument items show their current value or a placeholder.
6. The preview line (from `preview` fn) renders in the picker's preview pane.
7. The footer renders at the bottom.

Keyboard handling:
- Single-key press on an Action item: fires `ActionId` via the handler
  registry, closes the transient.
- Single-key press on a Flag item: toggles `TransientValue::Bool`,
  re-renders in-place, updates preview.
- Single-key press on an Argument item: opens minibuffer prompt; on confirm,
  sets the value and returns to the transient.
- Single-key press on a Submenu item: closes current transient, opens the
  nested `TransientSpec`.
- `j`/`k` / `C-n`/`C-p`: scroll groups if they overflow the viewport.
- `q` / `Esc` / `C-g`: dismiss.
- `BS` / `DEL`: back to parent transient (if nested).

**What actually got built differs from §8.2/8.3 in a few ways — see §8.8
for the real mechanism.** In short: there is no `PickerRegistry::
open_transient()` call site (the real entry point is
`Effect::OpenTransient { source: String }`, resolved by a
`TransientSourceRegistry`, since `TransientSpec` can't cross into
`lattice-grammar`'s `Effect` enum); `TransientItemKind` really is `Action
(CommandId)` / `Submenu(Arc<TransientSpec>)` / `Flag { name, default }` /
`Argument { name, default, prompt }` / `Dismiss` (used for confirmation
dialogs' `n`/`q`); and `Argument` is currently a no-op when pressed (§8.8).

### 8.4 Magit transients — concrete layouts

The layouts below (marginalia, nested Branch/Merge/Rebase menus,
diffstat, ahead/behind counts, warning glyphs, `Flag` toggles) are the
**target design**. **Partially built** — see §8.8 for what actually
renders today. In short: the group structure and most action items are
real (including `commit` and `stash` sub-transients), but there is no
marginalia column, no `Flag`/`Argument` item anywhere, no preview line,
and no Branch/Merge/Rebase sub-transient. File-dispatch's six items are
real but still act on the active buffer's own file, not a
cursor-resolved entry.

Every entry in the examples below carries **marginalia** — live git data
rendered as dimmed right-aligned text alongside the entry. Marginalia is
resolved from `lattice-vcs` or the `SectionIndex` when the `TransientSpec`
is built, not per-frame. It gives the user decision-making context without
requiring separate git commands.

#### Dispatch menu (`C-c g` globally)

```
┌─ Magit ───────────────────────────────────────────────────────┐
│                                                                │
│  Magit dispatch                                on main 3↑ 2↓   │
│                                                                │
│  ▸ Working tree                                                │
│    [s]  stage               Stage changes              3       │
│    [u]  unstage             Unstage changes            2       │
│    [x]  discard             Discard changes                    │
│    [c]  commit              Commit changes                     │
│                                                                │
│  ▸ History                                                     │
│    [l]  log                 Show commit history        HEAD    │
│    [L]  log-ref             Show log for ref                   │
│                                                                │
│  ▸ Branches, merging, rebasing                                │
│    [b]  branch              Branch operations          ▸ main  │
│    [m]  merge               Merge operations           ▸      │
│    [r]  rebase              Rebase operations          ▸      │
│                                                                │
│  ▸ Stashing                                                    │
│    [z]  stash               Stash operations           ▸ 2    │
│                                                                │
│  ▸ Remotes                                                     │
│    [F]  fetch               Fetch from remote          origin │
│    [P]  push                Push to remote             origin │
│                                                                │
│  ▸ Misc                                                        │
│    [gr] refr[h]             Refresh status buffer              │
│                                                                │
│  q dismiss                                                     │
└────────────────────────────────────────────────────────────────┘
```

Entries marked `▸` are Submenu items — pressing their key opens a nested
transient. The marginalia column shows: number of staged/unstaged files
(`3`/`2`), current branch (`main`), stash count (`2`), remote name
(`origin`), ahead/behind in the title row. All resolved from
`WorkingTree::statuses(repo)` and `Stash::list(repo)` when the transient is
built.

#### Branch menu (from dispatch `b`)

```
┌─ Branch ───────────────────────────────────────────────────────┐
│                                                                │
│  Branch                                          on main 3↑ 2↓ │
│                                                                │
│  ▸ Actions                                                     │
│    [b]  checkout           checkout existing branch    ▸ main   │
│    [c]  create             create a new branch                 │
│    [d]  delete             delete a branch              ⚠      │
│    [m]  merge              merge into current branch    main   │
│    [r]  rename             rename a branch                     │
│    [w]  worktree           create a worktree                   │
│                                                                │
│  ▸ Configure                                                   │
│    [-]  [ ] force          force-create or force-delete        │
│    [-]  [x] track          set upstream tracking        main   │
│                                                                │
│  ────────────────────────────────────────────────────────────  │
│  git checkout -b --track origin/main main                      │
│                                                                │
│  q dismiss   DEL back   -f toggle   -t toggle                  │
└────────────────────────────────────────────────────────────────┘
```

`[-f]` and `[-t]` are Flag items. Pressing `-` followed by `f` toggles the
force flag; the preview line updates from `git checkout main` to
`git checkout -f main`. Marginalia shows: current branch on `[b]` and
`[m]`, warning glyph `⚠` on `[d]` (destructive), upstream tracking branch
on `[-t]`.

#### File-dispatch menu (`C-c f` globally)

```
┌─ File ────────────────────────────────────────────────────────┐
│                                                                │
│  src/auth/login.rs                         modified  +12 -3   │
│                                                                │
│  ▸ Actions                                                     │
│    [s]  stage               stage this file          modified  │
│    [u]  unstage             unstage this file        staged    │
│    [x]  discard             discard changes           ⚠ +12 -3 │
│    [d]  diff                show diff for this file   +12 -3   │
│                                                                │
│  ▸ History                                                     │
│    [l]  log                 show log for this file    23 cmts  │
│    [b]  blame               blame this file           a1b2c3d  │
│                                                                │
│  ▸ Filesystem                                                  │
│    [r]  rename              rename / move this file           │
│    [D]  delete              delete this file            ⚠     │
│    [c]  checkout            checkout from HEAD          clean   │
│                                                                │
│  ────────────────────────────────────────────────────────────  │
│  file: src/auth/login.rs (+12 -3)                              │
│                                                                │
│  q dismiss   s stage   d diff   l log                          │
└────────────────────────────────────────────────────────────────┘
```

Marginalia shows: file status (`modified`/`staged`/`clean`), diffstat
(`+12 -3`), what `[x] discard` would destroy (`⚠ +12 -3`), commit count
for the file (`23 cmts`), last commit SHA (`a1b2c3d`). The `⚠` glyph
uses the existing severity icon system to flag destructive actions.

**Marginalia sources:**

| Context | Data | Source |
|---|---|---|
| File status | `modified` / `staged` / `clean` / `untracked` | `WorkingTree::path_status(repo, path)` |
| Diffstat | `+12 -3` | `git diff --stat <path>` (cached per transient-open) |
| Commit count | `23 cmts` | `git log --oneline <path>` line count (cached per transient-open) |
| Last SHA | `a1b2c3d` | `git log -1 --format=%h <path>` |
| Staged/unstaged counts | `3` / `2` | `WorkingTree::statuses(repo)` → count by `PathStatus` |
| Stash count | `2` | `Stash::list(repo).len()` |
| Ahead/behind | `3↑ 2↓` | `WorkingTree::ahead_behind(repo)` |
| Remote name | `origin` | `Reference::resolve(repo, "HEAD@{upstream}")` → parse remote |
| Warning glyph | `⚠` | Static — severity icon system glyph |

### 8.5 Transient state management

The transient's state (flag values, argument values) is held in a
`RefCell<TransientState>` on the mode's Guard. When a transient opens:

1. Magit's action handler builds a `TransientSpec` from the current
   `TransientState` (reading flag defaults, argument defaults, and building
   the `PreviewFn` closure).
2. Calls `ctx.picker.open_transient(spec, PickerDisplay::BottomPopup)`.
3. The picker renders the transient. On each flag toggle or argument change,
   the picker calls `PreviewFn` to update the preview line.
4. When the user presses an Action key, the picker calls
   `ctx.action_handlers.fire(action_id)` and closes.
5. On dismiss (`q` / `Esc`), the picker closes without action. The
   `TransientState` is preserved in the Guard for the next transient open.

### 8.6 Discovery — `C-c g` / `C-c f`, and `g?` for mode help

From any buffer (not just magit buffers), the global bindings provide direct
access:

- `C-x g` → opens `*magit:status*` (the primary entry point)
- `C-c g` → opens the repo-level dispatch transient
- `C-c f` → opens the file-dispatch transient — **its items are real**
  (stage / diff the active buffer's own file, §8.8); it still cannot
  resolve "the entry at cursor in a magit-status buffer", only "the
  active buffer's own file"

These follow emacs convention; all are unused in default vim normal mode.
All three are registered by `magit-global-mode` (`ActivationPolicy::
Universal`), not `magit-core` — `magit-core` only activates on magit
buffers, which would make these unreachable from anywhere else.

Within magit buffers, the dispatch and file-dispatch transients are accessed
through the same global bindings — there is no buffer-local dispatch key
(single-key candidates like `?` clash with reverse-search, `h` clashes with
left motion).

**`g?` for mode help.** A future Lattice-wide convention: `g?` opens a help
buffer for the current buffer's major mode. In a magit buffer, `g?` would
show the magit keybindings and ex-commands — the discoverability surface for
learning the mode's chords. This is a general mechanism, not magit-specific;
it applies to every major mode.

### 8.7 Sequencing — picker transient mode lands before Magit transients

The picker's transient-mode extension is NOT a magit-specific slice — it's
a **picker enhancement** that Magit consumes. It lands as a prerequisite
slice before MG.8 builds the magit transient definitions.

**PICK.1 deliverables:**
- `TransientSpec`, `TransientGroup`, `TransientItem`, `TransientItemKind`,
  `PreviewFn`, `TransientState` / `TransientValue` types in `lattice-picker`
- Picker rendering: grouped entry layout with section headers, key+label+
  description rendering, flag toggle indicators, preview pane repurposing
- Keyboard: single-key trigger dispatch, flag toggle, argument → minibuffer,
  submenu navigation, `BS`/`DEL` back, `q`/`Esc` dismiss
- `PickerRegistry::open_transient(spec, display)` API
- Tests: single-group transient, multi-group with scroll, flag toggle updates
  preview, nested submenu open + back, argument value entry → return,
  dismiss with q/Esc, TUI + GPUI render parity

MG.8 then builds magit-specific transient definitions (dispatch, branch,
merge, rebase, stash, file-dispatch) on top of this picker mode.

**Navigation is a selection, not a scroll offset.** A transient's
`<C-n>` / `<C-p>` move `Picker::transient_selected` — an index over the
spec's items, wrapping at both ends — and each renderer derives its
scroll from that every frame via `TransientSpec::scroll_for`. The
alternative, storing a row offset, is not workable here: its true
maximum is `row_count - visible`, and `visible` is renderer geometry
(the TUI derives it from the terminal area, GPUI from a fixed budget),
so the host cannot bound it. The offset version grew unbounded while
each peer clamped it privately at paint time, which is the same reason
the regular picker — bounding `selected` by `candidates.len()` — never
had the problem. The row arithmetic (`row_count`, `row_of_item`,
`selectable_count`, `scroll_for`) lives on `TransientSpec` so the two
peers cannot disagree about it; they previously each held a copy and
each got the group separator wrong.

The selection is rendered (`❯` plus a bold label, BMP-block and one
cell wide so no patched font is needed and no column shifts), and
`<CR>` fires it — routed through the item's own key so submenus, flags
and argument prompts behave identically however the item was reached.
Key presses remain the primary interaction; the selection exists so a
menu taller than its popup can be walked at all.

### 8.8 Current implementation — dispatch, discovery, and display

This is the largest gap between this fragment's original design and what
shipped. The mechanism differs in ways that matter beyond naming, so this
section documents the *actual* current wiring rather than annotating every
paragraph above in place.

**`Effect::OpenTransient { source: String }` carries only a name, not a
spec.** `magit-dispatch`/`magit-file-dispatch` (`C-c g`/`C-c f`) return this
new effect instead of aliasing `:magit-status` as they originally did.
`TransientSpec` lives in `lattice-picker`, which sits downstream of
`lattice-grammar` (where `Effect` is defined) — `lattice-grammar` cannot
depend on `lattice-picker`, so the effect can only carry a name. A
`lattice_picker::TransientSourceRegistry` (mirroring `PickerRegistry`'s
named-source shape, simplified — no candidate generator or arg-schema
concept, just a name and a zero-arg `Fn() -> TransientSpec` builder)
resolves the name to a spec at the RENDERER's effect-handling site. Magit's
`install(boot)` registers `"magit-dispatch"` and `"magit-file-dispatch"`
builders via `boot.register_service::<TransientSourceRegistryHandle>(...)`.

**Root dispatch's items fire real actions, but only through a mechanism
built specifically for this.** `TransientItemKind::Action(CommandId)`
dispatch (`do_transient_trigger` in `lattice-host/src/dispatch.rs`)
resolves ONLY via `ActionHandlerRegistry::lookup` — never through the
ex-command path. But every other `action:magit-*` handler in this crate is
registered PER-BUFFER (only live while its owning buffer is open) —
unusable for a menu reachable from any buffer. So `magit_global_mode.rs`
contributes a SEPARATE set of `action:magit-global-*` handlers
(status/commit/amend/log/diff/branch/stash/rebase — each just builds the
same `Effect::OpenSyntheticBuffer` its ex-command equivalent returns —
plus real fetch/pull/push/stash-push handlers, the six file-dispatch
handlers, and the branch-create wizard's finish handler; see below).

These are contributed via **`Mode::action_handlers()`**, NOT registered
from `on_activate`. An earlier version did the latter, gated by a
`static OnceLock` (because `ActivationPolicy::Universal` re-runs
`on_activate` on every buffer activation) — that was fundamentally racy:
`on_activate`'s future runs through `ModeRegistry::spawn_cascade`'s
try-sync-then-spawn path shared with every other mode in the same
activation batch, so if any of those has real async work, this mode's
own synchronous registration defers to a background task with no
guarantee it lands before the next keystroke. Symptom: the transient
opened fine but every item just dismissed it (`ActionHandlerRegistry::
lookup` returned `None`). `Mode::action_handlers()` sidesteps it
entirely — the host walks every mode's contributed list in a plain
synchronous loop at boot, strictly after the command registry is
frozen. See `magit_global_mode.rs`'s module doc for the full history.

The `CommandId`s the dispatch items need are resolved once at
`install()` time (`boot.commands_mut().id_by_name(...)`, since every
`action:magit-*` name is registered earlier in the same `install()`
call) and captured by value into the builder closures
(`transients::DispatchActionIds` / `FileDispatchActionIds`, built by
`resolve_dispatch_ids` / `resolve_file_dispatch_ids`) — the registry's
builders are zero-arg, so this is how a boot-time-resolved id reaches a
spec built long after boot, possibly once per `C-c g` press. Those two
resolver functions are shared with the regression tests in `lib.rs`
precisely so a field added later can't silently escape coverage.

**Root dispatch: 6 groups, 12 leaf items across 2 submenu levels.**
Working tree (`s` status, `d` diff, `c` ▸ commit) / History (`l` log) /
Branches (`b` branch) / Stashing (`z` ▸ stash) / Remotes (`f` fetch,
`F` pull, `P` push) / Misc (`r` rebase). Two sub-transients exist:
`commit_transient` (`c c` commit, `c a` amend) and `stash_transient`
(`z z` push, `z l` list). The richer marginalia layout in §8.4
(diffstat, ahead/behind, warning glyphs, per-item counts) does not
exist, and neither do the branch/merge/rebase sub-transients with
`Flag` toggles — no item in either menu is a `Flag`.

**Key assignments track Emacs magit's own `magit-dispatch` /
`magit-file-dispatch` wherever lattice has the capability** (`s` `c`
`d` `l` `b` `z` `r` `f` `F` `P`; file: `s` `u` `x` `d` `l` `b`) — this
is the UX-convention rule, not heuristic #2: muscle memory is the
dominant cost for a menu this central, so matching magit beats a
locally-reasoned alternative. Magit entries lattice has no
implementation behind (bisect, merge, tag, revert, reset, cherry-pick,
submodule, remote, patch; file-level stage-all/unstage-all, edit-blob,
trace-definition) are **absent, not present-and-inert** — the whole
point of the `Flag`-fallback regression test is that a menu row which
does nothing when pressed is indistinguishable from a bug.

**Remote/stash operations (`f`/`F`/`P`, `z z`) are real, run off the
actor thread, never block on credentials.** `magit_global_mode`'s
`action:magit-global-{fetch,pull,push,stash-create}` handlers each
spawn a detached `tokio::task` running `git fetch` / `git pull
--ff-only` / `git push` / `git stash push` on `spawn_blocking`, all
with `GIT_TERMINAL_PROMPT=0` so a missing/expired credential fails the
git subprocess immediately instead of hanging the task on interactive
input that can never arrive. Pull is deliberately `--ff-only` — it
never opens a merge-commit editor or leaves a conflict mid-merge for a
background task to get stuck in. Each handler returns an optimistic
`Effect::Echo` synchronously; the real outcome (success or the git
error text) lands via `tracing::info!`/`tracing::error!` only — same
documented limitation as every other detached background mutation in
this crate (commit, rebase execute): no synchronous path exists back to
the echo area from a task that outlives the handler call.

**File-dispatch (`C-c f`) has six real items.** All resolve the file
through the shared `active_file(ctx)` helper —
`BufferStore::path_for(id)` on whatever buffer was active when the
transient was opened, then `Repository::discover(path.parent())` (a
directory, per `gix`'s requirement) to get the workdir and a
repo-relative path. `s`/`u` call `Index::stage_path`/`unstage_path` on
`spawn_blocking`; `x` returns `Effect::Confirm` targeting
`action:magit-global-file-discard-execute`, which runs `git checkout
--` — the same two-step destructive-action shape magit-status's own
`x` uses. `d`/`l`/`b` open path-scoped `*magit:diff:<path>*` /
`*magit:log:<path>*` / `*magit:blame:<path>*` buffers (each mode parses
its own optional path out of the buffer name, §4.5/§11).

This resolves ONLY "the active buffer's own file" — it still cannot
resolve "the entry at cursor in a magit-status buffer" (there's no
plumbing carrying a `SectionIndex` cursor into the file-dispatch
transient's build), so invoking `C-c f` from inside `*magit:status*`
itself acts on whatever file `*magit:status*`'s own buffer resolves to
(i.e. nothing — it's synthetic, so `path_for` returns `None` and the
item no-ops), not the entry under the cursor. Use magit-status's own
`s`/`u`/`x`/`d` chords there. A real fix needs the same kind of
context-capture the branch-create wizard uses (stash the target in the
transient or a captured buffer id), not a structural change to
`file_dispatch_transient` itself.

**Three regression tests guard the menus** (`lattice-magit/src/lib.rs`):
every leaf resolves to a real `Action` and not the inert `Flag`
fallback (recursing through both submenus); no two items share a key
within one menu level (a duplicate makes the second unreachable, which
also presents as "this entry does nothing"); and an inverse vacuity
check asserting an all-unresolved spec DOES report every leaf inert —
without it, a walker that silently visited nothing would pass the
first two tests trivially.

**`TransientItemKind::Argument` is still a no-op** — `do_transient_trigger`
explicitly defers it; pressing an Argument item's key does nothing. This
is a *different* mechanism from the one below: `Argument` is meant to be
an in-place, in-transient-menu value edit (type a value, stay in the
transient, item shows the new value). It is unrelated to and NOT solved
by the new prompt mechanism just described — that one is a full-buffer
minibuffer swap (transient closes, prompt buffer opens, transient does
not resume). No item in this crate uses `Argument` today.

**A generic single-line prompt-with-callback mechanism now exists** —
`Effect::OpenPrompt` / `PickerAcceptOutcome::OpenPrompt` /
`PromptLineMode`, see §12.9 and `docs/dev/architecture/picker.md` §4.4
and `docs/dev/architecture/rich-minibuffer.md` §6. It backs the
branch-create wizard's second step; it does not back `Argument` (above).

**Display-preference bug, now fixed.** Transients used to ALWAYS render as
a floating popup regardless of `picker.display` — a real user-reported bug
("picker display should not have anything to do with transient's own
logic, but transient should respect the picker's display preference").
Now: `picker_use_minibuffer`/`picker_is_minibuffer` — computed once,
generically, by the picker's own render dispatch (TUI's `render.rs`,
GPUI's `window.rs`) — governs transient placement exactly like it already
governed regular candidate-list pickers. Transient code only contributes
ROW CONTENT: `transient_rows_gpui` (GPUI) and `transient_group_item_lines`
(TUI) are each a SINGLE shared windowing function used by BOTH the popup
and minibuffer-strip placement variants, so the actual group/item/scroll
computation cannot drift between the two. **The separation of concerns:
the picker owns placement, the transient owns content + interaction** —
worth stating explicitly since it's easy to reintroduce the bug by having
transient code make its own placement decision again.

### 8.12 The in-flight indicator (MG.27)

While a refresh is running, the headerline appends `refreshing` after
the view's own fields.

**It is a flag, not a `Field`, and that is the whole design.** Every
refresh path ends by calling `MagitHeaderline::set` with freshly
computed fields — so a marker living in that vector would be wiped by
the very completion it is supposed to survive until, and each per-view
builder would have to re-add it, which is a rule a new view can forget.
A separate `AtomicBool` composes: builders stay ignorant of it and
cannot drop it. Guarded by
`publishing_fields_does_not_clear_the_busy_marker`.

**Raised and cleared by an RAII guard**, not a matching pair of calls.
A refresh has several ways out — an early return when the buffer handle
is gone, a `spawn_blocking` that panics, a task cancelled because the
buffer closed — and each one would otherwise leave the row stuck on
`refreshing` forever. `Drop` covers all of them.

**Only a real change bumps the version**, so a refresh that begins and
ends between two cells-worker ticks costs no repaint at all (paramount
goal #1). No wake is needed: the worker already polls
`Headerline::version` every tick.

**A word, not the `⟳` the slice title proposed.** The
icon-degradation rule wants a BMP fallback for every glyph surface, and
`⟳` (U+27F3) is not in the fallback set — while `ui.nerd-fonts`, the
toggle that would choose between them, is buffer-local to the file tree
today rather than a global option this row can read. `refreshing` costs
a few columns on a row already made of words (`clean`, `3 staged`,
`AMEND`) and renders in every terminal font. The glyph is the natural
upgrade once that toggle is global.

### 8.11 `dv` — side-by-side, by composing the diff subsystem (MG.19)

`dv` on a diff row opens that file's baseline and its working-tree copy
in two scroll-bound panes. **Nothing about the diff is reimplemented**:
scroll binding, filler rows, `]c` / `[c`, and `do` / `dp` are all
consequences of a registered `PaneGroup`, which `lattice-diff` owns.
The whole slice is two effects in order:

1. `Effect::OpenSyntheticBuffer` — the baseline
   (`*magit:file:<ref>:<path>*`, §4.x's file-revision view) into the
   **current** pane;
2. `Effect::Diffsplit { path: <working-tree file> }` — the editable
   copy into a new vsplit, registering the session between them.

**The order is a correctness requirement, not a style.** `Diffsplit`
diffs its new pane against whatever pane is *active*. Reversed, the
editable copy would land on the left and `do` / `dp` would silently
mean the opposite of what the user intends. Guarded by
`the_baseline_pane_is_opened_before_the_split`.

**This path is where "synthetic buffers are Documents" stops being
decorative.** `do_diffsplit` refuses a non-`Document` active pane, so a
magit buffer that were its own `BufferKind` could not be one side of a
diff at all.

**Which version is the baseline** comes from `MagitView::diff_source` —
the same seam `s` / `u` / `x` use — but with one deliberate asymmetry.
`diff_source` yields `None` for the unscoped `*magit:diff*`, and
hunk staging *refuses* there, because a diff against HEAD mixes staged
with unstaged and there is no single tree to apply a hunk to. Showing
two versions has no such ambiguity: the question is "which version",
not "which tree do I write to". So `None` resolves to `HEAD` rather
than declining.

**The key is vim-fugitive's**, not magit's — magit has no side-by-side
view to have a key for. `dv` also lands in the `d`-prefixed family
`diff-mode` already owns (`do` / `dp` / `d2o`), and is inert in a
read-only buffer (`v` forces characterwise on a `d` that never
completes).

### 8.10 `D` — one chord for what magit splits across `D` and `L` (MG.23k)

Magit binds `D` to a diff-arguments transient and `L` to a
log-arguments one. `D` carries over unchanged: it is an editing
*operator*, so it is inert in a read-only magit buffer, exactly like
the `d` / `u` / `x` / `s` magit already shadows. **`L` does not** — it
is the bottom-of-screen motion, the same class as `M` (§4.6b) and `B`
(§8.9), which stay off chords entirely.

Rather than invent a second key, `D` asks the view what arguments *it*
has: `MagitView::argument_flags()` and `refresh_with_args()`, the same
polymorphism the trait already provides for `gr`. The transient's
*content* is chosen by `ctx.major_mode`, making it the second
context-varying source after the root dispatch. A view with no
arguments gets a menu that says so — the chord is on `magit-core-mode`
and therefore fires in every magit buffer, so silence would read as a
broken key.

**The arguments are per-view and stored per-buffer**, replayed on every
subsequent refresh so `gr` does not silently revert to the default.
They REPLACE rather than accumulate: the menu always opens with its
toggles clear, and "what the menu shows is what runs" is the only
reading that stays true after a refresh.

**One action, two tables, and a positional hazard.** Both views' run
rows fire `action:magit-view-refresh-args`, whose `args_schema` is the
union of `DIFF_ARGS` and `LOG_ARGS`. The action receives a
**positional** list, so a slot that shifts means a toggle lands in a
neighbour's slot and the wrong git flag runs — silently, producing a
diff that merely looks surprising. `VIEW_ARG_TABLES` is therefore the
single list both the schema builder and `view_argv`'s slot lookup read,
and two guards pin it: the schema equals the tables in order, and no
two flags share a name.

**`RemoteArgKind::ValueJoined` exists because git's CLI is not
uniform.** `git log --author x` accepts a separated value; `git diff -U
3` and `git diff --unified 3` are both errors — a long option's value
needs `=` and `-U`'s needs gluing. So the joiner rides on the argument
(`"--unified="`) and the value is appended. This was verified against
real git rather than assumed: the separated form was tried first and
rejected.

**Arguments that change *which* diff it is are deliberately absent.**
`--cached` and revision ranges would make `*magit:diff:staged:x*` show
unstaged content, leaving both the buffer name and the headerline
lying. The scope is in the name; the menu only changes how the same
diff reads.

### 8.9 Bisect — repo state, not a view (MG.21e/f/g)

Bisect has no buffer. It contributes two things: an **alert on
magit-status's headerline** while one is running, and a **`B`
sub-transient** whose rows depend on whether one is.

**Why not a `magit-bisect-mode` buffer.** magit-status is already a
buffer, so a second one wins no paramount-goal ground — and bisect
state *is* repo state, in the same class as `branch` / `ahead` /
`behind`, which is why `SectionIndex` carries it. The headerline is
where lattice already answers "what state is this repo in": it carries
`AMEND` and `REBASE IN PROGRESS` for exactly this reason. Between marks
you are compiling and testing, not reading a bisect buffer; what you
need is an indicator that survives you being somewhere else.

**Why not a `SectionKind`.** Every `SectionEntry` variant carries
diff-bearing-file invariants — a path, a stage operation, an expandable
patch. A bisect status has none of them, and adding a variant that can
never satisfy any of them would push a dead arm through thirteen match
sites in `refresh.rs`. The deferred bisect *log* is a list of commits
with verdicts; if it earns a home it is a read-only buffer (we already
render logs), not a `SectionEntry`.

**The numbers are git's, obtained from git's own plumbing.** `git
rev-list --bisect-vars` reports `bisect_nr` and `bisect_steps` — the
two halves of the "Bisecting: N revisions left to test after this
(roughly M steps)" line git prints. The first implementation computed
`count(rev-list bad ^good) - 1` instead and was wrong by a factor that
grows with the range (6 where git says 3): git reports the worst-case
half *after* the midpoint it chose, which is the bisection algorithm,
not a subtraction. A number that disagrees with what git prints in the
same terminal is worse than no number, so the honest fallback when git
gives nothing is `BISECTING` with no count — never `0 left`, which
reads as finished.

**The menu is gated, and the gate is a `stat`.** Outside a bisect,
`good` / `bad` / `skip` / `reset` error in git; `start` during one does
too. Magit gates for the same reason, and so does the no-inert-rows
policy. `Bisect::in_progress` reads `.git/BISECT_LOG` — the file git
creates on start and removes on reset — because the spec is built on
the actor thread when `C-c g` is pressed, and spawning `git` to answer
a yes/no question on a keystroke path is latency the gate does not need
(paramount goal #1).

**The gate is passed in, not probed, at the seam that matters.**
`dispatch_transient_with(ids, ctx, bisect_in_progress)` is pure;
`dispatch_transient` is the thin impure wrapper. Without the split,
every guard over the root menu's shape would silently depend on whether
the *developer's* checkout happened to be mid-bisect while the suite
ran.

**Every mark refreshes every magit view, not one.** A bisect mark moves
HEAD, so an open log and an open diff are exactly as stale as the
status buffer. `MagitViews::all()` — the peer of `BufferStates::all()`
added for the remote prompts — is how a handler with no buffer of its
own reaches them.

**`B` is a transient key only**, never a chord: it is vim's back-WORD
motion. Same rule as `M` (§4.6b) and `V`. Guarded by
`no_magit_mode_binds_m_or_b_as_a_chord`.

**Deferred and named:** `git bisect run <script>` (magit's `s`), the
bisect log as a buffer, and marking a revision other than the one git
checked out. None is needed for the core loop, and the no-inert-rows
policy applies.

## 9. Commit buffer design

The commit buffer (`*magit:commit*`/`*magit:amend*`) is a synthetic
Document in `magit-commit-mode`. Target design has two regions with the
diff region read-only and rendered as an inline overlay; **not built as
such** — the real buffer is one flat, fully-editable text buffer (the
mode's options set `NoFile`/`Number` only, no `ReadOnly`) with a fixed
marker line splitting it conceptually in two:

```
┌─────────────────────────────────────────────┐
│ --- Staged diff (review before committing) ---│  ← plain text, not
│ diff --git a/... (git diff --cached output)  │    read-only-enforced,
│ ...                                           │    not an overlay
│ --- Commit message (edit below) ---           │  ← MESSAGE_MARKER
│ Add user authentication endpoint              │  ← subject
│                                                │
│ Implements OAuth2 flow with...                │  ← body
│                                                │
└─────────────────────────────────────────────┘
```

`C-c C-c` confirm / `C-c C-k` abort are the only chords — there is no
`C-c C-d` toggle-diff (it was never implemented).

- **Diff region** (top): populated by `git diff --cached`, synchronously
  awaited during `on_activate` before the buffer is shown to the user —
  no headerline progress indicator exists. Amend (`ca`, buffer name
  `*magit:amend*`) additionally pre-populates the message region from
  `git log -1 --format=%B` (the current HEAD commit's message) instead of
  leaving it blank.
- **Message region** (below `MESSAGE_MARKER`): plain editable text; the
  confirm handler collects every non-blank line after the marker as the
  message.
- `C-c C-c`: if the message is empty, fails loud with an
  `Effect::Echo { level: Error, .. }` instead of silently no-op'ing (a real
  fix — it used to just do nothing). Otherwise runs `Commit::create`/
  `Commit::amend` on `spawn_blocking` in a detached background task and
  closes the buffer OPTIMISTICALLY (`Effect::QuitEditor`) before the git
  write completes — there is no `RepositoryEvent` to publish (§5.2); a
  failure surfaces only via `tracing::error!`, not back to the echo area,
  since there's no synchronous path from a detached task. A known,
  documented limitation, not a silently swallowed error.
- `C-c C-k`: close without committing.

## 10. Log and blame views

### 10.1 Log view

Generated from `git log --oneline --graph --decorate -N`:

```
* a1b2c3d (HEAD -> main, origin/main) Add user authentication
* e4f5g6h Fix login redirect loop
* i7j8k9l Merge branch 'feature/payments'
|\
| * m0n1o2p Implement Stripe checkout
|/
* u6v7w8x Initial commit
```

`<CR>` does NOT resolve from a stored section index — magit-log-mode
builds no `SectionIndex` at all (that structure is magit-status-only).
Instead `extract_sha` re-parses the buffer line at the cursor directly:
the first whitespace-delimited token that's ≥4 hex-digit characters,
wherever the graph-drawing characters happen to end (returns `None` for
graph-only connector lines with no commit). `<CR>` opens
`*magit:commit:<sha>*` (`magit-revision-mode`, §4.7) — a real synthetic
buffer; it used to write the same `git show` content to an uncleaned temp
file in the repo workdir and open that via a plain file buffer, which this
replaced. The buffer text itself carries no styled spans — plain
`git log --oneline --graph --decorate` CLI output, unstyled (no
`--color`, no post-processing into spans).

### 10.2 Blame view

Target design: gutter decorations in a dedicated blame column alongside
the original file, following `DiffSignKind`'s pattern. **Not built this
way.** The real `magit-blame-mode` is a separate whole-buffer view
(`*magit:blame:<path>*`, path extracted from the buffer name) that
REPLACES its own content with `git blame --line-porcelain <path>`
reformatted as one line per source line: `<sha-8> <author right-padded to
12>  <source line text>` — the blame annotation is inlined into the text
itself, not a gutter beside a separately-open file. `<CR>` on a line opens
`*magit:commit:<sha>*` for that line's blamed commit. `p` re-blames at the
PARENT of whatever revision is currently shown (`BlameState::rev`,
starting at `"HEAD"`, walked back via `git rev-parse <rev>^` — resolution
failure, e.g. at the root commit, logs via `tracing::debug!` and leaves
the buffer showing the current revision rather than erroring) — not
always "the commit at the cursor line" and not always "the parent of
HEAD"; repeated `p` presses keep walking further back. No per-revision
cache exists; each `p` press re-runs `git blame` on `spawn_blocking`.

## 11. Integration with the diff subsystem

Target design: three integration points consuming existing diff
primitives (`DiffSession`, `GitBaseline`, virtual-row deletion blocks,
`do`/`dp` hunk transfer operators). **None of the three currently
integrate with the diff subsystem at all** — no magit buffer registers a
`DiffSession`, and `diff-mode` is never active on any magit buffer.
What each view actually does:

1. **Inline diffs in magit-status** — loaded lazily via `=`/`<CR>` on
   demand (§6.3). Plain `git diff [--cached] -- <path>` / `git stash show
   -p` / `git show <sha>` output, inserted as text with line-level
   syntax-highlight spans (add/remove/hunk-header/diff-header) — not
   virtual-row deletion blocks, not sourced from a `DiffSession`.

2. **magit-diff** (§4.5) — `git diff HEAD` (staged + unstaged combined),
   one buffer, no panes, no `GitBaseline`. `]c`/`[c` here are exactly
   `magit-core`'s generic buffer-text scan for `@@`/`diff --git` lines
   (§7.5) — the same mechanism every other magit buffer uses, not a
   diff-system-registered motion. `do`/`dp` hunk-transfer operators do not
   apply — there is no diff session for them to operate against.

3. **Commit buffer diff preview** — loaded on open via `git diff --cached`
   as plain text (§9), not an inline overlay.

**`magit-diff-mode` keymap** adds `s`/`u` (stage/unstage file, not hunk —
§4.5) on top of `magit-core`'s shared chords. There is no `diff-mode`
active on this buffer to add `]c`/`[c`/`do`/`dp` "on top of" — those come
from `magit-core` alone, same as everywhere else.

## 12. Keybindings — complete grammar surface

### 12.0 Global bindings — emacs convention

Magit registers three global keybindings during `install(boot)`, following
emacs convention. `C-x g` is unused in default vim normal mode. `C-c g` and
`C-c f` use `C-c` as a multi-key chord prefix — a Lattice-specific departure
from vim (where `C-c` is a single-key interrupt terminator in normal mode).
In Lattice, `C-c` is a first-class chord prefix usable by any mode; magit
is the first heavy consumer of this convention.

| Chord | Action | Command |
|---|---|---|
| `C-x g` | Open magit-status for the current repo | `:magit-status` |
| `C-c g` | Open magit-dispatch (repo-level transient popup) | `magit-dispatch` |
| `C-c f` | Open magit-file-dispatch (file-level transient popup) | `magit-file-dispatch` |

`C-x g` opens the status buffer — the primary entry point. `C-c g` opens
the repo-level dispatch popup directly from any buffer, bypassing the status
buffer. `C-c f` opens the file-dispatch popup for the current buffer's file.

The user can remap any of these in `init.rs`:

```rust
keymap::global_bind(boot, "C-x g", "magit-status")?;       // the default
keymap::global_bind(boot, "C-c g", "magit-dispatch")?;     // the default
keymap::global_bind(boot, "C-c f", "magit-file-dispatch")?; // the default
```

### 12.1 `magit-core` (shared minor mode)

Every magit buffer is a read-only synthetic Document. In this context, many
native vim operators (`d`, `c`, `s`, `x`, `p`, `y`, `q` for macro recording)
have no meaningful effect. Bindings that would clash with fundamental
**navigation** keys (`h`, `j`, `k`, `l`, `w`, `b`, `e`) and **fold/scroll**
keys (`z`, which is vim's fold and scroll prefix and would be the worst clash
since magit also uses folds) are never overridden.

| Chord | Action | Command | Vim conflict resolved |
|---|---|---|---|
| `gr` | Refresh current magit buffer | `magit-refresh` | `gr` is unused in default vim; consistent with `gr` in `compilation-mode` |
| `q` | Close magit buffer (bury — return to previous, buffer stays open as cache) | `magit-close` | `q` starts macro recording, but macro recording is meaningless in a read-only synthetic buffer. Fugitive/magit convention. |
| `]]` | Next top-level section | `magit-next-section` | Standard vim-unimpaired |
| `[[` | Previous top-level section | `magit-prev-section` | Standard vim-unimpaired |
| `]f` | Next file/entry within current section | `magit-next-file` | `]f` in vim is "next file in directory" — file-adjacent, consistent |
| `[f` | Previous file/entry within current section | `magit-prev-file` | |
| `]c` | Next hunk | `magit-next-hunk` | Shared with diff system; shadows in magit buffers where `diff-mode` is not active (§7.5) |
| `[c` | Previous hunk | `magit-prev-hunk` | |
| `TAB` | Toggle section/hunk fold at cursor | `magit-toggle-fold` | Not a normal-mode key |
| `S-TAB` | Cycle section visibility (global) | `magit-cycle-sections` | Not a normal-mode key |

**Notes on removed bindings:**
- `g` → removed (clashes with vim prefix: `gg`, `gt`, `gf`, `gd`, etc.).
  Replaced by two-key `gr` — matching compilation-mode's `gr` for recompile.
- `h` → removed (clashes with left motion — fundamental navigation key).
  The global binding `C-c g` (§12.0) opens the dispatch transient from
  any buffer — no buffer-local dispatch key needed in magit buffers.
- `q` semantics changed: was `magit-kill-buffer`, now `magit-close`. The
  buffer is buried, not deleted — it stays in the buffer list and retains
  its content as a cache. `:bd` still kills it explicitly.

> **MG.23k** adds `D` to this set — "re-run this view with different
> git arguments", resolved through `MagitView::argument_flags` (§8.10).
> It fires in every magit buffer because the chord is one question; the
> views that have no answer say so.

### 12.2 `magit-status-mode`

The status buffer is read-only everywhere except inside the commit-message
region (which is a separate `magit-commit-mode` buffer). All single-letter
operator overrides below are acceptable because the operators (`s`, `u`, `X`,
`c`, `p`) have no write effect on a read-only buffer. Navigation keys are
preserved.

| Chord | Action | Command | Vim conflict resolved |
|---|---|---|---|
| `s` | Stage file at cursor (file-level, not hunk — §7.3) | `magit-stage` | `s` is vim's substitute operator. Fugitive convention; no-op on read-only buffer. |
| `u` | Unstage file at cursor (file-level, not hunk) | `magit-unstage` | `u` is vim's undo. Fugitive convention; no-op on read-only buffer. |
| `x` | Discard file at cursor (file-level, not hunk) | `magit-discard` | `x` is vim's delete character. No-op on read-only. `x` is easier to type than `X` (no shift); the read-only buffer makes the override safe. |
| `=` | Toggle inline diff at cursor (file entries only) | `magit-toggle-diff` | `=` is vim's format/indent operator. Fugitive convention. |
| `d` | Open the file at cursor's diff in a dedicated `magit-diff-mode` buffer, scoped to the section's baseline (§6.3) | `magit-diff-file` | `d` is vim's delete-operator prefix (`dd`, `dw`, ...) but has no meaning standalone; no-op on read-only, so safe to override. |
| `cc` | Commit — open `*magit:commit*` | `magit-commit` | `cc` = change-line (suppressed in read-only). Fugitive convention. |
| `ca` | Amend previous commit | `magit-commit-amend` | `ca` = change-around (suppressed in read-only). |
| `p` | Attempt `git add -p` — shows an error explaining it's unsupported (genuinely interactive over stdin; running it would hang the actor waiting on a child also waiting on stdin) | `magit-stage-patch` | `p` is vim's paste. No-op on read-only. Fugitive convention. |
| `<CR>` | Context-aware open/visit at cursor | `magit-visit` | See §12.3. |

**Operations reachable through the `C-c g` dispatch transient or
ex-commands** (removed from direct keybindings — the first key of each
two-key chord clashes with a fundamental vim navigation or operator
prefix). The transient column below is aspirational for most rows — only
a flat, one-item-per-group root dispatch exists today (§8.8); there are no
Branch/Merge/Stash/Rebase SUBMENUS yet, only the direct ex-commands:

| Operation | Removed binding | Clash | Access via |
|---|---|---|---|
| Open log | `ll`/`lo` | `l` = right motion | `:magit-log` |
| Open detail diff | `dd`/`dr` | `d` = delete operator prefix | `:magit-diff` |
| Branch checkout/delete/merge | `bb`/`bd`/`bm` | `b` = back-word motion | `:magit-branch` |
| Branch create | `bc` | `b` = back-word motion | `:magit-branch-create <name>` (from HEAD), or `c` inside `*magit:branch*` for the pick-base-then-name wizard (§12.9) |
| Stash operations | `zz`/`za`/`zp`/`zd` | `z` = fold/scroll prefix — worst clash since magit also uses folds | `:magit-stash-list` |
| Fetch / Push | `F` / `P` | `F{char}` = find-backwards, `P` = paste-before | No standalone ex-command — only via the root dispatch's real `F`/`P` items (`git pull --ff-only` / `git push`, §8.8) |
| Rebase | `rr`/`rc`/`ra` | `r{char}` = replace char | `:magit-rebase [upstream]` |

The `C-c g` dispatch transient is the **discoverability surface** in
target design; today it shows one item per group (status/commit/log/
branch/stash/rebase/pull/push — the last two now real, §8.8) rather
than every operation above — see §8.8 for exactly what it renders.

### 12.3 `<CR>` — context-aware open/visit

`<CR>` is a general "visit/drill-into" action, not file-dispatch. Its
behavior depends on what's under the cursor and which mode's buffer it's
pressed in — there is no single generic dispatcher; each major mode
registers its own `<CR>` handler:

| Mode | Cursor on | `<CR>` action |
|---|---|---|
| magit-status | File entry | Open the file for editing (working-tree version) |
| magit-status | Stash / Commit entry | Toggle its patch inline (same mechanism as `=` on a file — §6.3), NOT a separate buffer |
| magit-log | Commit line | Open `*magit:commit:<sha>*` (magit-revision-mode) |
| magit-blame | Blame line | Open `*magit:commit:<sha>*` for the line's commit |
| magit-branch | Branch name | Check out that branch |

"Hunk (staged/unstaged diff) → open the file at the hunk location" is
target design, **not built** — `<CR>` has no hunk-aware branch, though
the hunk-at-cursor resolution it would need now exists (§7.3).

`magit-status`'s `<CR>` is `action:magit-visit`, registered PER-BUFFER by
`magit_status_mode`'s `on_activate` (via `actions::register_action_handlers`)
— NOT a generic handler in `magit-core`, and NOT dispatched from a
`SectionIndex` entry kind. It calls the same `classify_line` cursor
classification §7.2 describes, then branches on `StatusLine::File` (open)
vs. `Stash`/`Commit` (toggle inline). Every other mode listed above
registers its own separate `<CR>` handler under its own `action:magit-*`
command name — "shadowing" is really "each major mode owns its own
binding," not one action dispatching differently per mode.

### 12.4 `magit-commit-mode`

The commit buffer is partially editable (the message region). `C-c` chords
are the emacs/magit convention for commit operations; `C-c` is not a normal-
mode prefix in vim (it's `C-c` in insert mode for escape-like behavior),
so there is no clash in the editable commit-message region.

| Chord | Action | Command |
|---|---|---|
| `C-c C-c` | Commit with message | `magit-commit-confirm` |
| `C-c C-k` | Abort commit | `magit-commit-abort` |

There is no `C-c C-d` toggle-diff-preview chord — never implemented (§9).

### 12.5 `magit-log-mode`

| Chord | Action | Command |
|---|---|---|
| `<CR>` | Show commit detail for the commit at cursor | `magit-log-show-commit` |

Log argument toggling (count, `--all`, `--graph`, path filter) is accessed
through the `C-c g` dispatch transient's Log submenu or via `:magit-log`
with arguments.

### 12.6 `magit-blame-mode`

| Chord | Action | Command |
|---|---|---|
| `<CR>` | Show commit for the blamed line | `magit-blame-show-commit` |
| `p` | Re-blame at the parent of the revision currently shown | `magit-blame-parent` |

`q`/`gr`/nav come from `magit-core` (not a blame-mode-specific `q`). Both
`<CR>` and `p` used to be dead — keymapped but never registered at all
(always fell through to the registry's dead-marker `Effect::None`); both
are real now (§10.2). Blame chunk navigation via `]c`/`[c` inherits
`magit-core`'s generic scan (§7.5) — since blame output contains no `@@`/
`diff --git` lines, `]c`/`[c` are effectively inert in a blame buffer
today, not "hunk motions" in the diff-system sense.

### 12.7 `magit-diff-mode`

There is no `diff-mode` active on this buffer to inherit chords from (§11)
— `]c`/`[c` come from `magit-core`'s generic scan, same as every other
magit buffer. `magit-diff-mode` itself adds only:

| Chord | Action | Command |
|---|---|---|
| `s` | Stage file at cursor (file-level, not hunk — §4.5) | `magit-stage` |
| `u` | Unstage file at cursor (file-level, not hunk) | `magit-unstage` |

No `x`/discard chord exists in `magit-diff-mode`.

### 12.8 `magit-stash-mode`

| Chord | Action | Command |
|---|---|---|
| `a` | Apply stash at cursor (keep in list) | `magit-stash-apply` |
| `p` | Pop stash at cursor (apply + drop) | `magit-stash-pop` |
| `d` | Drop stash at cursor (asks first — §12.13) | `magit-stash-drop` |
| `z` | Create new stash (`git stash`) | `magit-stash-create` |

There is no `<CR>` chord in `magit-stash-mode` — stash-detail-on-`<CR>` was
never implemented (nor is there an `action:magit-stash-show` command).

### 12.9 `magit-branch-mode`

| Chord | Action | Command |
|---|---|---|
| `<CR>` | Check out branch at cursor | `magit-branch-checkout` |
| `c` | Open the branch-create wizard (pick base, then type name) | `magit-branch-create` |
| `d` | Delete branch at cursor (force `-D`, asks first — §12.13) | `magit-branch-delete` |
| `m` | Merge branch at cursor into current | `magit-branch-merge` |

**`c` now opens a real two-step wizard, modeled on Emacs magit's own
branch-create flow, instead of pointing at an ex-command.** This closes
the gap this section used to document ("no generic single-line
interactive text-prompt-with-callback mechanism anywhere") — that
mechanism now exists (`Effect::OpenPrompt` / `PromptLineMode`, a new
host-level rich-minibuffer primitive documented in
`docs/dev/architecture/rich-minibuffer.md` §6 and
`docs/dev/architecture/picker.md` §4.4/§4bis.7) and the branch-create
wizard is its first real consumer:

1. `c`'s handler in `magit_branch_mode.rs` returns
   `Effect::OpenPicker { source: "magit-branch-pick-base" }` — no
   prompt yet, just a normal picker.
2. `BranchPickBaseSource` (`crates/lattice-magit/src/picker_sources.rs`,
   registered as `:picker magit-branch-pick-base`) lists `Branch::list`
   as candidates, routed through `RoutingPayload::BranchBase { name }`
   (picker.md §6.1 — branch names get MRU identity like theme names).
3. Accepting a candidate returns `PickerAcceptOutcome::OpenPrompt` — the
   picker-accept peer of `Effect::OpenPrompt`, same fields — asking
   "New branch name (from `<base>`):" and stashing `base` in the
   follow-up prompt buffer's synthetic name
   (`*magit:branch-create-from:<base>*`), exactly like magit's blame/
   rebase/revision modes already encode their target in a buffer name.
4. `<CR>` on the prompt fires `action:magit-branch-create-finish` — a
   GLOBAL handler in `magit_global_mode.rs` (not `magit_branch_mode`,
   since the prompt can outlive the branch buffer) — which reads the
   typed name via `ActionContext::prompt_value`, recovers `base` by
   parsing the prompt buffer's own name back out, and runs
   `Branch::create(repo, name, true, Some(&base))` on `spawn_blocking`
   in a detached task (result surfaces via `tracing`, not synchronously
   — same limitation as every other detached mutation in this crate).

The direct ex-command, `:magit-branch-create <name>` (creates from HEAD,
no base choice — `Branch::create(repo, name, true, None)`), is still
registered unchanged for the scriptable/quick path; the wizard is the
interactive path when a non-HEAD base is wanted.

### 12.9b `magit-remote-mode`

| Chord | Action | Command |
|---|---|---|
| `a` | Add a remote (asks name, then URL) | `magit-remote-add` |
| `r` | Rename the remote at cursor | `magit-remote-rename` |
| `d` | Remove the remote at cursor (does NOT ask — §4.6b) | `magit-remote-remove` |
| `u` | Set the fetch URL of the remote at cursor | `magit-remote-set-url` |
| `p` | Prune the remote at cursor | `magit-remote-prune` |

Magit's own keys are `a` / `r` / `k` / `p` plus `u` as a url variable
row. Four carry over; **`k` does not** — it is the up-motion, and
evil-collection-magit moves every magit `k` off it. Removal lands on
`d`, which is what magit-stash's drop (§12.8) and magit-branch's delete
(§12.9) already mean, so one rule covers all three list buffers.

`<CR>` is unbound: there is nothing to open a remote *into*, and an
inert chord is worse than an absent one.

`M` is a **transient key only** (§4.6b) — never a chord, so vim's
middle-of-screen motion survives in every magit buffer. Guarded by
`no_magit_mode_binds_m_as_a_chord`, the peer of the `V` guard.

Each of `a` / `r` / `u` returns `Effect::OpenPrompt` and finishes in a
`-finish` action. Because a prompt submit fires with the *prompt*
buffer's id, those handlers carry their target in the prompt buffer's
name and refresh through `BufferStates::all()` rather than by buffer id
— see §4.6b.

### 12.9c `magit-submodule-mode`

| Chord | Action | Command |
|---|---|---|
| `a` | Add a submodule (asks URL, then path) | `magit-submodule-add` |
| `u` | Update the submodule at cursor (`--init --recursive`) | `magit-submodule-update` |
| `s` | Sync the submodule at cursor's URL | `magit-submodule-sync` |
| `d` | Remove the submodule at cursor (asks first — §12.13) | `magit-submodule-remove` |

No `<CR>`: see §4.6d for the process-wide-workdir blocker. `o` on the
root dispatch opens this buffer and, like `M`, is a transient key only.

### 12.10 `magit-rebase-mode`

| Chord | Action | Command |
|---|---|---|
| `C-c C-c` | Execute rebase | `magit-rebase-confirm` |
| `C-c C-k` | Abort rebase (only if a rebase is actually in progress — and then it asks first, §12.13) | `magit-rebase-abort` |

The buffer is a REAL editable todo list (`pick`/`reword`/`squash`/`fixup`/
`drop`), built from `git log --reverse --format="pick %h %s"
<upstream>..HEAD` — not a hardcoded placeholder. `C-c C-c` collects the
buffer's non-comment lines and actually starts the rebase (§4.6); `C-c
C-k` checks `.git/rebase-merge`/`.git/rebase-apply` before running
`--abort`, so it cannot fail against a rebase that was never started.
That same check decides whether it asks: nothing in progress means
`C-c C-k` is only closing a buffer nobody ran, so it closes outright
(§12.13).

### 12.11 Ex-commands (dashed + namespaced)

| Command | Action |
|---|---|
| `:magit-status` | Open magit-status for the current repo |
| `:magit-log` | Open magit-log buffer (no ref/count argument yet — always `-50`) |
| `:magit-blame [path]` | Open blame for current file or path |
| `:magit-commit` | Open commit buffer |
| `:magit-diff` | Open `*magit:diff*` (`git diff HEAD`) — no ref argument yet |
| `:magit-stash-list` | Open stash list buffer |
| `:magit-branch` | Open branch list buffer |
| `:magit-branch-create <name>` | Create a branch from HEAD and check it out (no base choice — the interactive `c` wizard in `*magit:branch*` lets you pick a base, §12.9) |
| `:magit-remote` | Open the remote list buffer (§4.6b, §12.9b) — the same buffer `M` on the dispatch opens |
| `:magit-submodule` | Open the submodule list buffer (§4.6d, §12.9c) — the same buffer `o` opens |
| `:magit-rebase [upstream]` | Start interactive rebase; no arg resolves `@{upstream}` |
| `:magit-dispatch` | Open the repo-level dispatch transient (`Effect::OpenTransient`) |
| `:magit-file-dispatch` | Open the file-dispatch transient — its items are real (stage/diff the active buffer's file, §8.8) |

`:magit-fetch`, `:magit-push`, and `:magit-merge` are still **not
registered as standalone ex-commands** — pull/push are real operations
now (`git pull --ff-only` / `git push`, §8.8) but reachable only through
the root dispatch transient's `F`/`P` items (`action:magit-global-pull`/
`-push`), not through a `:` command. Branch merge exists only as the
branch-list buffer's `m` chord (`git merge <branch>`), not as a
standalone ex-command either.

### 12.12 Registrations

All commands register through `CommandRegistry` on crate install:

```rust
pub fn install(boot: &mut SubsystemBoot) {
    let magit_core = MagitCoreMode::new();
    let magit_status = MagitStatusMode::new();

    boot.modes_mut().register_minor(magit_core);
    boot.modes_mut().register_major(magit_status);
    // ... register all modes

    boot.commands_mut().register(CommandSpec::ex("magit-status", /* ... */));
    boot.commands_mut().register(CommandSpec::ex("magit-log", /* ... */));
    // ... register all ex-commands

    // Keymaps pushed through Mode::keymap()
    // Action handlers registered through Mode::action_handlers()
    // Guard Drop unregisters on deactivation
}
```

### 12.12b Action-handler registration — boot, not activation

**The defect.** A mode's `on_activate` runs inside the cascade future
`ModeRegistry::spawn_cascade` spawns. Registering action handlers there
— which every per-buffer magit mode did, because the handler closed
over an `Arc<Mutex<…State>>` built during activation — leaves a window
after the buffer opens where the chord resolves through the keymap, the
mode reads as active, and **no handler exists**. The keypress does
nothing. A user hitting `d` quickly after `:magit-branch` sees it; so
did MG.8's dead transients, whose fix was the same move.

**The contract.** Handlers live in `Mode::action_handlers()`, resolved
once at boot (`mode_action_handlers::register_mode_action_handlers`,
after every subsystem's `install()`). Per-buffer state is published into
a `BufferStates<S>` service keyed by `BufferId` and read from
`ActionContext` at call time.

Two rules make this correct:

1. **Publish above the first `.await`.** `spawn_cascade` polls the
   cascade future once, synchronously, on the App thread before spawning
   it. Everything before the first pending await has therefore run by
   the time `activate_major` returns. Moving an `.await` above the
   `publish` call silently reopens the window — and silently is the
   operative word: nothing fails, the chord just does nothing for a
   while. `magit-status-mode` was written that way at first and left
   `x` / `=` / `<CR>` dead for the width of a `git status`, the longest
   window of any view, in the most-used one. When touching an
   `on_activate`, check the ordering; the mechanical version is "first
   non-comment `.await` line number must exceed the `publish` line
   number", modulo early-return branches that never publish (the
   not-a-git-repo path). State that genuinely
   cannot be known until after an await (magit-commit's `diff_end_line`,
   magit-rebase's resolved `upstream`) publishes an inert initial value
   and is filled in through the `Arc<Mutex<_>>` afterwards — the
   handlers already refuse to act on the inert value. Only the cascade's
   *root* step (the major) gets this guarantee; implied minors run
   later, so `magit-core-mode` keeps a state-free handler shape.
2. **One boot registration per action id.**
   `ActionHandlerRegistry::register` inserts — last writer wins — and
   dropping a registration unregisters *by action id*. So an action
   bound by more than one mode must be registered exactly once, and must
   not be registered per-activation by anyone. `action:magit-refresh`
   (`gr`) is the case in point: five modes bound it, which only worked
   because per-activation registration installed one at a time. It is
   now owned by `magit-core-mode` (which binds the chord) and dispatched
   per buffer through the `MagitView` trait, so each view still owns its
   own refresh body and nothing branches on buffer kind. A test fails if
   any two modes contribute the same boot `action_name`.

Rule 2 is not merely a constraint of the new shape — **per-activation
registration was never safe once two buffers can hold handlers at
once**, because the registry is keyed by `CommandId` alone, with no
buffer dimension. Three real instances, all fixed by the migration:

- **Two modes, one action.** `action:magit-stage` / `action:magit-unstage`
  were registered from `on_activate` by both `magit-status-mode` and
  `magit-diff-mode`.
- **One mode, two buffers.** `magit-core-mode` is a minor that activates
  on *every* magit buffer and registered `q` and the six navigation
  chords per activation. Two magit buffers ⇒ two registrations.
- **Two modes, one action, different bodies.** `action:magit-close` was
  registered by `magit-status-mode` (`Effect::BufferDelete`) *and*
  `magit-core-mode` (`Effect::DismissPopup`), so `q` in the status
  buffer did one or the other depending on cascade ordering. Note this
  was not an open design question: `DismissPopup` is the documented and
  **already tested** intent —
  `q_on_magit_status_buries_it_and_never_quits_the_editor` asserts that
  `q` restores the buffer active before magit-status opened, and core's
  handler records the live bug (`q` quitting the editor) it fixed. The
  status-side registration contradicted that test and silently voided
  the guarantee whenever it won the race. Removing it makes the tested
  behaviour deterministic rather than changing it.

In every case the second registration won and the first deactivation
unregistered the action for **both**. It only looked safe because one
magit buffer tended to be open at a time. The old `ActionRegsGuard` doc
comment already named the hazard; holding the tokens bounded the damage
but could not prevent it. Boot registration removes it at the source —
**no `handlers.register(...)` call remains anywhere in `lattice-magit`**,
which is the cheapest regression check available.

Contract and helpers: `crates/lattice-magit/src/buffer_state.rs`.
Sequencing and per-mode migration status: the MG.13 entry in the slice
plan.

### 12.13 Destructive actions — the ask/execute contract

magit mutates a repository from a lot of single keystrokes. Most of
those are reversible and asking about them would be noise; a few throw
away work that git itself cannot hand back. Those go through one shape,
and only that shape:

- The chord's own action (`action:magit-<x>`) performs **no git call at
  all**. It reads the target off the buffer and returns
  `Effect::Confirm { prompt, yes_action, args }`.
- The git call lives in a separate `action:magit-<x>-execute`, named as
  that confirm's `yes_action`.
- The confirm **carries its target** in `args`, and the execute half
  acts on what it carried.

Answering `n` therefore cannot mutate anything — not by a guard that
could be forgotten, but because the code that mutates was never
reached.

**The confirmed target and the executed target are the same thing**
(IX.1/IX.2). An earlier revision of this section said the execute half
"re-reads its target at the cursor rather than carrying it through the
prompt", on the grounds that the confirm transient owns every keystroke
while it is open. It owns keystroke*s*, not the buffer — and that was
enough to be wrong:

1. `s` on a hunk; the mutation and its refresh run async.
2. Before it lands, `x` on a file row: the ask half returns `Confirm`.
3. The refresh lands *while the dialog is up*, rebuilding the buffer and
   moving the cursor (`Effect::CursorMoveIn` applies — a transient does
   not change the active document).
4. `y`. Re-reading the cursor row names a different file.

So the target is carried. **Carry the payload, not a pointer to it:** a
path, a SHA, a stash index, a synthesized patch — never a cursor row or
a row span, which a rebuild invalidates. For patch-shaped payloads this
also means `git apply`'s exact-context check refuses a stale one loudly
instead of applying it at a plausible offset.

Mechanically, the dialog is itself a transient and a transient item
resolves its arguments from transient *state*, so the host seeds that
state from the carried `args` at open time and projects it back at fire
time (`seed_transient_state` / `project_transient_state`, exact
inverses). An execute half must therefore **declare the slots its ask
half fills** — the projection is by name, so an undeclared slot means
the value lands nowhere and the handler silently falls back to
re-deriving. `every_destructive_execute_declares_the_slots_its_confirm_carries`
and `every_destructive_pair_carries_a_target_except_the_one_with_none`
pin both halves; the latter is what caught exactly that omission on
`magit-global-file-discard-execute`.

`magit-rebase-abort-execute` carries nothing, deliberately: there is one
in-progress rebase, so it has no target to name.

The prompt always names its target (`Delete branch feature/foo?`,
`Drop stash@{2}?`) — the transient covers the buffer the target was
read from, so a question that says only "Delete branch?" is
unanswerable without dismissing it.

The set (`crates/lattice-magit/src/confirm.rs`,
`DESTRUCTIVE_ACTIONS`):

| Chord | Ask | Execute | Why it is irreversible |
|---|---|---|---|
| `x` in magit-status | `magit-discard` | `magit-discard-execute` | `git checkout --` overwrites the worktree copy |
| `x` in `C-c f` | `magit-global-file-discard` | `…-execute` | same, on the active buffer's file |
| `d` in magit-branch | `magit-branch-delete` | `…-execute` | `Branch::delete` is a force delete (`-D`) — drops unmerged commits |
| `d` in magit-stash | `magit-stash-drop` | `…-execute` | a dropped stash is gone (`apply`/`pop` leave their content visible; only `drop` doesn't) |
| `C-c C-k` in magit-rebase | `magit-rebase-abort` | `…-execute` | `--abort` discards everything the rebase replayed — but **only asks when a rebase is in progress**; otherwise `C-c C-k` is just closing a todo buffer nobody ran, and closes outright |

That last gate is the one place the shape is conditional, and it is
conditional on the same `rebase-merge`/`rebase-apply` check that decides
whether `--abort` runs at all. It is a `stat` against a gitdir resolved
once at activation — cheap enough for the actor thread in response to an
explicit chord, and it *has* to run there because the confirm is the
effect the handler returns.

`DESTRUCTIVE_ACTIONS` is the single list, asserted two ways: a test
proves both halves of every row resolve in the command registry (an
unregistered execute half turns the whole action into
`confirm: unknown action`, a failure that would otherwise only surface
when a user presses the key), and `confirm::ask` debug-asserts its
`yes_action` appears in the table, so a new destructive action that
skips the list fails for its author rather than quietly for the user.

**What deliberately does *not* ask:** stage / unstage (index-only),
checkout, merge, branch-create, stash apply / pop / create, commit and
amend, rebase execute. Each is either reversible or is itself the
explicit confirm step of a dedicated buffer. This matches Emacs magit's
own `magit-no-confirm` default set; the standing rule is UX convention
over local rationale for surfaces carrying muscle memory.

## 13. Performance posture

The design follows a "lazy by default" strategy (§6). Every operation is
deferred until explicitly invoked by the user. The status buffer opens as a
fast file list — no pre-computed diffs, no full-repo git operations beyond
`git status --porcelain`.

- **Status buffer open (initial):** `git status --porcelain`-equivalent (via
  gitoxide) + `git stash list` + `git log --oneline -20`. For a repo with
  500 tracked files, completes in **10-50ms** on `spawn_blocking`. The
  buffer is a file list with status labels — no diff content. There is no
  headerline progress indicator (§4.1) — the buffer simply appears once the
  initial refresh completes.

- **Status buffer auto-refresh (after commit/stage/unstage):** same fast
  path. `StatusBufferState::expanded` is cleared UNCONDITIONALLY on every
  refresh (§6.2/§6.4) — there is no selective per-file invalidation by
  status change, since there is no per-file diff cache to invalidate. Any
  previously-expanded entry collapses; the user re-expands with `=`/`<CR>`
  if still wanted. Buffer update is a full replace via `apply_edit_batch`.

- **Diff/patch loading (`=` on a file, `<CR>` on a stash/commit):** a single
  `git diff`/`git stash show -p`/`git show` invocation on `spawn_blocking`.
  Content inserted as a local edit with syntax-highlight spans — NOT
  virtual-row deletion blocks (§6.3); no diff-system integration here at
  all. Section-level "`=` expands every file in the section" is target
  design, not built — only single-entry toggle exists today.

- **Per-keystroke (in magit buffer):** chord dispatch → mode filter → action
  handler. Identical overhead to any other buffer (<500ns p99). No WASM
  boundary.

- **Fold recompute (`MagitStatusFoldSource`):** recomputes live from
  `expanded` + a buffer scan on every `compute_folds()` call (§7.1) rather
  than caching stale ranges — O(buffer lines), not O(expanded-entries²);
  bounded by how much of the buffer is currently expanded, not repo size.

- **Blame:** each `<CR>`-triggered re-blame (or the initial load) is a full
  `git blame --line-porcelain` invocation on `spawn_blocking`, replacing the
  whole buffer. No gutter column, no per-file/per-revision cache (§10.2) —
  every `p` press re-runs blame from scratch against the new parent.

- **Comparison with Emacs magit:** Emacs magit runs `git diff --cached` and
  `git diff` on every status refresh — O(all-changed-lines). In large repos
  with 50+ changed files and large diffs (refactors touching hundreds of
  lines), this can take 2-10 seconds. Lattice's lazy approach never runs a
  diff command during status refresh at all — diffs load only on explicit
  `=`/`<CR>`, one file/stash/commit at a time.

- **No UI-thread work.** Zero I/O, parsing, git operations, or formatting on
  the render thread. Renderer sees ordinary Documents. `match buffer_kind` is
  untouched — magit buffers are ordinary Documents with major modes, folds, and
  decorations. The kind-agnostic-buffer invariant holds.

- **Async architecture — every mutation off the actor thread.** Every
  mutating git operation across the whole crate (stage/unstage/discard/
  checkout/delete/merge/stash-ops/rebase/commit/branch-create/pull/push/
  file-stage) runs via `tokio::task::spawn_blocking`, never synchronously
  on the actor thread. Three shapes, all avoiding a synchronous git call
  inline in an action handler:
  - **Refresh-in-place** (`spawn_mutation_and_refresh` in `actions.rs`,
    `magit_branch_mode.rs`, `magit_stash_mode.rs`): the handler returns
    `None` immediately; a spawned task runs the mutation, then re-runs the
    view's own refresh and applies it. Used by everything that only needs
    to update its own buffer's content (stage/unstage/discard, checkout/
    delete/merge branch, apply/pop/drop/create stash).
  - **Optimistic close** (commit-confirm, rebase-confirm/-abort): the
    handler returns the close effect (`Effect::QuitEditor`) SYNCHRONOUSLY,
    then runs the git write in a fire-and-forget background task, logging
    failure via `tracing::error!`. There is no synchronous path back to the
    echo area from a detached task — a real, documented limitation (§9,
    §4.6), not a silently swallowed error.
  - **Optimistic echo, no buffer to refresh** (pull/push in
    `magit_global_mode`'s `remote_op!` macro; file-stage; the
    branch-create wizard's finish handler): there is no per-buffer state
    to refresh — these fire from the global dispatch/prompt, not a magit
    buffer's own action. The handler returns `Effect::Echo` ("pulling…" /
    "staged `<path>`" / "creating branch…") synchronously, then a
    detached task runs the git write and logs success/failure via
    `tracing::info!`/`tracing::error!` only. Same "no synchronous path
    back" limitation as optimistic-close, just with no buffer to close
    either.

## 14. WIT surface gaps and WASM migration path

The `vcs-and-magit.md` committed to Magit as a WASM component plugin. The
current WIT surface has gaps that make this infeasible without substantial
host-services extensions:

| Magit need | WIT status | Gap |
|---|---|---|
| Read file/buffer content | ✅ `buffer.wit` (read-only `document` resource) | None |
| Write to buffer | ❌ | Need `document-write` or `apply-edit` effect mirror |
| Create buffers | ❌ | Need `ensure-named-document` equivalent in WIT |
| Spawn git process | ❌ | `spawn-process` designed for lighthouse, not built |
| Subscribe to events | ✅ `events.wit` (observation-only, async) | None |
| Call VCS data API | ❌ | Need WIT `vcs` interface mirroring Layer 1 API |
| Call diff subsystem | ❌ | Need WIT `diff` interface for session creation |
| HTTP fetch (push/fetch) | ❌ | `http-fetch` designed for lighthouse, not built |
| Persistent plugin state | ❌ | Need `kv` host-service |
| Fold registration | ❌ | Need WIT `folds` interface |
| Decoration production | ✅ `decorations.wit` (gutter decos, async) | Partial |

**Conclusion:** Magit v1 ships as a native Rust crate through `SubsystemBoot`.
The WIT surfaces it needs (buffer-write, spawn-process, VCS binding, diff
binding) represent 4–6 host-services extensions that are collectively larger
than the magit plugin itself. Building them as a prerequisite would add 3–4
slices of host-runtime work before a single magit view renders.

**WASM migration path (post-v1):** Once lighthouse's host-services ship and the
buffer-write + persistent-storage WIT interfaces land, the mode architecture
supports a clean migration — the `Mode` trait, `ActionHandlerRegistry`, and
`ModeActivator` patterns have identical semantics for native and WASM
implementations. The native crate is not temporary: the WASM boundary adds
per-operation overhead on every git process line and buffer mutation that may
never be competitive with native `spawn_blocking`. The WASM path exists for
third-party VCS backends (jujutsu, sapling) that want to plug into the same
seam.

## 15. Deliverable path: native crate vs WASM plugin

> **UX (higher court):** identical — the user sees magit-status, magit-log,
> magit-commit, etc. WIT vs native is invisible. UX is neutral.

**Option A: Native Rust crate (`lattice-magit`) through `SubsystemBoot` seam.**

> **Paramount goals:** protects #1 (no WASM boundary for git operations);
> protects #3 (modes own full surface — acid test holds); protects #4 (git ops
> on `spawn_blocking`, event-bus-driven).
> **#2 Extensibility:** the `SubsystemBoot` seam IS the extension model. Magit
> validates it with a production-grade feature-buffer. A third-party VCS tool
> installs through the same seam.
> **Heuristic #1 (long-term fit, on merit):** a self-contained subsystem with
> its own modes, git process integration, and performance-sensitive buffer
> generation is genuinely better as a native crate — the same reasoning that
> kept diff, terminal, and compilation as native subsystems. The mode
> architecture is identical either way.
> **Heuristic #2 (paramount, not other editors):** not "VSCode has a git
> extension." The justification is: 5+ WIT gaps (§14); `SubsystemBoot` is the
> proven extension mechanism for core feature-buffers; building the WIT
> host-services first adds 3–4 prerequisite slices that don't render a single
> magit view.
> **Standing-rule check:** ✅ every chord and action-handler body lives in
> `lattice-magit`. No `Editor::do_magit_*` shims. No new `Action` enum variants.

**Option B: WASM component plugin through WIT contract.**

> **Paramount goals:** protects #2 (strongest WIT surface validation —
> feature-buffer through WIT seam is the ultimate extensibility proof);
> sacrifices #1 (every git process line crosses WASM boundary); sacrifices
> delivery velocity (4–6 WIT host-services must ship first).
> **Heuristic #1:** building the host-services now is better long-term fit for
> the ecosystem. But the cost is 3–4 prerequisite slices — those could land in
> parallel with a native Magit proving the UX.

**Recommendation: Option A for v1, with Option C (parallel-track WASM) as the
sequencing strategy.** Magit ships v1 as a native `lattice-magit` crate. The
WIT host-services needed for WASM migration are documented in §14 and land
through lighthouse + ecosystem needs. When the WIT surface is complete, Magit
can be migrated — the mode surface is identical.

## 16. Options

Registered through the typed-options system (§5.12), owned by `lattice-magit`:

| Option | Type | Default | Description |
|---|---|---|---|
| `magit.auto-refresh` | `bool` | `true` | Auto-refresh on `RepositoryEvent` |
| `magit.refresh-debounce-ms` | `u32` | `100` | Debounce window for auto-refresh |
| `magit.status.show-untracked` | `bool` | `true` | Show untracked files section |
| `magit.status.show-stashes` | `bool` | `true` | Show stash list section |
| `magit.status.recent-commits-count` | `u32` | `20` | Recent commits to show |
| `magit.log.count` | `u32` | `50` | Default log entry count |
| `magit.log.graph` | `bool` | `true` | Show commit graph |
| `magit.log.decorate` | `bool` | `true` | Show branch/tag decorations |
| `magit.blame.author-width` | `u8` | `12` | Max author name width in blame gutter |
| `magit.blame.date-format` | `string` | `"relative"` | `relative`, `short`, or `iso` |
| `magit.commit.show-diff` | `bool` | `true` | Show staged diff in commit buffer |
| `magit.commit.confirm-before-push` | `bool` | `false` | Prompt before push after commit |
| `magit.diff.context-lines` | `u32` | `3` | Context lines in inline diffs |

## 17. Slice plan

Slices ordered by dependency. VCS.1 + VCS.2 are prerequisite infrastructure;
Magit views build incrementally on top.

| Slice | Description | Depends on | Status |
|---|---|---|---|---|
| **VCS.1** | `lattice-vcs` crate — Layer 1 data API. `Repository`, `GitBlob`, `Reference`, `WorkingTree`, `Index`, `Commit`, `Branch`, `Stash`, `PathStatus`. Read + Write API complete. `GitBaseline` as `DiffParticipantSource`. `gix` isolated. Zero `lattice-*` deps. | None | 📝 |
| **VCS.2** | Layer 2 subsystem — `RepositoryWatcher`, `RepositoryEvent` on event bus, auto-register `DiffSession(GitBaseline(HEAD))` on `DocumentOpened`, `git.auto-head-diff` option. Formerly D.7. | VCS.1 | 📝 |
| **PICK.1** | Picker transient-mode extension — `TransientSpec` / `TransientGroup` / `TransientItem` / `TransientItemKind` / `PreviewFn` / `TransientState` types in `lattice-picker`. Grouped entry rendering, single-key triggers, flag toggle + live preview update, argument → minibuffer, submenu navigation, `PickerRegistry::open_transient()`. General picker feature; NOT magit-specific. Serves which-key hints, command palette drilldown, future plugin transients. | None (picker subsystem) | 📝 |
| **MG.1** | `lattice-magit` crate scaffolding — crate layout, `MagitCoreMode` (minor), `MagitStatusMode` (major shell), `install(boot)`, mode+command registrations, `SubsystemBoot` wiring. No view content yet. | VCS.2, mode-architecture | 📝 |
| **MG.2** | magit-status buffer — section index (file paths + status labels, no diffs), `DiffCache` (lazy per-file diff cache), lightweight refresh (no diff commands), `=` toggle for on-demand diff loading, section fold registration, inline diff via D.3 virtual rows, auto-refresh on `RepositoryEvent`. (The headerline originally scoped here landed in MG.14 — §4.9 — across every view rather than status alone.) | MG.1 | 📝 |
| **MG.3** | magit-status actions — `s`/`u`/`x` for hunks and files, `cc`/`ca` commit/amend, `=` toggle inline diff, `p` stage hunk interactively, `<CR>` context-aware open/visit. | MG.2 | 📝 |
| **MG.4** | magit-commit — message editor, staged diff preview, `C-c C-c`/`C-c C-k`, amend. | MG.2 | 📝 |
| **MG.5** | magit-diff — dedicated diff view, reuse D.4 side-by-side, hunk staging from diff buffer (`s`/`u`/`x`). | MG.2 | 📝 |
| **MG.6** | magit-log — commit history with graph, ref decorations, `<CR>` show commit. | MG.2 | 📝 |
| **MG.7** | magit-blame — blame data loading, `BlameLineMap` cache, blame gutter column, `<CR>` show commit, `p` re-blame at parent. | MG.1 | 📝 |
| **MG.8** | Transient menus — `C-c g` dispatch, branch/merge/rebase/stash file-dispatch transient definitions, `TransientSpec` definitions, `TransientState` management, command preview generators, global bindings (`C-c g`, `C-c f`). Consumes PICK.1 picker transient mode. | MG.3, PICK.1 | 📝 |
| **MG.9** | magit-stash, magit-branch, magit-rebase — remaining operation buffers and actions. | MG.8 | 📝 |
| **MG.10** | Polish — persistent state cache, perf optimization, error handling (detached HEAD, bare repo, no-repo), `:help magit`, manual QA pass. | MG.1–MG.9 | 📝 |

**Dependency graph:**

```
VCS.1 → VCS.2 ─┬─→ MG.1 → MG.2 → MG.3 ─┬─→ MG.8 → MG.9
                │       │    ├── MG.4   │
PICK.1 ─────────┘       │    ├── MG.5   │
                         │    ├── MG.6   │
                         │    └── MG.7 ──┘

MG.1…MG.9 → MG.10 (can run in parallel with earlier slices)
```

**Parallelism notes:**
- VCS.1, VCS.2, and PICK.1 are independent and can be built concurrently.
- MG.1 depends on VCS.2 (needs `RepositoryEvent` and git data types).
- MG.4/MG.5/MG.6/MG.7 can run in parallel after MG.1 lands (each is an
  independent major mode definition + buffer provisioning).
- MG.8 waits for both MG.3 (status actions that feed transient dispatch) and
  PICK.1 (the picker's transient rendering mode).
- MG.10 can begin before MG.8/MG.9 complete — polish tasks like error handling
  and `:help magit` touch all views.

See [`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md)
for per-slice status, test counts, and commit references as slices land.

## 18. Testing strategy

- **Unit tests in `lattice-vcs`:** Repository discovery in temp git repos, blob
  read, ref resolution, working-tree status classification, `GitBaseline`
  snapshot round-trip, write-API (stage/unstage/commit/branch) in temp repos.
- **Unit tests in `lattice-magit`:** Section index construction from parsed git
  output, section fold range computation, hunk boundary detection from diff
  output, blame line map construction.
- **Integration tests** (headless host, git repos in `lattice-tests/fixtures/
  magit/`): `:magit-status` opens with expected sections, `s`/`u`/`x` produce
  correct git state changes, `:magit-commit` → confirm → HEAD advances,
  `:magit-log` renders expected commits, `:magit-blame` renders correct
  annotations.
- **Renderer tests (TUI + GPUI):** Section fold toggle renders correctly,
  inline diff hunks paint with deletion blocks, blame gutter column renders
  with correct width and content, headerline shows branch/status.
- **Stress tests:** Large repo (10k commits, 500 tracked files) — status opens
  within 500ms, refresh within 500ms, no corruption under rapid git state
  changes.
- **Edge cases:** Opening magit-status outside a git repo shows "Not a git
  repository"; detached HEAD shows "(HEAD detached at <sha>)"; bare repo denies
  write operations; stale hunk boundaries reject with "refresh and retry".

## 19. Risks

- **Git output parsing fragility.** Magit parses `git diff`, `git log`, `git
  blame`, `git status --porcelain`. The `--porcelain` format is explicitly
  versioned, but edge cases (binary files, submodules, renames, merge conflicts)
  can produce unexpected output. Mitigation: comprehensive fixture corpus;
  defensive parsing with graceful fallback (show raw output on parse failure).

- **Process spawning overhead.** The lazy strategy (§6) avoids the worst
  case: `=` loads diffs per-file on demand, not all files at once. Typical
  users expand 1-3 files per session. Even a full-section expand loads files
  independently on `spawn_blocking` — they stream in as each completes,
  and the UI remains responsive throughout. The per-command overhead
  (~5-50ms spawn + parse) is paid only for files the user explicitly
  expands, not for every changed file in the repo.

- **Muscle-memory fidelity.** Users with magit muscle memory expect exact
  keybinding fidelity. Mitigation: bind canonical magit keymap exactly; use
  magit's own help as the reference; user-facing docs mirror magit's help.

- **Vim keybinding conflicts.** `s` is vim's substitute operator. In magit
  buffers (normal-mode, read-only), `s` must mean "stage." The conflict is
  harmless because the buffer is read-only outside the commit-message region.
  Mitigation: test every magit chord against the default keymap; magit's
  keymap layer has higher priority per layered resolution order.

- **Layer 1 write-API correctness.** Partial hunk staging requires exact
  boundary matching. Mitigation: re-read file state before applying; reject
  stale hunks with a user-visible message.

## 20. Standing-rule verification

- **Mode ownership:** ✅ every chord, action handler, keymap, and buffer
  creation lives in `lattice-magit`. The acid test: landing this crate requires
  zero new `Editor::` methods.
- **No kind-specific logic:** ✅ magit buffers are ordinary Documents with a
  major mode. Renderer's `match buffer_kind` is untouched.
- **Substrate vs helper:** ✅ section index is mode-private helper data, not a
  `Document` trait method. Section fold provider is a `FoldSource` registered
  through the fold overlay service.
- **No UI-thread work:** ✅ git ops on `spawn_blocking`, buffer mutation via
  `apply_edit_batch_blocking`.
- **Dashed namespaced ex-commands:** ✅ `magit-status`, `magit-log`, etc.
- **Separate design from slice plan:** ✅ this fragment owns what + why;
  `slice-plans/magit.md` owns when + how.
- **Four artefacts per slice:** each slice ships design doc update, bench
  coverage, test coverage (unit + integration + renderer), and graceful error
  handling.

## 21. Cross-references

- [`vcs-and-magit.md`](../archive/vcs-and-magit.md) — superseded 2026-05-31 sketch (see
  header in that file).
- [`diff-system.md`](diff-system.md) — diff subsystem Magit consumes.
- [`diff-extraction.md`](diff-extraction.md) — `SubsystemBoot` pattern Magit's
  `install(boot)` follows.
- [`host-provider-boundary.md`](host-provider-boundary.md) — boundary keeping
  Magit inverted out of `lattice-host`.
- [`mode-architecture.md`](mode-architecture.md) — `Mode` trait, `ModeActivator`,
  `ActionHandlerRegistry`, Drop-based cleanup.
- [`compilation-mode.md`](compilation-mode.md) — synthetic-buffer + process
  spawning + tick drain pattern Magit reuses.
- [`lighthouse.md`](lighthouse.md) — bundled-plugin pattern; host-services
  Magit's WASM migration path depends on.
- [`fold-architecture.md`](fold-architecture.md) — section/hunk fold registration.
- [`virtual-rows.md`](virtual-rows.md) — deletion-block virtual rows.
- [`design.md`](design.md) §5.2.1 — unified command/grammar dispatch.
- [`design.md`](design.md) §5.9 — everything-is-a-buffer; synthetic Documents.
- [`design.md`](design.md) §5.10 — event system; `RepositoryEvent`.
- [`design.md`](design.md) §5.12 — typed options.
- [`../operations/implementation.md`](../operations/implementation.md) —
  VCS.1–VCS.2 and MG.1–MG.10 slice status tracked here.
- [`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md) —
  per-slice status, test counts, commit references.
