# lattice docs

Documentation lives in two trees with very different audiences:

```
docs/
├── user/        ← reference for *using* lattice
└── dev/         ← reference for *building* lattice
    ├── architecture/   live design specs
    ├── operations/     implementation status, benchmarks, verification
    ├── audit/          point-in-time design traces + load-bearing invariants
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
| File tree & Oil            | [user/filetree-oil.md](user/oil-mode.md) |
| Languages                  | [user/languages.md](user/languages.md)     |
| Insert completion          | [user/completion.md](user/completion.md)   |
| Modes (major + minor)      | [user/modes.md](user/modes.md)             |
| LSP                        | [user/lsp.md](user/lsp.md)                 |
| `lsp-mode` umbrella        | [user/lsp-mode.md](user/lsp-mode.md)       |
| Options / configuration    | [user/options.md](user/options.md)         |
| Plugins                    | [user/plugins.md](user/plugins.md)         |

The `lattice-help` crate embeds these into the binary at build time
(`crates/lattice-help/build.rs` auto-discovers every `docs/user/**/*.md`),
so it is self-contained — `:help options` works on a machine with
no network, no filesystem layout assumption beyond the binary
itself. A new topic is just a new `docs/user/<topic>.md`.

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
| [plugin-host.md](dev/architecture/plugin-host.md)                | Plugin host design fragment: the exercised-trait → WIT-mirror spine, capability/security model, per-seam rationale. |
| [plugin-observability.md](dev/architecture/plugin-observability.md) | Plugin boundary-trace observability (PO.1–5): the tracer, the gated hot-path grammar seam, the `*plugin-trace*` views, `plugin.trace-level`, `wasi:logging`. |
| [plugin-treesitter-seam.md](dev/architecture/plugin-treesitter-seam.md) | **v1 foundational** plugin↔tree-sitter query seam: point-in-time snapshot, node/cursor resources, host-side queries. Unlocks structural plugins (motions/text-objects/folds). |
| [plugin-auto-pair.md](dev/architecture/plugin-auto-pair.md)      | The first bundled 8b plugin: `auto` + `manual` pairing (the `vim-pairify` backward-stack close), its host prerequisites. |
| [lighthouse.md](dev/architecture/lighthouse.md)                  | The LSP server manager bundled plugin + the four host-services seams it forces (net/proc/task/register-server). |
| [diagrams.md](dev/architecture/diagrams.md)                      | ASCII architecture diagrams (3-layer, threading, buffers/panes, mode resolver, LSP, completion).|

## dev/guides/

How-to references for extending lattice (distinct from the design
specs above — these tell you how to *build* against a subsystem).

| Doc                                                              | What it covers                                                                     |
|------------------------------------------------------------------|------------------------------------------------------------------------------------|
| [plugin-authoring.md](dev/guides/plugin-authoring.md)            | Writing a plugin: toolchain (`wasm32-wasip2`, `wit-bindgen`), the WIT package, lifecycle + manifest, per-seam surface, fuel/capability model, building + testing a guest, the `fuzzy-finder` example. |

## dev/operations/

State-of-the-build artefacts. **Authoritative for "what is built
right now."**

| Doc                                                                       | What it covers                                                  |
|---------------------------------------------------------------------------|-----------------------------------------------------------------|
| [implementation.md](dev/operations/implementation.md)                     | The shipping ledger. What's done vs. spec'd, per slice.         |
| [benchmarks.md](dev/operations/benchmarks.md)                             | Latest measured perf numbers + how to reproduce them.           |
| [verify.md](dev/operations/verify.md)                                     | Manual verification checklist for end-to-end smoke testing.     |
| [embedded-docs-budget.md](dev/operations/embedded-docs-budget.md)         | Size-budget rationale + escape options when embedded user docs grow past the trigger. |
| [slice-plans/](dev/operations/slice-plans/)                               | Sliced rollout plans, one file per subsystem (diff, multibuffer, virtual rows, terminal, completion-pipeline, plugin-loader, plugin-observability, plugin-auto-pair, plugin-treesitter-seam, lighthouse). Sequencing-only; design lives in `dev/architecture/`. |

## dev/audit/

Point-in-time traces of *how a subsystem actually works*, written when a
bug or a "this smells redundant" hunch turns into a full investigation.
Each audit names the **load-bearing invariant** it found, the paths that
honour it, and the anomaly that motivated the write-up — kept live (not
archived) because the invariant still governs the code. Unlike
`dev/architecture/` (the stable *what/why*) an audit is dated and
trace-shaped; unlike `dev/archive/` it documents a *current* invariant,
not a landed refactor.

| Doc                                                       | What it covers                                                                                     |
|-----------------------------------------------------------|----------------------------------------------------------------------------------------------------|
| [audit/README.md](dev/audit/README.md)                    | Index + what an audit is for.                                                                       |
| [effect-dispatch.md](dev/audit/effect-dispatch.md)        | How an `Effect` reaches its host + renderer appliers; the "everything in `out.effects` was already host-applied" invariant. |

## dev/notes/

Ongoing tracking docs.

| Doc                                                     | What it covers                                                         |
|---------------------------------------------------------|------------------------------------------------------------------------|
| [lsp-features.md](dev/notes/lsp-features.md)            | LSP feature checklist with per-feature status notes.                    |
| [ui-tui-refactor.md](dev/archive/ui-tui-refactor.md)      | The `lattice-ui-tui` decomposition into per-feature App submodules.     |

## dev/archive/

Historical planning + audit artefacts for work that has fully
landed. Each file carries a `> **Status: ✅ Completed.**` banner
pointing to the closing slice. See
[`dev/archive/README.md`](dev/archive/README.md) for the index.

## When to read which

- **You want to use a feature** — start at `user/README.md`.
- **You're changing a subsystem** — read its spec under
  `dev/architecture/`, then check `dev/operations/implementation.md`
  for the build status. If an [`dev/audit/`](dev/audit/README.md) doc
  covers the area, read it too — it names the invariant a change most
  easily breaks.
- **You're auditing what ships today** — start with
  `dev/operations/`. Implementation > Benchmarks > Verify.
- **You're catching up on prior work** — `dev/notes/` for
  migration plans; `git log` for the actual landing record.

The top-level `CLAUDE.md` (project root) carries the project's
collaboration rules and goal hierarchy. It links into this tree
where useful but isn't itself part of the docs.
