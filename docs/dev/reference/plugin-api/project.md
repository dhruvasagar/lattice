<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `project`

**Direction:** guest calls into the host through it · **Capability:** filesystem · **Worlds:** `completion-source-plugin` (imports), `config-plugin` (imports), `context-plugin` (imports), `dashboard-plugin` (imports), `decorations-plugin` (imports), `events-plugin` (imports), `help-plugin` (imports), `keymap-plugin` (imports), `language-plugin` (imports), `media-plugin` (imports), `modes-plugin` (imports), `multibuffer-view-plugin` (imports), `picker-source-plugin` (imports), `plugin` (imports), `plugin-manager-plugin` (imports), `project-plugin` (imports), `scanned-excerpt-source-plugin` (imports), `sign-plugin` (imports), `theme-plugin` (imports), `transient-source-plugin` (imports)

Guest→host project resolution (PR.6, design
`docs/dev/architecture/project-resolution.md` §6).

A **project** is the tree a buffer belongs to — the answer `:terminal`,
`:compile` and `:search` root themselves at. It is found by walking up from
the buffer's own directory to the first directory holding a marker (`.git`,
`Cargo.toml`, …, configurable via `project.root-markers`); with no marker
anywhere, the editor's working directory stands in.

### An import, not a contribution seam

The host answers; the guest asks. Project resolution is CORE — terminal,
compilation, search, the file picker and magit all root from it — so it can
never depend on a plugin being alive. Were this a contribution seam, each of
those would need an "if the project plugin loaded, ask it, else fall back"
branch, and boot ordering would become load-bearing for correctness rather
than for features.

A `project.el`-style plugin therefore READS the root here and acts through
the ordinary effect seams; it does not supply the root.

### Resolution only

Deliberately just "where is the project". No file listing, no project list,
no switching — those are the plugin's job, and a host seam that grew them
would be re-implementing the plugin inside the host.

Sync, and available in every world. It may walk the filesystem on a cache
miss, but it runs on the plugin's own store and task — never the UI or actor
thread — and the host's cache is keyed by directory, so a project's buffers
share one walk.

## Functions (2)

### `root-for-buffer`

```wit
root-for-buffer: func(buffer: u64) -> option<project-info>
```

The project containing `buffer`.

`none` means **no such buffer** — an id the host does not know, which is
untrusted input from the guest rather than a real answer. A buffer that
exists always resolves: one with no path on disk (a scratch buffer, a
terminal) reports the working directory with `kind = pwd`.

**Example — Resolve the project root for a buffer, ignoring the working-directory fallback** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
/// The project a buffer belongs to, or `None` when there is not one.
///
/// `kind = pwd` means the editor's working directory standing in — the seam
/// documents that a guest wanting to say "not in a project" checks for this
/// rather than for an absent root. A list of *projects* that accumulated the cwd
/// would put `~` in front of the user forever, so this is where that is refused.
fn project_of_buffer(buffer: u64) -> Option<String> {
    let info = project::root_for_buffer(buffer)?;
    (info.kind != ProjectKind::Pwd).then_some(info.root)
}
```

### `root-for-path`

```wit
root-for-path: func(path: string) -> option<project-info>
```

The project containing `path`, which may name a file or a directory and
need not exist yet.

`none` only when the host has no resolver wired, which a real editor
always does; a relative path resolves against the editor's working
directory, never the plugin's.

**Example — Resolve the project containing a path the user typed** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
/// The project containing a path the user typed.
fn project_of_path(path: &str) -> Option<String> {
    let info = project::root_for_path(path)?;
    (info.kind != ProjectKind::Pwd).then_some(info.root)
}
```

## Types (2)

### enum `project-kind`

```wit
enum project-kind {
    marker,
    pwd,
}
```

How the root was decided.

**Cases**

- `marker` — A marker was found. The common case.
- `pwd` — No marker anywhere up the tree; this is the editor's working
  directory standing in. A guest that wants to say "not in a project"
  checks for this rather than for an absent root.

### record `project-info`

```wit
record project-info {
    root: string,
    kind: project-kind,
    marker: string,
}
```

A resolved project.

**Fields**

- `root`: `string` — Absolute path to the project root.
- `kind`: [`project-kind`](#enum-project-kind) — How `root` was decided.
- `marker`: `string` — The marker that decided it (`.git`, `Cargo.toml`, …), or the empty
  string when `kind` is `pwd`. Carried because "why is my root here"
  is the question that follows "where is it".

