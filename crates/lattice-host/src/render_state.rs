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
    pub diagnostics: Arc<DiagnosticsRenderState>,
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
            diagnostics: Arc::new(DiagnosticsRenderState::default()),
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
/// Slice 3a: empty placeholder. Slice 3b populates with:
/// - the listed/unlisted buffer index (`:ls` + buffer picker),
/// - `BufferFlags` per entry,
/// - the active buffer id.
#[derive(Debug, Default, Clone)]
pub struct BuffersRenderState {}

/// Pane tree's render-side projection.
///
/// Slice 3a: empty placeholder. Slice 3b populates with the
/// pane layout (split tree), active pane id, per-pane buffer
/// reference, status-line label cache.
#[derive(Debug, Default, Clone)]
pub struct PanesRenderState {}

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisibleHighlightsKey {
    pub snapshot_ptr: usize,
    pub syntax_text_version: u64,
    pub scroll: u32,
    pub viewport_height: u32,
    pub fold_hash: u64,
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
#[derive(Debug, Default, Clone)]
pub struct VisibleSpans {
    pub spans: Vec<Vec<lattice_syntax::StyledSpan>>,
    pub computed_for_key: VisibleHighlightsKey,
}

/// Active picker's render-side projection.
///
/// Slice 3a: empty placeholder. Slice 3b populates with the
/// candidate list, selection index, query, preview origin.
#[derive(Debug, Default, Clone)]
pub struct PickerRenderState {}

/// Insert-completion popup's render-side projection.
///
/// Slice 3a: empty placeholder. Slice 3b populates with
/// candidate list, selection, side-docs body.
#[derive(Debug, Default, Clone)]
pub struct CompletionRenderState {}

/// Help / hover / signature popup's render-side projection.
///
/// Slice 3a: empty placeholder. Slice 3b populates with the
/// active popup buffer id, placement, highlights cache.
#[derive(Debug, Default, Clone)]
pub struct PopupRenderState {}

/// `*messages*` buffer + echo line state.
///
/// Slice 3a: empty placeholder. Slice 3b populates with the
/// current echo level/text and the ring of messages.
#[derive(Debug, Default, Clone)]
pub struct MessagesRenderState {}

/// Modeline status (active mode chain, recording indicator,
/// partial chord, search indicator).
///
/// Slice 3a: empty placeholder. Slice 3b populates.
#[derive(Debug, Default, Clone)]
pub struct ModelineRenderState {}

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

    /// Identity-preservation contract for unrelated sub-states.
    ///
    /// Slice 3a is naive (every sub-state is freshly `Arc::new`d
    /// per publication), so we DO NOT yet expect identity
    /// preservation between calls. Document the contract so
    /// the Slice 3b/3c rewrite knows what's expected.
    ///
    /// This test asserts the current behaviour: every sub-state
    /// Arc identity changes per publication. When dirty-bit
    /// optimisation lands, flip the assertion to `ptr_eq`.
    #[test]
    fn substate_identity_changes_naively_per_publication() {
        let editor = Editor::default();
        let a = editor.render_state.load_full();
        editor.publish_render_state();
        let b = editor.render_state.load_full();
        // Slice 3a: naive — every sub-state Arc is fresh.
        assert!(!std::sync::Arc::ptr_eq(&a.diagnostics, &b.diagnostics));
        assert!(!std::sync::Arc::ptr_eq(&a.lsp, &b.lsp));
        assert!(!std::sync::Arc::ptr_eq(&a.popup, &b.popup));
    }
}
