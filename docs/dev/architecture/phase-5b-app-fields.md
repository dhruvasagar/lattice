# Phase 5.B.0 — App field audit

Anchor: [`phase-5-extraction.md`](phase-5-extraction.md). The plan committed to Option D (generic `App<R: Renderer>` with associated types). This doc is the field-level inventory that *confirms* what `R` needs to expose.

**TL;DR.** App has ~200 fields. **Two** are renderer-specific. Every other field is pure data, lattice-host-natural, or pulls types from already-host-side crates (lattice-core, lattice-grammar, lattice-protocol, lattice-lsp, lattice-runtime, lattice-syntax, lattice-mode, lattice-config, lattice-picker, lattice-completion, lattice-snippet, lattice-help). The Renderer trait needs just two associated types:

```rust
pub trait Renderer: 'static + Send + Sync {
    type Theme: 'static + Send + Sync;
    type PaneRenderRegistry: 'static + Send + Sync;
}
```

That's the whole surface from App's perspective. (Frame / Input / etc. live on `lattice-render::Renderer`, a separate trait Phase 5.6 introduces. The App-host trait does not need them.)

## Methodology

Read every field's declaration + docstring + ambient context in `lattice-ui-tui/src/app.rs` (the post-LSP-cache-extraction state, ~3,000 LoC). Classified by what the field's type touches:

- **HOST** — types resolvable in `lattice-host`'s current dep graph (lattice-core, lattice-grammar, lattice-protocol, lattice-lsp, lattice-runtime, lattice-syntax, lattice-mode, lattice-config, lattice-picker, lattice-completion, lattice-snippet, lattice-help) or std/tokio/arc-swap/toml.
- **RENDERER** — types defined in `lattice-ui-tui` itself that name ratatui or crossterm in their definition. These need to come from the renderer.
- **UNCERTAIN** — flagged for closer inspection. (None survived second pass.)

## RENDERER fields (2 total)

| Field | Type | Needs |
|---|---|---|
| `theme` | `crate::theme::Theme` (ratatui `Style`/`Color` throughout) | `R::Theme` |
| `pane_render_registry` | `crate::pane_render::PaneRenderRegistry` (stores fn-ptrs `fn(&mut ratatui::Frame, Rect, &App, ...)`) | `R::PaneRenderRegistry` |

That's the entire renderer-side surface on App. Every other field is HOST.

## HOST fields (the rest, by cluster)

Listing by responsibility group rather than alphabetically — easier to reason about coherence.

### Document + active-pane state
`document: DocumentHandle` · `snapshot_cache: SnapshotCache` · `document_buffer_id: BufferId` · `buffers: BufferRegistry` · `active_buffer: BufferKind` · `pane_tree: PaneTree` · `cursor: Position` · `scroll: u32` · `should_quit: bool` · `viewport_height: u32` · `terminal_width: Option<u16>` (logical columns, not pixels — host-shaped)

### Modal + dispatch
`modal: ModalState` · `partial_chord: Vec<KeyChord>` · `registry: Arc<CommandRegistry>` · `event_bus: Arc<EventBus>` · `builtins: Builtins` · `action_ids: ActionIds` · `keymap: KeymapHandle` · `completion_popup_layer` · `snippet_layer`

### Cmdline + echo
`command_line: String` · `last_message: Option<EchoMessage>` · `messages: Arc<Mutex<MessagesRing>>` · `pending_message_event_rx` · `pending_redraw: bool` · `command_history` · `command_history_cursor` · `command_history_pending` · `auto_submit_after_chord: bool`

### Syntax
`lang_registry: Arc<LangRegistry>` · `syntax: Option<SyntaxHandle>` · `last_parsed_text_version: u64` · `pending_syntax_edits` · `last_synced_syntax_version: u64` · `visible_highlights: Vec<Vec<StyledSpan>>` · `visible_highlights_key` · `pane_highlights: HashMap<usize, Vec<Vec<StyledSpan>>>`

### Search + vim state
`search_line` · `last_search` · `current_match` · `all_matches` · `substitute_preview` · `unnamed_register` · `pending_count` · `op_count` · `visual_anchor` · `last_change` · `last_visual` · `marks` · `replace_history` · `registers` · `pending_register` · `position_history` · `position_history_cursor` · `recent_files` · `tag_stack` · `pending_tag_origin` · `macros` · `macro_recording` · `last_played_macro` · `last_find` · `folds: Vec<Fold>` · `last_insert` · `pending_block_insert` · `recording_insert`

### Config + modes
`config: Arc<ConfigRegistry>` · `option_cache: OptionCache` · `mode_registry: Arc<ModeRegistry>` · `services: Arc<ServiceRegistry>` · `mode_guards: GuardStoreHandle` · `active_modes: HashMap<BufferId, ActiveModes>` · `buffer_locals` · `resolved_options` · `buffer_local_overrides` · `option_change_rx` · `help_topics: Arc<HelpTopicRegistry>` · `host_theme: lattice_host::ui::theme::Theme` (canonical neutral theme)

### Popup + help
`popup_buffer: Option<BufferId>` · `popup_back_stack: Vec<PopupSnapshot>` · `prev_pane_for_help: Option<PrevPaneState>` · `popup_placement: PopupPlacement` (rect, not ratatui)

### Completion (cmdline + insert)
`completion_registry: CompletionRegistry` · `completion_state: Option<CompletionState>` · `insert_completion: Option<InsertCompletionState>` · `pending_insert_completion_lsp_rx/_token` · `pending_completion_resolve_rx/_token` · `snippet_registry` · `insert_completion_snippet_meta` · `completion_accept_freq` · `pending_config_structural_sections` · `per_language_completion` · `completion_in_path_context: bool` · `active_snippet` · `snippet_dirs`

### Picker
`picker: Option<Picker>` · `picker_registry: Arc<PickerRegistry>` · `picker_mru` · `picker_mru_path` · `pending_picker_init` · `live_picker_query` · `previewing: bool`

### LSP request channels
`pending_hover_rx/_token` · `pending_definition_rx/_token` · `pending_nav_kind` · `pending_references_rx/_token` · `pending_symbols_rx/_token` · `pending_format_rx/_token` · `pending_signature_help_rx/_token` · `pending_completion_rx/_token/_items` · `pending_moniker_rx` · `pending_rename_rx/_token` · `pending_code_action_rx/_token/_items/_handle` · `pending_selection_range_rx/_token` · `pending_document_highlight_rx/_token` · `pending_folding_range_rx/_token` · `pending_document_links_rx/_token` · `pending_code_lens_rx/_token/_refresh_rx/_items/_server` · `pending_document_color_rx/_token/_color_presentations/_color_range` · `pending_inlay_hint_rx/_token` · `pending_semantic_tokens_rx/_token` · `pending_pull_diagnostics_rx/_token` · `pending_diagnostic_refresh_rx` · `pending_inlay_hint_refresh_rx` · `pending_semantic_tokens_refresh_rx` · `pending_lsp_detach_rx` · `pending_mode_lifecycle_rx`

### LSP per-buffer caches
`lsp_progress` · `lsp_selection_chain` · `lsp_selection_chain_index` · `lsp_document_highlights` · `last_document_highlight_issue_cursor` · `lsp_folds_cache` · `lsp_inlay_hints_cache` · `lsp_document_links_cache` · `lsp_code_lens_cache` · `lsp_document_color_cache` · `lsp_semantic_tokens_cache` · `lsp_pull_diagnostics_cache`

### LSP subsystem handles + watchers
`lsp: LspSupervisorHandle` · `lsp_file_watcher` · `lsp_diagnostics: DiagnosticsLayer` · `lsp_logger: LspLogger` · `lsp_log_event_rx` · `lsp_progress_event_rx` · `pending_apply_edit_rx` · `pending_configuration_rx` · `pending_show_document_rx` · `pending_show_message_request_rx` · `lsp_pending_show_message_requests` · `lsp_show_message_request_queue` · `lsp_next_show_message_request_id` · `lsp_config_tree: toml::Table` · `buffer_uris: HashMap<BufferId, lattice_lsp::Uri>`

## Implications

The split is dramatically cleaner than I expected before doing the audit. **The host can own ~99% of the App struct without any abstraction over the renderer's types.** The `R::Theme` and `R::PaneRenderRegistry` indirections cover everything that's actually renderer-shaped.

This means:

- The Renderer trait's surface is small enough to fix up-front without lock-in fear. Two associated types, both 'static + Send + Sync. Done.
- The migration's textual churn is bounded: every `impl App` block gains `<R: Renderer>`, every free function taking `&App` or `&mut App` gains `<R: Renderer>` and `App<R>`. Methods INSIDE impl blocks don't need `<R>` in their signatures unless they touch `R::Theme` or `R::PaneRenderRegistry`.
- Generic propagation through tests is real but local — `app_with(...)` becomes `app_with::<TestRenderer>(...)` or returns `App<TestRenderer>` via type alias.
- No code today reads or writes a renderer-typed field from outside `lattice-ui-tui`. The TUI's `sync_theme_from_config` writes `theme`; `render.rs` reads it. That's it. So the renderer-coupled call sites are all in lattice-ui-tui.

## The Renderer trait

```rust
// lattice-host/src/renderer.rs (new)
pub trait Renderer: 'static + Send + Sync {
    /// Renderer-specific cached theme. Adapted from the host's
    /// canonical `lattice_host::ui::theme::Theme` via the
    /// renderer's `From<&host::Theme>` impl. Rebuilt every time
    /// the host's option cascade writes `App.host_theme`.
    type Theme: 'static + Send + Sync;

    /// Renderer-specific per-mode pane render dispatch table.
    /// The TUI's PaneRenderRegistry stores
    /// `fn(&mut ratatui::Frame, Rect, &App<TuiRenderer>, ...)`;
    /// GPUI's stores its analogous function pointers / trait
    /// objects with its native context.
    type PaneRenderRegistry: 'static + Send + Sync;
}
```

That's the full surface. We can revisit if a future need emerges, but the audit doesn't surface any beyond these two.

## Concrete renderer impl shape

```rust
// lattice-ui-tui/src/lib.rs
pub struct TuiRenderer;

impl lattice_host::Renderer for TuiRenderer {
    type Theme = TuiTheme;
    type PaneRenderRegistry = TuiPaneRenderRegistry;
}

pub type App = lattice_host::App<TuiRenderer>;
```

```rust
// future lattice-ui-gpui/src/lib.rs
pub struct GpuiRenderer;

impl lattice_host::Renderer for GpuiRenderer {
    type Theme = GpuiTheme;
    type PaneRenderRegistry = GpuiPaneRenderRegistry;
}

pub type App = lattice_host::App<GpuiRenderer>;
```

## Open questions answered

1. **What associated types does `Renderer` need?** Just `Theme` + `PaneRenderRegistry`. Audit confirmed.
2. **Do any methods need `<R>` in their signatures (vs just being inside `impl<R> App<R>`)?** Most don't. The two exceptions are methods that touch `theme` or `pane_render_registry` directly. Practically these methods are TUI-side (the `sync_theme_from_config` cascade adapter; pane-render registration at boot). They migrate to lattice-host but become callable only when `R = TuiRenderer` because they construct `TuiTheme` values directly. Solution: leave them in lattice-ui-tui as `impl App` (where `App = host::App<TuiRenderer>`) — the type alias path lets them define methods on the concrete instantiation without ambiguity. GPUI provides analogous methods on its own concrete `App = host::App<GpuiRenderer>`.
3. **Tests?** Define a `MinimalRenderer` ZST in lattice-host (`impl Renderer for MinimalRenderer { type Theme = (); type PaneRenderRegistry = (); }`) for headless host-side tests. lattice-ui-tui's tests keep using `App<TuiRenderer>` via the existing `app_with(...)` helper.

## Migration plan refinement

With the audit's clarity, the slice plan in `phase-5-extraction.md` simplifies:

**5.B.1 — Define `Renderer` trait in lattice-host.** New file `lattice-host/src/renderer.rs` with the trait + a `MinimalRenderer` for tests. lattice-host doesn't yet depend on this — App still lives in lattice-ui-tui. ~half day.

**5.B.2 — Parametrize `lattice-ui-tui::App` in place.** Change `pub struct App { ..., theme: TuiTheme, pane_render_registry: PaneRenderRegistry, ... }` to `pub struct App<R: Renderer = TuiRenderer> { ..., theme: R::Theme, pane_render_registry: R::PaneRenderRegistry, ... }`. Define `TuiRenderer` and impl `Renderer for TuiRenderer { type Theme = TuiTheme; ... }`. Every existing reference to `App` continues to mean `App<TuiRenderer>` via the default. Every existing `impl App` becomes `impl<R: Renderer> App<R>` (the existing field-reads still compile because most methods don't touch `theme` or `pane_render_registry`). The few methods that DO touch those fields stay in lattice-ui-tui and use `impl App` (the concrete `App<TuiRenderer>` alias). ~1-2 days, chasing impl-block fixups.

**5.B.3 — Move `App` struct definition to lattice-host.** Once parametrized in place, the struct itself moves. `lattice-ui-tui::App` becomes `pub type App = lattice_host::App<TuiRenderer>;`. ~half day; mechanical.

**5.B.4 — Move impl blocks file-by-file.** Each `app/*.rs` impl block moves to `lattice-host/src/app/*.rs` as `impl<R: Renderer> App<R>`. Files where every method is renderer-agnostic move whole; the few that aren't stay in lattice-ui-tui. The audit indicates almost everything moves. ~1-2 weeks total, one or two files per commit.

**5.B.5 — Test helpers + picker_sources cleanup.** `app/test_helpers.rs` splits between host (renderer-neutral helpers) and TUI (helpers that need `crossterm::KeyEvent`). `picker_sources.rs` migrates with the host helpers; its tests already use `app_with(...)` which keeps working via the type alias.

The earlier estimate of 3-4 weeks for the keystone is unchanged. The audit confirms the work is feasible without architectural surprises.
