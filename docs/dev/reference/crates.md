<!-- @generated from crates/*/Cargo.toml and each crate root's `//!` overview
     by xtask/tests/crate_map.rs. Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p xtask`. -->

# Crate map

The workspace's 41 crates, layered by dependency: a crate appears in a layer above everything it depends on, so layer 0 is the foundation and layer 13 is the binary. Each summary is the first paragraph of the crate's own overview (`src/lib.rs`), and each name links to its published rustdoc. Only `lattice-*` runtime dependencies are shown — dev- and build-dependencies are not the architecture.

Why a crate exists at all is a design rule, not an accident of history: a new crate needs a new *mechanism* — a dependency surface it must keep out, enforced structurally — not merely a new feature. See heuristic #6 in `CLAUDE.md`.

## Layer 0

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-config-macros`](https://dhruvasagar.github.io/lattice/api/lattice_config_macros/) | Proc-macro front-end for `lattice-config`'s declarative option / group / overrides declarations (M.2.0+M.2.1, Design B + D from `docs/dev/architecture/mode-architecture.md` discussion notes). | — | `lattice-config` |
| [`lattice-plugin-api`](https://dhruvasagar.github.io/lattice/api/lattice_plugin_api/) | The plugin-API catalog: the `wit/` package parsed at build time into typed data, and the reference, JSON and agent bundle rendered from it (PI.1). | — | `lattice-host` |
| [`lattice-plugin-sdk-derive`](https://dhruvasagar.github.io/lattice/api/lattice_plugin_sdk_derive/) | `#[derive(PluginEvent)]` — the guest-side derive for plugin-defined events (PH7.8b.3). The companion proc-macro crate to `lattice-plugin-sdk` (the serde/serde_derive split: a proc-macro crate cannot also export a normal library, so the runtime trait lives in `lattice-plugin-sdk`, which re-exports this derive). | — | `lattice-plugin-sdk` |
| [`lattice-protocol`](https://dhruvasagar.github.io/lattice/api/lattice_protocol/) | The shared vocabulary of the editor: the value types, identifiers, event catalogue and wire envelopes that every other lattice crate speaks. It is the dependency floor — every crate depends on it, and it depends on no other lattice crate. | — | `lattice-agent`, `lattice-ai`, `lattice-compilation`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-dashboard`, `lattice-diff`, `lattice-format`, `lattice-grammar`, `lattice-help`, `lattice-host`, `lattice-keymap`, `lattice-listing`, `lattice-lsp`, `lattice-magit`, `lattice-mode`, `lattice-multibuffer`, `lattice-notify`, `lattice-picker`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-plugin-manager`, `lattice-plugin-trace`, `lattice-runtime`, `lattice-snippet`, `lattice-syntax`, `lattice-terminal`, `lattice-ui-tui` |
| [`lattice-theme`](https://dhruvasagar.github.io/lattice/api/lattice_theme/) | Renderer-neutral theme primitives. | — | `lattice-cells`, `lattice-compilation`, `lattice-dashboard`, `lattice-diff`, `lattice-host`, `lattice-listing`, `lattice-magit`, `lattice-multibuffer`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-syntax` |
| [`lattice-vcs`](https://dhruvasagar.github.io/lattice/api/lattice_vcs/) | Pure git data layer — read + write operations over a git repository. | — | `lattice-host`, `lattice-magit` |
| [`lattice-wit`](https://dhruvasagar.github.io/lattice/api/lattice_wit/) | The canonical plugin API — lattice's `wit/` package — as a crate, so a plugin can depend on a named ABI generation instead of a copied directory (WT.1). | — | `lattice-cli`, `lattice-plugin-loader` |

## Layer 1

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-cells`](https://dhruvasagar.github.io/lattice/api/lattice_cells/) | Cell-grid renderer substrate. | `lattice-theme` | `lattice-ai`, `lattice-compilation`, `lattice-completion`, `lattice-dashboard`, `lattice-diff`, `lattice-host`, `lattice-listing`, `lattice-magit`, `lattice-media`, `lattice-mode`, `lattice-multibuffer`, `lattice-plugin-host`, `lattice-plugin-manager`, `lattice-runtime`, `lattice-syntax`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-core`](https://dhruvasagar.github.io/lattice/api/lattice_core/) | The editor's text model: rope-backed buffers, documents with undo and dirty tracking, regex search, and the small renderer-neutral vocabularies (buffer ids and kinds, pane geometry, folds, indent units, project roots) that every other lattice crate shares. | `lattice-protocol` | `lattice-agent`, `lattice-ai`, `lattice-cli`, `lattice-compilation`, `lattice-completion`, `lattice-config`, `lattice-dashboard`, `lattice-diff`, `lattice-grammar`, `lattice-help`, `lattice-host`, `lattice-listing`, `lattice-lsp`, `lattice-magit`, `lattice-mode`, `lattice-multibuffer`, `lattice-notify`, `lattice-picker`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-plugin-manager`, `lattice-plugin-trace`, `lattice-runtime`, `lattice-snippet`, `lattice-syntax`, `lattice-terminal`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-plugin-sdk`](https://dhruvasagar.github.io/lattice/api/lattice_plugin_sdk/) | The guest-side Rust SDK for lattice plugins: typed event payloads, typed options and typed configuration shapes layered over the plugin-host WIT wire. Compiled INTO plugins (Rust today; other component-model languages use the WIT directly), never into the host. | `lattice-plugin-sdk-derive` | — |

## Layer 2

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-grammar`](https://dhruvasagar.github.io/lattice/api/lattice_grammar/) | Vim modal editing engine and the unified command/grammar dispatch. | `lattice-core`, `lattice-protocol` | `lattice-agent`, `lattice-ai`, `lattice-compilation`, `lattice-completion`, `lattice-config`, `lattice-dashboard`, `lattice-diff`, `lattice-host`, `lattice-keymap`, `lattice-listing`, `lattice-lsp`, `lattice-magit`, `lattice-mode`, `lattice-multibuffer`, `lattice-notify`, `lattice-picker`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-plugin-manager`, `lattice-plugin-trace`, `lattice-runtime`, `lattice-snippet`, `lattice-syntax`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-media`](https://dhruvasagar.github.io/lattice/api/lattice_media/) | Inline media: resolving an image block's pixels off the UI thread, so the renderer only ever paints decoded pixels or a placeholder (IM.4). | `lattice-cells` | `lattice-host`, `lattice-ui-gpui` |

## Layer 3

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-completion`](https://dhruvasagar.github.io/lattice/api/lattice_completion/) | Pluggable completion pipeline (DESIGN.md §5.11.3). | `lattice-cells`, `lattice-core`, `lattice-grammar`, `lattice-protocol` | `lattice-config`, `lattice-help`, `lattice-host`, `lattice-lsp`, `lattice-magit`, `lattice-mode`, `lattice-picker`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-snippet`, `lattice-syntax`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-keymap`](https://dhruvasagar.github.io/lattice/api/lattice_keymap/) | The editor's keymap engine: the chord trie, the layered runtime registry every keystroke resolves against, the built-in vim keymap catalog, and the introspection models (`:describe-key`, which-key) derived from them. | `lattice-grammar`, `lattice-protocol` | `lattice-diff`, `lattice-host`, `lattice-magit`, `lattice-mode`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-runtime` |

## Layer 4

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-config`](https://dhruvasagar.github.io/lattice/api/lattice_config/) | The typed options system: every editor, mode, renderer and plugin setting is a registered, typed value in one [`ConfigRegistry`], read on the hot path by type and written at the boundaries (`:set`, `lattice.toml`, `:setlocal`, plugins) by name (DESIGN.md §5.12). | `lattice-completion`, `lattice-config-macros`, `lattice-core`, `lattice-grammar`, `lattice-protocol` | `lattice-agent`, `lattice-ai`, `lattice-cli`, `lattice-compilation`, `lattice-dashboard`, `lattice-diff`, `lattice-host`, `lattice-listing`, `lattice-lsp`, `lattice-magit`, `lattice-mode`, `lattice-multibuffer`, `lattice-notify`, `lattice-picker`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-plugin-manager`, `lattice-plugin-trace`, `lattice-snippet`, `lattice-syntax`, `lattice-terminal`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-runtime`](https://dhruvasagar.github.io/lattice/api/lattice_runtime/) | Async runtime for lattice (DESIGN.md §5.2.1, §5.6.8, §5.7). | `lattice-cells`, `lattice-core`, `lattice-grammar`, `lattice-keymap`, `lattice-protocol` | `lattice-agent`, `lattice-ai`, `lattice-cli`, `lattice-compilation`, `lattice-dashboard`, `lattice-diff`, `lattice-host`, `lattice-lsp`, `lattice-magit`, `lattice-mode`, `lattice-multibuffer`, `lattice-notify`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-plugin-manager`, `lattice-plugin-trace`, `lattice-terminal`, `lattice-ui-tui` |

## Layer 5

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-mode`](https://dhruvasagar.github.io/lattice/api/lattice_mode/) | The mode system's foundation: the `Mode` trait, the mode registry, the per-buffer set of active modes, and the typed lifecycle events (M.1). | `lattice-cells`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-keymap`, `lattice-protocol`, `lattice-runtime` | `lattice-agent`, `lattice-ai`, `lattice-compilation`, `lattice-dashboard`, `lattice-diff`, `lattice-help`, `lattice-host`, `lattice-listing`, `lattice-lsp`, `lattice-magit`, `lattice-multibuffer`, `lattice-notify`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-plugin-manager`, `lattice-plugin-trace`, `lattice-snippet`, `lattice-syntax`, `lattice-terminal`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-picker`](https://dhruvasagar.github.io/lattice/api/lattice_picker/) | Vertico-style picker (DESIGN.md §5.9.7, §5.9.10). | `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-protocol` | `lattice-host`, `lattice-magit`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-snippet`, `lattice-ui-gpui`, `lattice-ui-tui` |

## Layer 6

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-compilation`](https://dhruvasagar.github.io/lattice/api/lattice_compilation/) | Native compilation mode: runs a build / test / lint command off-thread and streams its output into a read-only `*compilation*` buffer (CM.1). | `lattice-cells`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-protocol`, `lattice-runtime`, `lattice-theme` | `lattice-host`, `lattice-plugin-host`, `lattice-plugin-loader` |
| [`lattice-dashboard`](https://dhruvasagar.github.io/lattice/api/lattice_dashboard/) | Lattice launch **dashboard** — the branded start page shown when the editor opens with no file argument. | `lattice-cells`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-protocol`, `lattice-runtime`, `lattice-theme` | `lattice-host`, `lattice-plugin-host`, `lattice-plugin-loader` |
| [`lattice-listing`](https://dhruvasagar.github.io/lattice/api/lattice_listing/) | Filesystem entry listings as buffers — the two views the editor offers over a directory tree. | `lattice-cells`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-protocol`, `lattice-theme` | `lattice-host`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-notify`](https://dhruvasagar.github.io/lattice/api/lattice_notify/) | Notifications: telling the user about work that has no buffer (NOTIF.1a). | `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-protocol`, `lattice-runtime` | `lattice-host`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-snippet`](https://dhruvasagar.github.io/lattice/api/lattice_snippet/) | TextMate-format snippet engine for lattice (Phase 4.2.g.4; design in `docs/dev/architecture/insert-completion.md` §8). | `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-picker`, `lattice-protocol` | `lattice-host`, `lattice-ui-tui` |
| [`lattice-syntax`](https://dhruvasagar.github.io/lattice/api/lattice_syntax/) | Tree-sitter-backed syntax highlighting for `lattice` (DESIGN.md §5.3). | `lattice-cells`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-protocol`, `lattice-theme` | `lattice-diff`, `lattice-format`, `lattice-help`, `lattice-host`, `lattice-lsp`, `lattice-magit`, `lattice-multibuffer`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-terminal`](https://dhruvasagar.github.io/lattice/api/lattice_terminal/) | lattice-terminal: PTY-backed terminal-buffer substrate. | `lattice-config`, `lattice-core`, `lattice-mode`, `lattice-protocol`, `lattice-runtime` | `lattice-host`, `lattice-ui-gpui`, `lattice-ui-tui` |

## Layer 7

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-diff`](https://dhruvasagar.github.io/lattice/api/lattice_diff/) | Pure data layer for Lattice's diff subsystem. | `lattice-cells`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-keymap`, `lattice-mode`, `lattice-protocol`, `lattice-runtime`, `lattice-syntax`, `lattice-theme` | `lattice-agent`, `lattice-ai`, `lattice-format`, `lattice-host`, `lattice-magit` |
| [`lattice-help`](https://dhruvasagar.github.io/lattice/api/lattice_help/) | Buffer-backed help model (DESIGN.md §5.11). | `lattice-completion`, `lattice-core`, `lattice-mode`, `lattice-protocol`, `lattice-syntax` | `lattice-host`, `lattice-lsp`, `lattice-plugin-loader`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-multibuffer`](https://dhruvasagar.github.io/lattice/api/lattice_multibuffer/) | Multibuffers: one buffer composed of excerpts from other buffers and files, and every concern that comes with them. | `lattice-cells`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-protocol`, `lattice-runtime`, `lattice-syntax`, `lattice-theme` | `lattice-host`, `lattice-lsp`, `lattice-magit`, `lattice-plugin-loader` |
| [`lattice-plugin-host`](https://dhruvasagar.github.io/lattice/api/lattice_plugin_host/) | Plugin host — the WASM Component Model extension substrate (Phase 7). | `lattice-cells`, `lattice-compilation`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-dashboard`, `lattice-grammar`, `lattice-keymap`, `lattice-mode`, `lattice-picker`, `lattice-protocol`, `lattice-runtime`, `lattice-syntax`, `lattice-theme` | `lattice-plugin-loader`, `lattice-plugin-manager`, `lattice-plugin-trace` |

## Layer 8

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-agent`](https://dhruvasagar.github.io/lattice/api/lattice_agent/) | `lattice-agent` — the editor-capability port that lattice's agent integrations are built on. | `lattice-config`, `lattice-core`, `lattice-diff`, `lattice-grammar`, `lattice-mode`, `lattice-protocol`, `lattice-runtime` | `lattice-ai` |
| [`lattice-format`](https://dhruvasagar.github.io/lattice/api/lattice_format/) | External formatters, and how their output reaches a buffer. | `lattice-diff`, `lattice-protocol`, `lattice-syntax` | `lattice-host` |
| [`lattice-lsp`](https://dhruvasagar.github.io/lattice/api/lattice_lsp/) | `lattice-lsp` -- the LSP client (DESIGN.md §5.4, Phase 4). | `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-help`, `lattice-mode`, `lattice-multibuffer`, `lattice-protocol`, `lattice-runtime`, `lattice-syntax` | `lattice-ai`, `lattice-host`, `lattice-ui-gpui`, `lattice-ui-tui` |
| [`lattice-magit`](https://dhruvasagar.github.io/lattice/api/lattice_magit/) | Magit — git porcelain as a core plugin. | `lattice-cells`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-diff`, `lattice-grammar`, `lattice-keymap`, `lattice-mode`, `lattice-multibuffer`, `lattice-picker`, `lattice-protocol`, `lattice-runtime`, `lattice-syntax`, `lattice-theme`, `lattice-vcs` | `lattice-host` |
| [`lattice-plugin-loader`](https://dhruvasagar.github.io/lattice/api/lattice_plugin_loader/) | `lattice-plugin-loader` — the editor-side plugin loader (Phase 8). | `lattice-compilation`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-dashboard`, `lattice-grammar`, `lattice-help`, `lattice-keymap`, `lattice-mode`, `lattice-multibuffer`, `lattice-picker`, `lattice-plugin-host`, `lattice-protocol`, `lattice-runtime`, `lattice-syntax`, `lattice-theme`, `lattice-wit` | `lattice-cli`, `lattice-host`, `lattice-plugin-manager`, `lattice-plugin-trace` |

## Layer 9

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-ai`](https://dhruvasagar.github.io/lattice/api/lattice_ai/) | `lattice-ai` — lattice's AI coding-agent integration layer. | `lattice-agent`, `lattice-cells`, `lattice-config`, `lattice-core`, `lattice-diff`, `lattice-grammar`, `lattice-lsp`, `lattice-mode`, `lattice-protocol`, `lattice-runtime` | `lattice-host` |
| [`lattice-plugin-trace`](https://dhruvasagar.github.io/lattice/api/lattice_plugin_trace/) | The buffer-backed plugin boundary-trace views: `plugin-trace-mode` and the `:plugin-trace` ex-command (PO.4). | `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-protocol`, `lattice-runtime` | `lattice-host`, `lattice-plugin-manager` |

## Layer 10

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-plugin-manager`](https://dhruvasagar.github.io/lattice/api/lattice_plugin_manager/) | The buffer-backed `:plugins` manager view: `plugins-mode` and the `:plugins` ex-command (PL8.H.2). | `lattice-cells`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-mode`, `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-plugin-trace`, `lattice-protocol`, `lattice-runtime` | `lattice-host` |

## Layer 11

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-host`](https://dhruvasagar.github.io/lattice/api/lattice_host/) | `lattice-host` -- the renderer-agnostic editor substrate. | `lattice-ai`, `lattice-cells`, `lattice-compilation`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-dashboard`, `lattice-diff`, `lattice-format`, `lattice-grammar`, `lattice-help`, `lattice-keymap`, `lattice-listing`, `lattice-lsp`, `lattice-magit`, `lattice-media`, `lattice-mode`, `lattice-multibuffer`, `lattice-notify`, `lattice-picker`, `lattice-plugin-api`, `lattice-plugin-loader`, `lattice-plugin-manager`, `lattice-plugin-trace`, `lattice-protocol`, `lattice-runtime`, `lattice-snippet`, `lattice-syntax`, `lattice-terminal`, `lattice-theme`, `lattice-vcs` | `lattice-ui-gpui`, `lattice-ui-tui` |

## Layer 12

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-ui-gpui`](https://dhruvasagar.github.io/lattice/api/lattice_ui_gpui/) | The GPUI renderer: lattice's windowed, GPU-accelerated front end, a peer of the terminal renderer (`lattice-ui-tui`) over the same `lattice-host` editor substrate (Phase 5.7). The real window needs the `window` feature, which `lattice-cli` turns on under its `gui` feature. | `lattice-cells`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-help`, `lattice-host`, `lattice-listing`, `lattice-lsp`, `lattice-media`, `lattice-mode`, `lattice-notify`, `lattice-picker`, `lattice-syntax`, `lattice-terminal` | `lattice-cli` |
| [`lattice-ui-tui`](https://dhruvasagar.github.io/lattice/api/lattice_ui_tui/) | Terminal renderer for `lattice` (DESIGN.md §5.6.1 `TuiRenderer`). | `lattice-cells`, `lattice-completion`, `lattice-config`, `lattice-core`, `lattice-grammar`, `lattice-help`, `lattice-host`, `lattice-listing`, `lattice-lsp`, `lattice-mode`, `lattice-notify`, `lattice-picker`, `lattice-protocol`, `lattice-runtime`, `lattice-snippet`, `lattice-syntax`, `lattice-terminal` | `lattice-cli` |

## Layer 13

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| [`lattice-cli`](https://dhruvasagar.github.io/lattice/api/lattice_cli/) | `lattice` -- the editor binary. | `lattice-config`, `lattice-core`, `lattice-plugin-loader`, `lattice-runtime`, `lattice-ui-gpui`, `lattice-ui-tui`, `lattice-wit` | — |
