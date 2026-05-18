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
/// Slice 3a: empty placeholder. Slice 3b populates with:
/// - the active buffer's kind ([`lattice_core::BufferKind`]),
/// - the snapshot pointer (cheap rope `Arc` clone),
/// - cursor + scroll + viewport height,
/// - modal state + visual anchor + selection range,
/// - kind-specific overlays the renderer composites on top
///   (help anchors, oil entries, etc.).
#[derive(Debug, Default, Clone)]
pub struct ActiveDocumentRenderState {}

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
}

/// Tree-sitter highlights + visible-range cache.
///
/// Slice 3a: empty placeholder. Slice 3b populates with the
/// per-buffer highlight cache the renderer reads each paint.
#[derive(Debug, Default, Clone)]
pub struct SyntaxRenderState {}

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
