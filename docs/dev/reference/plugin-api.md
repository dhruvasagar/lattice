<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# Lattice Plugin API

The plugin API is the WIT package `lattice:plugin-host@0.1.0` — 31 interfaces ("seams") and 25 worlds. It is the whole contract: a plugin written in any language with Component-Model tooling (Rust, Go, Zig, JavaScript, …) sees exactly what is on these pages and nothing else. This reference is generated from the `.wit` files in `crates/lattice-wit/wit/`, so it cannot disagree with them.

New to writing plugins? Start with the [plugin authoring guide](../../dev/guides/plugin-authoring.md), then come back here for the detail. The same reference in machine-readable form — every seam, signature, type and member — is `docs/dev/reference/plugin-api.json` in the repository and `/plugin-api.json` on the documentation site.

## How to read this reference

- **A plugin targets one world.** The world decides which seams the plugin *exports* (implements — the host calls it) and which it *imports* (calls into the host). In Rust: `wit_bindgen::generate!({ world: "comment-plugin", path: "…/wit" })`.
- **Direction** on each seam says which of those it is. A seam marked *shared types only* is never called; other seams `use` its types.
- **Capability** is what the seam requires of a plugin's grant. Most are `none`: the host does the I/O and hands the guest data.
- **Resources** (`resource document`) are handles to host-owned state. A `borrow<document>` parameter is valid for that call only.
- **Errors** are `result<T, string>`: an `err` carries a message the host surfaces to the user, so it should say what went wrong.
- **WIT to Rust** (wit-bindgen): kebab-case becomes `snake_case` for functions and fields and `UpperCamelCase` for types; `list<T>` is `Vec<T>`, `option<T>` is `Option<T>`, `result<T, E>` is `Result<T, E>`, `borrow<r>` is `&R`.

## Worlds (25)

| World | Exports (you implement) | Imports (you may call) |
|---|---|---|
| `auto-pair-plugin` | [`grammar-callbacks`](plugin-api/grammar-callbacks.md) | [`buffer`](plugin-api/buffer.md), [`config`](plugin-api/config.md), [`grammar`](plugin-api/grammar.md), [`help`](plugin-api/help.md), [`modes`](plugin-api/modes.md), [`tree-sitter`](plugin-api/tree-sitter.md), [`types`](plugin-api/types.md) |
| `comment-plugin` | [`grammar-callbacks`](plugin-api/grammar-callbacks.md) | [`buffer`](plugin-api/buffer.md), [`config`](plugin-api/config.md), [`grammar`](plugin-api/grammar.md), [`help`](plugin-api/help.md), [`modes`](plugin-api/modes.md), [`tree-sitter`](plugin-api/tree-sitter.md), [`types`](plugin-api/types.md) |
| `completion-source-plugin` | [`completion-source`](plugin-api/completion-source.md) | [`host-services`](plugin-api/host-services.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`types`](plugin-api/types.md) |
| `config-plugin` | — | [`config`](plugin-api/config.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md) |
| `context-plugin` | [`context`](plugin-api/context.md) | [`host-services`](plugin-api/host-services.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`tree-sitter`](plugin-api/tree-sitter.md), [`types`](plugin-api/types.md) |
| `dashboard-plugin` | — | [`dashboard`](plugin-api/dashboard.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md) |
| `decorations-plugin` | [`decorations`](plugin-api/decorations.md) | [`host-services`](plugin-api/host-services.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`types`](plugin-api/types.md) |
| `error-parser-plugin` | — | [`error-parser`](plugin-api/error-parser.md), [`logging`](plugin-api/logging.md) |
| `events-plugin` | — | [`events`](plugin-api/events.md), [`host-services`](plugin-api/host-services.md), [`logging`](plugin-api/logging.md), [`multibuffer-view-registry`](plugin-api/multibuffer-view-registry.md), [`project`](plugin-api/project.md), [`types`](plugin-api/types.md) |
| `grammar-plugin` | [`grammar-callbacks`](plugin-api/grammar-callbacks.md) | [`buffer`](plugin-api/buffer.md), [`grammar`](plugin-api/grammar.md), [`tree-sitter`](plugin-api/tree-sitter.md), [`types`](plugin-api/types.md) |
| `help-plugin` | — | [`help`](plugin-api/help.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md) |
| `keymap-plugin` | — | [`keymap`](plugin-api/keymap.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md) |
| `language-plugin` | — | [`language`](plugin-api/language.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md) |
| `media-plugin` | [`media`](plugin-api/media.md) | [`host-services`](plugin-api/host-services.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`types`](plugin-api/types.md) |
| `modes-plugin` | — | [`logging`](plugin-api/logging.md), [`modes`](plugin-api/modes.md), [`project`](plugin-api/project.md) |
| `multibuffer-view-plugin` | [`multibuffer-view-source`](plugin-api/multibuffer-view-source.md) | [`host-services`](plugin-api/host-services.md), [`logging`](plugin-api/logging.md), [`multibuffer-view-registry`](plugin-api/multibuffer-view-registry.md), [`project`](plugin-api/project.md), [`types`](plugin-api/types.md) |
| `picker-source-plugin` | [`picker-source`](plugin-api/picker-source.md) | [`host-services`](plugin-api/host-services.md), [`logging`](plugin-api/logging.md), [`picker-registry`](plugin-api/picker-registry.md), [`project`](plugin-api/project.md), [`types`](plugin-api/types.md) |
| `plugin` | — | [`buffer`](plugin-api/buffer.md), [`host-services`](plugin-api/host-services.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`types`](plugin-api/types.md), [`ui`](plugin-api/ui.md) |
| `plugin-manager-plugin` | — | [`logging`](plugin-api/logging.md), [`plugin-manager`](plugin-api/plugin-manager.md), [`project`](plugin-api/project.md) |
| `project-plugin` | [`grammar-callbacks`](plugin-api/grammar-callbacks.md), [`picker-source`](plugin-api/picker-source.md), [`transient-source`](plugin-api/transient-source.md) | [`buffer`](plugin-api/buffer.md), [`config`](plugin-api/config.md), [`events`](plugin-api/events.md), [`grammar`](plugin-api/grammar.md), [`help`](plugin-api/help.md), [`host-services`](plugin-api/host-services.md), [`modes`](plugin-api/modes.md), [`picker-registry`](plugin-api/picker-registry.md), [`project`](plugin-api/project.md), [`tree-sitter`](plugin-api/tree-sitter.md), [`types`](plugin-api/types.md) |
| `scanned-excerpt-source-plugin` | — | [`config`](plugin-api/config.md), [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`scanned-excerpt-source`](plugin-api/scanned-excerpt-source.md), [`tree-sitter`](plugin-api/tree-sitter.md), [`types`](plugin-api/types.md) |
| `sign-plugin` | — | [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`signs`](plugin-api/signs.md) |
| `theme-plugin` | — | [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`theme`](plugin-api/theme.md) |
| `transient-source-plugin` | [`transient-source`](plugin-api/transient-source.md) | [`logging`](plugin-api/logging.md), [`project`](plugin-api/project.md), [`types`](plugin-api/types.md) |
| `treesitter-context-plugin` | [`context`](plugin-api/context.md), [`grammar-callbacks`](plugin-api/grammar-callbacks.md) | [`buffer`](plugin-api/buffer.md), [`config`](plugin-api/config.md), [`grammar`](plugin-api/grammar.md), [`help`](plugin-api/help.md), [`modes`](plugin-api/modes.md), [`tree-sitter`](plugin-api/tree-sitter.md), [`types`](plugin-api/types.md) |

## Seams (31)

| Seam | Direction | Capability | Functions | Types | Summary |
|---|---|---|---|---|---|
| [`buffer`](plugin-api/buffer.md) | imports | - | 5 | 2 | Mirrors the native `Document` / `Buffer` read seam (plugin-host.md §4.2, §9.6). |
| [`command`](plugin-api/command.md) | types | - | 0 | 0 | Mirrors `CommandRegistry` + `CommandInvocation` + the closed `Effect` enum (lattice-grammar). |
| [`completion-source`](plugin-api/completion-source.md) | exports | - | 2 | 0 | Mirrors `lattice_completion` completion sources (PH7.6). |
| [`config`](plugin-api/config.md) | imports | - | 8 | 7 | Mirrors `ConfigRegistry` (lattice-config). |
| [`context`](plugin-api/context.md) | exports | - | 1 | 0 | The structural-**context** producer API (treesitter-context.md, TC.2): the scopes a pane pins above its text once their own header lines have scrolled away — the `nvim-treesitter-context` / sticky-scroll idea. |
| [`dashboard`](plugin-api/dashboard.md) | imports | - | 1 | 7 | CR.4: plugin-contributed dashboard sections. |
| [`decorations`](plugin-api/decorations.md) | exports | - | 1 | 0 | The decoration **producer** API (plugin-host.md §5 `decorations`, PH7.9), mirroring `Mode::gutter_decorations` + `GutterDecoration` (lattice-mode). |
| [`error-parser`](plugin-api/error-parser.md) | types | - | 0 | 2 | CM.6: plugin-contributed compilation-output parsers. |
| [`events`](plugin-api/events.md) | imports | - | 3 | 1 | The event/hook **subscription** API (plugin-host.md §5 `events`, PH7.8). |
| [`grammar`](plugin-api/grammar.md) | imports | - | 5 | 0 | The grammar-**extension** API (plugin-host.md §4.1, PH7.7). |
| [`grammar-callbacks`](plugin-api/grammar-callbacks.md) | exports | - | 6 | 0 | The behavior callbacks a grammar plugin **exports**; the host calls one by `callback` id on dispatch (the PH7.3d callback-id trampoline). |
| [`help`](plugin-api/help.md) | imports | - | 1 | 0 | CR.3: plugin-contributed `:help` pages. |
| [`host-services`](plugin-api/host-services.md) | imports | fs | 20 | 1 | Guest→host services (plugin-host.md §5). |
| [`keymap`](plugin-api/keymap.md) | imports | - | 1 | 1 | The `keymap` guest→host binding-registration seam (PL8.D.1). |
| [`language`](plugin-api/language.md) | imports | - | 1 | 2 | LG.3c: plugin-contributed languages. |
| [`logging`](plugin-api/logging.md) | imports | - | 1 | 1 | Guest→host structured logging (plugin observability Layer 2, design `docs/dev/architecture/plugin-observability.md` §8). |
| [`media`](plugin-api/media.md) | exports | - | 1 | 0 | The inline-media **producer** API (IM.6, `inline-media.md` §7). |
| [`modes`](plugin-api/modes.md) | imports | - | 3 | 8 | Mirrors the `Mode` trait declaration surface + `ModeRegistry` (lattice-mode). |
| [`multibuffer-view-registry`](plugin-api/multibuffer-view-registry.md) | imports | - | 2 | 0 | MV.1 — the seam by which a plugin **owns a multibuffer view**. |
| [`multibuffer-view-source`](plugin-api/multibuffer-view-source.md) | exports | - | 1 | 0 |  |
| [`picker-registry`](plugin-api/picker-registry.md) | imports | - | 1 | 0 | OR.5b — the host import a picker plugin registers its sources through. |
| [`picker-source`](plugin-api/picker-source.md) | exports | - | 2 | 1 | Mirrors `PickerSourceGenerator` (`lattice_picker::source`). |
| [`plugin-manager`](plugin-api/plugin-manager.md) | imports | proc | 1 | 3 | PM.7: the `require` seam — how a user's `init.rs` declares the plugins it wants (plugin-manager.md §3). |
| [`project`](plugin-api/project.md) | imports | fs | 2 | 2 | Guest→host project resolution (PR.6, design `docs/dev/architecture/project-resolution.md` §6). |
| [`scanned-excerpt-source`](plugin-api/scanned-excerpt-source.md) | types | - | 0 | 4 | OM.A1: plugin-contributed agenda rows. |
| [`signs`](plugin-api/signs.md) | imports | - | 1 | 1 | Mirrors the sign registry (`lattice_mode::SignRegistry`). |
| [`theme`](plugin-api/theme.md) | imports | - | 2 | 3 | Mirrors the theme-element registry (`lattice-theme`). |
| [`transient-source`](plugin-api/transient-source.md) | exports | - | 2 | 0 | TR.2b: plugin-contributed transient menus. |
| [`tree-sitter`](plugin-api/tree-sitter.md) | imports | - | 25 | 6 | Structural queries for plugins (plugin-treesitter-seam.md). |
| [`types`](plugin-api/types.md) | types | - | 0 | 147 | Shared boundary records/variants — the owned, WIT-serializable mirrors of the native grammar + picker/completion types (plugin-host.md §4). |
| [`ui`](plugin-api/ui.md) | imports | - | 3 | 0 | The UI-contribution surface (design.md §9.4 `ui`): guest→host emits **data only**, never draw calls (§7, paramount #1). |
