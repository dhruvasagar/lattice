# Phase 5.B — App architecture decision

Anchors: [`phase-5-extraction.md`](phase-5-extraction.md), [`phase-5b-app-fields.md`](phase-5b-app-fields.md).

The field audit confirmed that ~99% of `App`'s ~200 fields are renderer-agnostic. The plan committed to Option D — a generic `App<R: Renderer>` struct in `lattice-host` parametrised over a host-side `Renderer` trait with two associated types. This doc records the architectural problem that surfaced *during* the 5.B.3 attempt to move the struct, and the pivot to Option E (composition) that resolved it.

## What broke Option D

Rust's orphan rule: **inherent `impl` blocks must live in the crate that defines the implementing type.** Once `App` moves to `lattice-host`, every `impl App<TuiRenderer>` in `lattice-ui-tui` is forbidden, because `App` is foreign to `lattice-ui-tui`. The `<TuiRenderer>` specialisation doesn't help — the implementing *type* is `App`, and that's what the orphan rule checks.

That breaks the plan in [`phase-5b-app-fields.md`](phase-5b-app-fields.md) "Open question 2": the answer there — "leave [TUI-specific methods] in lattice-ui-tui as `impl App` (where `App = host::App<TuiRenderer>`)" — is not legal Rust. The type alias does not change the orphan-rule answer.

The reachable workarounds inside Option D are:

1.	**Mega-commit**: move `App` and *every* `impl App` block across ~25 files in `lattice-ui-tui/src/app/*.rs` in a single commit (~35k LoC of code motion). Within that commit, each block becomes `impl<R: Renderer> App<R>`, and the handful of methods that touch `theme` / `pane_render_registry` concretely either (a) move to `impl App<TuiRenderer>` *in lattice-host*, which is legal but forces lattice-host to know about `TuiRenderer` (a layering violation — host must not depend on its renderer), or (b) get rewritten as extension-trait impls in lattice-ui-tui.
2.	**Extension-trait pattern**: same as (1)(b), used everywhere a TUI-specific method exists. Every call site needs `use TuiAppExt;` (or one of several traits, depending on how we slice). Lots of imports, especially across tests.

Neither is awful, but neither is graceful. (1) sacrifices the "every commit green" discipline that has held throughout the Phase 5 migration. (2) imposes lifelong ergonomic cost on call sites for a small number of TUI methods.

## What Option F (`trait App`) would look like

A trait can mean two distinct things in this context:

- **Pure interface trait + duplicated state.** Each renderer's app struct holds its own copy of the ~198 renderer-agnostic fields, and the trait declares the common behaviour. The compiler enforces the contract, but the data duplicates between renderers. As the editor evolves, the two App structs drift unless we paper over it with macros, which restore the old problem in a different form.
- **Interface trait + shared state struct.** A `trait Editor` declares methods; concrete impl structs hold an `EditorCore` for shared state plus their renderer-specific fields. This is *Option E with an extra trait layer*. The trait only earns its keep if some host-level code needs to be generic over "any renderer's editor" — but nothing in the codebase has that shape. Host-level code takes `&mut EditorCore` directly.

In both readings, the trait doesn't pay for itself. Option F is not chosen.

## Option E: composition

```rust
// lattice-host:
pub struct Editor {
	pub cursor: Position,
	pub mode: ModalState,
	pub buffer_registry: BufferRegistry,
	// ... ~198 renderer-agnostic fields, taken from the audit's HOST clusters
}

impl Editor {
	pub fn dispatch(&mut self, action: Action) { ... }
	pub fn set_message(&mut self, msg: EchoMessage) { ... }
	// ... ~99% of today's `impl App` methods
}

// lattice-ui-tui:
pub struct App {
	pub editor: Editor,
	pub theme: TuiTheme,
	pub pane_render_registry: TuiPaneRenderRegistry,
}

impl App {
	pub fn new(...) -> Self { ... }
	pub fn sync_theme_from_config(&mut self) { ... }
	// ... small set of methods that touch theme / pane_render_registry
}
```

`Editor` is the renderer-agnostic editor state. `App` is a thin renderer-specific wrapper composing `Editor` with the renderer's caches. GPUI's analogue:

```rust
// future lattice-ui-gpui:
pub struct App {
	pub editor: Editor,
	pub theme: GpuiTheme,
	pub pane_render_registry: GpuiPaneRenderRegistry,
}
```

Same `Editor`, different wrapper. No code duplication; no generics; no orphan rule.

### Evaluation against the paramount goals

1.	**Performance.** Direct field access through `self.editor.foo` is zero-cost — the compiler flattens the access pattern to a single pointer add. Identical to Option D's monomorphised generics. No virtual dispatch.

2.	**Extensibility.** Adding a renderer = "define an App struct composing `Editor` + your renderer-specific fields, implement the rendering crate's trait." No constraints on the renderer's App shape beyond holding an `Editor`. Plugin host code (Phase 6+) talks to `Editor` directly — never to the renderer wrapper.

3.	**Vim modal editing.** Unaffected; the modal engine and grammar live on `Editor`.

4.	**Asynchronicity.** Unaffected; async actors hold `&mut Editor` (renderer-agnostic) or are owned by it as today.

### Evaluation against the design heuristics

1.	*Best long-term fit beats easy implementation.* Option D is "fancier" in type-system terms; Option E is more boring Rust. Both are zero-cost. The orphan rule is a structural feature of the language — fighting it with extension traits is paying ongoing tax for a one-time architectural choice. Composition is the long-term-fit answer.

2.	*Evaluate against paramount goals, not other editors.* Both options serve the goals equivalently. The deciding factor is structural cleanliness, not feature parity.

3.	*Treat user-suggested options as input, not the menu.* The user proposed F (trait); the answer is E (composition), with the reasoning above.

4.	*Confirm the plan before non-trivial work.* This doc is that confirmation. Code change waits.

5.	*Non-trivial design changes ship four artefacts together.* For 5.B.3 onwards: docs (this file + ledger updates), tests (existing test suite covers; per-cluster commits keep coverage), benches (no perf impact — composition is zero-cost — so no new bench targets), error handling (unchanged from today). 

### What composition costs

- **Field access prefix.** Code that today writes `app.cursor = pos` becomes `app.editor.cursor = pos`. Inside `impl Editor` methods (which is where most of today's `impl App` methods migrate to), it's still `self.cursor = pos` — no prefix needed. The prefix appears only at the *outer boundary*: where the TUI runtime calls into editor state from outside an `impl Editor` method. The audit notes those call sites are bounded to `render.rs`, `runtime.rs`, the keymap catalog dispatchers, and a few `impl App` methods that genuinely need both `editor` and `theme` in scope.
- **Two struct names.** `Editor` (state) vs `App` (renderer wrapper). The naming is honest: `Editor` is the editor; `App` is the renderer-specific composed thing.
- **No `Deref` impl.** Resisting the temptation to add `impl Deref<Target = Editor> for App` keeps method resolution unambiguous. Worth the small ergonomic cost.

## Migration plan

The migration is intrinsically per-cluster: each commit moves a coherent slice of fields from `App` to `Editor`, plus the methods that touch only those fields. Methods that touch fields not yet migrated stay on `App` and use `self.editor.foo` for the migrated subset.

Field clusters from the audit, ordered by migration friendliness:

1.	**Document + active-pane state** (cursor, scroll, document, buffers, pane_tree, viewport, ...).
2.	**Modal + dispatch** (modal, partial_chord, registry, builtins, action_ids, keymap, ...).
3.	**Cmdline + echo** (command_line, last_message, messages, command_history, ...).
4.	**Syntax** (lang_registry, syntax, pending_syntax_edits, visible_highlights, ...).
5.	**Search + vim state** (search_line, last_search, marks, registers, position_history, ...).
6.	**Config + modes** (config, option_cache, mode_registry, services, ...).
7.	**Popup + help** (popup_buffer, popup_back_stack, prev_pane_for_help, ...).
8.	**Completion** (completion_registry, completion_state, insert_completion, snippets, ...).
9.	**Picker** (picker, picker_registry, picker_mru, ...).
10.	**LSP request channels** (~40 pending_*_rx/_token fields).
11.	**LSP per-buffer caches** (~12 lsp_*_cache fields).
12.	**LSP subsystem handles + watchers** (lsp, lsp_file_watcher, lsp_diagnostics, ...).

Each cluster gets its own commit. Method moves accompany field moves: methods that only touch migrated fields move to `impl Editor` in lattice-host; methods that touch both migrated and unmigrated fields stay on `impl App` and access migrated fields via `self.editor`.

Halfway through, App looks like:

```rust
pub struct App {
	pub editor: Editor,           // grows over migration
	pub theme: TuiTheme,
	pub pane_render_registry: TuiPaneRenderRegistry,
	// fields that haven't migrated yet, shrinking
}
```

The endpoint: `App` holds `editor` + `theme` + `pane_render_registry` only. Every other field has migrated to `Editor`. `impl App`'s surface is small and TUI-specific.

### What ships before the per-cluster work begins

**5.B.3 (revised — small commit, ships green, no semantic change):**

1.	Revert the 5.B.2 `App<R: Renderer>` parametrisation: change `pub struct App<R: lattice_host::Renderer = crate::TuiRenderer>` back to `pub struct App`. Restore `theme: crate::theme::Theme` and `pane_render_registry: crate::pane_render::PaneRenderRegistry` to concrete types.
2.	Define `lattice_host::editor::Editor` as an empty pub struct (the destination for the migration).
3.	Add `pub editor: lattice_host::editor::Editor` field to `App`. Initialise to `Editor::default()` (derive `Default`).
4.	Decide on the fate of `lattice_host::Renderer` + `TuiRenderer` + `MinimalRenderer`. **Recommendation: keep them.** They became unused-by-`App` but are still potentially useful for Phase 5.6's `lattice-render::Renderer` trait, where they may legitimately appear as marker bounds or trait-object segregators. Keeping them is harmless; if Phase 5.6 doesn't end up needing them, we delete then.

After 5.B.3, all subsequent commits are field-cluster migrations (5.B.4, 5.B.5, …) until App's surface is reduced to the renderer-specific shape.

### Why composition unblocks the "every commit green" discipline

Under Option D's mega-commit, intermediate states are not buildable: `impl App` blocks in unmoved files can't see the new `App<R>` signature; `impl<R: Renderer> App<R>` blocks in moved files can't be referenced from unmoved code that still says `App`. The only legal landing is "everything at once."

Under Option E, intermediate states *are* buildable:
- A field moved from `App` to `Editor` is accessed via `self.editor.foo` at the call site (whether that call site is in `impl App` or anywhere else).
- A method moved from `impl App` to `impl Editor` is callable as `app.editor.foo(...)` from callers; if the caller is itself in `impl Editor`, it's just `self.foo(...)`.
- The host-side methods on `Editor` work directly; the renderer-side wrapper code that needs both `editor` and `theme` borrows them separately at the same scope.

Each commit can:
- Move N fields from `App` to `Editor`,
- Move M methods from `impl App` to `impl Editor` (where the method's body references no unmoved fields and no renderer-specific fields),
- Rewrite call sites for the moved fields from `self.foo` → `self.editor.foo` *within* methods that stay on `impl App` and still need those fields,
- Leave everything else untouched,

and the workspace continues to build and test.

This is the property that broke Option D and that Option E restores.

## Decision

Pivot from Option D to Option E. Restart 5.B.3 with the smaller scope above. The remaining slices (5.B.4 onward) become per-cluster migrations. The end state is the same one the audit anticipated — host owns the editor's logic; renderer owns its rendering — but reached through composition rather than generic parametrisation.
