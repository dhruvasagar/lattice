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

### Hard Case §2 -- `pane_render.rs` and per-mode render fn-pointers

```rust
pub type PaneRenderFn = fn(&mut Frame, Rect, &App, &DocumentSnapshot, &PaneState, bool, usize);
pub type PaneStatusFn = fn(&App, &PaneState) -> String;
```

`App` holds `pane_render_registry: PaneRenderRegistry` (owns these fn pointers). `render.rs::build_pane_render_registry()` (render.rs:1669) wires providers for help, file-tree, oil, etc. The architecture (registry of per-mode render providers) is the right shape -- the leak is the typed signature.

**Move:**

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

**Cost:** trait introduction, one host module gains the trait, each renderer ships its own impls. The dispatch table that already exists is largely unchanged in shape.

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

| Slice | Commit | What landed |
|---|---|---|
| 5.0 | `2d46bf2` | Extraction audit doc |
| 5.1 | `8f12c6a` | Empty `lattice-host` crate |
| 5.2 wave 1 | `8a5b256` | Trivial re-export shims (buffers, file_tree, oil, popup, help, help_topics) |
| 5.2 wave 2 | `7286312` | actions + excommand (~2.6k LoC) |
| 5.2 wave 3 | `2454530` | host_generators (~200 LoC) |
| 5.3 | `d024635` | Renderer-neutral theme types + `App.host_theme` |
| 5.4 | `6657cbb` | chord.rs split (neutral types to host, crossterm adapter stays) |
| 5.2 wave 4 | `f78c64d` | keymap leaves (keymap.rs + keymap_trie.rs + keymap_registry.rs) |
| 5.2 | `d3ec49c` | Action enum + EchoLevel + EchoMessage + FindKind extracted |
| 5.2 | `f32c677` | Catalogs reclassified MIXED; reverted move |
| 5.2 | `950a728` | `Fold` moved to `lattice-core::folding` |
| 5.2 | `a0cda39` | folds.rs + modes.rs migrated |
| 5.2 | `00e5f8a` | buffer_registry.rs migrated |
| 5.2 | `5753e38` | pane shim migrated |
| 5.2 | `93fc8d8` | App helper state types extracted (SearchLine, LastSearch, UnnamedRegister, PrevPaneState) |

**Total moved from `lattice-ui-tui`:** ~17k LoC. **Test count:** 1599 throughout (1424 ui-tui + 175 host at session end).

## Remaining work

The slices that haven't landed gate on **the App keystone migration**. App lives in `lattice-ui-tui/src/app.rs` (~5500 LoC after extractions) and is referenced by:

- Every `app/*.rs` submodule (~35k LoC of method impls)
- `picker_sources.rs` (tests use `app_with`)
- `render.rs`, `runtime.rs` (TUI-coupled but read App fields)
- The keymap catalog files (import `crate::app::Action` -- still resolves via lattice-host re-export)
- The pane render registry (Hard Case §2 -- references `&App`)

Moving App needs its own focused session. Sketch of approach:

1. Define `lattice_host::app::App` as a new struct with the full field set
2. All `impl App` blocks in `app/*.rs` either: move to lattice-host (renderer-agnostic methods) OR stay in lattice-ui-tui (renderer-coupled methods that touch ratatui/crossterm directly via `app.theme`, etc.)
3. `lattice-ui-tui::app` becomes a re-export hub: `pub use lattice_host::app::*;`
4. Test infrastructure (`app_with`, etc.) moves with the helpers it tests

This is several days of work and want to be done with the user in the loop on architectural calls (e.g., where to draw the line between renderer-agnostic and renderer-coupled methods on App).

## Slice ordering

Each slice ships green; lattice-ui-tui keeps working at every commit.

**5.0 -- this document.** No code. ✓

**5.1 -- create `lattice-host` shell.** New empty crate. Workspace dep entry. lattice-ui-tui depends on it (currently nothing flows through). Builds + tests green. ~30 minutes.

**5.2 -- move pure HOST modules (no theme touch).** Migrate every module classified HOST in the table above that doesn't transitively depend on `theme.rs` or `chord.rs`. The list is most of `app/` (lifecycle, lsp, dispatch, edit, motions, options, folds, search, completion, picker, mode, popup, cmdline, visual, macros, messages, file_tree, oil, syntax, display, state, operators, highlights, lsp_log_buffers, lsp_watcher), plus the top-level keymap*.rs, buffer_registry.rs, buffers.rs, modes.rs, folds.rs, picker_sources.rs, host_generators.rs, excommand.rs, actions.rs, tui_options.rs (renames to ui_options.rs). lattice-ui-tui re-exports the moved types via `pub use lattice_host::*;` so consumers (lattice-cli) don't break. **This is the bulk of the work** -- estimate 1-2 weeks of careful migration, one or two modules per commit, each landing green. May surface accidental couplings (e.g., a HOST module reaching into `crate::theme` to read a field it shouldn't); those resolve case by case.

**5.3 -- renderer-neutral theme.** Hard Case §1. Define `lattice_host::ui::Theme` + adapter `lattice_ui_tui::tui_theme::TuiTheme`. Change `App.theme` type. `sync_theme_from_config` becomes neutral on the host side; TUI re-runs the adapter on every change. Tests: the existing theme tests run against the neutral form; a small TUI-side test pins the adaptation. ~2-3 days.

**5.4 -- split `chord.rs` and `input.rs`.** Hard Case §3. Neutral `KeyChord` + dispatch logic to host; crossterm adapters stay in TUI. Tests rewrite from `KeyEvent` construction to `KeyChord` construction for the host-side dispatch tests; the existing `KeyEvent`-based tests stay in lattice-ui-tui for the adapter. ~2-3 days.

**5.5 -- pane render registry abstraction.** Hard Case §2. Trait-objected `PaneRenderer` + `PaneStatus` in host. TUI registers ratatui-shaped impls. The dispatch table doesn't change in shape. ~1-2 days.

**5.6 -- define `lattice-render` trait surface.** §5.6.1 sketch becomes real type definitions: `Frame`, `InputEvent`, `LayoutConstraints`, `LayoutResult`, `Renderer` trait. lattice-ui-tui implements `Renderer` for its existing render path. `lattice-host::run(app, renderer: impl Renderer)` becomes the new entry point. `runtime.rs` becomes the function that constructs `ratatui::Terminal` and passes the TUI's `impl Renderer` to host's `run`. ~1 week.

**5.7 -- `lattice-cli` gains `--tui`.** Plumbed but tautological for now (default still picks TUI; no other renderer exists). Sets up the dispatch shape Phase 5.8+ will use. ~few hours.

**5.8 -- `lattice-ui-gpui` scaffold.** New crate. Window opens. Hello-world rect. No editor content. ~1 day to learn the GPUI surface; ~1 week for the actual scaffold.

**5.9+ -- real GPUI: text, atlas, panes, input, popups.** This is Phase 5 proper in the design doc's roadmap-week-12 sense. Multiple weeks. Splits into more slices as it goes.

**5.last (separately scheduled) -- flip default to GPUI.** Only after GPUI parity. `--tui` remains as the explicit opt-in. Don't flip on day one of GPUI parity; give it a release cycle as opt-in (`--gpu`) before flipping default. This protects the user's existing TUI workflow.

## What we are not doing in Phase 5

- **No new picker primitives** beyond what already exists. Phase 6 owns the `DocumentRenderer` work and the picker / popup / status-line elaboration. The host crate inherits today's picker as-is.
- **No reorganisation of picker sources.** `picker_sources.rs` moves to `lattice-host` whole. The "Files / Grep don't belong here" conversation (parked) stays parked.
- **No `lattice-render-common` crate** factoring shared composition logic across renderers. Premature; we have one renderer.
- **No `wgpu` + `parley` fallback path** to GPUI. The design doc names it as a fallback but Phase 5's primary thread is GPUI. Revisit only if GPUI churns under us.
- **No accessibility tree work** beyond stubbing the `Renderer::accessibility_tree` method. GPUI's accessibility story comes after parity.

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
