# Phase 5 extraction audit

Anchor: design.md §5.6 (rendering layered architecture), §11 (project layout), §13 Phase 5–6.

This is the planning artefact for Phase 5. **It does not introduce any code change.** It establishes what's in `lattice-ui-tui` today, what's renderer-coupled versus renderer-agnostic, where each module lives after Phase 5, and the slice ordering that lets us get there without the editor breaking at any intermediate commit.

## Goal

`lattice-ui-tui` is currently the host: it owns the `App` struct, every keystroke dispatch path, every picker source generator, LSP coordination, mode lifecycle, options cascade, file ops, AND the ratatui paint code. Phase 5 splits the host out so the design's renderer model (§5.6.1: `EditorRenderer` / `DocumentRenderer` / `TuiRenderer` / `WebRenderer` / `CanvasRenderer`) can actually exist. After Phase 5, GPUI lands as a peer renderer, the TUI shrinks to an adapter behind the same trait, and `lattice --tui` selects it explicitly. GPUI becomes the default only after parity.

This work is foundational and slow. The constraint that lattice keeps working at every commit means many small migrations, not one big PR.

## Current state (the numbers)

`lattice-ui-tui` is **71,629 LoC across 50+ modules**. By inspection, only ~11k LoC are genuinely TUI-coupled (ratatui or crossterm in production code paths). The rest -- the App struct, picker sources, mode lifecycle, LSP coordination, keymap registry, dispatch, fold computation, search, completion engine, etc. -- is renderer-agnostic logic that ended up here for historical reasons (lattice-ui-tui was the only renderer when the host was written).

The audit below lists the renderer-coupled modules first (they stay), then the host modules (they move). "Public-surface leak" means a `pub` type or `pub` fn whose signature names a `ratatui::*` or `crossterm::*` type -- those signatures are the boundary work.

## Target crate shape

Renames §11 of design.md's "the rest of `lattice-ui-tui`'s contents" into named crates:

```
crates/
	lattice-host/                # NEW. Renderer-agnostic. App + dispatch + picker sources +
	                             # mode lifecycle + LSP coordination + options cascade +
	                             # keymap registry + buffer registry + folds + search +
	                             # completion engine + file ops + oil + file-tree state.
	                             # Public entry: `fn run(app: App, renderer: impl Renderer)`.
	                             # Exports the App struct and a renderer-neutral Theme.

	lattice-render/              # NEW. Renderer trait (§5.6.1), Frame, InputEvent,
	                             # LayoutConstraints, AccessibilityNode. Pure trait + data.
	                             # Zero renderer impl.

	lattice-render-editor/       # NEW (5.7+). EditorRenderer helpers: logical→visual
	                             # position translation, path-1 monospace primitives.
	                             # Backend-agnostic; both TUI and GPUI consume it.

	lattice-ui-tui/              # SHRINKS to: ratatui Renderer impl + crossterm input bridge +
	                             # TuiTheme adapter (semantic host theme → ratatui Style) +
	                             # the per-pane render function pointers wired against ratatui.
	                             # ~11k LoC. Depends on lattice-host + lattice-render.

	lattice-ui-gpui/             # NEW (5.7+). GPUI Renderer impl. Depends on lattice-host
	                             # + lattice-render + lattice-render-editor. Independent of
	                             # lattice-ui-tui.

	lattice-cli/                 # EXISTS. Gains `--tui` flag. Currently default → TUI;
	                             # flips to default → GPUI once parity ships.
```

What's deliberately NOT named:

- **No `lattice-render-document` yet** (that's Phase 6: popups, status lines, pickers, previews; needs taffy + cosmic-text). Phase 5 ends before that.
- **No "renderer-common" crate** for shared composition logic. The instinct to factor out `compose_visible_lines` between renderers is real but premature -- we have one renderer today; splitting before GPUI exists invents abstractions for an audience of one.

## Module classification

Every `.rs` file under `crates/lattice-ui-tui/src/` audited. Five buckets:

- **HOST** — moves to `lattice-host`. Renderer-agnostic.
- **TUI_RENDER** — stays. Ratatui paint code.
- **TUI_INPUT** — stays. Crossterm event decoding (plus the renderer-neutral pieces of the dispatcher that get extracted -- see Hard Case §3).
- **THEME_TUI** — stays. Renderer-specific theme realisation (ratatui `Style` adapter for the host's semantic theme).
- **BOOT** — App construction. The body moves to `lattice-host`; a thin TUI startup adapter stays.
- **MIXED** — split required (Hard Cases section below lists each).

### TUI-coupled (stays in `lattice-ui-tui`)

| File | LoC | Bucket | Note |
|---|---|---|---|
| `render.rs` | 5,708 | TUI_RENDER | Frame composition, line wrapping into `Vec<Line<Span>>`, gutter, diagnostics paint, status lines, popups. Public signatures take `&mut ratatui::Frame`. |
| `input.rs` | 4,091 | MIXED | `translate(ctx, &KeyEvent) -> Option<Action>` -- crossterm input on the outside; mode-aware dispatch on the inside. Split: dispatch logic → host, crossterm shim → here. |
| `runtime.rs` | 560 | BOOT | Main loop. Sets up `ratatui::Terminal<CrosstermBackend>`, polls crossterm events, calls `App::apply`, paints frame. Stays as the TUI's startup adapter. |
| `theme.rs` | 249 | THEME_TUI | Every field is `ratatui::Style` / `ratatui::Color`. Hard Case §1. |
| `icons.rs` | 184 | THEME_TUI | `icon_for_entry()` returns `(glyph, ratatui::Style)`. Already routes through renderer-neutral `lattice-core::ui::icons`; this file is the ratatui adapter. |
| `pane_render.rs` | 131 | MIXED | `PaneRenderFn = fn(&mut Frame, Rect, &App, ...)`. The registry concept is host; the fn-pointer signature is TUI. Hard Case §2. |
| `chord.rs` | 1,049 | MIXED | Contains the renderer-neutral `KeyChord` / `KeyKind` / `SpecialKey` types AND the `from_event(&KeyEvent)` crossterm adapter. Hard Case §3. |

Total renderer-coupled: **~11,972 LoC**. Plus tests embedded in these files.

### Host (moves to `lattice-host`)

Top-level, all renderer-agnostic in production paths:

| File | LoC | Bucket |
|---|---|---|
| `app.rs` | 6,015 | HOST (with one caveat -- `App.theme: Theme` at line 1769 holds the ratatui-typed theme; the field changes type when the renderer-neutral theme lands, see Hard Case §1) |
| `actions.rs` | 1,262 | HOST |
| `excommand.rs` | 1,332 | HOST |
| `keymap_registry.rs` | 1,261 | HOST |
| `keymap_normal.rs` | 2,771 | HOST |
| `keymap_insert.rs` | 1,029 | HOST |
| `keymap_replace.rs` | 344 | HOST |
| `keymap_visual.rs` | 476 | HOST |
| `keymap_trie.rs` | 591 | HOST |
| `keymap.rs` | 691 | HOST |
| `picker_sources.rs` | 2,291 | HOST |
| `host_generators.rs` | 209 | HOST |
| `buffer_registry.rs` | 760 | HOST |
| `buffers.rs` | 7 | HOST |
| `modes.rs` | 430 | HOST |
| `folds.rs` | 841 | HOST |
| `tui_options.rs` | 154 | HOST (despite the name -- it's option *spec declarations* for `ui.*`, not TUI rendering; renames to `ui_options.rs` on the move) |
| `lib.rs` | 71 | (rewires on the move; host re-exports + thin TUI lib) |

App subdir (`crates/lattice-ui-tui/src/app/`), all HOST except where noted:

| File | LoC | Bucket |
|---|---|---|
| `lsp.rs` | 10,494 | HOST -- LSP supervisor drains, request/response handlers; no ratatui in production signatures |
| `lifecycle.rs` | 3,378 | HOST |
| `help.rs` | 2,637 | HOST |
| `dispatch.rs` | 2,625 | HOST (crossterm only in `#[cfg(test)]` blocks at lines 1686, 1698 -- harness, not production) |
| `completion.rs` | 2,486 | HOST |
| `picker.rs` | 2,372 | HOST |
| `edit.rs` | 1,838 | HOST |
| `options.rs` | 1,547 | HOST |
| `motions.rs` | 1,329 | HOST |
| `folds.rs` | 1,357 | HOST |
| `mode.rs` | 1,173 | HOST |
| `cmdline.rs` | 1,088 | HOST |
| `boot.rs` | 1,217 | BOOT -- App construction body moves to host; the call site flips between renderers; uses `ratatui::style::Style` at line 1028 in `sync_theme_from_config`, which becomes the TUI's theme adapter |
| `search.rs` | 1,135 | HOST |
| `highlights.rs` | 886 | HOST |
| `popup.rs` | 616 | HOST |
| `lsp_watcher.rs` | 432 | HOST |
| `lsp_log_buffers.rs` | 380 | HOST |
| `test_helpers.rs` | 343 | TEST -- moves with whatever it tests, ends up split between host and TUI tests |
| `visual.rs` | 319 | HOST |
| `messages.rs` | 296 | HOST |
| `display.rs` | 287 | HOST |
| `file_tree.rs` | 266 | HOST |
| `oil.rs` | 271 | HOST |
| `macros.rs` | 194 | HOST |
| `syntax.rs` | 75 | HOST |
| `state.rs` | 23 | HOST (doc-only) |
| `operators.rs` | 22 | HOST (stub) |

Total host: **~59,000 LoC** across ~45 modules.

## Hard cases

### Hard Case §1 -- `theme.rs` and `App.theme`

Every field on `Theme` (theme.rs) is `ratatui::Style` or `ratatui::Color`. `App.theme: crate::theme::Theme` (app.rs:1769) is the field renderers read each frame. `boot.rs::sync_theme_from_config` (app/boot.rs:1028) walks every `ui.*` option and writes ratatui-typed values into theme fields.

**Move:**

1. New host type `lattice_host::ui::Theme` with *semantic* fields and a *renderer-neutral* color enum:

	```rust
	pub struct Theme {
		pub pane_status_active: Style,
		pub diagnostic_error: SeverityStyle,
		pub file_tree_dir: Style,
		// ... mirrors the field set, but expressed in neutral types.
	}

	pub struct Style {
		pub fg: Color,
		pub bg: Color,
		pub attributes: Attributes,
	}

	pub enum Color {
		Default,
		Named(NamedColor),       // Black, Red, ..., enough for 16-color TUI fallback.
		Indexed(u8),             // 256-color palette.
		Rgb(u8, u8, u8),         // 24-bit; GPUI uses this directly; TUI maps when terminal supports it.
	}

	pub struct Attributes {
		pub bold: bool,
		pub italic: bool,
		pub underline: bool,
		pub dim: bool,
		pub reverse: bool,
	}
	```

2. `App.theme` changes type to `lattice_host::ui::Theme`. App still owns one theme; renderers don't see it directly.

3. `lattice-ui-tui` adds `TuiTheme` -- a `From<&host::Theme>` adapter that produces a struct shaped like today's `Theme`, ratatui-typed throughout. The TUI renderer holds the adapted view; the host writes the neutral form on `:set ui.*`; the TUI rebuilds the adapter on theme change.

4. `lattice-ui-gpui` (when it lands) does the same thing in reverse: adapts `host::Theme` into GPUI's `Hsla` / variable-font selection / sub-pixel offsets.

**Why neutral enum colors, not `ratatui::Color`-ish:** the spec needs to be expressive enough for GPUI's full RGBA + alpha world without losing the TUI's 16-color and 256-color paths. Three of the variants (`Default`, `Named`, `Indexed`) exist for the TUI's benefit; `Rgb` exists for everyone but maps to "closest 256-color cell" in the TUI when the terminal doesn't support truecolor. The TUI adapter owns the lossy mapping.

**Cost:** ~15-20 fields converted; one adapter file (~150 LoC); every `:set ui.*` cascade ending in `sync_theme_from_config` re-routes through the adapter. Mechanical.

### Hard Case §2 -- `pane_render.rs` and per-mode render fn-pointers (**superseded post-5.4**)

> **Status:** the trait-object plan below is **superseded** by the Option-E pivot. See the "Plan revision (post-5.4)" note above and the revised slice 5.6. The text is retained as a record of the original design thinking; the actual move is much smaller in the composition world (only the mode-walking lookup is host; the registry stays renderer-specific, no trait objects).

```rust
pub type PaneRenderFn = fn(&mut Frame, Rect, &App, &DocumentSnapshot, &PaneState, bool, usize);
pub type PaneStatusFn = fn(&App, &PaneState) -> String;
```

`App` holds `pane_render_registry: PaneRenderRegistry` (owns these fn pointers). `render.rs::build_pane_render_registry()` (render.rs:1669) wires providers for help, file-tree, oil, etc. The architecture (registry of per-mode render providers) is the right shape -- the leak is the typed signature.

**Original move (pre-Option-E, trait-objected):**

The registry is host. The fn-pointer type goes generic over the renderer:

```rust
pub trait PaneRenderer {
	fn render(&mut self, ctx: &mut RenderContext, app: &App, state: &PaneState, focused: bool, index: usize);
}

pub trait PaneStatus {
	fn status(&self, app: &App, state: &PaneState) -> String;
}

pub struct PaneRenderRegistry {
	renderers: HashMap<PaneRenderKey, Box<dyn PaneRenderer>>,
	statuses: HashMap<PaneRenderKey, Box<dyn PaneStatus>>,
}
```

`RenderContext` is the renderer's own concrete type, passed by `&mut` -- ratatui wraps `&mut Frame + Rect`; GPUI wraps its own paint context. Each renderer registers its own `impl PaneRenderer` against the keys it knows how to render.

Alternative considered (rejected): typed via associated type `trait PaneRenderer { type Ctx; ... }`. Forces the registry's HashMap value type to be generic over `Ctx`, which propagates through every callsite. The `&mut RenderContext` trait-objected form is simpler, and the renderer-neutral `RenderContext` can carry whatever opaque state the active renderer needs.

**Cost (original):** trait introduction, one host module gains the trait, each renderer ships its own impls. The dispatch table that already exists is largely unchanged in shape.

**Post-Option-E revision (slice 5.6).** Composition made the registry naturally renderer-specific -- each renderer's `App` already holds its own typed `PaneRenderRegistry` with native-shaped fn pointers, and that's the right shape. The trait-object machinery isn't needed. The actual 5.6 work is the small remaining piece: move the mode-walking resolution logic (currently `impl App` in `pane_render.rs`, walks active minors then major) to `lattice_host::pane_render::lookup` parameterised by a tiny `ProviderLookup` trait. Host owns the algorithm; renderers own the storage. No v-table calls on the paint hot path.

### Hard Case §3 -- `chord.rs` and crossterm-shaped key chords

`chord.rs` (1,049 LoC) contains both:

- Renderer-neutral types: `KeyChord`, `KeyKind`, `SpecialKey`. These are pure data; they fit `lattice-host` (they're the keys the trie indexes by).
- Crossterm adapters: `KeyChord::from_event(&crossterm::event::KeyEvent)`, `format_chord(&KeyEvent)`. These convert between crossterm's event shape and the neutral chord. TUI-only.

`input.rs` (4,091 LoC) has the same split internally:

- `TranslateContext` and the mode-dispatching match arms -- pure host logic that takes a chord and produces an `Action`.
- The outer wrapper that accepts `&crossterm::event::KeyEvent`, calls `chord.rs::from_event`, then runs the dispatch -- TUI input shim.

**Move:**

Split each file at the crossterm seam.

- `lattice-host` gets: `KeyChord`, `KeyKind`, `SpecialKey`, and a new `fn dispatch_chord(ctx: DispatchContext, chord: KeyChord) -> Option<Action>` (the full dispatch table). The host knows nothing about crossterm.
- `lattice-ui-tui` keeps a thin adapter: `fn translate_event(ctx, &KeyEvent) -> Option<Action> { let chord = KeyChord::from_crossterm(event); dispatch_chord(ctx, chord) }`.

GPUI ships its own `fn translate_event(ctx, &GpuiKeyEvent) -> Option<Action>` that produces the same `KeyChord` via a GPUI-shaped converter. Both renderers consume the same host dispatch.

**Cost:** medium. The 4k LoC `input.rs` is mostly host-side dispatch logic with a thin crossterm wrapper; the split is mechanical but touches every test case currently constructing `KeyEvent` directly (those become test helpers in `lattice-ui-tui` or use `KeyChord` directly in host tests). Estimate: 1-2 days of careful surgery.

### Hard Case §4 -- `App.theme` and the theme-driven hot path

Render reads theme fields per frame: `theme.diagnostic_error_style`, `theme.pane_status_active`, `theme.cursor_line_bg`. This is hot. Adapting `host::Theme` → ratatui Style on every read would be silly.

The TUI adapter caches: `TuiTheme` holds pre-computed ratatui `Style` mirrors. Rebuilt only when the host's `host::Theme` changes (which fires on `:set ui.*`). The hot-path frame read goes straight to `TuiTheme.diagnostic_error_style` -- a ratatui-typed `Style` lookup, same as today.

GPUI does the analogous caching with its own native style shape.

This pattern -- "host owns the canonical neutral state; each renderer owns a cached adapted view" -- generalises beyond theme. Iconography fits it (`lattice-core::ui::icons` is the neutral source; `lattice-ui-tui::icons` and a future `lattice-ui-gpui::icons` are renderer-specific atlases). Pane render dispatch (Hard Case §2) fits it too.

## Session log

| Slice        | Commit                                                    | What landed                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
|--------------|-----------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 5.0          | `2d46bf2`                                                 | Extraction audit doc                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| 5.1          | `8f12c6a`                                                 | Empty `lattice-host` crate                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 5.2 wave 1   | `8a5b256`                                                 | Trivial re-export shims (buffers, file_tree, oil, popup, help, help_topics)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 5.2 wave 2   | `7286312`                                                 | actions + excommand (~2.6k LoC)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 5.2 wave 3   | `2454530`                                                 | host_generators (~200 LoC)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 5.3          | `d024635`                                                 | Renderer-neutral theme types + `App.host_theme`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 5.4          | `6657cbb`                                                 | chord.rs split (neutral types to host, crossterm adapter stays)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 5.2 wave 4   | `f78c64d`                                                 | keymap leaves (keymap.rs + keymap_trie.rs + keymap_registry.rs)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 5.2          | `d3ec49c`                                                 | Action enum + EchoLevel + EchoMessage + FindKind extracted                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 5.2          | `f32c677`                                                 | Catalogs reclassified MIXED; reverted move                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 5.2          | `950a728`                                                 | `Fold` moved to `lattice-core::folding`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| 5.2          | `a0cda39`                                                 | folds.rs + modes.rs migrated                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 5.2          | `00e5f8a`                                                 | buffer_registry.rs migrated                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 5.2          | `5753e38`                                                 | pane shim migrated                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| 5.2          | `93fc8d8`                                                 | App helper state types extracted (SearchLine, LastSearch, UnnamedRegister, PrevPaneState)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 5.2          | `1f1ca50`                                                 | Batch: OptionCache, LastFind, MacroRecording, TagStackEntry, PositionEntry, PositionSource, ReplaceEntry, LastVisual, SubstitutePreview, PendingBlockInsert all extracted to `lattice_host::state`                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| 5.2          | `3d66a3b`                                                 | LSP cache + outcome types moved to `lattice_lsp::cache` (~620 LoC, 35 types)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 5.B.0        | `f7416a1`                                                 | App field audit doc -- [`phase-5b-app-fields.md`](phase-5b-app-fields.md) (2 of ~200 fields renderer-specific)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| 5.B.1        | `6f651a4`                                                 | `lattice_host::Renderer` trait + `MinimalRenderer` headless impl                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| 5.B.2        | `3649c18`                                                 | `App<R: Renderer = TuiRenderer>` parametrisation (subsequently reverted in 5.B.3 -- see Option-E pivot below)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| 5.B (design) | (this commit)                                             | [`phase-5b-app-design.md`](phase-5b-app-design.md) -- Option D → Option E pivot. Composition replaces generic parametrisation.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| 5.B.3        | `3e0db86`                                                 | Reverted 5.B.2's `App<R>` generics; defined empty `lattice_host::editor::Editor` and added `editor: Editor` field on `App`. Per-cluster field migration begins from 5.B.4.                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 5.B.4        | `8fc1d5b`                                                 | Macros cluster → `Editor` (macro recording / replay state + helpers).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| 5.B.5        | `58fb496`                                                 | Marks + registers cluster → `Editor` (named marks, unnamed register, numbered + named registers).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| 5.B.6        | `cedbaca`                                                 | Position history + tag stack cluster → `Editor` (unified jump/mark ring backing storage).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 5.B.7        | `7a9fb4d`                                                 | Search state → `Editor` (search_line, last_search, hlsearch toggles, substitute preview).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 5.B.8        | `1944035`                                                 | Vim repeat (`.`) + visual state → `Editor` (last_change, last_visual, pending block insert).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 5.B.9        | `0879473`                                                 | Replace + insert state → `Editor` (replace overstrike stack, insert anchors).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| 5.B.10       | `4c7bd1e`                                                 | Popup cluster (3 of 4) → `Editor` (popup buffer + active popup; back-stack stayed on `App` pending C2/C3).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 5.B.11       | `49bbe15`                                                 | Cmdline + echo cluster → `Editor` (command_line, last_message, messages, command_history).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 5.B.12       | `e9203e1`                                                 | Syntax cluster → `Editor` (lang_registry, syntax, pending_syntax_edits).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| 5.B.13       | `f3d0652`                                                 | Picker cluster → `Editor` (picker, picker_registry, picker_mru, live-picker query state).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 5.B.14       | `2eb61ad`                                                 | Config + modes cluster → `Editor` (config, option_cache, mode_registry, services).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| 5.B.15       | `37b4dbd`                                                 | Modal + dispatch cluster → `Editor` (modal, partial_chord, action_ids, builtins, keymap handle).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| 5.B.16       | `94dcebb`                                                 | Active-pane state subset → `Editor` (cursor, scroll, viewport plumbing; pane_tree deferred to C4a).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 5.B.17       | `acf5b2a`                                                 | LSP per-buffer caches → `Editor` (~12 `lsp_*_cache` fields).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 5.B.18a      | `d00b4d9`                                                 | LSP field scaffolding on `Editor` (subsystem handle + diagnostics + watcher slots, no behaviour change).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| 5.B.18b      | `f1ba0f9`                                                 | LSP subsystem + server channels → `Editor`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 5.B.19       | `d5b5b82`                                                 | LSP request channel call sites migrated to `Editor` (~40 pending_*_rx / pending_*_token redirects).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 5.B.C1       | `0d84c6f`                                                 | Cmdline `CompletionState` + `completion_registry` → `Editor`. `CompletionState` re-exported from host.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| 5.B.C2       | `bb4911e` + `e51db26`                                     | `VisibleHighlightsKey` promoted to host; visible_highlights cache key storage → `Editor`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 5.B.C3       | `bb4911e`                                                 | `PopupSnapshot` promoted to host; ui-tui consumes host type.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 5.B.C4a      | `ed8c4ec`                                                 | `pane_tree` → `Editor`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| 5.B.C4b/c    | `db31aeb`                                                 | `document` + `snapshot_cache` → `Editor`. Removed duplicate `CompletionState` in `host::state`; dropped `Editor::Default` panic placeholder; fields initialised in `boot`.                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 5.B.C4 final | `c57346c`                                                 | `folds` → `Editor`; render/highlights updated; popup construction begins consuming host `PopupSnapshot`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| 5.B.C5       | `e51b4ed` + `2e3af79` + `be5c1d0` + `3465f6f` + `9401d4e` | `LspFileWatcher` type moved to host; ui-tui holds the optional host watcher; refresh/drain wired through host impl. Workspace green; final LSP/document/folds redirects landed.                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 5.B.20       | `f99af23`                                                 | **Completion cluster tail + popup back-stack.** `insert_completion`, `snippet_registry`, `insert_completion_snippet_meta`, `completion_accept_freq`, `per_language_completion`, `completion_in_path_context`, `active_snippet`, `snippet_dirs`, `popup_back_stack`, and `pending_config_structural_sections` move to `Editor`. `SnippetCandidateMeta` moves from `lattice-ui-tui::app` to `lattice-host::state`; ui-tui re-exports. **5.B endpoint reached:** `App` now holds `{editor, pane_render_registry, theme, lsp_file_watcher}` — four fields. 1424 ui-tui + 177 host + 180 lsp = 1781 tests, identical to the pre-5.B baseline. |
| 5.4 (1/5)    | `3087e5d`                                                 | Leaf translators (`translate_search` / `translate_command` / `translate_command_chord_capture` / `translate_picker`) take `KeyChord` instead of `KeyEvent`. |
| 5.4 (2/5)    | `08b45a3`                                                 | Normal-mode dispatch (`translate_normal` / `compute_normal_action` / `lookup_normal*`) takes `KeyChord`. Chord conversion hoists to one site at the top of `translate()`. |
| 5.4 (3/5)    | `b2e555a`                                                 | `keymap_normal.rs` (~1.5k LoC of production code) moves to `lattice-host`; ui-tui keeps test module + re-export stub. |
| 5.4 (4/5)    | `b70baa0`                                                 | `keymap_insert` / `keymap_visual` / `keymap_replace` move to host with `dispatch_*` taking `&KeyChord`. |
| 5.4 (5/5)    | `fcb5512`                                                 | `input.rs` itself moves to `lattice-host`; `lattice-ui-tui::input` shrinks to a ~30-line crossterm `KeyEvent → KeyChord` shim. |
| 5.4 (docs)   | `ec98c09`                                                 | Implementation-ledger rows 5.2 + 5.4 + Phase-5 summary marked ✅. |
| 5.5.A        | `ff6add5`                                                 | Scaffold `Editor::dispatch(action) -> DispatchOutcome` + `RendererSignal { ThemeChanged, Quit }`. Body is a stub `handle_action(editor, action, &mut out)` free fn (no behaviour change). `App::apply` keeps its full body. (~50 LoC.) |
| 5.5.B        | `fdbbf54`                                                 | Preamble migrated: macro-recording capture + partial-chord clear move to `Editor::dispatch`'s top. `App::apply` now calls `self.editor.dispatch(action.clone())` before its own match. (~30 LoC moved.) |
| 5.5.C        | `6c7ee59`                                                 | 10 helper-free `Action` arms move into `Editor::dispatch`: `Action::None`, `Quit`, `AbsorbPartialChord`, `PushDigit`, `Echo`, `CommandLineCancel`, `SelectRegister`, `CommandLineDeleteChord`, `CommandLineDismissCompletion`, `EnterSearch`. First emission of `RendererSignal::Quit`. App's match keeps the grouped no-op arm for exhaustiveness until 5.5.G collapses it. (~80 LoC moved.) |
| 5.5.D        | (this commit)                                             | Pure-editor mutation helpers (batch 1) move to `impl Editor`: `set_message`, `ensure_cursor_visible`, `clamp_cursor_to_buffer` + `clamp_cursor_to_active_buffer`, `dismiss_popup` + `dismiss_stale_popup_registry`, `maybe_reparse_syntax`, `recompute_folds` + `recompute_syntax_folds` + `recompute_lsp_folds`, `foldmethod`, plus the query family they depend on (`popup_help`, `active_text`, `active_cursor`, `active_buffer_id`, `active_pane_buffer_id`, `document_syntax_for`, `minor_mode_enabled_for`, `lsp_folding_mode_enabled_for`). Read-only-help guard moves into `Editor::dispatch`'s preamble with `action_is_document_mutation` (now `lattice_host::dispatch::`); App reads `outcome.consumed` to bail. `DispatchOutcome.consumed: bool` is the interim coordination field — disappears in 5.5.G when App's match collapses. **Deferred to later sub-slices:** `enter_mode`, `do_insert_text`, `do_delete_char_backward`, and the open/close-line family. Their bodies pull in `apply_edit_blocking` + `publish_document_changed` + the LSP signature-help / on-type-formatting autopilots, which belong to 5.5.E's effect-handler cluster. App keeps thin delegating wrappers so existing call sites compile unchanged. (~400 LoC moved.) |

**Total moved from `lattice-ui-tui`:** ~28k LoC (post-5.4). The keymap family (~4,600 LoC) and input dispatch (~430 LoC) joined the migration on top of 5.B's ~24k. **Test count:** 1424 ui-tui + 177 host + 180 lsp = 1781 passing; workspace green throughout.

## Remaining work

**5.B is done** ([`phase-5b-app-design.md`](phase-5b-app-design.md) Option-E composition migration). Clusters 1–12 plus the C-wave landed in 5.B.4 → 5.B.19 + C1–C5, and the final tail (completion cluster #8 remainder + popup back-stack + `pending_config_structural_sections`) closed out in 5.B.20. `App` is now four fields: `editor`, `pane_render_registry`, `theme`, `lsp_file_watcher`. The first three are the renderer-specific shape this doc committed to; the fourth wraps a host-typed watcher (revisit during 5.5 if it should disappear behind the renderer trait).

**Approach: composition (Option E from [`phase-5b-app-design.md`](phase-5b-app-design.md)).** Option D (`App<R: Renderer>` parametrised over a host-side trait) was attempted in 5.B.2 and reverted in 5.B.3 once the Rust orphan rule made every renderer-specific method either (a) require a single mega-commit moving 35k LoC, or (b) require lifelong extension-trait machinery. Option E (composition) achieves the same separation more cleanly:

1. `lattice_host::editor::Editor` holds the renderer-agnostic editor state.
2. Each renderer crate's `App` struct composes `editor: Editor` alongside its renderer-specific caches (`theme`, `pane_render_registry`).
3. Per-cluster commits relocate field clusters from `App` into `Editor`, moving the methods that touch only those fields into `impl Editor` in lattice-host.
4. `App`'s remaining inherent impl surface ends up genuinely TUI-only.

Every per-cluster commit ships green: methods that still live in `impl App` access migrated fields via `self.editor.foo`; methods that have moved to `impl Editor` use `self.foo` directly. The discipline that broke under Option D is preserved under Option E.

**Phase 5.4 closed** (commits `3087e5d`, `08b45a3`, `b2e555a`, `b70baa0`, `fcb5512` plus `ec98c09` for the doc update). The deferred input.rs split and the 5.2 keymap-catalog migration both landed in the same arc: every per-mode keymap catalog (`keymap_normal/insert/visual/replace`) plus the unified `translate(ctx, chord) -> Action` dispatcher live in `lattice-host`; `lattice-ui-tui::input` is now a ~30-line crossterm shim.

### Plan revision (post-5.4)

The remaining-slices plan below (5.5 → 5.last) **has been rewritten** post-Phase-5.4. The original plan assumed the Option-D world (`App<R: Renderer>` generic over a host-side trait); two of its slices were artefacts of that assumption and no longer fit the Option-E composition reality:

- **Original 5.5 (trait-objected `PaneRenderer` in host)** is dropped. In the composition world the registry is correctly renderer-specific (each renderer's `App` holds its own typed registry); only the mode-walking lookup is host-side. The renamed 5.6 below captures that small move without the trait-object overengineering.
- **Original 5.6 (`lattice-render` crate with `Frame` / `InputEvent` / `LayoutConstraints`)** is dropped. ratatui's pull-based draw loop and GPUI's retained-mode element tree are structurally different; forcing a common `Frame` type buys nothing and constrains both renderers. If a real shared primitive emerges later (likely the §5.6.7 `DocumentRenderer` for popups/pickers/help) it can live in `lattice-host::ui` rather than a separate crate. The existing `lattice_host::Renderer` trait stays in place for now as documentation of the renderer contract.

The audit that drove this rewrite is in [`phase-5-dispatch-extraction.md`](phase-5-dispatch-extraction.md): in the Option-E world, the single remaining blocker for parallel GPUI work is that `App::apply` (dispatch — the `Action → state mutation` table) lives in `lattice-ui-tui`. Every other host concern is already in `lattice-host`. The revised 5.5 relocates dispatch and unblocks GPUI directly.

## Slice ordering

Each slice ships green; lattice-ui-tui keeps working at every commit.

**5.0 -- this document.** No code. ✓

**5.1 -- create `lattice-host` shell.** New empty crate. Workspace dep entry. lattice-ui-tui depends on it (currently nothing flows through). Builds + tests green. ~30 minutes.

**5.2 -- move pure HOST modules (no theme touch).** Migrate every module classified HOST in the table above that doesn't transitively depend on `theme.rs` or `chord.rs`. The list is most of `app/` (lifecycle, lsp, dispatch, edit, motions, options, folds, search, completion, picker, mode, popup, cmdline, visual, macros, messages, file_tree, oil, syntax, display, state, operators, highlights, lsp_log_buffers, lsp_watcher), plus the top-level keymap*.rs, buffer_registry.rs, buffers.rs, modes.rs, folds.rs, picker_sources.rs, host_generators.rs, excommand.rs, actions.rs, tui_options.rs (renames to ui_options.rs). lattice-ui-tui re-exports the moved types via `pub use lattice_host::*;` so consumers (lattice-cli) don't break. **This is the bulk of the work** -- estimate 1-2 weeks of careful migration, one or two modules per commit, each landing green. May surface accidental couplings (e.g., a HOST module reaching into `crate::theme` to read a field it shouldn't); those resolve case by case.

**5.3 -- renderer-neutral theme.** Hard Case §1. Define `lattice_host::ui::Theme` + adapter `lattice_ui_tui::tui_theme::TuiTheme`. Change `App.theme` type. `sync_theme_from_config` becomes neutral on the host side; TUI re-runs the adapter on every change. Tests: the existing theme tests run against the neutral form; a small TUI-side test pins the adaptation. ~2-3 days.

**5.4 -- split `chord.rs` and `input.rs` + keymap family.** ✓ shipped (see commits referenced in the post-5.4 note above). Renderer-neutral `KeyChord` + the unified `translate(ctx, chord) -> Action` dispatcher + every per-mode catalog now live in `lattice-host`. `lattice-ui-tui::input` is a ~30-line crossterm `KeyEvent → KeyChord` shim.

**5.5 -- dispatch extraction (`App::apply` → `Editor::dispatch`).** The largest remaining architectural unlock. Move the ~2.6k-LoC `App::apply` body (and the ~60-variant `apply_effect` handler, and the dozens of pure-editor-state-mutating `do_*` helpers) from `lattice-ui-tui::app::dispatch` to `lattice_host::editor::Editor::dispatch(action) -> DispatchOutcome`. After this slice, `lattice-ui-gpui` can `editor.dispatch(action)` without any `lattice-ui-tui` dependency. Sliced into ~8 sub-slices (5.5.A–H) to keep reviews tractable; each lands green. Focused design doc: [`phase-5-dispatch-extraction.md`](phase-5-dispatch-extraction.md). ~1-2 weeks.

**5.6 -- pane-provider lookup → host.** The mechanical move surfaced by retiring the original 5.5. The mode-walking resolution (walk active minors then major to find the `PaneRenderProvider`) is renderer-neutral logic that lives on `impl App` today. Move to `lattice_host::pane_render::lookup(&impl ProviderLookup, &Editor, BufferId) -> Option<&Provider>` where `ProviderLookup` is a tiny trait abstracting "give me the provider for this ModeId." Each renderer keeps its concrete registry (`PaneRenderRegistry` stays in `lattice-ui-tui` with ratatui-shaped fn pointers; `lattice-ui-gpui` ships its own with GPUI-shaped impls). No trait objects on the hot path. ~1-2 hours.

**5.7 -- `lattice-ui-gpui` scaffold.** New crate. Window opens. Compose `GpuiApp { editor: Editor, gpui_theme, gpui_pane_render_registry, ... }` against the now-renderer-ready `lattice-host`. Render a placeholder; validate end-to-end that the host substrate is reusable without ui-tui in the dep tree. ~1 day to learn the GPUI surface; ~1 week for the actual scaffold.

**5.8+ -- GPUI feature parity, incrementally.** Input adapter (GPUI key event → `KeyChord`), text paint, panes, pickers, popups, syntax highlighting, status line, statusline, completion popup. Each is a slice of its own; the TUI continues working at every commit because every concern routes through `Editor`. Multiple weeks.

**5.9 -- `lattice-cli` gains `--gpu` / `--tui` flags.** Once GPUI runs end-to-end, plumb the renderer selection through the CLI. Initial default stays TUI; `--gpu` is opt-in. ~few hours.

**5.last (separately scheduled) -- flip default to GPUI.** Only after GPUI parity. `--tui` remains as the explicit opt-in. Don't flip on day one of GPUI parity; give it a release cycle as opt-in (`--gpu`) before flipping default. This protects the user's existing TUI workflow.

## What we are not doing in Phase 5

- **No new picker primitives** beyond what already exists. Phase 6
  owns the `DocumentRenderer` work and the picker / popup /
  status-line elaboration. The host crate inherits today's picker
  as-is.
- **No reorganisation of picker sources.** `picker_sources.rs` moves
  to `lattice-host` whole. The "Files / Grep don't belong here"
  conversation (parked) stays parked.
- **No `lattice-render-common` crate** factoring shared composition
  logic across renderers. Premature; we have one renderer.
- **No `wgpu` + `parley` fallback path** to GPUI. The design doc names
  it as a fallback but Phase 5's primary thread is GPUI. Revisit only
  if GPUI churns under us.
- **No accessibility tree work** beyond stubbing the
  `Renderer::accessibility_tree` method. GPUI's accessibility story
  comes after parity.

## Open questions to resolve before 5.2

1. **Crate name.** `lattice-host` is descriptive but generic. `lattice-app` is shorter but less honest -- the crate isn't just "the app," it's the editor's renderer-agnostic logic substrate. Standing recommendation: `lattice-host`.

2. **`lattice-host` versus splitting host across several smaller crates.** Splitting now ( lattice-dispatch, lattice-picker-sources, lattice-mode-coordination, ...) is a bigger Phase-5.2 cost for cleaner long-term structure. One crate first; split later if any sub-piece grows independently. Standing recommendation: one crate.

3. **`tui_options.rs` rename.** Today it owns `:set ui.*` option specs even though the prefix `ui.` is renderer-neutral semantically. Renames to `ui_options.rs` (or stays as-is for diff hygiene during 5.2). Standing recommendation: rename on the move.

4. **Test scaffolding (`test_helpers.rs`).** Currently host + TUI tests share it. After 5.2 the host-side helpers move to `lattice-host` test infrastructure; the TUI-event-driven helpers (anything constructing `KeyEvent` directly) stay in lattice-ui-tui. Some helpers become duplicated (e.g., `app_with`) -- one per crate. Acceptable.

5. **Pane geometry types.** `PaneState` today reaches for `ratatui::layout::Rect` in some signatures. Audit during 5.2: replace `Rect` with `lattice_host::ui::Rect` (a renderer-neutral `x: u32, y: u32, width: u32, height: u32` struct). Renderers translate to/from their native rect types at paint time.

These get resolved on the 5.2 starting commit, not now.

## What ships with each non-trivial slice (per CLAUDE.md "Design Heuristics §5")

Code + docs + tests + graceful errors, not just code. For Phase 5:

- **Docs**: this file gets updated as the live ledger of what's landed (move from "planned" → "✓ shipped" per slice). Cross-references in design.md §5.6 + §11 as crate names become real.
- **Tests**: each module migration's existing tests must follow it. Cross-crate test coverage doesn't drop. The host's render-agnostic tests get richer (they can run without a `ratatui::Terminal`).
- **Benches**: no new bench targets in 5.2-5.6 (the work is structural). 5.7+ adds GPUI-specific paint benches against the existing TUI numbers.
- **Errors**: the renderer-trait surface gets `Result`-returning methods at the integration points (frame paint, layout) so renderer failures surface to the host rather than panic. Today's `runtime.rs` already handles crossterm errors at the boundary; we extend the discipline.
