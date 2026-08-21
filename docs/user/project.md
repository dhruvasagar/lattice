---
summary: "Every buffer belongs to a project, found by walking up for a marker like .git or Cargo.toml; :project-root reports it and the marker that decided it, and project.root-markers configures the list."
related: [ex:project-root, ex:cd, ex:pwd]
---

# project

Lattice works out which **project** each buffer belongs to, by looking
at where the file sits on disk.

There is no project to open, switch, or configure to get this. Open a
file and its project is whatever tree it sits in.

---

## Quick reference

| Command | Does |
|---|---|
| `:project-root` | Print the active buffer's project root and the marker that decided it |
| `:pwd` | Print the working directory — a separate thing, see below |
| `:cd {dir}` | Change the working directory |
| `:set project.root-markers?` | Show the marker list roots are found by |

---

## How a project is found

Starting at the buffer's own directory, Lattice walks **up** until it
finds a directory containing one of these:

```
.git  .hg  .jj  .lattice  Cargo.toml  go.work  go.mod
package.json  pyproject.toml  flake.nix
```

The **first** one it meets wins. If nothing matches all the way up, the
working directory stands in.

`:project-root` tells you both halves of the answer:

```
/home/you/src/lattice (.git)
/home/you/notes (no project marker; working directory)
```

The marker is shown because a root one directory higher than you
expected is nearly always a stray `Cargo.toml` or `package.json`, and
naming it turns a puzzling answer into an obvious one.

### The nearest project wins

The walk stops at the *first* marker, so a crate inside a Cargo
workspace is its own project:

```
~/src/lattice/                 ← .git, workspace Cargo.toml
└── crates/lattice-host/       ← its own Cargo.toml
    └── src/dispatch.rs        ← project root is crates/lattice-host
```

That is usually what you want when acting on the code in front of you.
It is deliberately *not* what the language server does:
rust-analyzer roots at the workspace, because it has to see every member
to work at all. The two questions are different and keep different
answers.

A submodule or a `git worktree` checkout is likewise its own project.

---

## Several projects at once

Each buffer answers for itself, so projects do not fight. Open files
from three checkouts and each reports its own root — there is no
"current project" to switch, and therefore none to switch back.

Move between buffers freely; `:project-root` always answers for the one
you are looking at.

---

## Projects and the working directory

They are different, and both are useful:

- The **project** is a property of a buffer, found on disk.
- The **working directory** (`:pwd`, changed with `:cd`) is a property of
  the editor. It resolves relative paths for `:e` and `:w`.

`:cd` only affects buffers that have **no** project — those fall back to
the working directory, so moving it moves them. A buffer inside a
project ignores `:cd` entirely; its root is where its markers say, not
where you last changed to.

---

## Changing the marker list

```
:set project.root-markers=.git,Cargo.toml,WORKSPACE.bazel
```

or in `config.toml`:

```toml
[project]
root-markers = [".git", "Cargo.toml", "WORKSPACE.bazel"]
```

The list **replaces** the built-in one rather than adding to it, so
`:set project.root-markers?` first to see what you are editing.

Order matters for what gets *reported*: with `.git` ahead of
`Cargo.toml`, a repository that is also a crate says `.git`.

An empty list is refused — it would silently root every buffer at the
working directory, which looks like the editor working correctly while
every project-aware command goes to the wrong place.

---

## See also

- [Terminal](help:terminal-mode)
- [Compilation](help:compilation-mode)
- [Project search](help:project-search-mode)
