<p align="center">
	<img src="./assets/readme-banner.svg" alt="Lattice - a modal, GPU-accelerated, plugin-first text editor in Rust" width="720" />
</p>

<p align="center">
	A modal, GPU-accelerated, plugin-first text editor written in Rust.
	Combines <strong>vim's modal editing power</strong> with
	<strong>emacs's extensibility model</strong> on a non-blocking,
	multi-threaded core.
</p>

<p align="center">
	<img src="./assets/media/screenshots/hero-dark.png" alt="Lattice editing a buffer, with the project file tree and a describe-buffer popup open" width="900" />
</p>

> **0.9 — alpha.** The editor is usable; the distribution is new. Expect
> rough edges in install and first-run rather than in editing. Please file
> what you hit: [issues](https://github.com/dhruvasagar/lattice/issues).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
```

Installs `lattice` plus its bundled core plugins into `~/.local`. The
GPU-rendered build is installed when it is published for your platform and the
terminal-only build otherwise (its binary runs in the terminal too — launch the
GPU window with `lattice --gui`). Force a build with `--gui` / `--cli`; install
elsewhere with `--prefix`.

Or grab an archive from [releases](https://github.com/dhruvasagar/lattice/releases).
Full instructions, including building from source (Rust 1.94+): [install guide](https://dhruvasagar.github.io/lattice/install/).

## What works today

| | |
|---|---|
| **Modal editing** | Vim grammar — operators, motions, text objects, registers, counts, macros, folds, marks |
| **Code intelligence** | LSP: completion, diagnostics, hover, rename, references, inlay hints, symbols, code actions |
| **Syntax** | Tree-sitter, 20 languages, incremental and O(viewport) |
| **Git** | A magit port — status, stage/unstage by hunk, commit, rebase, blame, log, branches, stashes |
| **Extensibility** | WASM Component Model plugin host; config is Rust compiled to WASM |
| **Two renderers** | Terminal (first-class, for SSH) and GPU (`--gui`) |
| **AI agents** | Claude Code over MCP; opencode's own TUI in a terminal buffer, or an ACP conversation buffer with diff review |

## See it

<table>
<tr>
<td width="50%"><a href="./assets/media/screenshots/magit-gpui.png"><img src="./assets/media/screenshots/magit-gpui.png" alt="The magit status buffer with staged and unstaged sections and the repo dispatch transient open" width="100%"></a><br><sub><b>A real magit, inside a modal editor.</b> Stage by hunk, commit, rebase, blame — the porcelain, not a git wrapper. Neither Zed nor Helix has one, and fugitive is not magit.</sub></td>
<td width="50%"><a href="./assets/media/screenshots/buffer-splits-gpui.png"><img src="./assets/media/screenshots/buffer-splits-gpui.png" alt="A four-way split showing the file tree, a source file, project search results and a terminal, with the buffer list open over them" width="100%"></a><br><sub><b>Everything is a buffer.</b> The file tree, search results and the terminal are buffers, not panels — so the same grammar moves through all of them.</sub></td>
</tr>
<tr>
<td width="50%"><a href="./assets/media/screenshots/org-and-agents-gpui.png"><img src="./assets/media/screenshots/org-and-agents-gpui.png" alt="An org agenda beside a coding-agent conversation buffer with a proposed diff under review" width="100%"></a><br><sub><b>Org-mode and agents, as buffers.</b> An agenda in one pane, an agent's proposed diff under review in the next. Org is a plugin, which is the point.</sub></td>
<td width="50%"><a href="./assets/media/screenshots/two-renderers.png"><img src="./assets/media/screenshots/two-renderers.png" alt="The same file open side by side in the terminal renderer and the GPU-rendered window" width="100%"></a><br><sub><b>Two first-class renderers.</b> The same core behind a terminal and a GPU window. Neither is the fallback.</sub></td>
</tr>
</table>

## Org-mode, and what a plugin can be

Org in lattice is a **plugin**, not a feature —
[`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin),
developed in its own repository. You install it yourself. It is not bundled
and never will be: its tree-sitter grammar is 2.2 MB of generated C, and that
is the plugin's build artefact, not the editor's.

**If you use org**, you get headline editing, TODO states and priorities,
tables, the clock, capture, refile, archive, the agenda, and org-roam.

**If you want to write a plugin**, it is the reference implementation — and
the honest answer to "how far does this plugin API actually go?" It
contributes across more than a dozen seams: a whole language with its own
tree-sitter grammar, four modes, a complete editing grammar, an agenda built
on the multibuffer, pickers, completion, transients, signs, decorations,
themes, and its own `:help` pages — **without a single line in lattice's
tree.** Nothing in lattice knows what a headline is.

Every plugin, bundled or not: [plugins](https://dhruvasagar.github.io/lattice/plugins/).

The API is WIT, and you build against a published version of it rather than a
checkout of this repo:

```toml
[build-dependencies]
lattice-wit = "0.1"          # this pin IS the ABI generation you target
[dependencies]
lattice-plugin-sdk = "0.1"   # optional: typed config shapes
```

Plugins ship as **source** and are compiled on the machine that runs them, so
an editor upgrade rebuilds them rather than breaking them. The exception, and
the one thing to read before shipping a plugin, is what that pin commits you
to: [plugin authoring guide](docs/dev/guides/plugin-authoring.md). Then the
[patterns guide](docs/dev/guides/plugin-patterns.md) for recipes, and the
[plugin-API reference](docs/dev/reference/plugin-api.md) for every signature
and type.

## Rough edges at 0.9

Unsigned binaries (macOS quarantines browser downloads); LSP servers must be
installed by hand; syntax colours are not yet fully themeable; ARM Linux and
ARM Windows GUI builds are best-effort, so the installer falls back to the
terminal build there.
The full list is [known limitations](https://dhruvasagar.github.io/lattice/docs/start/known-limitations/).

---

## Why another editor?

Three editors dominate today: Vim/Neovim (best modal editing, single-threaded
core, vimscript-only first-class config), Emacs (best extensibility, single-
threaded core, elisp-only first-class config), and VS Code (best plugin
ecosystem, web stack, latency dominated by Electron).

Lattice picks the strongest property from each and rebuilds them on a modern
foundation:

- **Strict vim grammar.** Counts, registers, operators, motions, text
  objects, ex-ranges, dot-repeat, marks, macros — semantics preserved
  exactly. The grammar is the public command API; the default keymap is a
  config file.
- **Emacs-class extensibility through WebAssembly.** Plugins are sandboxed
  WASM components: cross-language, capability-gated, fuel-limited,
  crash-isolated. A misbehaving plugin cannot freeze the editor.
- **Imperceptible input latency.** Keystroke → glyph indistinguishable from
  the terminal/compositor echoing the key — within one display frame under any
  background load, measured against the best-in-class reference and ratcheted
  by CI (it only gets faster). The UI thread never blocks. Multi-threaded by
  construction (one tokio task per document, snapshot-based render reads,
  bounded-mailbox dispatch).
- **GPU-accelerated rendering.** Sub-pixel-precise text, smooth scroll,
  layered paint paths optimized per content type (code vs. rich text vs.
  inline media). TUI is a first-class peer — not a throwaway.

The full design is in [`docs/dev/architecture/design.md`](docs/dev/architecture/design.md) (v0.6, ~3600 lines), including the architectural comparison against Zed, the closest peer, in [Appendix C](docs/dev/architecture/design.md) / [`docs/dev/architecture/comparison-zed.md`](docs/dev/architecture/comparison-zed.md).

---

## Paramount goals

In priority order when they conflict:

1. **Performance.** Imperceptible keystroke→glyph latency — match-or-beat
   the best-in-class reference, always within one display frame under load,
   ratcheted by CI (never regress; only gets faster).
2. **Extensibility.** WebAssembly Component Model plugin host from day one.
   WIT is the canonical API. Plugins ship in any language with
   component-model toolchain support (Rust, Zig, Go, AssemblyScript, …).
3. **Extensible vim modal editing.** Strict vim semantics. The grammar
   (operators, motions, text objects, registers, ranges, counts) IS the
   public command API. Adding new motions / text objects / operators is
   first-class — including future tree-sitter-driven variants.
4. **Asynchronicity.** Three-layer architecture (UI / Core / Plugins)
   communicating via typed message passing. Multi-threaded by construction.
   Each plugin instance owns its own `wasmtime::Store` and runs as a tokio
   task; many plugins execute in parallel across cores.

Deliberate deviations from vim and emacs — a unified `:` / functional
command dispatcher, everything-is-a-buffer (file tree, diagnostics,
terminal, REPL all placed via splits, no fixed sidebar), and one extension
substrate (`init.rs` compiled to WASM, no vimscript / elisp / Lua) — are
detailed in [`docs/dev/architecture/design.md`](docs/dev/architecture/design.md) §2.2, §5.9, §5.12.

---

## Documentation

Everything below is published at **<https://dhruvasagar.github.io/lattice/>**
and lives in this repository under `docs/`. The site is rebuilt from `main`,
and its build fails on a dead link.

**Using the editor** — the [user documentation](https://dhruvasagar.github.io/lattice/docs/)
([`docs/user/`](docs/user/)): getting started, the tutor, every mode and
command. The same pages are the editor's own `:help`.

**Writing a plugin**

| | |
|---|---|
| [Plugin authoring guide](docs/dev/guides/plugin-authoring.md) | Toolchain, ABI and versions, the `plugin.toml` manifest, sync vs async seams, the runtime contract. Read first. |
| [Plugin patterns](docs/dev/guides/plugin-patterns.md) | Recipes: an operator, an action, a motion, an ex-command, a mode with options, a picker, events, reading the buffer and syntax tree, persistent state. Code quoted from plugins CI builds. |
| [Plugin-API reference](docs/dev/reference/plugin-api.md) ([site](https://dhruvasagar.github.io/lattice/dev/plugin-api/)) | Generated from the WIT: every world and its entry points, every seam, function signature, type and field, with examples. As JSON: [`plugin-api.json`](docs/dev/reference/plugin-api.json). |
| [Bundled plugins](plugins/) | `comment`, `auto-pair`, `project`, `treesitter-context` — small, complete templates. |

**Contributing to the editor**

| | |
|---|---|
| [Developing lattice](docs/dev/guides/developing-lattice.md) | **Start here** — dev loop, architecture mental model, mode ownership, "add your first X" walkthroughs. |
| [Developer documentation](https://dhruvasagar.github.io/lattice/dev/) | Every design fragment, guide and audit, organised by subsystem ([`docs/dev/`](docs/dev/)). |
| [Design spec](docs/dev/architecture/design.md) | Authoritative for what should exist. |
| [Implementation ledger](docs/dev/operations/implementation.md) | Authoritative for what does exist. |
| [Crate map](docs/dev/reference/crates.md) | All workspace crates, layered by dependency, with what each owns. |
| [Rust API](https://dhruvasagar.github.io/lattice/api/) | rustdoc for every crate (locally: `cargo doc -p <crate> --no-deps --open`). |
| [Benchmarks](docs/dev/operations/benchmarks.md) | Latest measured numbers vs. the §8.2 commitments. |
| [How the API docs are generated](docs/dev/architecture/api-docs.md) | What is derived from what, and the tests that keep it current. |

When something disagrees, `design.md` and `implementation.md` are the
authoritative sources for what should exist and what currently does.

**For AI agents** — [`AGENTS.md`](AGENTS.md) (orientation for agents working
in this repo) and [`CLAUDE.md`](CLAUDE.md) (the project's working rules). On the
site, [`llms.txt`](https://dhruvasagar.github.io/lattice/llms.txt) indexes
every doc as plain Markdown, and
[`llms-full.txt`](https://dhruvasagar.github.io/lattice/llms-full.txt) is
everything needed to write a plugin in one file.

---

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). In short: the design doc is
load-bearing (open an issue with the design rationale before a non-trivial
PR), the four paramount goals above override stylistic preferences when
they conflict, and there are no backwards-compatibility shims for vim or
emacs configs — that's an explicit non-goal.

---

## License

Licensed under the [MIT License](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you shall be licensed as above,
without any additional terms or conditions.
