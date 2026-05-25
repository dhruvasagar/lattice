//! `RenderState` — the renderer's read contract with the host.
//!
//! Phase 5.8.AF.5 / Slice 3a.
//!
//! ## Why this exists
//!
//! Per paramount goal #4 (CLAUDE.md):
//!
//! > Nothing blocks the UI — enforced architecturally, not by
//! > discipline.
//!
//! Renderers must not read `Editor` fields directly during
//! render — that read path needs the same `&Editor` reference
//! the dispatcher holds for mutation, which couples render
//! latency to whatever happens to be mutating `Editor` at the
//! moment. `RenderState` is the wait-free read seam: dispatch
//! publishes a fresh snapshot into an `ArcSwap<RenderState>`
//! at the end of every tick; the renderer loads it once per
//! frame and reads everything it needs from there.
//!
//! This is the substrate for two follow-on slices:
//!
//! - **Slice 3b** moves every drain in `run_tick_pending` into
//!   per-subsystem background tasks. Each task writes through
//!   the same publication path; the renderer never sees a
//!   half-built mutation.
//! - **Slice 3c** moves `Editor` to its own thread. Channels
//!   replace `&mut Editor` references; the renderer becomes a
//!   pure `RenderState` reader.
//!
//! Both slices preserve the read contract this slice
//! establishes — `RenderState` doesn't change shape, just who
//! produces it and on which thread.
//!
//! ## Per-subsystem sub-states
//!
//! `RenderState` is split into 11 sub-state structs, one per
//! UI-visible subsystem. Each is `Arc`-wrapped so a subsystem
//! whose backing state didn't change between publications can
//! share its sub-state `Arc` across frames (identity-preserved).
//! In Slice 3b, subsystem background tasks publish their own
//! sub-state directly without re-snapshotting unrelated
//! domains.
//!
//! The active-buffer hot-path state (cursor, scroll, viewport,
//! modal, visual selection, snapshot pointer) lives in its own
//! [`ActiveDocumentRenderState`] — separate from the buffer registry
//! ([`BuffersRenderState`]) because the read frequencies differ
//! by orders of magnitude: the active-buffer state churns on
//! every motion/edit (per-frame critical), the registry churns
//! only on `:b` / `:e` / `:bd`. Splitting lets Slice 3b
//! republish them on independent cadences.
//!
//! For Slice 3a only `DiagnosticsRenderState` carries real
//! data — that's the proof-of-life migration path. The other
//! sub-states are deliberately empty placeholders so the shape
//! is in place when their backing fields migrate in later
//! slices.

use std::sync::Arc;

/// The renderer's read contract with the host. Built fresh by
/// [`crate::dispatch::Editor::build_render_state`] at the end of
/// every dispatch tick and stored into the editor's
/// `ArcSwap<RenderState>`. Renderers load with
/// `editor.render_state.load_full()` once per frame.
#[derive(Debug, Clone)]
pub struct RenderState {
    pub active_document: Arc<ActiveDocumentRenderState>,
    pub buffers: Arc<BuffersRenderState>,
    pub panes: Arc<PanesRenderState>,
    pub lsp: Arc<LspRenderState>,
    pub syntax: Arc<SyntaxRenderState>,
    pub picker: Arc<PickerRenderState>,
    pub completion: Arc<CompletionRenderState>,
    pub popup: Arc<PopupRenderState>,
    pub messages: Arc<MessagesRenderState>,
    pub modeline: Arc<ModelineRenderState>,
    /// Slice 3c.final.B.10: typed-options registry published as a
    /// wait-free Arc clone so the renderer's
    /// `picker_display_is_minibuffer` (and any future per-frame
    /// typed-option read) doesn't take an actor round-trip.
    pub options: Arc<OptionsRenderState>,
    /// Slice 3c.final.B.11: active modes per buffer, published as
    /// `Arc<HashMap<BufferId, Arc<ActiveModes>>>` so per-buffer
    /// reads in the modeline + future hot paths are wait-free.
    pub modes: Arc<ModesRenderState>,
    /// Slice 3c.final.B.9: buffer-locals per buffer, published as
    /// `Arc<HashMap<BufferId, Arc<BufferLocals>>>` so the
    /// modeline / help-render / file-tree / oil paint paths read
    /// without an actor round-trip.
    pub buffer_locals: Arc<BufferLocalsRenderState>,
    pub diagnostics: Arc<DiagnosticsRenderState>,
    /// Issue #29 (2026-05-22): tab pages snapshot. Per-tab
    /// labels + active idx + the resolved `show`-decision so
    /// both peers paint the tabline from the same source.
    pub tabs: Arc<TabsRenderState>,
    /// Phase 5.8.AF.5 / Slice 3c.final.B (group 5): translator
    /// inputs — published so the renderer's input loop can build
    /// a `TranslateContext` from owned snapshots instead of
    /// `&'a` borrows that tie it to `Editor`'s lifetime.
    pub translator: Arc<TranslatorRenderState>,
    /// Phase 5.8.AF.5 / Slice 3c.final.B (group 6): renderer
    /// lifecycle flags (should_quit, pending_redraw,
    /// terminal_width). Carries the per-tick "renderer should
    /// notice this" signals that the main loop reads before
    /// composing the next frame.
    pub lifecycle: Arc<LifecycleRenderState>,
    /// Phase 5.8.AF.5 / Slice 3c.final.B (group 6): resolved
    /// host theme. `Theme` is `Copy` (every field is `Copy`),
    /// so the publish is a plain struct move — no `Arc`
    /// indirection needed.
    pub theme: crate::ui::theme::Theme,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            active_document: Arc::new(ActiveDocumentRenderState::default()),
            buffers: Arc::new(BuffersRenderState::default()),
            panes: Arc::new(PanesRenderState::default()),
            lsp: Arc::new(LspRenderState::default()),
            syntax: Arc::new(SyntaxRenderState::default()),
            picker: Arc::new(PickerRenderState::default()),
            completion: Arc::new(CompletionRenderState::default()),
            popup: Arc::new(PopupRenderState::default()),
            messages: Arc::new(MessagesRenderState::default()),
            modeline: Arc::new(ModelineRenderState::default()),
            options: Arc::new(OptionsRenderState::default()),
            modes: Arc::new(ModesRenderState::default()),
            buffer_locals: Arc::new(BufferLocalsRenderState::default()),
            diagnostics: Arc::new(DiagnosticsRenderState::default()),
            tabs: Arc::new(TabsRenderState::default()),
            translator: Arc::new(TranslatorRenderState::default()),
            lifecycle: Arc::new(LifecycleRenderState::default()),
            theme: crate::ui::theme::Theme::default(),
        }
    }
}

/// Active buffer's hot-path render-side projection.
///
/// Carries everything the renderer needs to draw the currently-
/// active buffer regardless of its kind (`Document` / `Help` /
/// `Oil` / `FileTree`). Per "everything is a buffer" (CLAUDE.md):
/// the same fields apply uniformly to every kind — the kind
/// itself is one of the carried fields.
///
/// Split out of [`BuffersRenderState`] (which is the *registry*
/// of all buffers) because read frequencies differ by orders of
/// magnitude:
///
/// - The active-buffer state churns on every motion / edit /
///   scroll — per-frame critical.
/// - The registry churns only on `:b N` / `:e <path>` / `:bd`.
///
/// Splitting lets Slice 3b republish them independently — a
/// motion republishes `ActiveDocumentRenderState` without forcing
/// `BuffersRenderState` to allocate a new Arc.
///
/// Phase 5.8.AF.5 / Slice 3c.1: populated. Renderers migrate
/// their direct `editor.X` reads to this sub-state in Slices
/// 3c.2 (TUI) + 3c.3 (GPUI); the field set covers every paint-
/// time hot-path read.
#[derive(Debug, Clone)]
pub struct ActiveDocumentRenderState {
    /// Buffer kind (Document / Help / Oil / FileTree). The
    /// renderer's paint switch dispatches on this for kind-
    /// specific overlays (oil row prefix, file-tree decorations,
    /// help anchors, …).
    pub buffer_kind: lattice_core::BufferKind,
    /// BufferId of the currently-active document. For the
    /// Document kind this equals `active_pane_buffer_id`; for
    /// help / oil / file-tree the kinds may diverge (a help
    /// popup sits over a document pane).
    pub document_buffer_id: lattice_core::BufferId,
    /// BufferId of the active pane's surface (what the user
    /// sees in the focused pane). Used by per-pane reads.
    pub active_pane_buffer_id: lattice_core::BufferId,
    /// Cursor position (line + byte). Per-frame critical.
    pub cursor: lattice_protocol::position::Position,
    /// First visible buffer line. Drives the viewport's top.
    pub scroll: u32,
    /// Viewport height in screen-cell rows (active pane's
    /// content area). Set by the renderer; read back here for
    /// motions, scroll math, and the gutter.
    pub viewport_height: u32,
    /// Modal state (Normal / Insert / Visual / OpPending /
    /// Command / Search / Replace). Drives cursor shape, the
    /// modeline label, and gates per-mode paint behavior.
    pub modal: lattice_grammar::ModalState,
    /// Visual selection anchor; `None` when not in Visual.
    pub visual_anchor: Option<lattice_protocol::position::Position>,
    /// Active document's snapshot pointer (cheap rope `Arc`
    /// clone). Captured at publication time so the renderer
    /// holds a per-frame consistent view. Wait-free read for
    /// downstream consumers (line iteration, byte indexing).
    pub snapshot: Arc<lattice_runtime::DocumentSnapshot>,
    /// Pending motion-count accumulator (e.g. `3` in `3dw`).
    /// Slice 3c.atomic.J: mirrored here so the input translator
    /// can build its `TranslateContext` from a published snapshot
    /// instead of reaching through `app.editor.X` per keystroke.
    pub pending_count: u32,
    /// Operator-pending count (e.g. `2` in `d2w`). Same
    /// rationale as `pending_count`.
    pub op_count: u32,
    /// `true` while a macro is being recorded (`q<reg>`).
    /// Used by the translator to gate the `q` rebind and by the
    /// modeline's recording indicator.
    pub macro_recording: bool,
    /// `true` while the insert-completion popup is open.
    /// Gates insert-mode keystroke translation (Tab cycle,
    /// CR accept, Esc dismiss).
    pub completion_open: bool,
    /// `true` while a picker overlay is open. Gates the
    /// normal-mode keymap so picker-local keys take precedence.
    pub picker_open: bool,
    /// `true` while a snippet's tab-stop chain is active.
    /// Gates Tab / S-Tab to drive `next_tabstop` / `prev_tabstop`
    /// instead of falling back to insert-completion / outdent.
    pub snippet_active: bool,
    /// Terminal-mode T2.a (2026-05-25): `true` when
    /// `terminal-insert-mode` is active on the active Terminal
    /// buffer. Drives the translate-layer branch that encodes
    /// keystrokes to ANSI bytes (and emits
    /// `Action::TerminalInput`) instead of running them through
    /// the normal-in-terminal vim grammar.
    pub terminal_insert_active: bool,
    /// Terminal-mode T2.b.0 (2026-05-25): resolved value of the
    /// `terminal.esc-exits` typed option. Mirrored into the
    /// render state so the input translator can build its
    /// `TranslateContext` from the published snapshot rather
    /// than reaching into `editor.config` per keystroke. When
    /// `true`, `<Esc>` while `terminal_insert_active` emits
    /// `Action::ExitTerminalInsert` instead of encoding to
    /// `\x1b` for the PTY.
    pub terminal_esc_exits: bool,
    /// Terminal-mode T3.b.2 (2026-05-25): `true` when the
    /// active Terminal buffer has a linewise Visual selection
    /// in flight (i.e. `TerminalBuffer::visual.is_some()`).
    /// Drives the modeline label (`TERMINAL-VISUAL`) and the
    /// translate-layer routing for `j` / `k` (extend head vs
    /// scroll viewport) without renderers having to reach into
    /// the buffer registry themselves.
    pub terminal_visual_active: bool,
    /// Terminal-mode T2.c (2026-05-25): DECCKM bit read from
    /// the active terminal's alacritty `Term`. When `true`,
    /// the translate layer feeds it to
    /// `keymap_terminal::key_to_ansi_with_mode` so arrow keys
    /// encode as SS3 (`ESC O A`) rather than CSI
    /// (`ESC [ A`). Programs like vim / less / htop / fzf
    /// flip this with `ESC [ ? 1 h`.
    pub terminal_app_cursor_keys: bool,
    /// Terminal-mode T2.c (2026-05-25): `true` between the
    /// `<C-\>` arming chord and the subsequent confirm key.
    /// When set, the next translate call routes:
    ///   - `<C-n>` → `ExitTerminalInsert`
    ///   - any other chord → encode `\x1c` + the chord's
    ///     normal PTY bytes
    /// Cleared by both paths so the next chord starts fresh.
    pub terminal_insert_exit_pending: bool,
    /// 2026-05-25: program basename ("zsh", "bash", "cargo") of
    /// the child process driving the active Terminal buffer.
    /// Published from `TerminalBuffer::program_name` so the
    /// modeline can surface "what's running here" rather than
    /// the generic `TERMINAL` label. Empty when the active
    /// buffer is not a Terminal.
    pub terminal_program_name: std::sync::Arc<str>,
    /// Phase 5.8.AF.5 / Slice 3c.final.B (group 2): folds for
    /// the active document. Renderers read
    /// `rs.active_document.folds` instead of `app.editor.folds`.
    /// `Arc<[Fold]>` so subsequent reader frames share the
    /// allocation; typical fold count is <20 so cloning at
    /// publish-time is sub-µs.
    pub folds: std::sync::Arc<[lattice_core::Fold]>,
    /// Hlsearch matches in the active document. Each entry is a
    /// `ProtoRange` covering one occurrence; the renderer paints
    /// every range with the softer match bg. Cap is bounded by
    /// `:set max_hits` (default 1000) so the clone is bounded.
    pub all_matches: std::sync::Arc<[lattice_protocol::position::Range]>,
    /// Primary search hit the cursor sits on (painted with the
    /// strongest match colour). `None` outside Search mode.
    pub current_match: Option<lattice_protocol::position::Range>,
    /// Resolved visual selection range (anchor → head, normalised).
    /// `None` when not in Visual. Mirrors the host's
    /// `Editor::visual_selection_range()` helper so renderers don't
    /// need to reach for that method through `&Editor`.
    pub visual_range: Option<lattice_protocol::position::Range>,
    /// `:s/pat/repl/...` preview overlay. `None` while no
    /// substitute is being typed. The renderer paints the
    /// match ranges (and replacement text, if any) with the
    /// destructive-preview colour. `Arc` for cheap cloning.
    pub substitute_preview: Option<std::sync::Arc<crate::state::SubstitutePreview>>,
    /// Active document's selection set (multi-cursor / linewise /
    /// blockwise). Already an `Arc` on `DocumentHandle` so this
    /// is one Arc bump.
    pub selections: std::sync::Arc<lattice_protocol::SelectionSet>,
    /// Hot-path option cache (typed-options resolved values).
    /// `Copy` so the publish is a plain struct move. Used heavily
    /// by per-row paint (whitespace glyphs, current-line highlight,
    /// line-number style).
    pub option_cache: crate::state::OptionCache,
}

impl Default for ActiveDocumentRenderState {
    fn default() -> Self {
        // Default uses a `Document` kind with `BufferId(0)` and
        // an empty snapshot. Renderers reading the default
        // before the first dispatch publication see a
        // consistent zero-state.
        Self {
            buffer_kind: lattice_core::BufferKind::Document,
            document_buffer_id: lattice_core::BufferId(0),
            active_pane_buffer_id: lattice_core::BufferId(0),
            cursor: lattice_protocol::position::Position::ZERO,
            scroll: 0,
            viewport_height: 0,
            modal: lattice_grammar::ModalState::Normal,
            visual_anchor: None,
            snapshot: Arc::new(lattice_runtime::DocumentSnapshot::default()),
            pending_count: 0,
            op_count: 0,
            macro_recording: false,
            completion_open: false,
            picker_open: false,
            snippet_active: false,
            terminal_insert_active: false,
            terminal_esc_exits: true,
            terminal_visual_active: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_program_name: Arc::from(""),
            folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
            all_matches: Arc::from(
                Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
            ),
            current_match: None,
            visual_range: None,
            substitute_preview: None,
            selections: Arc::new(lattice_protocol::SelectionSet::default()),
            option_cache: crate::state::OptionCache::default(),
        }
    }
}

/// Buffer registry's render-side projection — the list of
/// buffers the editor knows about, independent of which one is
/// currently active.
///
/// Per "everything is a buffer" (CLAUDE.md): files, help, oil,
/// file-tree, `*messages*`, scratch — all are entries in this
/// one index. Active-buffer hot-path state lives in
/// [`ActiveDocumentRenderState`]; this sub-state is touched on
/// registry changes only.
///
/// Phase 5.8.AF.5 / Slice 3c.final.B (group 1): populated.
/// `registry` carries a clone of the editor's [`BufferRegistry`]
/// — the registry is internally `Arc<Mutex<...>>`-backed so the
/// clone is one Arc bump and the inner lookups (`document_handle`,
/// `name_of`, `with_oil`, `flags_of`, `kind_of`) see the latest
/// editor state without any further publication.
///
/// `uris` mirrors `Editor::buffer_uris` — published as a fresh
/// `HashMap` clone per publish since the editor's field is owned
/// directly. The renderer reads `rs.buffers.uris.get(&id)` instead
/// of `app.editor.buffer_uris.get(&id)`.
#[derive(Debug, Default, Clone)]
pub struct BuffersRenderState {
    /// Cloned [`crate::buffer_registry::BufferRegistry`]. Wait-free
    /// to construct (one Arc bump); inner methods take their own
    /// lock for each call.
    pub registry: crate::buffer_registry::BufferRegistry,
    /// LSP URI per buffer id. Published fresh each tick. For ~10
    /// buffers the clone is sub-µs; if the registry grows large,
    /// migrate to `Arc<HashMap<...>>` on the editor side to
    /// collapse the clone into one Arc bump.
    pub uris: std::sync::Arc<std::collections::HashMap<lattice_core::BufferId, lattice_lsp::Uri>>,
}

/// Pane tree's render-side projection.
///
/// Phase 5.8.AF.5 / Slice 3c.final.B (group 1): populated.
/// Carries a clone of the editor's [`PaneTree`] inside an `Arc`
/// so subsequent reader frames share the same allocation.
/// Renderers read `rs.panes.tree.X()` instead of
/// `app.editor.pane_tree.X()`; every existing `PaneTree` method
/// (`root`, `leaves`, `active`, `active_index`, `compute_rects`)
/// flows through unchanged.
///
/// `Arc::new(self.pane_tree.clone())` on publish is the simple
/// shape; the `PaneTree::clone` cost is bounded by the tree depth
/// (one `Vec<PaneState>` + a handful of `Box<PaneNode>` allocations
/// for splits). For typical 1–3 pane layouts this is sub-µs.
/// Optimisation path (post-1.0): keep an `Arc<PaneTree>` on the
/// editor side and `Arc::make_mut` on mutation so publish collapses
/// to one Arc bump.
#[derive(Debug, Clone)]
pub struct PanesRenderState {
    pub tree: std::sync::Arc<lattice_core::ui::pane::PaneTree>,
}

impl Default for PanesRenderState {
    fn default() -> Self {
        Self {
            tree: std::sync::Arc::new(lattice_core::ui::pane::PaneTree::default()),
        }
    }
}

/// Issue #29 (2026-05-22): published per-frame tab snapshot.
/// Carries the user-visible label for each tab + the active
/// index + the resolved visibility decision (`auto` ⇒ Multi-
/// or-zero already evaluated by the publisher).
#[derive(Debug, Clone)]
pub struct TabsRenderState {
    /// One entry per tab. Index parallels `Editor::tabs`.
    pub items: std::sync::Arc<[TabRenderItem]>,
    /// Active tab index (mirror of `Editor::active_tab`).
    pub active: usize,
    /// Whether the tabline should be rendered this frame. The
    /// publisher evaluates `tabline.show` × `tabs.len()` and
    /// stores the final decision so both peers don't re-derive.
    pub visible: bool,
}

#[derive(Debug, Clone)]
pub struct TabRenderItem {
    pub id: lattice_core::ui::tab::TabId,
    /// User-visible label. Derived by the publisher from the
    /// tab's `label` override or, when None, from the active
    /// pane's buffer name (basename of path, or `[scratch]`).
    pub label: std::sync::Arc<str>,
}

impl Default for TabsRenderState {
    fn default() -> Self {
        Self {
            items: std::sync::Arc::from([]),
            active: 0,
            visible: false,
        }
    }
}

/// LSP feature data the renderer reads beyond diagnostics.
///
/// Slice 3a stubbed this empty. Slice 3b.0 wires the first
/// drained subsystem: `document_highlights`. Subsequent 3b
/// sub-slices add the remaining LSP caches one at a time
/// (hovers, signature help, inlay hints, semantic tokens, code
/// actions, document links, code lenses) following the same
/// `Arc<ArcSwapOption<...>>` shape -- the spawned request task
/// writes directly, the renderer reads wait-free.
#[derive(Debug, Default, Clone)]
pub struct LspRenderState {
    /// `textDocument/documentHighlight` cache for the active
    /// buffer + symbol position. Cloned `Arc` shared with
    /// `Editor.lsp_document_highlights` so the spawned request
    /// task's `.store()` is observable by readers without any
    /// republication of `RenderState` itself.
    ///
    /// Renderers read via
    /// `rs.lsp.document_highlights.load()` and self-validate
    /// `cache.buffer_id == active_buffer_id` to ignore results
    /// that raced a buffer switch.
    pub document_highlights:
        Arc<arc_swap::ArcSwapOption<lattice_lsp::cache::DocumentHighlightCache>>,
    /// Slice 3b.1: per-buffer `textDocument/inlayHint` cache.
    /// Spawned request task writes via
    /// `PerBufferCacheExt::insert_for`; renderers read wait-free
    /// via `.get_for(buffer_id)` and get a detached
    /// `Arc<LspInlayHintCache>`.
    pub inlay_hints:
        crate::per_buffer_cache::PerBufferCache<lattice_lsp::cache::LspInlayHintCache>,
    /// Slice 3b.1: per-buffer `textDocument/foldingRange` cache.
    /// Same shape as `inlay_hints`; renderers read via
    /// `.get_for(buffer_id)`.
    pub folds: crate::per_buffer_cache::PerBufferCache<lattice_lsp::cache::LspFoldsCache>,
    /// Slice 3b.2: per-buffer `textDocument/semanticTokens/*`
    /// cache. Spawned request task handles Items / Delta-applied /
    /// Empty outcomes by writing directly via `insert_for` (or
    /// `remove_for` on Delta result_id mismatch). Renderers read
    /// via `.get_for(buffer_id)`.
    pub semantic_tokens:
        crate::per_buffer_cache::PerBufferCache<lattice_lsp::cache::LspSemanticTokensCache>,
    /// Slice 3b.3: per-buffer `textDocument/codeLens` cache.
    /// Spawned request task writes via `insert_for`; the
    /// `codeLens/refresh` drain evicts per-server entries via
    /// `PerBufferCacheExt::retain`. Renderers read via
    /// `.get_for(buffer_id)`.
    pub code_lens: crate::per_buffer_cache::PerBufferCache<lattice_lsp::cache::LspCodeLensCache>,
    /// Slice 3b.4: per-buffer `textDocument/documentLink` cache.
    pub document_links:
        crate::per_buffer_cache::PerBufferCache<lattice_lsp::cache::LspDocumentLinksCache>,
    /// Slice 3b.4: per-buffer `textDocument/documentColor` cache.
    pub document_color:
        crate::per_buffer_cache::PerBufferCache<lattice_lsp::cache::LspDocumentColorCache>,
    /// Slice 3b.5: per-buffer `textDocument/diagnostic` (pull)
    /// result_id cache. The actual diagnostics live in
    /// `diagnostics.layer` (DiagnosticsLayer); this slot tracks
    /// only the (version, result_id) pair the next pump uses for
    /// the `previousResultId` short-circuit.
    pub pull_diagnostics:
        crate::per_buffer_cache::PerBufferCache<lattice_lsp::cache::LspPullDiagnosticsCache>,
    /// Phase 5.8.AF.5 / Slice 3c.final.B (group 4): LSP `$/progress`
    /// updates keyed by `(server_name, token)`. Published as a fresh
    /// `Arc<HashMap<...>>` per tick — typical concurrent progress
    /// items < 10 so the clone is sub-µs. Renderers read
    /// `rs.lsp.progress.iter()` to populate the modeline progress
    /// strip instead of `app.editor.lsp_progress.iter()`.
    pub progress: std::sync::Arc<
        std::collections::HashMap<
            (std::sync::Arc<str>, String),
            lattice_lsp::LspProgressUpdate,
        >,
    >,
    /// Slice 3c.final.B (group 4): LSP supervisor handle clone.
    /// The handle is internally `Arc<ArcSwap<SupervisorSnapshot>>`-
    /// backed so `Clone` is one Arc bump and `servers_for(uri)`
    /// stays wait-free. Renderers query
    /// `rs.lsp.supervisor.servers_for(&uri)` for the modeline's
    /// `[lsp:rust]` indicator instead of `app.editor.lsp.servers_for(...)`.
    pub supervisor: lattice_lsp::LspSupervisorHandle,
}

/// Tree-sitter highlights + visible-range cache.
///
/// Phase 5.8.AF.5 / Slice X2: split into two halves.
///
/// **Inputs** (`syntax_handle`, `scroll`, `viewport_height`,
/// `fold_hash`, `text_version`) are written by dispatch's
/// `publish_render_state` from current `Editor` state. The
/// background highlights worker reads them via the published
/// `RenderState` snapshot to decide whether to recompute.
///
/// **Output** (`visible_spans`) is a nested `Arc<ArcSwap<...>>`
/// so the worker can publish a fresh `VisibleSpans` *without*
/// going through `publish_render_state`. The outer `RenderState`
/// `Arc` stays stable across a frame; the inner spans cell can
/// be swapped at any time. Renderers read with
/// `render_state.syntax.visible_spans.load()` — wait-free.
///
/// Goal #1 ("no parsing on the UI thread") is enforced by this
/// split: the tree-sitter walk runs on the worker, not in any
/// renderer's per-frame body.
#[derive(Debug, Clone)]
pub struct SyntaxRenderState {
    /// Active document's syntax handle. `None` when no language
    /// is attached (scratch buffer, plain text). The worker calls
    /// `.snapshot()` on this each tick to capture the current
    /// tree state for the highlight walk.
    pub syntax_handle: Option<Arc<lattice_syntax::SyntaxHandle>>,
    /// First visible line (the worker passes this as `start` to
    /// `highlight_lines(start, end_line)`).
    pub scroll: u32,
    /// Visible pane height in lines. The worker computes
    /// `end_line = scroll + viewport_height` (clamped by the
    /// snapshot's line count) when [`Self::end_line_override`]
    /// is `None`.
    pub viewport_height: u32,
    /// Fold-aware highlight-window upper bound. `Some(n)` makes
    /// the worker walk `[scroll, n)` instead of the default
    /// `[scroll, scroll + viewport_height)`. Set by the peer
    /// when closed folds collapse multiple buffer lines onto a
    /// single visible row, so a `n_row` viewport may need to
    /// highlight `n_row + interior_fold_lines` buffer lines for
    /// the post-fold tail to render with syntax styling. Slice
    /// X2.9 plumbing -- before this the legacy
    /// `Editor::refresh_highlights_window` accepted `end_line`
    /// as an explicit argument; the X2 worker now reads it
    /// through the same render-state cell as every other input.
    pub end_line_override: Option<u32>,
    /// Caller-tracked signature of closed folds in the visible
    /// range. Folds change which physical lines are visible, so
    /// the cache key must include this to avoid serving stale
    /// spans across fold toggles.
    pub fold_hash: u64,
    /// Current document text version. The stale-snapshot HOLD
    /// (worker recompute path) compares the document's version
    /// against the snapshot's `text_version()` to decide whether
    /// the snapshot is still current or has fallen behind.
    pub text_version: u64,
    /// Worker-published output cell. Nested `Arc<ArcSwap<...>>`
    /// so the worker can store a fresh result without rebuilding
    /// the outer `RenderState`. The same `Arc` identity is
    /// carried across every `publish_render_state` call (cloned
    /// from `Editor::syntax_visible_spans_cell`), so the worker's
    /// writes survive subsequent publishes.
    pub visible_spans: Arc<arc_swap::ArcSwap<VisibleSpans>>,
    /// Perf plan A.2 slice A.2a: parallel cell publishing pre-painted
    /// rows for the active pane. Same nested `Arc<ArcSwap<...>>`
    /// shape as `visible_spans` so the worker can swap a fresh
    /// `VisibleRows` without rebuilding the outer `RenderState`.
    /// Inner `Arc` identity is the same per-cell handle cloned from
    /// `Editor::syntax_visible_rows_cell` at every publish.
    ///
    /// GPUI's `editor_element` consumes this via
    /// `rs.syntax.visible_rows.load()` — the prepainted rows save
    /// per-frame composition on the UI thread (paramount goal #1).
    /// The TUI peer still reads `visible_spans` until its compose
    /// loop migrates; both cells are populated on every recompute.
    pub visible_rows: Arc<arc_swap::ArcSwap<VisibleRows>>,
    /// Perf plan A.2 slice A.2b.1: active document's flattened
    /// inlay-hint list, pre-gated by the buffer's
    /// `lsp-inlay-hint-mode` enable-flag. Populated once per
    /// publish from `Editor::lsp_inlay_hints_cache` for the active
    /// document; empty when the mode is off, no LSP, or no hints
    /// have arrived yet.
    ///
    /// Why on `SyntaxRenderState` and not `LspRenderState`:
    /// inlays are an INPUT to the syntax worker's row composition
    /// (A.2b.2). The worker walks this list to splice inlay text
    /// into `RowPrepaint.combined`; downstream readers (GPUI's
    /// active-pane prepaint) consume the woven rows. The raw
    /// per-buffer LSP cache stays on `lsp.inlay_hints` for the
    /// inactive-pane fallback path that flattens its own list.
    ///
    /// Coordinates: `byte` is a utf-8 offset against the active
    /// document's line text; `text` already has `padding_left`
    /// / `padding_right` spaces baked in at the publish boundary.
    pub inlay_hints: Arc<[InlayHintRow]>,
    /// Perf plan A.2 slice A.2b.2: content hash of `inlay_hints`.
    /// Paired with [`VisibleHighlightsKey::inlay_version`] so the
    /// worker invalidates its row cache when the inlay payload
    /// changes (arrivals, mode-gate flip, label edits). Stable
    /// across pure-scroll ticks (same payload → same hash → cache
    /// hit). Built by the publisher in the same pass that builds
    /// `inlay_hints` so the two stay aligned by construction.
    pub inlay_version: u64,
    /// Perf plan B.2: worker-published output cell for per-row
    /// pre-bucketed STATIC overlay quads (doc_highlight,
    /// all_matches, substitute). Same nested
    /// `Arc<ArcSwap<...>>` shape as `visible_rows` so the worker
    /// can swap a fresh bucket without rebuilding the outer
    /// `RenderState`. Inner `Arc` identity is the per-cell handle
    /// cloned from `Editor::syntax_static_overlay_quads_cell` at
    /// every publish.
    ///
    /// Active-pane only — inactive panes keep the legacy
    /// per-frame bucket path in their renderer (the worker only
    /// pre-paints the active pane's window). Cursor-coupled
    /// layers (`visual_range`, `current_match`) are merged in by
    /// the renderer at prepaint time; they're cheap per-row
    /// (one range each) and would force a worker wake on every
    /// cursor blink if pushed off-thread.
    pub static_overlay_quads: Arc<arc_swap::ArcSwap<StaticOverlayQuads>>,
    /// Perf plan B.2: active document's LSP document-highlight
    /// ranges, pre-converted from utf-16 columns to utf-8 byte
    /// offsets at publish time. The worker consumes this list
    /// directly when bucketing the `DocHighlight` layer instead
    /// of forcing the renderer to repeat the per-frame
    /// conversion against the snapshot text. Empty when the
    /// active buffer has no highlights or the LSP isn't attached
    /// — matches the steady-state no-highlight path on a single
    /// cheap branch. Parallels [`Self::inlay_hints`] (A.2b.1).
    pub doc_highlights: Arc<[lattice_protocol::position::Range]>,
    /// Perf plan B.2: content hash of the static-overlay payload
    /// (doc_highlights + all_matches + substitute_matches).
    /// Paired with
    /// [`VisibleHighlightsKey::static_overlay_version`] so the
    /// worker invalidates its overlay bucket when any layer
    /// changes (search query bump, LSP response, substitute
    /// input edit). Independent from `inlay_version` so search
    /// churn doesn't invalidate the row cache and vice versa.
    /// Built by the publisher from the same payload in
    /// [`static_overlay_state_version`] so the hash stays
    /// byte-aligned with the published list.
    pub static_overlay_version: u64,
    /// Slice 3c.final.B.8: per-pane span cache published as
    /// `Arc<HashMap<pane_idx, Arc<Vec<Vec<StyledSpan>>>>>`.
    /// Outer Arc is the per-publish handle (cheap clone); inner
    /// `Arc<Vec<...>>` so a per-pane lookup is one more Arc bump
    /// without cloning the spans. The cache is owner-streamed by
    /// `refresh_pane_highlights` — the publish step here just
    /// surfaces the current snapshot to renderer threads.
    pub pane_highlights:
        Arc<std::collections::HashMap<usize, Arc<Vec<Vec<lattice_syntax::StyledSpan>>>>>,
}

impl Default for SyntaxRenderState {
    fn default() -> Self {
        Self {
            syntax_handle: None,
            scroll: 0,
            viewport_height: 0,
            end_line_override: None,
            fold_hash: 0,
            text_version: 0,
            visible_spans: Arc::new(arc_swap::ArcSwap::from_pointee(VisibleSpans::default())),
            visible_rows: Arc::new(arc_swap::ArcSwap::from_pointee(VisibleRows::default())),
            inlay_hints: Arc::from(Vec::<InlayHintRow>::new().into_boxed_slice()),
            inlay_version: 0,
            static_overlay_quads: Arc::new(arc_swap::ArcSwap::from_pointee(
                StaticOverlayQuads::default(),
            )),
            doc_highlights: Arc::from(
                Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
            ),
            static_overlay_version: 0,
            pane_highlights: Arc::new(std::collections::HashMap::new()),
        }
    }
}

/// Cache key identifying the inputs that produced a particular
/// `VisibleSpans`. Worker compares the *current* inputs against
/// `VisibleSpans::computed_for_key` to short-circuit recompute on
/// a no-op tick (cursor blink, unchanged scroll/viewport/folds).
///
/// `snapshot_ptr` is the `Arc::as_ptr` of the snapshot the spans
/// were computed against — distinct snapshots produce distinct
/// keys even if `text_version` happens to match.
///
/// Migrated from `crates/lattice-host/src/highlights.rs` in X2;
/// the renderer's read contract is now the canonical owner.
///
/// Perf plan A.2 slice A.2b.2: `inlay_version` axis — a content
/// hash of the gated `SyntaxRenderState.inlay_hints` payload. When
/// inlays arrive, change, or the mode-gate flips, the hash bumps
/// and the worker recomposes rows so the inlay splice stays
/// current. Stable across pure-scroll / cursor-blink ticks (the
/// hash is recomputed from the same payload).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisibleHighlightsKey {
    pub snapshot_ptr: usize,
    pub syntax_text_version: u64,
    pub scroll: u32,
    pub viewport_height: u32,
    pub fold_hash: u64,
    pub inlay_version: u64,
    /// Perf plan B.2: content hash of the static-overlay payload
    /// (doc_highlights + all_matches + substitute_matches). Bumps
    /// independently from `inlay_version` so a search-query change
    /// invalidates the overlay bucket without forcing a row
    /// recompose, and an inlay arrival doesn't invalidate the
    /// overlay bucket. Built from
    /// [`static_overlay_state_version`] at publish time so the
    /// hash and the payload stay aligned by construction.
    pub static_overlay_version: u64,
}

/// Worker-published syntax highlight spans for the active
/// document's visible window.
///
/// `spans[i]` covers visible line `i` (i.e. document line
/// `scroll + i`). Empty `spans` (the `Default`) means no
/// highlights yet — renderer paints plain text until the first
/// worker tick lands.
///
/// `computed_for_key` carries the inputs that produced these
/// spans so the worker can skip recompute on identical keys.
#[derive(Debug, Clone)]
pub struct VisibleSpans {
    /// Perf plan D.1: `Arc<[T]>` instead of `Vec<T>` so the HOLD
    /// path's clone collapses to a single Arc bump instead of
    /// allocating a fresh `Vec<Vec<StyledSpan>>` per stale-snapshot
    /// wake. Held-key bursts can hit HOLD on every wake while the
    /// parser catches up; the previous `Vec` clone was measured at
    /// +32-75% on `worker_stale_snapshot_hold` after A.2a/B.1
    /// landed (rows + spans both cloned per HOLD).
    pub spans: Arc<[Vec<lattice_syntax::StyledSpan>]>,
    pub computed_for_key: VisibleHighlightsKey,
}

impl Default for VisibleSpans {
    fn default() -> Self {
        Self {
            spans: Arc::from(Vec::<Vec<lattice_syntax::StyledSpan>>::new().into_boxed_slice()),
            computed_for_key: VisibleHighlightsKey::default(),
        }
    }
}

/// One coloured run within a [`RowPrepaint`]'s `combined` text.
///
/// Perf plan A.2. Carries either a [`lattice_syntax::Style`] tag
/// for source bytes or an `Inlay` discriminant for inlay-spliced
/// bytes. Runs are NOT baked to RGB — the resolved palette lives
/// on [`crate::ui::theme::Theme`] (published per-frame in
/// [`RenderState::theme`]) and renderers map `style → Rgba` at
/// paint time. Reasons for the tag, not the colour:
///
/// - A theme switch doesn't invalidate the worker's cache (only
///   the colour resolution at paint changes; the run topology
///   doesn't).
/// - The worker stays theme-independent — no `Theme` reads, no
///   `Theme::default()` fallback that silently breaks user themes.
/// - The cache key on [`VisibleHighlightsKey`] doesn't need a
///   theme hash; runs are stable across theme changes.
///
/// Slice A.2b.2 promoted this from a struct to an enum so the
/// worker can mark inlay-text bytes distinctly. Consumers map
/// `Inlay` to their inlay-virtual-text colour (resolved from
/// `RenderState.theme` on the GPUI side; a TuiStyle modifier on
/// the TUI side) without having to track byte ranges separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRun {
    /// Source-text run carrying a tree-sitter style tag.
    Source {
        /// Number of utf-8 bytes in this run inside `combined`.
        len: u32,
        /// Style tag from the highlight grammar. `Style::Default`
        /// for runs that fall outside any tree-sitter capture.
        style: lattice_syntax::Style,
    },
    /// Inlay-virtual-text run inserted by the worker from
    /// [`SyntaxRenderState::inlay_hints`]. Carries only the byte
    /// length; the colour is consumer-resolved.
    Inlay {
        /// Number of utf-8 bytes of inlay text in this run.
        len: u32,
    },
}

impl RowRun {
    /// Byte length of this run inside the row's `combined` text.
    /// Convenience accessor so consumers don't need to match every
    /// time the partition is walked. (Clippy: paired enums often
    /// also expose `is_empty`, but `RowRun::Inlay { len: 0 }`
    /// would be a worker bug — the partition is built with the
    /// invariant that every run is non-empty, so an `is_empty`
    /// would be misleading.)
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u32 {
        match self {
            RowRun::Source { len, .. } | RowRun::Inlay { len } => *len,
        }
    }
}

/// One visible row pre-painted by the highlights worker for the
/// active pane.
///
/// Perf plan A.2 slice A.2a. Carries the combined text the renderer
/// shapes/paints plus the colour-run partition over it.
///
/// Slice A.2b.2: `combined` now includes inlay text spliced in at
/// the byte boundaries published on
/// [`SyntaxRenderState::inlay_hints`]. The partition `runs`
/// distinguishes source bytes ([`RowRun::Source`]) from inlay
/// bytes ([`RowRun::Inlay`]) so consumers can colour them
/// independently. `inlay_offsets` records where the splices
/// happened so cursor / decoration code can remap utf-8 byte
/// offsets in the original line onto column positions in
/// `combined` (the analogue of GPUI's `byte_to_combined_col`).
///
/// Storage choices:
/// - `combined: Box<str>` — one heap allocation per row, sized to
///   the line + inlays. `Box<str>` over `String` shaves the unused
///   capacity word and signals immutability. Bounded by
///   `viewport_height` (typically <200 rows × <200 chars = <40 kB
///   per recompute).
/// - `runs: Vec<RowRun>` — adjacent-equal Source runs merged at
///   build time; Inlay runs always break the merge so the consumer
///   can colour them distinctly.
/// - `inlay_offsets: Arc<[(u32, u32)]>` — one entry per spliced
///   inlay: `(orig_byte, char_width)`. `orig_byte` is the utf-8
///   offset into the SOURCE line where the inlay was inserted
///   (NOT into `combined`); `char_width` is the inlay text's
///   character count. `Arc<[T]>` so the HOLD reuse path on the
///   worker bumps an Arc instead of cloning the inner Vec.
#[derive(Debug, Default, Clone)]
pub struct RowPrepaint {
    pub combined: Box<str>,
    pub runs: Vec<RowRun>,
    pub inlay_offsets: Arc<[(u32, u32)]>,
}

/// Worker-published pre-painted rows for the active pane's visible
/// window.
///
/// Perf plan A.2 slice A.2a. `rows[i]` corresponds to buffer line
/// `start + i` where `start` is the worker's recompute scroll input.
/// Empty `rows` (the `Default`) means no rows yet — the renderer
/// paints from its rope fallback path until the first worker tick
/// lands.
///
/// Published in a second `ArcSwap` cell alongside [`VisibleSpans`]
/// — the legacy cell stays so the TUI peer's existing `StyledSpan`
/// grid path keeps working without an adapter slice. GPUI consumes
/// `rows`; TUI keeps reading `spans` (migration deferred).
#[derive(Debug, Clone)]
pub struct VisibleRows {
    /// Perf plan D.1: `Arc<[T]>` for the same reason as
    /// [`VisibleSpans::spans`]. HOLD reuses the prior rows via one
    /// Arc bump instead of cloning every `RowPrepaint` (each carries
    /// a `Box<str>` + `Vec<RowRun>` — non-trivial allocs at 120
    /// rows). The bench `worker_stale_snapshot_hold/120` measured
    /// +75% with the per-element `Vec` clone; this collapses it.
    pub rows: Arc<[RowPrepaint]>,
    pub computed_for_key: VisibleHighlightsKey,
}

impl Default for VisibleRows {
    fn default() -> Self {
        Self {
            rows: Arc::from(Vec::<RowPrepaint>::new().into_boxed_slice()),
            computed_for_key: VisibleHighlightsKey::default(),
        }
    }
}

/// Perf plan B.2: overlay layer tag carried on each per-row
/// pre-bucketed quad in [`StaticOverlayQuads`]. The renderer uses
/// the tag to interleave cursor-coupled layers (`current_match`,
/// `visual_range`) at the right precedence at prepaint time:
///
/// ```text
/// doc_highlight  →  all_matches  →  current_match  →  visual  →  substitute
/// ```
///
/// Push order = paint order = visual precedence (`paint_quad`
/// overwrites; later quads in each row's Vec win).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayLayer {
    /// LSP `textDocument/documentHighlight` ranges — symbol
    /// occurrences under the cursor. Cursor-settle cadence (the
    /// LSP returns a new response after the cursor lands on a
    /// new symbol); we treat it as static across cursor blinks.
    DocHighlight,
    /// `hlsearch` matches across the active document. Bumps on
    /// `text_version` edits and search-query changes.
    AllMatches,
    /// `:s/pat/repl/` preview overlay. Bumps as the substitute
    /// command line is typed.
    Substitute,
}

/// Perf plan B.2: one pre-bucketed static-overlay quad inside a
/// row of [`StaticOverlayQuads`]. Coordinates are in
/// **source utf-8 byte space** — the byte offsets into the SOURCE
/// line text (NOT into `RowPrepaint.combined`).
///
/// Why source-byte and not combined-column: both renderer peers
/// already do their own coordinate transforms (GPUI runs
/// `byte_to_combined_col` for cursor / diagnostic underlines per
/// frame; TUI uses source bytes directly for overlay application).
/// Publishing in source-byte space lets the TUI consume the bucket
/// without any reverse mapping, and the per-quad conversion GPUI
/// pays on prepaint is cheap (one `chars().count()` walk on a
/// single line, amortised over a handful of quads per row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowOverlayQuad {
    pub layer: OverlayLayer,
    pub source_byte_start: u32,
    pub source_byte_end: u32,
}

/// Perf plan B.2: worker-published per-row pre-bucketed
/// static-overlay quads for the active pane's visible window.
///
/// `quads[i]` is the per-row tagged quad list for visible line
/// `i` (i.e. doc line `scroll + i`). Each entry tags its overlay
/// layer ([`OverlayLayer`]) so the renderer can interleave
/// cursor-coupled layers (`current_match`, `visual_range`) in
/// the right precedence order at prepaint time.
///
/// Active-pane only — inactive panes keep the legacy per-frame
/// bucket path. The cell is published on every `recompute`
/// alongside [`VisibleRows`] but invalidates on its own axis
/// ([`VisibleHighlightsKey::static_overlay_version`]) so a
/// search-query bump doesn't force a row recompose, and vice
/// versa.
#[derive(Debug, Clone)]
pub struct StaticOverlayQuads {
    /// `Arc<[T]>` per the D.1 pattern — HOLD / partial-reuse
    /// paths bump the outer Arc instead of cloning the per-row
    /// `Vec`s. Typical viewport (120 rows) × typical quads/row
    /// (≤ a few per layer) keeps this comfortably small.
    pub quads: Arc<[Vec<RowOverlayQuad>]>,
    pub computed_for_key: VisibleHighlightsKey,
}

impl Default for StaticOverlayQuads {
    fn default() -> Self {
        Self {
            quads: Arc::from(Vec::<Vec<RowOverlayQuad>>::new().into_boxed_slice()),
            computed_for_key: VisibleHighlightsKey::default(),
        }
    }
}

/// Perf plan A.2 slice A.2b.2: content hash of a flattened inlay-
/// hint list. Stable per-payload (same vec → same hash) so it can
/// drive [`VisibleHighlightsKey::inlay_version`] for the worker's
/// row-cache invalidation. Empty list hashes to 0 — matches the
/// `inlay_version: 0` default and keeps the steady-state no-hint
/// path on a single cheap branch.
///
/// Implementation is a fold over each row's `(line, byte, text)`
/// triple using the default `DefaultHasher` (SipHash 1-3, suitable
/// for non-cryptographic versioning). For the typical viewport of
/// <200 hints this is sub-µs once per publish.
pub fn inlay_hints_version(rows: &[InlayHintRow]) -> u64 {
    use std::hash::{Hash, Hasher};
    if rows.is_empty() {
        return 0;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for r in rows {
        r.line.hash(&mut h);
        r.byte.hash(&mut h);
        r.text.hash(&mut h);
    }
    h.finish()
}

/// Perf plan B.2: content hash of the three static-overlay layer
/// payloads. Drives [`VisibleHighlightsKey::static_overlay_version`]
/// for the worker's overlay-bucket invalidation. All-empty payloads
/// hash to 0 so the steady-state no-overlay path stays on a single
/// cheap branch (matches the `static_overlay_version: 0` default).
///
/// Each layer is tagged with a distinct discriminator byte
/// (0 / 1 / 2) before its ranges are folded in so the SAME range
/// list appearing in different layers produces distinct hashes —
/// avoids accidental cross-layer collisions.
///
/// Implementation is a fold over each range's `(start.line,
/// start.byte, end.line, end.byte)` quadruple using
/// `DefaultHasher` (SipHash 1-3, suitable for non-cryptographic
/// versioning). For the bounded sizes the editor enforces
/// (`max_hits` caps `all_matches` at 1000; doc_highlights /
/// substitute lists are typically <50) this is sub-µs once per
/// publish.
pub fn static_overlay_state_version(
    doc_highlights: &[lattice_protocol::position::Range],
    all_matches: &[lattice_protocol::position::Range],
    substitute_matches: &[lattice_protocol::position::Range],
) -> u64 {
    use std::hash::{Hash, Hasher};
    if doc_highlights.is_empty() && all_matches.is_empty() && substitute_matches.is_empty() {
        return 0;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let fold_layer = |h: &mut std::collections::hash_map::DefaultHasher,
                      tag: u8,
                      ranges: &[lattice_protocol::position::Range]| {
        tag.hash(h);
        for r in ranges {
            r.start.line.hash(h);
            r.start.byte.hash(h);
            r.end.line.hash(h);
            r.end.byte.hash(h);
        }
    };
    fold_layer(&mut h, 0, doc_highlights);
    fold_layer(&mut h, 1, all_matches);
    fold_layer(&mut h, 2, substitute_matches);
    h.finish()
}

/// Perf plan B.4: identity-preserving sub-state cache.
///
/// `Editor::build_render_state` rebuilds every sub-state `Arc`
/// from scratch on every publish today. Most publishes don't
/// touch most sub-states (cursor moves, scroll, etc. only update
/// `active_document`), so the inner allocations and deep clones
/// for the rest are wasted.
///
/// This struct lives behind `Editor::publish_cache` (a
/// `std::sync::Mutex<PublishCache>` because `Editor` is shared as
/// `Arc<Editor>` and therefore must be `Sync`). The mutex is
/// uncontested in practice — only `build_render_state` takes the
/// lock, and only the actor thread calls it.
///
/// Each slot pairs a `u64` version (captured from the
/// corresponding `Versioned<T>` field on `Editor` at the moment
/// the cached Arc was built) with the cached `Arc<SubState>`.
/// On the next publish, if the field's current version matches
/// the cached version, the cached Arc is reused (same Arc
/// identity preserved across the publish — `Arc::ptr_eq` returns
/// true). Otherwise the slot is rebuilt and the new
/// `(version, Arc)` pair is stored.
///
/// **Targeted sub-states:**
///
/// B.4.a (5 subs):
///
/// - `panes` — full sub-state Arc. Mutates on
///   `pane_tree.split_active` / `close_active` / `set_active` /
///   tab swap; otherwise stable.
/// - `modes` — full sub-state Arc. Mutates on `activate_mode` /
///   `deactivate_mode`; otherwise stable.
/// - `buffer_locals` — full sub-state Arc. Mutates on the few
///   `buffer_locals.entry(...).or_default()` / `.insert` / `.remove`
///   sites; otherwise stable. Largest savings because the per-entry
///   clone deep-walks the typed-map.
/// - `pane_highlights_map` — INNER `Arc<HashMap<...>>` of the
///   syntax sub-state. The outer `SyntaxRenderState` Arc rebuilds
///   every publish (its other fields churn per-frame), but the
///   per-pane spans map can be reused.
/// - `lsp_progress` — INNER `Arc<HashMap<...>>` of the `lsp`
///   sub-state. Same shape as `pane_highlights_map`; saves the
///   HashMap clone when no `$/progress` event fired.
///
/// B.4.b (3 subs):
///
/// - `buffers` — full `Arc<BuffersRenderState>`. Keyed on
///   `buffer_uris.version()` alone. The inner `registry` field is
///   `Arc<Mutex<...>>`-backed so the SAME registry handle inside a
///   reused Arc still sees current state — no version dependency
///   on registry mutations needed for this sub-state's cache
///   hit/miss decision. Saves the `buffer_uris.clone()` HashMap
///   allocation per no-op publish.
/// - `tabs` — full `Arc<TabsRenderState>`. Composite key over
///   `tabs.version()` (tab list shape) + `active_tab` (per-publish
///   read) + `pane_tree.version()` (active pane's buffer) +
///   `buffers.version()` (label-resolving names). Saves the
///   `build_tabs_render_state` walk per no-op publish.
#[derive(Debug, Default)]
pub struct PublishCache {
    pub panes: Option<(u64, std::sync::Arc<PanesRenderState>)>,
    pub modes: Option<(u64, std::sync::Arc<ModesRenderState>)>,
    pub buffer_locals: Option<(u64, std::sync::Arc<BufferLocalsRenderState>)>,
    pub pane_highlights_map: Option<(
        u64,
        std::sync::Arc<
            std::collections::HashMap<usize, std::sync::Arc<Vec<Vec<lattice_syntax::StyledSpan>>>>,
        >,
    )>,
    pub lsp_progress: Option<(
        u64,
        std::sync::Arc<
            std::collections::HashMap<
                (std::sync::Arc<str>, String),
                lattice_lsp::LspProgressUpdate,
            >,
        >,
    )>,
    /// Perf plan B.4.b: keyed on `buffer_uris.version()` only.
    pub buffers: Option<(u64, std::sync::Arc<BuffersRenderState>)>,
    /// Perf plan B.4.b: keyed on a composite of `tabs.version()`,
    /// `active_tab`, `pane_tree.version()`, `buffers.version()`.
    /// The composite is encoded into one `u64` via a small fold so
    /// the cache slot shape stays uniform with the other entries.
    pub tabs: Option<(u64, std::sync::Arc<TabsRenderState>)>,
}

impl PublishCache {
    /// Reset every slot. Useful in tests that want a clean
    /// baseline; production code never needs this (a version
    /// mismatch already triggers rebuild).
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Perf plan B.4: cache-or-build helper for the sub-state Arc
/// memoisation in `build_render_state`. Returns the cached Arc
/// when `current_version` matches the version stored in `slot`;
/// otherwise calls `build`, stores the result, and returns it.
///
/// Inlined into a single helper so each cached sub-state is one
/// line at the call site instead of the same `if let Some((v, arc))
/// = ... { ... } else { ... }` pattern repeated five times.
pub fn cached_or_build<T, F: FnOnce() -> std::sync::Arc<T>>(
    slot: &mut Option<(u64, std::sync::Arc<T>)>,
    current_version: u64,
    build: F,
) -> std::sync::Arc<T> {
    if let Some((v, arc)) = slot.as_ref() {
        if *v == current_version {
            return arc.clone();
        }
    }
    let next = build();
    *slot = Some((current_version, next.clone()));
    next
}

/// One inlay-hint row published on
/// [`SyntaxRenderState::inlay_hints`].
///
/// Perf plan A.2 slice A.2b.1. Caller flattens the LSP
/// [`InlayHintLabel`](lattice_lsp::lsp_types::InlayHintLabel) to a
/// plain string and pre-applies `padding_left` / `padding_right`
/// spacing; consumers splice `text` into shaped lines at `byte`
/// (utf-8 byte offset into the original line's text) without
/// further label processing.
///
/// The renderer-side type (`lattice_ui_gpui::editor_element::InlayHintRow`)
/// is a re-export of this struct so the two peers exchange the same
/// shape across the published `RenderState`.
///
/// Sort order is the publisher's responsibility — A.2b.1 publishes
/// in the same order the LSP cache stores hints (insertion order);
/// the worker re-sorts by `(line, byte)` during its row-weave pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHintRow {
    /// 0-based buffer-line index.
    pub line: u32,
    /// 0-based utf-8 byte offset into that line's text.
    pub byte: u32,
    /// Pre-flattened label with `padding_left` / `padding_right`
    /// applied.
    pub text: String,
}

/// Active picker's render-side projection.
///
/// Phase 5.8.AF.5 / Slice 3c.final.B (group 3): populated.
/// Carries an `Arc<Picker>` clone when a picker is open. The
/// renderer reads candidate list, selection index, query, title
/// through this clone instead of `app.editor.picker.as_ref()`.
/// `Picker` is large enough that the publish path goes through
/// `Arc::new(picker.clone())` per tick when open; the typical
/// candidate count keeps the clone sub-µs.
#[derive(Debug, Default, Clone)]
pub struct PickerRenderState {
    pub state: Option<std::sync::Arc<lattice_picker::Picker>>,
}

/// Insert-completion + cmdline-completion popup state.
///
/// Phase 5.8.AF.5 / Slice 3c.final.B (group 3): populated.
/// Two slots — `insert` for the in-buffer ghost popup
/// (`InsertCompletionState`) and `state` for the cmdline
/// completion popup (`CompletionState`). Both `Arc`-wrapped so
/// reader frames share the allocation; both `None` when no
/// popup is open.
#[derive(Debug, Default, Clone)]
pub struct CompletionRenderState {
    pub insert: Option<std::sync::Arc<lattice_completion::InsertCompletionState>>,
    pub state: Option<std::sync::Arc<crate::state::CompletionState>>,
}

/// Help / hover / signature popup's render-side projection.
///
/// Phase 5.8.AF.5 / Slice 3c.final.B (group 3): populated.
/// `buffer_id` mirrors `Editor::popup_buffer` (the active popup's
/// id, `None` when no popup is open). `help` carries an
/// `Arc<HelpBuffer>` snapshot of the popup content. `placement`
/// echoes `Editor::popup_placement`. `help_highlights` is the
/// per-line markdown highlight span list seeded into the popup
/// buffer's locals.
#[derive(Debug, Clone)]
pub struct PopupRenderState {
    pub buffer_id: Option<lattice_core::BufferId>,
    pub help: Option<std::sync::Arc<lattice_help::HelpBuffer>>,
    pub help_highlights: std::sync::Arc<[Vec<lattice_syntax::StyledSpan>]>,
    pub placement: lattice_core::ui::popup::PopupPlacement,
    /// 2026-05-22 popup-anchor: cursor position snapshotted at
    /// popup-open time. CursorAnchored renderers read this so
    /// the popup stays pinned to the symbol it was invoked from
    /// instead of re-deriving from the active cursor every frame
    /// (which made the popup follow motions). Both TUI and GPUI
    /// peers consume the same field from the published RS.
    pub anchor: Option<lattice_protocol::Position>,
    /// Document scroll at popup-open time. Used by CursorAnchored
    /// renderers to convert `anchor.line` (document coordinates)
    /// into a screen row without being confused by State B, where
    /// `active_document.scroll` reflects the POPUP's scroll
    /// rather than the document's. Fixed once at open; survives
    /// the State A → B transition.
    pub doc_scroll_at_anchor: u32,
}

impl Default for PopupRenderState {
    fn default() -> Self {
        Self {
            buffer_id: None,
            help: None,
            help_highlights: std::sync::Arc::from(
                Vec::<Vec<lattice_syntax::StyledSpan>>::new().into_boxed_slice(),
            ),
            placement: lattice_core::ui::popup::PopupPlacement::default(),
            anchor: None,
            doc_scroll_at_anchor: 0,
        }
    }
}

impl PopupRenderState {
    /// Convenience: `true` while a popup is open. Equivalent to
    /// `buffer_id.is_some()` but reads cleaner at call sites that
    /// previously gated on `app.editor.popup_buffer.is_some()`.
    pub fn is_open(&self) -> bool {
        self.buffer_id.is_some()
    }
}

/// Buffer-locals per buffer. Slice 3c.final.B.9 — drops the
/// per-frame `read_editor(|e| e.buffer_locals.get(&buf).and_then(...))`
/// chain in the modeline, help-render, file-tree, and oil paint
/// paths to a wait-free Arc-bump lookup off the published
/// snapshot.
///
/// Outer `Arc<HashMap<...>>` for cheap clone-on-publish; per-entry
/// `Arc<BufferLocals>` so reads don't clone the typed-map body.
/// Mutation surface (mode `on_activate` / `on_deactivate` setters,
/// pulled-diagnostics writes, file-tree refresh) deep-clones each
/// modified entry via `BufferLocals::clone` and replaces the Arc;
/// reads stay wait-free under concurrent mutation.
#[derive(Debug, Default, Clone)]
pub struct BufferLocalsRenderState {
    pub map: std::sync::Arc<
        std::collections::HashMap<
            lattice_core::BufferId,
            std::sync::Arc<lattice_mode::BufferLocals>,
        >,
    >,
}

/// Active modes per buffer. Slice 3c.final.B.11 — drops the
/// per-frame `read_editor(|e| e.active_modes.get(&buf))` call in
/// the modeline `is_messages_buffer` check to a wait-free Arc-bump
/// lookup off the published snapshot.
///
/// Outer `Arc<HashMap<...>>` for cheap clone-on-publish; per-entry
/// `Arc<ActiveModes>` so reads don't clone the inner mode chain.
/// Mutation surface (every `activate_mode` / `deactivate_mode`)
/// rebuilds the modified entry's Arc — rare path (buffer-switch),
/// not per-frame.
#[derive(Debug, Default, Clone)]
pub struct ModesRenderState {
    pub map:
        std::sync::Arc<std::collections::HashMap<lattice_core::BufferId, std::sync::Arc<lattice_mode::ActiveModes>>>,
}

/// Typed-options registry handle. Slice 3c.final.B.10 — drops the
/// per-frame `read_editor(|e| e.config.get_typed::<X>())` calls
/// in `picker_display_is_minibuffer` and elsewhere to a wait-free
/// Arc bump off the published snapshot. The inner `ConfigRegistry`
/// is already Arc-shared, so a publish here is one Arc clone.
#[derive(Debug, Default, Clone)]
pub struct OptionsRenderState {
    pub config: std::sync::Arc<lattice_config::ConfigRegistry>,
}

/// `*messages*` buffer + echo line state.
///
/// Slice 3c.final.B.7: populates the per-frame echo-area read.
/// Renderers paint `last` as the bottom-row message (replacing the
/// modeline when present). The full ring of messages stays
/// host-side; only the surface the renderer paints lives here.
#[derive(Debug, Default, Clone)]
pub struct MessagesRenderState {
    /// Last echo-area message — `None` when the row is blank.
    /// Wrapped in `Arc` so the per-publish clone is one Arc bump
    /// regardless of how long the text is.
    pub last: Option<std::sync::Arc<crate::action::EchoMessage>>,
}

/// Modeline status (cmdline text, search indicator, mode hints).
///
/// Slice 3c.final.B.7: populates the per-frame fields the
/// renderer reads through `read_editor` today (cmdline text,
/// search pattern + direction, auto-submit hint). The active mode
/// chain remains via the existing `active_modes` lookup; future
/// slices may lift that here too.
#[derive(Debug, Clone)]
pub struct ModelineRenderState {
    /// Renderer-side cmdline text. `Arc<str>` so per-publish clone
    /// is one Arc bump regardless of length.
    pub cmdline_text: std::sync::Arc<str>,
    /// `:describe-key<CR>` armed the chord-capture prompt; the
    /// renderer paints a "press a chord" hint after the cursor.
    pub auto_submit_hint: bool,
    /// `/` or `?` search pattern; `None` when no search is in
    /// flight. `Arc<str>` for the same reason as `cmdline_text`.
    pub search_pattern: Option<std::sync::Arc<str>>,
    /// `/` (forward) or `?` (backward); accompanies
    /// `search_pattern` and is `None` whenever `search_pattern` is.
    pub search_direction: Option<lattice_grammar::SearchDirection>,
}

impl Default for ModelineRenderState {
    fn default() -> Self {
        Self {
            cmdline_text: std::sync::Arc::from(""),
            auto_submit_hint: false,
            search_pattern: None,
            search_direction: None,
        }
    }
}

/// Renderer lifecycle flags published per tick.
///
/// Phase 5.8.AF.5 / Slice 3c.final.B (group 6). Carries the
/// three per-tick "renderer should notice this" signals:
///
/// - `should_quit` — set by `:q` / `:wq` / `:qa!` (host-side
///   `Editor::should_quit`). The TUI's `main_loop` reads this at
///   the top of every iteration to break out; the GPUI peer's
///   `on_key_down` reads it after dispatch to call `cx.quit()`.
/// - `pending_redraw` — set by `<C-l>` (`RedrawScreen`) so the
///   TUI peer clears the terminal buffer on the next frame. The
///   field is "renderer-consumed" — a separate
///   `acknowledge_pending_redraw` action (slice C target) is
///   needed to clear it from the renderer side once consumed.
/// - `terminal_width` — last reported terminal column count from
///   the TUI peer. Mirrored here so any future renderer-thread
///   reader sees the published value instead of the live
///   `Editor::terminal_width` field.
#[derive(Debug, Default, Clone)]
pub struct LifecycleRenderState {
    pub should_quit: bool,
    pub pending_redraw: bool,
    pub terminal_width: Option<u16>,
}

/// Translator inputs for the renderer's input loop.
///
/// Phase 5.8.AF.5 / Slice 3c.final.B (group 5) — closes the
/// audit's slice-D holdout for the `TranslateContext` `&'a` borrow
/// batch (`builtins`, `keymap`, `partial_chord`). The translator
/// runs on the renderer thread; in the slice-E end-state it can
/// no longer borrow through `&Editor`. Publishing these inputs as
/// owned/Arc-backed values lets `runtime.rs` build a
/// `TranslateContext` from a single snapshot load per keystroke.
///
/// All three fields are cheap to publish:
/// - `builtins` is `Copy` so the field is a plain move.
/// - `keymap` is an `Arc<KeymapRegistry>`-backed handle; `Clone`
///   is one Arc bump and `resolve()` stays wait-free via the
///   handle's internal `ArcSwap`.
/// - `partial_chord` is small (typically 0–2 entries during a
///   chord sequence) so the per-publish `Arc<[KeyChord]>` clone
///   is sub-µs.
#[derive(Debug, Default, Clone)]
pub struct TranslatorRenderState {
    pub builtins: lattice_grammar::builtins::Builtins,
    pub keymap: crate::keymap_registry::KeymapHandle,
    pub partial_chord: std::sync::Arc<[crate::chord::KeyChord]>,
}

/// Diagnostics — the proof-of-life sub-state for Slice 3a.
///
/// Carries a clone of `lattice_lsp::DiagnosticsLayer`. The
/// layer is itself `Arc<ArcSwap<DiagnosticsSnapshot>>`-backed,
/// so cloning is cheap (one Arc bump) and lookups
/// (`.line_severity`, `.diagnostics_arc`, `.diagnostics_for`)
/// are wait-free.
#[derive(Debug, Default, Clone)]
pub struct DiagnosticsRenderState {
    /// The diagnostics layer the renderer queries for per-line
    /// severity, per-buffer diagnostic lists, and counts.
    pub layer: lattice_lsp::DiagnosticsLayer,
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;
    use crate::action::Action;
    use lattice_lsp::DiagnosticEvent;
    use lattice_lsp::lsp_types::{
        Diagnostic, DiagnosticSeverity, Position, PublishDiagnosticsParams, Range, Uri,
    };
    use std::str::FromStr;
    use std::sync::Arc;

    /// Calling `dispatch()` publishes a fresh `RenderState` Arc
    /// into the editor's `ArcSwap`. The Arc identity must
    /// differ across dispatches — otherwise readers can't tell
    /// the snapshot is fresh.
    #[test]
    fn dispatch_publishes_fresh_render_state_arc() {
        let mut editor = Editor::default();
        let before = editor.render_state.load_full();
        // A no-op action is enough — the dispatch tail always
        // republishes regardless of whether state changed.
        editor.dispatch(Action::None);
        let after = editor.render_state.load_full();
        assert!(
            !std::sync::Arc::ptr_eq(&before, &after),
            "dispatch must publish a fresh RenderState Arc (identity changes)"
        );
    }

    /// `publish_render_state` is the manual hook subsystems will
    /// call in Slice 3b. Verify it produces a fresh Arc too —
    /// not folded into the `dispatch()` tail by accident.
    #[test]
    fn publish_render_state_replaces_arc() {
        let editor = Editor::default();
        let before = editor.render_state.load_full();
        editor.publish_render_state();
        let after = editor.render_state.load_full();
        assert!(
            !std::sync::Arc::ptr_eq(&before, &after),
            "publish_render_state must store a fresh Arc"
        );
    }

    /// The proof-of-life path: write a diagnostic into the
    /// editor's `lsp_diagnostics` layer, publish, and confirm
    /// the renderer-side read through `render_state` sees it.
    #[test]
    fn diagnostics_substate_reflects_published_layer() {
        let editor = Editor::default();
        let uri = Uri::from_str("file:///tmp/test.rs").expect("valid uri");
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 4,
                    character: 0,
                },
                end: Position {
                    line: 4,
                    character: 5,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: None,
            message: "synthetic".to_string(),
            related_information: None,
            tags: None,
            data: None,
        };
        editor.lsp_diagnostics.apply(DiagnosticEvent::from_lsp(
            Arc::from("rust"),
            PublishDiagnosticsParams {
                uri: uri.clone(),
                version: None,
                diagnostics: vec![diag],
            },
        ));
        // Force a publication so the render-state layer reflects
        // the freshly-written diagnostic. In prod this happens
        // at the dispatch tail; tests poke it directly.
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(
            rs.diagnostics.layer.line_severity(&uri, 4),
            Some(DiagnosticSeverity::ERROR),
            "renderer reading via render_state must see the diagnostic the editor wrote"
        );
        assert_eq!(
            rs.diagnostics.layer.line_severity(&uri, 0),
            None,
            "lines without a diagnostic return None through the same path"
        );
    }

    /// Slice 3c.1: `ActiveDocumentRenderState` reflects current
    /// editor state after dispatch. Mutating `editor.cursor`
    /// directly + publishing produces a snapshot whose
    /// `cursor` field matches.
    #[test]
    fn active_document_substate_reflects_editor_fields() {
        use lattice_protocol::position::Position;
        let mut editor = Editor::default();
        editor.cursor = Position::new(7, 3);
        editor.scroll = 5;
        editor.viewport_height = 30;
        editor.publish_render_state();
        let rs = editor.render_state.load();
        assert_eq!(rs.active_document.cursor, Position::new(7, 3));
        assert_eq!(rs.active_document.scroll, 5);
        assert_eq!(rs.active_document.viewport_height, 30);
        assert_eq!(rs.active_document.modal, lattice_grammar::ModalState::Normal);
        assert_eq!(rs.active_document.buffer_kind, lattice_core::BufferKind::Document);
        // Snapshot is a fresh Arc clone from `editor.document`.
        // Identity isn't preserved across publications (naive
        // rebuild today); the value is what matters.
        assert_eq!(rs.active_document.snapshot.buffer.byte_len(), 0);
        // Slice 3c.atomic.J: translator-context mirror fields
        // default to zero/false when no count, no macro, no
        // picker, no completion, no snippet is active.
        assert_eq!(rs.active_document.pending_count, 0);
        assert_eq!(rs.active_document.op_count, 0);
        assert!(!rs.active_document.macro_recording);
        assert!(!rs.active_document.completion_open);
        assert!(!rs.active_document.picker_open);
        assert!(!rs.active_document.snippet_active);
    }

    /// Slice 3c.atomic.J: writing the translator-context
    /// fields directly + publishing produces a snapshot whose
    /// mirror fields match. Proves `runtime.rs` building
    /// `TranslateContext` from `app.ad()` sees the same values
    /// it used to read from `app.editor.X` directly.
    #[test]
    fn active_document_substate_reflects_translator_context_fields() {
        let mut editor = Editor::default();
        editor.pending_count = 7;
        editor.op_count = 3;
        editor.publish_render_state();
        let rs = editor.render_state.load();
        assert_eq!(rs.active_document.pending_count, 7);
        assert_eq!(rs.active_document.op_count, 3);
        // The Option-typed fields (`macro_recording`,
        // `completion_state`, `picker`, `active_snippet`) need
        // domain types to populate. The mirror's contract is
        // tested via the `.is_some()` projection; constructing
        // those types here would just be `.is_some()` returning
        // true for a freshly-built variant, so the existing
        // `false` baseline from the prior test plus the explicit
        // u32 mirrors here are enough to lock the contract.
    }

    /// Slice 3b.0 template proof: a write into the editor's
    /// `lsp_document_highlights` `ArcSwapOption` is visible
    /// through `render_state.lsp.document_highlights.load()`
    /// without re-publishing `RenderState`.
    ///
    /// This is the contract every Slice 3b.* migration relies
    /// on: the background request task `.store()`s directly
    /// into the cache slot, and renderer reads through
    /// `RenderState` see the new value immediately because the
    /// sub-state's Arc points at the same underlying ArcSwap.
    #[test]
    fn document_highlights_substate_reflects_arcswap_writes() {
        use lattice_lsp::cache::DocumentHighlightCache;
        use lattice_lsp::lsp_types::{
            DocumentHighlight, DocumentHighlightKind, Position as LspPosition, Range as LspRange,
        };
        use lattice_protocol::position::Position;
        use std::sync::Arc;
        let editor = Editor::default();
        // Force a publication so RenderState.lsp carries a clone
        // of the editor's lsp_document_highlights ArcSwap.
        editor.publish_render_state();
        // Sanity: empty initially.
        assert!(
            editor
                .render_state
                .load()
                .lsp
                .document_highlights
                .load()
                .is_none(),
            "renderer must see None before any task writes"
        );
        // Simulate the spawned task's write -- same code path
        // it executes when the LSP response arrives.
        editor
            .lsp_document_highlights
            .store(Some(Arc::new(DocumentHighlightCache {
                buffer_id: editor.document_buffer_id,
                cursor: Position::new(0, 0),
                highlights: vec![DocumentHighlight {
                    range: LspRange {
                        start: LspPosition {
                            line: 1,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 1,
                            character: 5,
                        },
                    },
                    kind: Some(DocumentHighlightKind::READ),
                }],
            })));
        // Renderer reads through RenderState -- no re-publish
        // needed. The sub-state's Arc points at the same
        // ArcSwap the task wrote to.
        let rs = editor.render_state.load();
        let cache = rs
            .lsp
            .document_highlights
            .load_full()
            .expect("post-store, renderer must see the cache");
        assert_eq!(
            cache.highlights.len(),
            1,
            "renderer must see the highlight the task stored"
        );
        assert_eq!(cache.highlights[0].range.start.line, 1);
    }

    /// Perf plan A.2 slice A.2b.1: `syntax.inlay_hints` is empty
    /// by default — no LSP cache entries, no mode toggle,
    /// `Editor::default()` straight off the constructor.
    #[test]
    fn syntax_inlay_hints_empty_on_default_editor() {
        let editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load();
        assert!(
            rs.syntax.inlay_hints.is_empty(),
            "expected empty inlay_hints on default editor; got {} entries",
            rs.syntax.inlay_hints.len()
        );
    }

    /// Perf plan A.2 slice A.2b.1: even with hints in the LSP
    /// cache, `syntax.inlay_hints` stays empty while the
    /// `lsp-inlay-hint-mode` minor mode is OFF for the active
    /// buffer. The publish-time gate is the same one the
    /// renderer used to evaluate per-pane — moved off the hot
    /// path onto dispatch.
    #[test]
    fn syntax_inlay_hints_empty_when_mode_disabled() {
        use crate::per_buffer_cache::PerBufferCacheExt;
        use lattice_lsp::cache::LspInlayHintCache;
        use lattice_lsp::lsp_types::{InlayHint, InlayHintLabel, Position as LspPosition};
        let editor = Editor::default();
        editor.lsp_inlay_hints_cache.insert_for(
            editor.document_buffer_id,
            LspInlayHintCache {
                document_version: editor.document.snapshot().version,
                hints: vec![InlayHint {
                    position: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    label: InlayHintLabel::String(": i32".into()),
                    kind: None,
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: None,
                    data: None,
                }],
                requested_first_line: 0,
                requested_last_line: u32::MAX,
            },
        );
        // Mode is OFF (Editor::default's `active_modes` doesn't
        // include `lsp-inlay-hint-mode`) — gate must drop the
        // hint despite the cache being non-empty.
        editor.publish_render_state();
        let rs = editor.render_state.load();
        assert!(
            rs.syntax.inlay_hints.is_empty(),
            "expected empty inlay_hints when mode is off; got {} entries",
            rs.syntax.inlay_hints.len()
        );
    }

    // Happy-path coverage (cache populated, mode enabled, hints
    // flattened with padding + utf-16 → utf-8 conversion) is
    // exercised at the App layer by
    // `lattice_ui_tui::render::tests::inlay_hint_overlay_splices_virtual_text`
    // and will gain a direct worker-level test in A.2b.2 (the
    // worker will read `syntax.inlay_hints` and splice into
    // `RowPrepaint`; that path is unit-testable without an
    // `editor_boot` fixture).

    /// Non-cached sub-states still rebuild Arc-fresh per publish.
    ///
    /// Perf plan B.4 introduced the per-sub-state cache for
    /// `panes` / `modes` / `buffer_locals` plus the inner-Arc
    /// memoisation for `syntax.pane_highlights` and `lsp.progress`.
    /// The other sub-states — `diagnostics`, the outer `lsp`,
    /// `popup`, and the cursor-coupled `active_document` — still
    /// rebuild on every publish because their inputs change every
    /// tick (or because the savings haven't been measured worth the
    /// surface). This test pins the current behaviour for the
    /// non-cached set: Arc identity changes per publication.
    ///
    /// The positive contract for the CACHED set lives in
    /// [`cached_substates_preserve_arc_identity_on_no_op_publish`].
    #[test]
    fn substate_identity_changes_naively_per_publication() {
        let editor = Editor::default();
        let a = editor.render_state.load_full();
        editor.publish_render_state();
        let b = editor.render_state.load_full();
        // These sub-states are not cached by B.4 — Arc identity
        // changes per publication.
        assert!(!std::sync::Arc::ptr_eq(&a.diagnostics, &b.diagnostics));
        assert!(!std::sync::Arc::ptr_eq(&a.lsp, &b.lsp));
        assert!(!std::sync::Arc::ptr_eq(&a.popup, &b.popup));
    }

    /// Perf plan B.4: identity-preserving Arc publish for the
    /// cached sub-states.
    ///
    /// On a no-op republish (publish twice with no mutation
    /// between), every cached sub-state's `Arc` survives — same
    /// pointer, no allocation. This is the wait-free read seam's
    /// new contract: renderers can short-circuit per-pane /
    /// per-mode work by comparing `Arc::ptr_eq` on consecutive
    /// frames.
    ///
    /// Covers (B.4.a + B.4.b):
    /// - `panes` (outer `Arc<PanesRenderState>`)
    /// - `modes` (outer `Arc<ModesRenderState>`)
    /// - `buffer_locals` (outer `Arc<BufferLocalsRenderState>`)
    /// - `buffers` (outer `Arc<BuffersRenderState>`)
    /// - `tabs` (outer `Arc<TabsRenderState>`)
    /// - `syntax.pane_highlights` (inner per-pane spans map Arc)
    /// - `lsp.progress` (inner progress HashMap Arc)
    #[test]
    fn cached_substates_preserve_arc_identity_on_no_op_publish() {
        let editor = Editor::default();
        editor.publish_render_state();
        let a = editor.render_state.load_full();
        editor.publish_render_state();
        let b = editor.render_state.load_full();
        // Full sub-state caches (B.4.a).
        assert!(
            std::sync::Arc::ptr_eq(&a.panes, &b.panes),
            "panes sub-state should reuse its Arc when pane_tree.version() hasn't moved"
        );
        assert!(
            std::sync::Arc::ptr_eq(&a.modes, &b.modes),
            "modes sub-state should reuse its Arc when active_modes.version() hasn't moved"
        );
        assert!(
            std::sync::Arc::ptr_eq(&a.buffer_locals, &b.buffer_locals),
            "buffer_locals sub-state should reuse its Arc when buffer_locals.version() hasn't moved"
        );
        // Full sub-state caches (B.4.b).
        assert!(
            std::sync::Arc::ptr_eq(&a.buffers, &b.buffers),
            "buffers sub-state should reuse its Arc when buffer_uris.version() hasn't moved"
        );
        assert!(
            std::sync::Arc::ptr_eq(&a.tabs, &b.tabs),
            "tabs sub-state should reuse its Arc when its composite key hasn't moved"
        );
        // Inner-Arc caches (parent SyntaxRenderState / LspRenderState
        // still rebuild because their other fields churn per-frame).
        assert!(
            std::sync::Arc::ptr_eq(&a.syntax.pane_highlights, &b.syntax.pane_highlights),
            "syntax.pane_highlights inner Arc should be reused when pane_highlights.version() hasn't moved"
        );
        assert!(
            std::sync::Arc::ptr_eq(&a.lsp.progress, &b.lsp.progress),
            "lsp.progress inner Arc should be reused when lsp_progress.version() hasn't moved"
        );
    }

    /// Perf plan B.4.b: a registry-only mutation (no buffer_uris
    /// change) preserves the `buffers` sub-state cache, but
    /// invalidates the `tabs` cache because tab labels depend on
    /// `buffers.name_of(...)`.
    #[test]
    fn buffers_substate_survives_registry_only_mutation_but_tabs_invalidates() {
        use crate::buffer_registry::{BufferData, BufferEntry};
        use crate::buffers::BufferFlags;
        use crate::file_tree::FileTreeBuffer;
        use lattice_core::BufferId;
        let editor = Editor::default();
        editor.publish_render_state();
        let a = editor.render_state.load_full();
        // Registry mutation only — buffer_uris untouched.
        let id = BufferId(7_777);
        editor.buffers.insert(BufferEntry {
            id,
            name: Some("*scratch-versioned-test*".to_string()),
            flags: BufferFlags::default(),
            data: BufferData::FileTree(FileTreeBuffer {
                id,
                content: lattice_core::Buffer::empty(),
                cursor: lattice_protocol::position::Position::ZERO,
                scroll: 0,
            }),
        });
        editor.publish_render_state();
        let b = editor.render_state.load_full();
        // `buffers` cache survives because `buffer_uris.version()`
        // didn't move — the inner registry handle still sees the
        // newly inserted buffer through the shared Arc<Mutex<...>>.
        assert!(
            std::sync::Arc::ptr_eq(&a.buffers, &b.buffers),
            "buffers Arc should survive a registry-only mutation when buffer_uris didn't change"
        );
        // `tabs` cache invalidates because the composite key
        // includes `buffers.version()`, which bumped on `insert`.
        assert!(
            !std::sync::Arc::ptr_eq(&a.tabs, &b.tabs),
            "tabs Arc must rebuild after a buffer insert (tab labels depend on registry names)"
        );
    }

    /// Perf plan B.4: mutating one cached input invalidates ONLY
    /// that sub-state's cached Arc; the others survive.
    ///
    /// Touching `editor.active_modes` (via DerefMut) bumps the
    /// modes-version counter; the next `build_render_state` rebuilds
    /// the `modes` sub-state but leaves `panes` / `buffer_locals`
    /// alone — their versions haven't moved, so the cache hits.
    #[test]
    fn cached_substate_invalidation_is_per_field() {
        use lattice_core::BufferId;
        use lattice_mode::ActiveModes;
        let mut editor = Editor::default();
        editor.publish_render_state();
        let a = editor.render_state.load_full();
        // Touch active_modes through DerefMut: insert bumps the
        // wrapped HashMap's version counter once.
        editor
            .active_modes
            .insert(BufferId(99), ActiveModes::default());
        editor.publish_render_state();
        let b = editor.render_state.load_full();
        // `modes` invalidated (version bumped).
        assert!(
            !std::sync::Arc::ptr_eq(&a.modes, &b.modes),
            "modes Arc must rebuild after `active_modes.insert` bumps the version"
        );
        // `panes` and `buffer_locals` untouched — Arc identity
        // preserved.
        assert!(
            std::sync::Arc::ptr_eq(&a.panes, &b.panes),
            "panes Arc must survive a mutation to a different sub-state"
        );
        assert!(
            std::sync::Arc::ptr_eq(&a.buffer_locals, &b.buffer_locals),
            "buffer_locals Arc must survive a mutation to a different sub-state"
        );
    }

    /// Slice 3c.final.B (group 1): publishing a `RenderState`
    /// while the editor holds a multi-pane tree exposes the
    /// tree through `rs.panes.tree`. Renderers reading the
    /// snapshot see the same `active_index`, `leaves()` count,
    /// and `root` shape they used to read from
    /// `app.editor.pane_tree.X()` directly.
    #[test]
    fn panes_substate_reflects_pane_tree() {
        use lattice_core::ui::pane::{PaneState, PaneTree, SplitOrientation};
        let mut editor = Editor::default();
        editor.pane_tree = crate::versioned::Versioned::new(PaneTree::single(PaneState::default()));
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        editor.pane_tree.set_active(1);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(
            rs.panes.tree.leaves().len(),
            2,
            "renderer must see both leaves through `rs.panes.tree.leaves()`"
        );
        assert_eq!(
            rs.panes.tree.active_index(),
            1,
            "renderer must see the same active_index as the editor"
        );
    }

    /// Slice 3c.final.B (group 1): the buffers sub-state's
    /// registry clone routes the renderer's `name_of` / kind
    /// queries to the same underlying buffer-id index the editor
    /// owns. Writing into the editor's registry is observable
    /// through the published clone without re-publishing
    /// (registry is `Arc<Mutex<...>>`-backed).
    #[test]
    fn buffers_substate_registry_clone_observes_editor_writes() {
        use crate::buffer_registry::{BufferData, BufferEntry};
        use crate::buffers::BufferFlags;
        use crate::file_tree::FileTreeBuffer;
        use lattice_core::{BufferId, BufferKind};
        let editor = Editor::default();
        editor.publish_render_state();
        // Insert via the editor's registry handle, then read
        // through the published render-state's clone. FileTree
        // is the simplest constructible variant for this assert.
        let inserted_id = BufferId(424242);
        editor.buffers.insert(BufferEntry {
            id: inserted_id,
            name: Some("*scratch-test*".to_string()),
            flags: BufferFlags::default(),
            data: BufferData::FileTree(FileTreeBuffer {
                id: inserted_id,
                content: lattice_core::Buffer::empty(),
                cursor: lattice_protocol::position::Position::ZERO,
                scroll: 0,
            }),
        });
        let rs = editor.render_state.load_full();
        assert_eq!(
            rs.buffers.registry.kind_of(inserted_id),
            Some(BufferKind::FileTree),
            "registry clone observes the post-publish insert"
        );
        assert_eq!(
            rs.buffers.registry.name_of(inserted_id).as_deref(),
            Some("*scratch-test*"),
        );
    }

    /// Slice 3c.final.B (group 2): mutating editor.folds and
    /// publishing exposes the same fold list through
    /// `rs.active_document.folds`.
    #[test]
    fn active_document_folds_reflects_editor_state() {
        use lattice_core::Fold;
        let mut editor = Editor::default();
        editor.folds.push(Fold {
            start_line: 5,
            end_line: 10,
            closed: true,
            identity: None,
        });
        editor.folds.push(Fold {
            start_line: 20,
            end_line: 30,
            closed: false,
            identity: None,
        });
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(rs.active_document.folds.len(), 2);
        assert_eq!(rs.active_document.folds[0].start_line, 5);
        assert!(rs.active_document.folds[0].closed);
        assert_eq!(rs.active_document.folds[1].end_line, 30);
        assert!(!rs.active_document.folds[1].closed);
    }

    /// Slice 3c.final.B (group 2): hlsearch matches /
    /// current_match / option_cache round-trip through the
    /// published snapshot.
    #[test]
    fn active_document_search_and_options_reflect_editor_state() {
        use lattice_protocol::position::{Position, Range};
        let mut editor = Editor::default();
        let r = Range::new(Position::new(2, 0), Position::new(2, 5));
        editor.all_matches.push(r);
        editor.current_match = Some(r);
        editor.option_cache.show_whitespace = true;
        editor.option_cache.current_line_highlight = true;
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(rs.active_document.all_matches.len(), 1);
        assert_eq!(rs.active_document.all_matches[0], r);
        assert_eq!(rs.active_document.current_match, Some(r));
        assert!(rs.active_document.option_cache.show_whitespace);
        assert!(rs.active_document.option_cache.current_line_highlight);
    }

    /// Slice 3c.final.B (group 4): editor.lsp_progress is
    /// published as a fresh `Arc<HashMap<...>>` per tick. Mutating
    /// the editor's map and re-publishing makes the new entry
    /// visible through `rs.lsp.progress`.
    /// Slice 3c.final.B (group 5): translator substate carries
    /// the published `builtins`, `keymap`, and `partial_chord`
    /// so the renderer's input loop can build a
    /// `TranslateContext` from the snapshot.
    #[test]
    fn translator_substate_reflects_editor_inputs() {
        use crate::chord::KeyChord;
        let mut editor = Editor::default();
        // Seed a non-empty partial_chord so the publish path
        // exercises the slice conversion.
        editor.partial_chord = vec![KeyChord::char('g')];
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(rs.translator.partial_chord.len(), 1);
        // Builtins is `Copy`; the published snapshot has the
        // same default-shaped value as editor.
        let _: lattice_grammar::builtins::Builtins = rs.translator.builtins;
        // Keymap handle clones to an Arc-backed view; verify
        // we can dereference it without panic.
        let _ = &rs.translator.keymap;
    }

    /// Slice 3c.final.B.9: buffer_locals map round-trip.
    #[test]
    fn buffer_locals_map_reflects_editor_state() {
        use lattice_core::BufferId;
        use lattice_mode::BufferLocals;
        let mut editor = Editor::default();
        let buf = BufferId(7);
        editor.buffer_locals.insert(buf, BufferLocals::new());
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(rs.buffer_locals.map.contains_key(&buf));
        editor.buffer_locals.remove(&buf);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(!rs.buffer_locals.map.contains_key(&buf));
    }

    /// Slice 3c.final.B.8: pane_highlights map round-trip.
    #[test]
    fn pane_highlights_reflect_editor_state() {
        let mut editor = Editor::default();
        // Insert a synthetic span set for pane index 1 (the test
        // doesn't care about the span content; only the shape +
        // map round-trip matters).
        editor
            .pane_highlights
            .insert(1, vec![vec![], vec![]]);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        let spans = rs
            .syntax
            .pane_highlights
            .get(&1)
            .expect("pane 1 entry");
        assert_eq!(spans.len(), 2);
        // Removal also round-trips.
        editor.pane_highlights.remove(&1);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(!rs.syntax.pane_highlights.contains_key(&1));
    }

    /// Slice 3c.final.B.11: active-modes map round-trip. Inserts
    /// an entry at a synthetic buffer id and verifies the
    /// published map carries it (the `set_major` API on
    /// `ActiveModes` is `pub(crate)` to `lattice-mode`, so we can't
    /// populate the chain from outside that crate — the
    /// round-trip-shape assertion is what matters here).
    #[test]
    fn modes_map_reflects_editor_state() {
        use lattice_core::BufferId;
        use lattice_mode::ActiveModes;
        let mut editor = Editor::default();
        let buf = BufferId(42);
        editor.active_modes.insert(buf, ActiveModes::new());
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(
            rs.modes.map.contains_key(&buf),
            "published map should carry the inserted entry",
        );
        // Removal also round-trips.
        editor.active_modes.remove(&buf);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(
            !rs.modes.map.contains_key(&buf),
            "removed entry should not appear in next publish",
        );
    }

    /// Slice 3c.final.B.10: typed-options registry round-trip.
    #[test]
    fn options_registry_reflects_editor_state() {
        let editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        // The published `config` Arc shares the registry identity
        // with `editor.config` (one Arc::clone per publish).
        assert!(
            std::sync::Arc::ptr_eq(&rs.options.config, &editor.config),
            "options.config should be the same Arc instance as editor.config",
        );
    }

    /// Slice 3c.final.B.7: messages + modeline round-trip
    /// through the published snapshot.
    #[test]
    fn messages_and_modeline_reflect_editor_state() {
        use crate::action::{EchoLevel, EchoMessage};
        use crate::state::SearchLine;
        use lattice_grammar::SearchDirection;
        use lattice_protocol::position::Position;
        let mut editor = Editor::default();

        // Default: empty cmdline, no message, no search.
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(rs.messages.last.is_none());
        assert_eq!(rs.modeline.cmdline_text.as_ref(), "");
        assert!(!rs.modeline.auto_submit_hint);
        assert!(rs.modeline.search_pattern.is_none());
        assert!(rs.modeline.search_direction.is_none());

        // Populated.
        editor.last_message = Some(EchoMessage {
            text: "hello".to_string(),
            level: EchoLevel::Info,
        });
        editor.command_line = "describe-key ".to_string();
        editor.auto_submit_after_chord = true;
        editor.search_line = Some(SearchLine {
            direction: SearchDirection::Backward,
            pattern: "needle".to_string(),
            origin: Position::ZERO,
        });
        editor.publish_render_state();
        let rs = editor.render_state.load_full();

        let last = rs.messages.last.as_deref().expect("last set");
        assert_eq!(last.text, "hello");
        assert_eq!(last.level, EchoLevel::Info);
        assert_eq!(rs.modeline.cmdline_text.as_ref(), "describe-key ");
        assert!(rs.modeline.auto_submit_hint);
        assert_eq!(
            rs.modeline.search_pattern.as_deref(),
            Some("needle"),
        );
        assert_eq!(
            rs.modeline.search_direction,
            Some(SearchDirection::Backward),
        );
    }

    /// Slice 3c.final.B (group 6): lifecycle flags + theme
    /// round-trip through the published snapshot.
    #[test]
    fn lifecycle_and_theme_reflect_editor_state() {
        let mut editor = Editor::default();
        editor.should_quit = true;
        editor.pending_redraw = true;
        editor.terminal_width = Some(120);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(rs.lifecycle.should_quit);
        assert!(rs.lifecycle.pending_redraw);
        assert_eq!(rs.lifecycle.terminal_width, Some(120));
        // Theme is `Copy`; the field is the editor's current
        // host_theme by value.
        assert_eq!(rs.theme, editor.host_theme);
    }

    #[test]
    fn lsp_progress_reflects_published_map() {
        use lattice_lsp::{LspProgressKind, LspProgressUpdate};
        use std::sync::Arc;
        let mut editor = Editor::default();
        let key = (Arc::<str>::from("rust"), "token-1".to_string());
        editor.lsp_progress.insert(
            key.clone(),
            LspProgressUpdate {
                server_id: Arc::from("rust"),
                token: "token-1".to_string(),
                kind: LspProgressKind::Begin,
                title: Some("indexing".to_string()),
                message: None,
                percentage: None,
                cancellable: false,
            },
        );
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(rs.lsp.progress.contains_key(&key));
        assert_eq!(rs.lsp.progress.len(), 1);
    }

    /// Slice 3c.final.B (group 4): popup_buffer / placement
    /// fields published into `PopupRenderState`. With no popup
    /// open the substate reports `is_open() == false` and
    /// `help` is `None`.
    #[test]
    fn popup_substate_defaults_closed() {
        let editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(!rs.popup.is_open());
        assert!(rs.popup.help.is_none());
        assert!(rs.popup.help_highlights.is_empty());
    }

    /// Slice 3c.final.B (group 3): picker + completion slots
    /// default to None when no overlay is open.
    #[test]
    fn picker_and_completion_substates_default_closed() {
        let editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(rs.picker.state.is_none());
        assert!(rs.completion.insert.is_none());
        assert!(rs.completion.state.is_none());
    }

    /// Slice 3c.final.B (group 1): `Editor::buffer_uris` is
    /// published as a fresh `Arc<HashMap<...>>` per tick.
    /// Mutating the editor's map and re-publishing makes the
    /// new entry visible through `rs.buffers.uris`.
    #[test]
    fn buffers_substate_uris_reflects_published_map() {
        use lattice_core::BufferId;
        use lattice_lsp::Uri;
        use std::str::FromStr;
        let mut editor = Editor::default();
        let id = BufferId(7);
        let uri = Uri::from_str("file:///tmp/foo.rs").expect("valid uri");
        editor.buffer_uris.insert(id, uri.clone());
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(
            rs.buffers.uris.get(&id),
            Some(&uri),
            "renderer must see the URI the editor inserted before publishing"
        );
        assert!(
            rs.buffers.uris.get(&BufferId(9999)).is_none(),
            "absent ids return None through the published map"
        );
    }
}
