# lattice docs

Documentation lives in two trees with very different audiences:

```
docs/
├── user/        ← reference for *using* lattice
└── dev/         ← reference for *building* lattice
    ├── architecture/   live design specs
    ├── operations/     implementation status, benchmarks, verification
    ├── notes/          ongoing tracking docs (lsp-features, ui-tui-refactor)
    └── archive/        completed planning + audit artefacts (historical)
```

## user/

Hand-written reference for end users. What `:help [topic]` opens
in-editor — the `<topic>.md` filename is the topic name.

| Topic                      | File                                       |
|----------------------------|--------------------------------------------|
| Index / topic catalogue    | [user/README.md](user/README.md)           |
| Modal editing              | [user/modal-editing.md](user/modal-editing.md) |
| Ex-commands                | [user/ex-commands.md](user/ex-commands.md) |
| Folding                    | [user/folding.md](user/folding.md)         |
| Buffers + panes            | [user/buffers.md](user/buffers.md)         |
| File tree & Oil            | [user/filetree-oil.md](user/filetree-oil.md) |
| Languages                  | [user/languages.md](user/languages.md)     |
| Insert completion          | [user/completion.md](user/completion.md)   |
| Modes (major + minor)      | [user/modes.md](user/modes.md)             |
| LSP                        | [user/lsp.md](user/lsp.md)                 |
| `lsp-mode` umbrella        | [user/lsp-mode.md](user/lsp-mode.md)       |
| Options / configuration    | [user/options.md](user/options.md)         |

The `lattice-help` crate embeds these via `include_str!`, so the
binary is self-contained — `:help options` works on a machine with
no network, no filesystem layout assumption beyond the binary
itself.

## dev/architecture/

The design specs. **Authoritative for "what is lattice supposed to
be."** Read these to understand a subsystem's invariants before
changing it.

| Doc                                                              | What it covers                                                                                  |
|------------------------------------------------------------------|-------------------------------------------------------------------------------------------------|
| [design.md](dev/architecture/design.md)                          | The full design spec (v0.4). Three-layer architecture, vim grammar, plugin host, perf budgets. |
| [mode-architecture.md](dev/architecture/mode-architecture.md)    | Major + minor modes, option resolution layer stack, customize, introspection.                  |
| [keymap-architecture.md](dev/architecture/keymap-architecture.md)| Layered keymap registry, chord trie, dispatch.                                                  |
| [lsp-architecture.md](dev/architecture/lsp-architecture.md)      | LSP supervisor / actor / client, attach lifecycle, capability gating.                          |
| [insert-completion.md](dev/architecture/insert-completion.md)    | Insert-mode completion: sources, ranking, popup, ghost text.                                   |
| [diagrams.md](dev/architecture/diagrams.md)                      | ASCII architecture diagrams (3-layer, threading, buffers/panes, mode resolver, LSP, completion).|

## dev/operations/

State-of-the-build artefacts. **Authoritative for "what is built
right now."**

| Doc                                                                       | What it covers                                                  |
|---------------------------------------------------------------------------|-----------------------------------------------------------------|
| [implementation.md](dev/operations/implementation.md)                     | The shipping ledger. What's done vs. spec'd, per slice.         |
| [benchmarks.md](dev/operations/benchmarks.md)                             | Latest measured perf numbers + how to reproduce them.           |
| [verify.md](dev/operations/verify.md)                                     | Manual verification checklist for end-to-end smoke testing.     |
| [embedded-docs-budget.md](dev/operations/embedded-docs-budget.md)         | Size-budget rationale + escape options when embedded user docs grow past the trigger. |
| [terminal-mode-plan.md](dev/operations/terminal-mode-plan.md)             | Sliced rollout plan for the embedded terminal-mode subsystem.   |

## dev/notes/

Ongoing tracking docs.

| Doc                                                     | What it covers                                                         |
|---------------------------------------------------------|------------------------------------------------------------------------|
| [lsp-features.md](dev/notes/lsp-features.md)            | LSP feature checklist with per-feature status notes.                    |
| [ui-tui-refactor.md](dev/notes/ui-tui-refactor.md)      | The `lattice-ui-tui` decomposition into per-feature App submodules.     |

## dev/archive/

Historical planning + audit artefacts for work that has fully
landed. Each file carries a `> **Status: ✅ Completed.**` banner
pointing to the closing slice. See
[`dev/archive/README.md`](dev/archive/README.md) for the index.

## When to read which

- **You want to use a feature** — start at `user/README.md`.
- **You're changing a subsystem** — read its spec under
  `dev/architecture/`, then check `dev/operations/implementation.md`
  for the build status.
- **You're auditing what ships today** — start with
  `dev/operations/`. Implementation > Benchmarks > Verify.
- **You're catching up on prior work** — `dev/notes/` for
  migration plans; `git log` for the actual landing record.

The top-level `CLAUDE.md` (project root) carries the project's
collaboration rules and goal hierarchy. It links into this tree
where useful but isn't itself part of the docs.
