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
    /// I.5.1 (2026-06-05): inner `ArcSwap` so a keystroke can
    /// republish the active-document substate WITHOUT reswapping the
    /// whole monolith `RenderState` (I.5.3 the keystroke-only publish).
    /// The outer monolith Arc stays the single shared cell threaded to
    /// every renderer + worker; this inner cell is what the hot path
    /// stores into. Readers go through `.active_document.load().load()`.
    /// Mirrors the established `VirtualRowsRenderState.pane_matrices`
    /// nested-`ArcSwap` shape.
    pub active_document: Arc<arc_swap::ArcSwap<ActiveDocumentRenderState>>,
    pub buffers: Arc<BuffersRenderState>,
    pub panes: Arc<PanesRenderState>,
    pub lsp: Arc<LspRenderState>,
    pub syntax: Arc<SyntaxRenderState>,
    pub picker: Arc<PickerRenderState>,
    pub completion: Arc<CompletionRenderState>,
    pub popup: Arc<PopupRenderState>,
    pub messages: Arc<MessagesRenderState>,
    pub modeline: Arc<ModelineRenderState>,
    /// ML.0b-2: published snapshot of the configurable-modeline element
    /// system (descriptors + content, both `Arc`-backed so this clone is
    /// cheap). The renderers lay out Left/Center/Right zones from this
    /// (ML.1/ML.2). Distinct from `modeline` above, which is the legacy
    /// cmdline/search sub-state — different surface, kept separate.
    pub modeline_elements: lattice_mode::ModelineSnapshot,
    /// Editor's working directory set by `:cd`. `None` means
    /// fall back to `std::env::current_dir()`.
    pub current_dir: Option<std::path::PathBuf>,
    /// Slice 3c.final.B.10: typed-options registry published as a
    /// wait-free Arc clone so the renderer's
    /// `picker_display_is_minibuffer` (and any future per-frame
    /// typed-option read) doesn't take an actor round-trip.
    pub options: Arc<OptionsRenderState>,
    /// PI.4: per-buffer mode-resolved options (renderer-agnostic option
    /// resolution). Read via [`Self::resolved_option_for`].
    pub resolved_opts: Arc<ResolvedOptionsRenderState>,
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
    /// T.4 (theme-system): the resolved theme read table, snapshotted
    /// from the `ThemeRegistryHandle` at publish. Renderers read
    /// `resolved_theme.get(theme_ids.<elem>)` — an O(1) array index
    /// (design §7). T.6.t deleted the flat host `Theme` field that
    /// used to ride alongside this; all style reads now go through the
    /// resolved table, and non-style chrome (glyphs, separator chars,
    /// dim/nerd-fonts flags) through the typed-options registry. The
    /// TUI rebuilds its ratatui cache from this only when
    /// `ResolvedTheme::version()` changes (no per-frame adaptation);
    /// GPUI adapts inline.
    pub resolved_theme: std::sync::Arc<crate::ui::theme::ResolvedTheme>,
    /// T.4: builtin element ids (Copy), interned once at boot. Paired
    /// with [`Self::resolved_theme`] so a read is
    /// `resolved_theme.get(theme_ids.x)`.
    pub theme_ids: crate::ui::theme::BuiltinElementIds,
    /// S2.1 (2026-05-26): cell-grid renderer substrate state.
    /// Carries the published `CellMatrix` cell + the inputs the
    /// cell-builder worker (S2.2+) reads to rebuild. See
    /// [`CellsRenderState`] and
    /// `docs/dev/architecture/cell-grid-renderer.md`.
    ///
    /// I.5.2: inner `ArcSwap` so the keystroke fast path can
    /// republish the cells substate WITHOUT reswapping the whole
    /// monolith (I.5.3). Mirrors [`active_document`](Self::active_document).
    /// Readers go through `.cells.load()`.
    pub cells: Arc<arc_swap::ArcSwap<CellsRenderState>>,
    /// D.3.d.1 (2026-05-29): inline-diff overlay render state.
    /// Carries the active document's `DiffSignMap` for the
    /// gutter-sign column. Renderers read via
    /// `rs.diff.sign_map.sign_at(line_idx)`. Snapshot is taken
    /// in `build_render_state` from
    /// `editor.diff_signs_for_active()`; absent ⇒ empty map.
    pub diff: Arc<DiffRenderState>,
    /// D.3.b.1 (2026-05-29): virtual-rows render state.
    /// Carries the published `VirtualRowMatrix` so the
    /// renderer can interleave deletion-block + multibuffer-
    /// header rows between document rows. Snapshot is taken
    /// in `build_render_state` from
    /// `editor.virtual_rows_matrix_cell.load_full()`.
    pub virtual_rows: Arc<VirtualRowsRenderState>,
    /// §12 async-result render-wake — the off-keystroke paint gate.
    ///
    /// A content hash over every render-visible surface that is NOT
    /// owned by the cells / virtual-rows workers (those fire
    /// `paint_request` on their own content change). The actor's
    /// off-keystroke arms (`async_landed` / inline-diag) compare this
    /// against the previously-published value and fire `paint_request`
    /// only when it moved, so an async arrival that changes the modeline
    /// badge, a diagnostics overlay, a popup, etc. reaches a frame
    /// without a keystroke — and a no-op publish does NOT (which also
    /// keeps the GPUI paint-bridge's `run_tick_pending` → re-publish
    /// loop from spinning). Stamped by
    /// [`Editor::compute_paint_revision`](crate::dispatch::Editor::compute_paint_revision)
    /// in `build_render_state`. The cells `MatrixVersion` axis is the
    /// dual: it gates cell rebuilds; this gates everything else
    /// (`lattice-cells/src/version.rs` enumerates the overlay set).
    pub paint_revision: u64,
    /// PL8.E: per-buffer WASM gutter-decoration cache. A clone of `Editor`'s
    /// `wasm_decorations.cache` slot, so an off-render-path producer task's
    /// writes are observed without republishing this snapshot. Renderers merge
    /// `get_for(buffer_id).decorations` into the same gutter partition they walk
    /// for `Mode::gutter_decorations` — the producer never runs at paint time
    /// (paramount #1). Empty when no decoration plugin is loaded.
    pub wasm_gutter_decorations:
        crate::per_buffer_cache::PerBufferCache<crate::wasm_decorations::WasmGutterDecorationCache>,
    /// CM.3c: per-buffer severity gutter index for the `*compilation*`
    /// buffer, keyed by [`BufferId`] (mirrors `diff.sign_maps`). Written
    /// by the `AppEffect::CompilationGutterSet` host arm from the
    /// off-thread compilation drain; each entry is the buffer's full
    /// `(line, level)` list. Renderers look up `compilation_severity
    /// .get(buffer_id)` per pane and inject it into the mode's
    /// `gutter_decorations` via
    /// [`lattice_mode::CompilationSeverityData`] — the renderer never
    /// depends on `lattice-compilation`. Empty when no compilation has
    /// produced marks. The value `Arc` makes both the publish clone and
    /// the render-path read O(1).
    pub compilation_severity: std::sync::Arc<
        std::collections::HashMap<
            lattice_core::BufferId,
            std::sync::Arc<Vec<(u32, lattice_mode::GutterSeverityLevel)>>,
        >,
    >,
    /// CM.3c (2026-07-22): per-buffer compilation location-line index
    /// for theme-based highlighting of navigable lines in the
    /// `*compilation*` buffer. Written by the
    /// `AppEffect::CompilationLocationLines` host arm from the
    /// off-thread compilation drain; renderers check
    /// `compilation_location_lines.get(buffer_id)` when painting each
    /// line and apply the `compilation.location` theme element bg to
    /// any row whose index appears in the set. Empty when no
    /// compilation is active or no location lines have been produced.
    /// The value `Arc` makes both the publish clone and the render-path
    /// read O(1).
    pub compilation_location_lines: std::sync::Arc<
        std::collections::HashMap<
            lattice_core::BufferId,
            std::sync::Arc<Vec<(u32, u32, u32)>>,
        >,
    >,
    /// CM.3d (2026-07-22): snapshot of `Editor::compilation_theme_colors`.
    pub compilation_theme_colors: std::sync::Arc<(u32, u32)>,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            active_document: Arc::new(arc_swap::ArcSwap::from_pointee(
                ActiveDocumentRenderState::default(),
            )),
            buffers: Arc::new(BuffersRenderState::default()),
            panes: Arc::new(PanesRenderState::default()),
            lsp: Arc::new(LspRenderState::default()),
            syntax: Arc::new(SyntaxRenderState::default()),
            picker: Arc::new(PickerRenderState::default()),
            completion: Arc::new(CompletionRenderState::default()),
            popup: Arc::new(PopupRenderState::default()),
            messages: Arc::new(MessagesRenderState::default()),
            modeline: Arc::new(ModelineRenderState::default()),
            modeline_elements: lattice_mode::ModelineSnapshot::default(),
            current_dir: None,
            options: Arc::new(OptionsRenderState::default()),
            resolved_opts: Arc::new(ResolvedOptionsRenderState::default()),
            modes: Arc::new(ModesRenderState::default()),
            buffer_locals: Arc::new(BufferLocalsRenderState::default()),
            diagnostics: Arc::new(DiagnosticsRenderState::default()),
            tabs: Arc::new(TabsRenderState::default()),
            translator: Arc::new(TranslatorRenderState::default()),
            lifecycle: Arc::new(LifecycleRenderState::default()),
            resolved_theme: Arc::new(crate::ui::theme::ResolvedTheme::default()),
            theme_ids: crate::ui::theme::BuiltinElementIds::default(),
            cells: Arc::new(arc_swap::ArcSwap::from_pointee(CellsRenderState::default())),
            diff: Arc::new(DiffRenderState::default()),
            virtual_rows: Arc::new(VirtualRowsRenderState::default()),
            paint_revision: 0,
            wasm_gutter_decorations: crate::per_buffer_cache::empty(),
            compilation_severity: std::sync::Arc::new(std::collections::HashMap::new()),
            compilation_location_lines: std::sync::Arc::new(std::collections::HashMap::new()),
            compilation_theme_colors: std::sync::Arc::new((0x45475a, 0x89b4fa)),
        }
    }
}

impl RenderState {
    /// Resolve the fold list + `foldenable` that a pane showing
    /// `buffer_id` must render with.
    ///
    /// Folds are **per-buffer** (a buffer's `zf` / `za` / computed
    /// + overlay folds are shared by every pane showing it — the
    /// user-confirmed model). The bug this fixes: both renderers
    /// previously sourced folds for *every* pane from
    /// `active_document.folds`, so an inactive pane showing a
    /// *different* buffer rendered with the **active** buffer's
    /// folds — folding buffer A elided lines in buffer B's pane
    /// (GPUI), and switching focus away made A's inactive pane drop
    /// its folds (TUI). Both renderers now call this so the source
    /// is the pane's own buffer, uniformly (TUI/GPUI parity).
    ///
    /// The active buffer reads the live `active_document.folds`
    /// (freshest — updated synchronously on the fold keystroke);
    /// any other buffer reads its published per-pane entry in
    /// `cells.panes` (sourced from its `DocumentFolds` buffer-local).
    /// A buffer in no pane (no `cells.panes` entry) yields an empty
    /// list — nothing to elide.
    /// PI.0: the horizontal-centring pad for a pane rendering `buffer_id`.
    /// Resolved from THAT buffer's `CenterContentWidth` local + the width
    /// of the pane showing it — never the active-buffer identity.
    ///
    /// Previously the renderers gated centring on `buffer_id ==
    /// document_buffer_id`, so a picker preview that swapped
    /// `document_buffer_id` to the previewed file collapsed the dashboard's
    /// centring to 0 while the pane still showed the dashboard. Reading the
    /// rendered buffer's own local keeps centring attached to the buffer
    /// that carries it. Shared by the TUI and GPUI peers.
    pub fn content_left_pad_for(&self, buffer_id: lattice_core::BufferId) -> u32 {
        let block_width = self
            .buffer_locals
            .map
            .get(&buffer_id)
            .and_then(|l| l.get::<crate::modes::CenterContentWidth>())
            .map(|c| c.0)
            .unwrap_or(0);
        if block_width == 0 {
            return 0;
        }
        let tree = &self.panes.tree;
        let viewport_width = tree
            .leaves()
            .iter()
            .find(|p| p.buffer_id == buffer_id)
            .map(|p| p.viewport_width)
            .unwrap_or_else(|| tree.active().viewport_width);
        viewport_width.saturating_sub(block_width) / 2
    }

    /// PI.4: resolve option `D` for `buffer_id` from the published
    /// per-buffer resolved-options snapshot — the renderer-agnostic seam
    /// both peers use (mirror of `Editor::resolved_option`). Falls back to
    /// the global typed-option default when the buffer has no cached entry
    /// (transient publish gap / a buffer resolved lazily). O(1) `TypeId`
    /// lookup on the `Arc`-shared `ResolvedOptions`.
    pub fn resolved_option_for<D: lattice_config::OptionDecl>(
        &self,
        buffer_id: lattice_core::BufferId,
    ) -> Arc<D::Value>
    where
        D::Value: Clone + Send + Sync + 'static,
    {
        if let Some(resolved) = self.resolved_opts.map.get(&buffer_id)
            && let Some(v) = resolved.get::<D>()
        {
            return v;
        }
        self.options
            .config
            .get_typed::<D>()
            .expect("option not registered")
    }

    /// PI.4: whether a pane showing `buffer_id` paints its cursorline
    /// (`:set cursorline` / `current-line-highlight-mode`), resolved
    /// per-buffer. Both peers read this for the focused preview pane so the
    /// previewed buffer keeps its own cursorline (e.g. an LSP-reference /
    /// grep location preview keeps the target line highlighted).
    pub fn current_line_highlight_for(&self, buffer_id: lattice_core::BufferId) -> bool {
        *self.resolved_option_for::<lattice_config::CursorLine>(buffer_id)
    }

    pub fn folds_for_buffer(
        &self,
        buffer_id: lattice_core::BufferId,
    ) -> (Arc<[lattice_core::Fold]>, bool) {
        let ad = self.active_document.load();
        if ad.document_buffer_id == buffer_id {
            return (ad.folds.clone(), ad.option_cache.foldenable);
        }
        let cells = self.cells.load();
        if let Some(pane) = cells.panes.iter().find(|p| p.buffer_id == buffer_id) {
            return (pane.folds.clone(), pane.foldenable);
        }
        (Arc::from([]), ad.option_cache.foldenable)
    }

    /// Resolve the byte-baked inlay-hint rows a pane showing
    /// `buffer_id` must render.
    ///
    /// Like [`Self::folds_for_buffer`], this gives every pane its OWN
    /// buffer's decoration so active and inactive panes render through
    /// ONE code path (inactive == active, modulo dimming). It replaces
    /// a duplicated seam where the active pane spliced the baked
    /// `syntax.inlay_hints` while inactive panes re-derived hints from
    /// the per-buffer LSP cache with their own utf-16→utf-8 conversion —
    /// two sources that could drift.
    ///
    /// The active buffer reads the canonical baked list
    /// (`syntax.inlay_hints`); any other buffer reads its published
    /// `cells.panes` entry (built by `build_inlay_hints_for_buffer`,
    /// identical gating + byte-baking). A buffer in no pane yields empty.
    pub fn inlay_hints_for_buffer(&self, buffer_id: lattice_core::BufferId) -> Arc<[InlayHintRow]> {
        let ad = self.active_document.load();
        if ad.document_buffer_id == buffer_id {
            return self.syntax.inlay_hints.clone();
        }
        let cells = self.cells.load();
        if let Some(pane) = cells.panes.iter().find(|p| p.buffer_id == buffer_id) {
            return pane.inlay_hints.clone();
        }
        Arc::from([])
    }
}

/// D.3.b.1 (2026-05-29): renderer-side projection of the
/// virtual-rows worker's published `VirtualRowMatrix`.
/// Carries the matrix so the TUI / GPUI renderer can
/// interleave virtual rows between document rows when
/// painting visible content.
///
/// `matrix` defaults to an empty `VirtualRowMatrix` so the
/// renderer never has to handle an `Option`; an empty matrix
/// reports no rows for any line.
#[derive(Clone, Debug)]
pub struct VirtualRowsRenderState {
    pub matrix: Arc<lattice_cells::VirtualRowMatrix>,
    /// D.4.d.2.1.d (2026-05-30): `PaneId → virtual-rows matrix`
    /// lookup derived from `CellsRenderState::panes` at publish
    /// time so renderers can find a pane's virtual-rows matrix
    /// by id without scanning the panes slice. One entry per
    /// visible Document leaf; non-Document panes are absent
    /// (the publisher filters them out of
    /// `CellsRenderState::panes`). Mirror of
    /// [`CellsRenderState::pane_matrices`].
    ///
    /// Use [`Self::matrix_for_pane`] for the read; direct
    /// access to the map is fine when batching multiple
    /// lookups.
    pub pane_matrices: Arc<
        std::collections::HashMap<
            lattice_core::ui::pane::PaneId,
            Arc<arc_swap::ArcSwap<lattice_cells::VirtualRowMatrix>>,
        >,
    >,
}

impl Default for VirtualRowsRenderState {
    fn default() -> Self {
        Self {
            matrix: Arc::new(lattice_cells::VirtualRowMatrix::empty()),
            pane_matrices: Arc::new(std::collections::HashMap::new()),
        }
    }
}

impl VirtualRowsRenderState {
    /// D.4.d.2.1.d: look up the virtual-rows matrix cell for
    /// `pane_id`. Returns `None` when the pane is not a Document
    /// leaf (file tree / help / messages / oil / terminal panes
    /// skip the cells path entirely, so they're absent from
    /// `pane_matrices` — see
    /// `crate::dispatch::Editor::build_cells_panes`). Mirror of
    /// [`CellsRenderState::matrix_for_pane`].
    pub fn matrix_for_pane(
        &self,
        pane_id: lattice_core::ui::pane::PaneId,
    ) -> Option<&Arc<arc_swap::ArcSwap<lattice_cells::VirtualRowMatrix>>> {
        self.pane_matrices.get(&pane_id)
    }
}

/// D.3.d.1 (2026-05-29): renderer-side projection of the
/// active document's diff overlay state. Carries the
/// `DiffSignMap` for the gutter-sign column; future D.3.e
/// (line tints) reads through the same map.
///
/// `sign_map` defaults to an empty `Arc<DiffSignMap>` so
/// renderers never have to handle the `Option` path; an
/// empty map's `sign_at` is `None` for every line.
#[derive(Clone, Debug, Default)]
pub struct DiffRenderState {
    pub sign_map: Arc<crate::diff::overlay::DiffSignMap>,
    /// D-fix.3b: per-buffer sign maps for EVERY live diff session, so a
    /// side-by-side diff tints BOTH panes — each pane reads its own
    /// buffer's map by `BufferId`. The proposed/current (right) buffer maps
    /// to the current-side `sign_map()`; the baseline (left) buffer maps to
    /// the `baseline_sign_map()`. Renderers look up `sign_maps.get(buffer_id)`
    /// per pane; the legacy single `sign_map` above stays the active-doc map
    /// for the modeline hunk count.
    pub sign_maps: Arc<
        std::collections::HashMap<lattice_core::BufferId, Arc<crate::diff::overlay::DiffSignMap>>,
    >,
    /// D.3.g (2026-05-29): hunk count for the active
    /// document's `DiffSession`, if any. `None` means no
    /// session is open for the active buffer. `Some(0)` means
    /// a session is open but the buffer currently matches
    /// baseline (no hunks). The modeline diff-mode indicator
    /// reads this to render `[diff: N hunks]` only when a
    /// session is active.
    pub active_session_hunk_count: Option<usize>,
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
    /// First visible display column (horizontal scroll). Drives the
    /// body's left clip when `wrap` is off; 0 under wrap.
    pub leftcol: u32,
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
    /// `true` while the `:` line sits on an `ArgKind::Chord` arg slot
    /// (e.g. the armed `:describe-key ` prompt) — the next keystroke is
    /// CAPTURED as that chord, not dispatched. Published (computed via
    /// [`crate::dispatch::Editor::chord_capture_active`]) so the actor-based
    /// GPUI peer can read it the same way the TUI App computes it live;
    /// drives `TranslateContext::chord_capture`.
    pub chord_capture: bool,
    /// `true` while a snippet's tab-stop chain is active.
    /// Gates Tab / S-Tab to drive `next_tabstop` / `prev_tabstop`
    /// instead of falling back to insert-completion / outdent.
    pub snippet_active: bool,
    /// PU refactor (2026-07-22): `true` when a Steal popup has
    /// keyboard focus (State B). Replaces the architectural leak
    /// where `active_buffer == BufferKind::Help` doubled as a
    /// focus-state canary. Set/cleared in `Editor`'s popup-
    /// mutation methods; published here so TUI and GPUI renderers
    /// read `popup_focused` directly instead of re-deriving it
    /// from `buffer_kind`.
    pub popup_focused: bool,
    /// MB.1 (rich minibuffer): `true` while the `*command-line*` buffer
    /// is focused for editing (the `:` line is open). Swapping
    /// `self.document` to that buffer would otherwise make the active
    /// pane render the command-line text; renderers read this flag to
    /// route the active pane to its own (registry-keyed) buffer instead —
    /// the Help-popup pattern. See `docs/dev/architecture/rich-minibuffer.md`.
    pub command_line_active: bool,
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
    /// T-clean-1 Phase A.1 (2026-05-28): the active Terminal
    /// pane's cursor in alacritty grid coordinates
    /// `(absolute_line, col)`. Derived by the publisher from
    /// `self.cursor` (doc-space) + `synthetic.origin_top_line`.
    /// Renderers read this instead of reaching into
    /// `TerminalBuffer::nav_cursor` so the bespoke mirror can
    /// retire (Phase A.3). `None` when the active buffer is
    /// not a Terminal or has no SyntheticDoc (Insert mode).
    pub terminal_nav_cursor: Option<(i32, u16)>,
    /// T-clean-1 Phase A.1 (2026-05-28): published copy of
    /// `TerminalBuffer::visual` for renderer consumption. Same
    /// shape as on the buffer (grid coords). Renderers read
    /// this so the buffer-side field can later move to a
    /// doc-space source. `None` when not in terminal-Visual.
    pub terminal_visual: Option<lattice_terminal::TerminalVisualState>,
    /// Phase 5.8.AF.5 / Slice 3c.final.B (group 2): folds for
    /// the active document. Renderers read
    /// `rs.active_document.load().folds` instead of `app.editor.folds`.
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
    /// Rectangular block extents when `modal == Visual(Blockwise)`,
    /// `None` otherwise. Renderers prefer this over `visual_range`
    /// in Blockwise — `visual_range` only expresses a linear span
    /// and can't represent the per-line column band a block needs.
    /// Mirrors [`crate::visual::Editor::visual_block_extents`].
    pub visual_block_extents: Option<crate::visual::BlockExtents>,
    /// `:s/pat/repl/...` preview overlay. `None` while no
    /// substitute is being typed. The renderer paints the
    /// match ranges (and replacement text, if any) with the
    /// destructive-preview colour. `Arc` for cheap cloning.
    pub substitute_preview: Option<std::sync::Arc<crate::state::SubstitutePreview>>,
    /// Active document's selection set (multi-cursor / linewise /
    /// blockwise). Already an `Arc` on `RopeDocumentHandle` so this
    /// is one Arc bump.
    pub selections: std::sync::Arc<lattice_protocol::SelectionSet>,
    /// Hot-path option cache (typed-options resolved values).
    /// `Copy` so the publish is a plain struct move. Used heavily
    /// by per-row paint (whitespace glyphs, current-line highlight,
    /// line-number style).
    pub option_cache: crate::state::OptionCache,
    /// K.4.6 follow-up (2026-06-02): per-composed-row source line
    /// number lookup. `None` for regular Documents (the gutter
    /// uses the composed row index, which IS the source line
    /// number — identity mapping). `Some(arr)` for Multibuffer
    /// views, where `arr[composed_row]` gives the source line
    /// number in the originating source buffer.
    ///
    /// The renderer's `render_gutter_for` reads
    /// `arr[composed_row]` when present and formats THAT as the
    /// gutter label, so the user sees the actual source file's
    /// line numbers (e.g. 429, 430, 432 — skipping non-hit
    /// lines) rather than the composed-buffer row indices
    /// (0, 1, 2 — meaningless for navigation).
    ///
    /// Substrate-published per Option (a) confirmed in the K.4.6
    /// design discussion: the gutter has one job ("show this
    /// row's display line number"), the mapping is data, no
    /// kind-special-casing in the renderer.
    pub display_line_numbers: Option<Arc<[u32]>>,
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
            leftcol: 0,
            viewport_height: 0,
            modal: lattice_grammar::ModalState::Normal,
            visual_anchor: None,
            snapshot: Arc::new(lattice_runtime::DocumentSnapshot::default()),
            pending_count: 0,
            op_count: 0,
            macro_recording: false,
            completion_open: false,
            picker_open: false,
            chord_capture: false,
            snippet_active: false,
            popup_focused: false,
            command_line_active: false,
            terminal_insert_active: false,
            terminal_esc_exits: true,
            terminal_visual_active: false,
            terminal_app_cursor_keys: false,
            terminal_insert_exit_pending: false,
            terminal_program_name: Arc::from(""),
            terminal_nav_cursor: None,
            terminal_visual: None,
            folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
            all_matches: Arc::from(
                Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
            ),
            current_match: None,
            visual_range: None,
            visual_block_extents: None,
            substitute_preview: None,
            selections: Arc::new(lattice_protocol::SelectionSet::default()),
            option_cache: crate::state::OptionCache::default(),
            display_line_numbers: None,
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
    pub inlay_hints: crate::per_buffer_cache::PerBufferCache<lattice_lsp::cache::LspInlayHintCache>,
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
    // ML.3c: `progress` + `server_status` removed. The modeline badge
    // they fed is produced by `lattice_lsp::modeline` from its own
    // `LspProgressStore` (decision A); no renderer reads them.
    /// Slice 3c.final.B (group 4): LSP supervisor handle clone.
    /// The handle is internally `Arc<ArcSwap<SupervisorSnapshot>>`-
    /// backed so `Clone` is one Arc bump and `servers_for(uri)`
    /// stays wait-free. Renderers query
    /// `rs.lsp.supervisor.servers_for(&uri)` for the modeline's
    /// `[lsp:rust]` indicator instead of `app.editor.lsp.servers_for(...)`.
    pub supervisor: lattice_lsp::LspSupervisorHandle,
}

/// Tree-sitter syntax inputs + static-overlay bucket cache.
///
/// Phase 5.8.AF.5 / Slice X2: split into two halves.
///
/// **Inputs** (`syntax_handle`, `scroll`, `viewport_height`,
/// `fold_hash`, `text_version`, `doc_highlights`,
/// `static_overlay_version`) are written by dispatch's
/// `publish_render_state` from current `Editor` state. The
/// background overlay worker reads them via the published
/// `RenderState` snapshot to decide whether to re-bucket.
///
/// **Output** (`static_overlay_quads`) is a nested
/// `Arc<ArcSwap<...>>` so the worker can publish a fresh
/// `StaticOverlayQuads` *without* going through
/// `publish_render_state`. The outer `RenderState` `Arc` stays
/// stable across a frame; the inner cell can be swapped at any
/// time. Renderers read with
/// `render_state.syntax.static_overlay_quads.load()` — wait-free.
///
/// display-line B4.2 (gut + rename): the dead `visible_spans` /
/// `visible_rows` prepaint output cells were deleted; syntax colour
/// now flows through the cells / `DisplayMatrix` substrate, and only
/// the static-overlay bucket remains as a worker output here.
///
/// Goal #1 ("no parsing on the UI thread") is enforced by this
/// split: the overlay bucketing runs on the worker, not in any
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
    // display-line B4.2 (gut + rename): the worker-published
    // `visible_spans` + `visible_rows` prepaint output cells were
    // deleted. Their consumers (TUI compose loop, GPUI active-pane
    // shaping) migrated to the cells / `DisplayMatrix` substrate in
    // the B-series, leaving these cells with zero readers. The
    // surviving worker output is `static_overlay_quads` below.
    /// Perf plan A.2 slice A.2b.1: active document's flattened
    /// inlay-hint list, pre-gated by the buffer's
    /// `lsp-inlay-hint-mode` enable-flag. Populated once per
    /// publish from `Editor::lsp_inlay_hints_cache` for the active
    /// document; empty when the mode is off, no LSP, or no hints
    /// have arrived yet.
    ///
    /// Why on `SyntaxRenderState` and not `LspRenderState`:
    /// inlays are an INPUT to the cells / `DisplayMatrix` worker's
    /// row composition (A.2b.2). The cells worker walks this list to
    /// splice inlay text into each composed row; downstream readers
    /// (GPUI's active-pane prepaint) consume the woven rows. The raw
    /// per-buffer LSP cache stays on `lsp.inlay_hints` for the
    /// inactive-pane fallback path that flattens its own list.
    /// (display-line B4.2: the dead `RowPrepaint` cell that
    /// previously consumed this is gone.)
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
    // DR.2 (decoration-retention): the per-pane `pane_highlights` span
    // cache was retired from the published syntax sub-state. Inactive
    // panes read their own retained per-pane `DisplayMatrix` (via
    // `CellsRenderState::display_matrix_for_pane`), the same canonical
    // producer the active pane uses.
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
            inlay_hints: Arc::from(Vec::<InlayHintRow>::new().into_boxed_slice()),
            inlay_version: 0,
            static_overlay_quads: Arc::new(arc_swap::ArcSwap::from_pointee(
                StaticOverlayQuads::default(),
            )),
            doc_highlights: Arc::from(
                Vec::<lattice_protocol::position::Range>::new().into_boxed_slice(),
            ),
            static_overlay_version: 0,
        }
    }
}

/// Cell-grid renderer substrate state. Mirror of
/// [`SyntaxRenderState`] in shape (wait-free output cell + read
/// inputs); replaces the per-frame `shape_line` path for
/// code-class buffers.
///
/// **S2.1**: only the plumbing is in place — `matrix` is published
/// but stays empty until S2.2 spawns the cell-builder worker that
/// will write to it. The other fields are populated from current
/// `Editor` state by `build_render_state` so the worker has its
/// full input set the moment it lands.
///
/// Anchor: [`docs/dev/architecture/cell-grid-renderer.md`](../../../docs/dev/architecture/cell-grid-renderer.md).
#[derive(Debug, Clone)]
pub struct CellsRenderState {
    /// Worker-published output cell. Inner Arc identity stays
    /// stable across publishes (cloned from
    /// `Editor::cells_matrix_cell`) so the worker's writes
    /// survive subsequent publishes — same stability pattern as
    /// [`SyntaxRenderState::visible_spans`].
    pub matrix: Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>>,

    /// Aggregate version stamp of the inputs below. Worker
    /// compares against the *published* matrix's
    /// [`lattice_cells::CellMatrix::version`] to decide rebuild —
    /// see [`lattice_cells::MatrixVersion::differs_from`]. Folds
    /// `text_version`, `syntax`-derived stamp, `inlay_hints`
    /// content hash, `folds` content hash, and `theme` version
    /// into one comparison value.
    pub version: lattice_cells::MatrixVersion,

    /// Active document snapshot. The cell-builder walks this
    /// line-by-line. `None` when no document is active (initial
    /// boot or between buffer switches). Cloned by reference from
    /// the active `RopeDocumentHandle.snapshot()` at publish time.
    pub snapshot: Option<Arc<lattice_runtime::DocumentSnapshot>>,

    /// Active buffer's syntax handle. Consumed by S2.3 cell
    /// construction to resolve per-cell foreground colour from
    /// the syntax span set. `None` when no language is attached
    /// — cells fall back to the theme default fg.
    pub syntax_handle: Option<Arc<lattice_syntax::SyntaxHandle>>,

    /// Pre-flattened inlay hints for the active buffer. Same
    /// payload as [`SyntaxRenderState::inlay_hints`]; carried
    /// here so the cell-builder can splice inlay text into cells
    /// without re-reading the LSP cache (S2.3).
    pub inlay_hints: Arc<[InlayHintRow]>,

    /// Active buffer's fold ranges. Consumed by S2.3 for row
    /// elision (folded source lines do not produce matrix rows).
    pub folds: Arc<[lattice_core::Fold]>,

    /// Visible pane height in matrix rows. S2.4 reads this to
    /// pick `chunk_size = 2 × viewport_height` when above the
    /// whole-doc-mode threshold.
    pub viewport_height: u32,

    /// `:set foldenable` for the active buffer. The cell-builder
    /// feeds this into [`crate::folds::FoldIndex::from_folds`] so
    /// elision predicates collapse to `false` when folding is off
    /// — `zi` then yields the unfolded matrix without a separate
    /// code path. Folded into [`lattice_cells::MatrixVersion::folds`]
    /// at publish time so toggling foldenable invalidates the
    /// matrix.
    pub foldenable: bool,

    /// S2.4.b (2026-05-26): single-edit delta covering the bump
    /// from the previous publish's text_version to this publish's
    /// text_version. `Some(d)` when exactly one
    /// `apply_edit_blocking` happened since the last build and the
    /// worker can take the incremental rebuild path; `None`
    /// otherwise (no edit, batch, undo / redo, multi-edit
    /// coalescing) — in which case the worker conservatively
    /// full-rebuilds. Sourced from `Editor::last_edit_for_cells`
    /// via `take()` at `build_render_state` time, so subsequent
    /// publishes without further edits see `None`.
    pub last_edit: Option<lattice_cells::EditDelta>,

    /// T.5 (theme-system): the resolved read table + builtin ids the
    /// cell-builder uses to resolve each span's `lattice_syntax::Style`
    /// → `resolved.get(syntax_element_id(ids, s))` (O(1) array index).
    /// Snapshotted at publish. T.6.t deleted the flat host `Theme` field
    /// that used to ride alongside it; the matrix invalidation key
    /// (`MatrixVersion::theme`) is now `ResolvedTheme::version()`, set at
    /// `build_render_state` time, so a palette change still rebuilds the
    /// matrix with fresh colours.
    pub resolved_theme: std::sync::Arc<crate::ui::theme::ResolvedTheme>,
    pub theme_ids: crate::ui::theme::BuiltinElementIds,

    /// 2026-05-27: `display.whitespace.*` snapshot. Worker
    /// substitutes whitespace bytes with marker glyphs +
    /// `WS_MARKER` flag when `show` is true. A hash of this
    /// struct is folded into
    /// [`lattice_cells::MatrixVersion::whitespace`] at publish
    /// time so any `:set` of a whitespace option invalidates the
    /// matrix and triggers a rebuild.
    pub whitespace: crate::cells_worker::WhitespaceConfig,

    /// D.4.d.1.a (2026-05-29): one entry per visible Document
    /// pane. Populated by `publish_render_state` from
    /// `pane_tree.leaves()`; non-Document leaves are skipped.
    ///
    /// D.4.d.1.b consumes this slice in
    /// [`crate::cells_worker::recompute`] — each entry's
    /// `matrix` is the per-buffer registry cell the worker
    /// writes through. The active pane's entry shares Arc
    /// identity with [`Self::matrix`] so today's renderer
    /// read path keeps landing on the worker's writes for
    /// the active pane; [`Self::pane_matrices`] +
    /// [`Self::matrix_for_pane`] are the per-pane read
    /// surface renderers can use to find a non-active pane's
    /// matrix without iterating `panes`.
    pub panes: Arc<[PaneCellsInputs]>,

    /// D.4.d.1.c (2026-05-29): `PaneId → matrix` lookup
    /// derived from [`Self::panes`] at publish time so
    /// renderers can find a pane's matrix by id without
    /// scanning the panes slice. One entry per visible
    /// Document leaf; non-Document panes are absent (the
    /// renderer's per-kind dispatch already knows not to
    /// consult cells for those).
    ///
    /// Use [`Self::matrix_for_pane`] for the read; direct
    /// access to the map is fine when batching multiple
    /// lookups.
    pub pane_matrices: Arc<
        std::collections::HashMap<
            lattice_core::ui::pane::PaneId,
            Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>>,
        >,
    >,

    /// B2.1 (2026-06-04): active-pane per-line display matrix.
    /// Clone of `Editor::display_matrix_cell` (stable Arc identity so
    /// the worker's writes survive subsequent publishes) — the
    /// per-line analogue of [`Self::matrix`]. Empty until the B2.2
    /// worker build path writes through it. See
    /// `docs/dev/architecture/display-line.md`.
    pub display_matrix: Arc<arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>>,

    /// B2.1 (2026-06-04): `PaneId → display matrix` lookup, the
    /// per-line analogue of [`Self::pane_matrices`]. One entry per
    /// visible Document leaf; derived at publish time from
    /// [`Self::panes`]. Read via [`Self::display_matrix_for_pane`].
    pub display_pane_matrices: Arc<
        std::collections::HashMap<
            lattice_core::ui::pane::PaneId,
            Arc<arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>>,
        >,
    >,
}

impl CellsRenderState {
    /// B2.1 (2026-06-04): look up the per-line display matrix for
    /// `pane_id`. `None` when the pane is not a Document leaf (same
    /// semantics as [`Self::matrix_for_pane`]).
    pub fn display_matrix_for_pane(
        &self,
        pane_id: lattice_core::ui::pane::PaneId,
    ) -> Option<&Arc<arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>>> {
        self.display_pane_matrices.get(&pane_id)
    }

    /// D.4.d.1.c: look up the cell matrix for `pane_id`.
    /// Returns `None` when the pane is not a Document leaf
    /// (file tree / help / messages / oil / terminal panes
    /// skip the cells path entirely — see
    /// `crate::dispatch::Editor::build_cells_panes`).
    pub fn matrix_for_pane(
        &self,
        pane_id: lattice_core::ui::pane::PaneId,
    ) -> Option<&Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>>> {
        self.pane_matrices.get(&pane_id)
    }
}

/// D.4.d.1.a (2026-05-29): per-visible-Document-pane build
/// inputs for the cell-builder worker. Mirrors the shape of
/// the top-level [`CellsRenderState`] active-doc fields but
/// keyed by `(pane_id, buffer_id)` so the worker can resolve
/// each entry's matrix Arc via
/// [`crate::editor::Editor::cells_matrix_for`] at publish
/// time and rebuild per visible buffer.
///
/// K.4.7 (2026-06-07): per-excerpt syntax entry for multibuffer
/// panes. The cells worker reads `excerpt_syntax` to apply
/// per-source tree-sitter highlights across composed rows.
///
/// - `composed_start` / `composed_end`: inclusive row range in the
///   multibuffer's composed coordinate space.
/// - `source_start`: the first source-buffer row that maps to
///   `composed_start` (used to translate highlight results back).
/// - `handle`: per-excerpt highlight provider (impl `ExcerptHighlighter`).
///   Owned by `MultibufferState`; accessed via trait object so the host
///   never depends on the concrete `SyntaxHandle` type.
#[derive(Clone)]
pub struct ExcerptSyntax {
    pub composed_start: u32,
    pub composed_end: u32,
    pub source_start: u32,
    pub handle: std::sync::Arc<dyn lattice_cells::ExcerptHighlighter>,
}

impl std::fmt::Debug for ExcerptSyntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExcerptSyntax")
            .field("composed_start", &self.composed_start)
            .field("composed_end", &self.composed_end)
            .field("source_start", &self.source_start)
            .finish_non_exhaustive()
    }
}

/// `last_edit` is `Some(delta)` only for the active pane
/// (edits land on the active document; the publish path
/// `take()`s `Editor::last_edit_for_cells` exactly once);
/// non-active panes always carry `None` so the worker
/// conservatively full-rebuilds on text-version bumps for
/// those buffers. That is correct today because non-active
/// buffers don't take edits in normal use; LSP-driven edits
/// to non-active buffers also flush through `apply_edit`
/// which clears the slot.
#[derive(Debug, Clone)]
pub struct PaneCellsInputs {
    /// `PaneTree` leaf id. Stable across publishes until the
    /// pane is closed or reorganised.
    pub pane_id: lattice_core::ui::pane::PaneId,
    /// Document buffer this pane is showing. Worker uses
    /// this as the registry key.
    pub buffer_id: lattice_core::BufferId,
    /// Per-pane matrix output cell. Cloned from
    /// `Editor::cells_matrix_for(buffer_id)` so worker
    /// writes via `cell.store(...)` are visible through
    /// every later `render_state.load_full()`. Active-pane
    /// entries share Arc identity with
    /// [`CellsRenderState::matrix`].
    pub matrix: Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>>,
    /// B2.1 (2026-06-04): per-pane per-line display-matrix output
    /// cell. Cloned from `Editor::display_matrix_for(buffer_id)` so
    /// worker writes via `cell.store(...)` are visible through every
    /// later `render_state.load_full()`. Active-pane entries share
    /// Arc identity with [`CellsRenderState::display_matrix`].
    pub display_matrix: Arc<arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>>,
    /// D.4.d.2.1.b (2026-05-29): per-pane virtual-rows matrix
    /// output cell. Cloned from
    /// `Editor::virtual_rows_matrix_for(buffer_id)` so the
    /// virtual-rows worker (D.4.d.2.1.c) can write via
    /// `cell.store(...)` and have the writes visible
    /// through every later `render_state.load_full()`.
    /// Active-pane entries share Arc identity with
    /// [`crate::editor::Editor::virtual_rows_matrix_cell`]
    /// (boot-seeded invariant from D.4.d.2.0) so the
    /// existing single-document read path through
    /// [`VirtualRowsRenderState::matrix`] keeps landing on
    /// the worker's writes for the active pane until
    /// D.4.d.2.1.d switches the renderer to a per-pane
    /// lookup.
    pub virtual_rows_matrix: Arc<arc_swap::ArcSwap<lattice_cells::VirtualRowMatrix>>,
    /// Aggregate version for this pane's inputs. Worker
    /// compares against `matrix.load().version` to
    /// short-circuit cache hits per pane.
    pub version: lattice_cells::MatrixVersion,
    /// Document snapshot for `buffer_id`. `None` when the
    /// buffer is missing from the registry (transient race
    /// during close); the worker treats `None` as "skip,
    /// matrix unchanged."
    pub snapshot: Option<Arc<lattice_runtime::DocumentSnapshot>>,
    /// Syntax handle for `buffer_id`, if any. Resolves
    /// through `Editor::document_syntax_for` so non-active
    /// buffers carrying a parsed `Syntax` are still themed.
    pub syntax_handle: Option<Arc<lattice_syntax::SyntaxHandle>>,
    /// Flattened inlay hints for `buffer_id`. Empty when
    /// `lsp-inlay-hint-mode` is off for this buffer or no
    /// hints have arrived.
    pub inlay_hints: Arc<[InlayHintRow]>,
    /// Fold ranges for `buffer_id`. Sourced from
    /// `buffer_locals[buffer_id].DocumentFolds`.
    pub folds: Arc<[lattice_core::Fold]>,
    /// Pane-local visible-buffer height. Sourced from
    /// `PaneState.viewport_height` (Issue #25). Drives the
    /// worker's chunked-mode threshold.
    pub viewport_height: u32,
    /// Pane-local first visible source line. For the active pane this
    /// is `Editor::scroll`; inactive panes carry their stashed
    /// `PaneState.scroll`. H.3 (2026-06-04): the cells worker windows
    /// the chunked-mode matrix around `[scroll, scroll +
    /// viewport_height)` (plus overscan) rather than building the
    /// whole document, so large-file build + rebuild are O(viewport)
    /// not O(file). H.3a plumbs the field; H.3b makes the worker read
    /// it. Mirrors the long-standing `SyntaxRenderState.scroll`
    /// precedent for the legacy windowed-highlight path.
    pub scroll: u32,
    /// Pane-local visible-buffer width in columns. Sourced from
    /// `PaneState.viewport_width`. Soft-wrap (W.1): the cells
    /// worker stamps `CellMatrix.wrap_width` from this when `wrap`
    /// is on (W.2). `0` means "not yet laid out".
    pub viewport_width: u32,
    /// Soft-wrap (W.2): `:set wrap` resolved for this pane's
    /// buffer. When `true` (and `viewport_width > 0`) the cells
    /// worker stamps the published matrix's `wrap_width` so
    /// consumers expand each source line into `⌈col/width⌉`
    /// display rows. `false` ⇒ `wrap_width` stays `0` (one
    /// display row per source line — the historical default).
    pub wrap: bool,
    /// Columns reserved for this pane's gutter (line-number column
    /// + diagnostic + diff-sign cells). The cells worker subtracts
    /// this from `viewport_width` to get the soft-wrap width, so
    /// the stamped `wrap_width` — read by `segment_count` (the
    /// vertical scroll clamp) and both renderers' paint paths —
    /// matches the width the renderer actually wraps body text at.
    /// Without this the clamp under-counts wrapped display rows and
    /// `G` clips the document tail. `0` for gutterless panes (the
    /// floating popups). Computed via
    /// [`crate::cells_worker::gutter_cols`], shared with
    /// [`crate::editor::Editor::body_text_width`] to keep the
    /// vertical and horizontal clamps in lockstep.
    pub wrap_reserved_cols: u32,
    /// Per-pane foldenable. Global today (no per-buffer
    /// setting); kept here so a future per-buffer
    /// `foldenable` doesn't require a substate reshape.
    pub foldenable: bool,
    /// Single-edit delta for the incremental rebuild path.
    /// `Some(delta)` only for the active pane on the publish
    /// cycle when exactly one edit was applied; `None`
    /// otherwise. See struct-level docstring.
    pub last_edit: Option<lattice_cells::EditDelta>,
    /// K.4.7 (2026-06-07): per-excerpt syntax entries for multibuffer
    /// panes. Empty for ordinary single-document panes.
    pub excerpt_syntax: Arc<[ExcerptSyntax]>,
    /// PU.1b-2a: generic per-buffer *static* highlight spans (indexed
    /// by source line), merged ON TOP OF the grammar spans in the
    /// matrix build. Sourced from the buffer's `ExtraHighlights` local
    /// (help links; later, any non-grammar-derivable styling). Empty
    /// `[]` for every buffer without the local — the merge is a no-op
    /// then, so ordinary panes render byte-identically.
    pub extra_spans: Arc<[Vec<lattice_syntax::StyledSpan>]>,
}

impl Default for CellsRenderState {
    fn default() -> Self {
        Self {
            matrix: Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_cells::CellMatrix::empty(),
            )),
            version: lattice_cells::MatrixVersion::ZERO,
            snapshot: None,
            syntax_handle: None,
            inlay_hints: Arc::from(Vec::<InlayHintRow>::new().into_boxed_slice()),
            folds: Arc::from(Vec::<lattice_core::Fold>::new().into_boxed_slice()),
            viewport_height: 0,
            foldenable: true,
            last_edit: None,
            resolved_theme: Arc::new(crate::ui::theme::ResolvedTheme::default()),
            theme_ids: crate::ui::theme::BuiltinElementIds::default(),
            whitespace: crate::cells_worker::WhitespaceConfig::default(),
            panes: Arc::from(Vec::<PaneCellsInputs>::new().into_boxed_slice()),
            pane_matrices: Arc::new(std::collections::HashMap::new()),
            display_matrix: Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::display_matrix::DisplayMatrix::empty(),
            )),
            display_pane_matrices: Arc::new(std::collections::HashMap::new()),
        }
    }
}

/// Cache key identifying the inputs that produced a particular
/// [`StaticOverlayQuads`]. The overlay worker compares the *current*
/// inputs against `StaticOverlayQuads::computed_for_key` to
/// short-circuit re-bucketing on a no-op tick (cursor blink,
/// unchanged scroll/viewport/folds).
///
/// `snapshot_ptr` is the `Arc::as_ptr` of the snapshot the bucket
/// was computed against — distinct snapshots produce distinct keys
/// even if `text_version` happens to match.
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

// display-line B4.2 (gut + rename): `VisibleSpans` (the worker's
// per-line styled-span output) was deleted with its overlay worker.
// Syntax colour now flows through the cells / `DisplayMatrix`
// substrate; nothing reads a span-grid cell off `RenderState` any
// more. `RowRun` (below) survives — it is the `DisplayLine` run
// type, a separate live consumer.

/// One coloured run within a display row's combined text.
///
/// Perf plan A.2. Carries either a [`lattice_syntax::Style`] tag
/// for source bytes or an `Inlay` discriminant for inlay-spliced
/// bytes. Runs are NOT baked to RGB — renderers map `style → Rgba`
/// at paint time against the resolved theme table. Reasons for the
/// tag, not the colour:
///
/// - A theme switch doesn't invalidate the producer's cache (only
///   the colour resolution at paint changes; the run topology
///   doesn't).
/// - The producer stays theme-independent — no theme reads.
///
/// Slice A.2b.2 promoted this from a struct to an enum so the
/// producer can mark inlay-text bytes distinctly. Consumers map
/// `Inlay` to their inlay-virtual-text colour without having to
/// track byte ranges separately.
///
/// display-line B-series: this is the `DisplayLine` run type, owned
/// by the cells / `DisplayMatrix` substrate. (The deleted
/// `RowPrepaint` was the original consumer; B4.2 removed it but
/// `display_matrix.rs` keeps `RowRun` as its style-run type.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRun {
    /// Source-text run carrying a tree-sitter style tag.
    Source {
        /// Number of utf-8 bytes in this run inside the combined text.
        len: u32,
        /// Style tag from the highlight grammar. `Style::Default`
        /// for runs that fall outside any tree-sitter capture.
        style: lattice_syntax::Style,
    },
    /// Inlay-virtual-text run. Carries only the byte length; the
    /// colour is consumer-resolved.
    Inlay {
        /// Number of utf-8 bytes of inlay text in this run.
        len: u32,
    },
}

impl RowRun {
    /// Byte length of this run inside the row's combined text.
    /// Convenience accessor so consumers don't need to match every
    /// time the partition is walked. (Clippy: paired enums often
    /// also expose `is_empty`, but `RowRun::Inlay { len: 0 }`
    /// would be a producer bug — the partition is built with the
    /// invariant that every run is non-empty, so an `is_empty`
    /// would be misleading.)
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u32 {
        match self {
            RowRun::Source { len, .. } | RowRun::Inlay { len } => *len,
        }
    }
}

// display-line B4.2 (gut + rename): `RowPrepaint` (one pre-painted
// visible row) and `VisibleRows` (the per-pane prepaint output cell)
// were deleted with the overlay worker's dead span/row cache. Their
// consumers migrated to the cells / `DisplayMatrix` substrate in the
// B-series; nothing reads a prepaint-rows cell off `RenderState` any
// more. `RowRun` (above) survives as the `DisplayLine` run type.

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
/// line text (not into any combined / inlay-spliced row text).
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
/// bucket path. The cell is published on every overlay-worker
/// `recompute` and invalidates on its own axis
/// ([`VisibleHighlightsKey::static_overlay_version`]) so a
/// search-query bump re-buckets without touching unrelated state.
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
/// (ML.3c retired the `lsp_progress` inner-Arc cache with
/// `RenderState.lsp.progress`; DR.2 retired the sibling
/// `pane_highlights_map` cache.)
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
    /// PI.4: keyed on `Editor::resolved_options_version`.
    pub resolved_opts: Option<(u64, std::sync::Arc<ResolvedOptionsRenderState>)>,
    // DR.2 (decoration-retention): `pane_highlights_map` cache slot
    // retired with the `pane_highlights` producer. ML.3c: `lsp_progress`
    // cache slot retired with `RenderState.lsp.progress`.
    /// Perf plan B.4.b: keyed on `buffer_uris.version()` only.
    pub buffers: Option<(u64, std::sync::Arc<BuffersRenderState>)>,
    /// Perf plan B.4.b: keyed on a composite of `tabs.version()`,
    /// `active_tab`, `pane_tree.version()`, `buffers.version()`.
    /// The composite is encoded into one `u64` via a small fold so
    /// the cache slot shape stays uniform with the other entries.
    pub tabs: Option<(u64, std::sync::Arc<TabsRenderState>)>,
    /// Slice I.4 (publish coalescing): depth of the in-flight
    /// `dispatch` / `handle_effect` batch. While `> 0`, intermediate
    /// `publish_render_state()` calls (from chained setters,
    /// `ensure_cursor_visible`, `maybe_reparse_syntax`, …) suppress
    /// their build/store/wake and instead set `publish_pending`; the
    /// single real publish fires once when the outermost batch
    /// unwinds. Collapses ~6 whole-world publishes per keystroke to 1
    /// (and 12 worker wakes to 2). Lives here (not on `Editor`) because
    /// `PublishCache` is `Default`-derived and already the actor's
    /// single publish-side lock — no new `Editor` construction churn,
    /// and `build_render_state` already takes this lock.
    pub publish_batch_depth: u32,
    /// A suppressed publish occurred during the current batch; flushed
    /// once when the batch depth returns to 0.
    pub publish_pending: bool,
    /// §12 paint gate: the `RenderState::paint_revision` of the last
    /// real (un-suppressed) publish. `publish_render_state` compares the
    /// freshly-built revision against this; a change means a
    /// render-visible non-cell surface moved off-keystroke, so the actor
    /// fires `paint_request`. Lives here (not on `Editor`) for the same
    /// reason as the cache slots: `PublishCache` is `Default`-derived and
    /// already the actor's single publish-side lock, so no `Editor`
    /// construction churn. `u64::MAX` would be a valid revision, so this
    /// starts at 0 and the first publish (revision rarely 0) paints — a
    /// harmless extra first frame.
    pub last_paint_revision: u64,
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
    /// PU.5c: the ephemeral registry buffer backing the completion-docs
    /// side popup (`Editor::completion_docs_buffer`). Both renderers read
    /// it to source the docs snapshot + per-buffer options from the
    /// registry and route the content through the `PaneId::COMPLETION_DOCS`
    /// compose seam. `None` when no docs buffer exists.
    pub docs_buffer_id: Option<lattice_core::BufferId>,
}

/// Help / hover / signature popup's render-side projection.
///
/// Phase 5.8.AF.5 / Slice 3c.final.B (group 3): populated.
/// `buffer_id` mirrors `Editor::popup_buffer` (the active popup's
/// id, `None` when no popup is open). `placement` echoes
/// `Editor::popup_placement`. Help content + syntax / link styling
/// are carried by the registry Document at `buffer_id` and its live
/// cells-worker `DisplayMatrix` (grammar spans + the `ExtraHighlights`
/// link overlay) — both renderers source the popup's CONTENT, TITLE,
/// and line-count from the registry Document directly (PU.2 / PU.1b-4b),
/// so no popup-side `HelpBuffer` snapshot is published.
#[derive(Debug, Clone)]
pub struct PopupRenderState {
    pub buffer_id: Option<lattice_core::BufferId>,
    /// PU.1b-4b: the popup's State-A view scroll (= `Editor::popup_scroll`).
    /// Genuine popup-only view state — NOT in the registry Document (the
    /// Document carries no per-popup scroll), so it is published here for
    /// both renderers' State-A (popup-shown-but-not-focused) anchor. State
    /// B (focused) reads the live `active_document.scroll` instead.
    pub scroll: u32,
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
            scroll: 0,
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
    pub map: std::sync::Arc<
        std::collections::HashMap<
            lattice_core::BufferId,
            std::sync::Arc<lattice_mode::ActiveModes>,
        >,
    >,
    /// Read-only after boot — one Arc bump per `ModesRenderState`
    /// publish. Lets the renderer call `mode.status_line_items()`
    /// without an actor round-trip.
    pub mode_registry: std::sync::Arc<lattice_mode::ModeRegistry>,
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

/// PI.4: per-buffer mode-resolved options, published so BOTH renderer
/// peers resolve a buffer's options (Number, Wrap, CursorLine, …) through
/// ONE renderer-agnostic seam — [`RenderState::resolved_option_for`] —
/// instead of the TUI reading the live editor and GPUI reading the active
/// document's `option_cache`. Mirror of the host's
/// `Editor::resolved_options`; a buffer absent from the map falls back to
/// the global typed-option default via [`OptionsRenderState::config`].
#[derive(Debug, Default, Clone)]
pub struct ResolvedOptionsRenderState {
    pub map: std::sync::Arc<
        std::collections::HashMap<
            lattice_core::BufferId,
            std::sync::Arc<lattice_config::ResolvedOptions>,
        >,
    >,
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
    /// MB.2: `true` while the `:` line is expanded into the tier-2
    /// mini-buffer band; the renderer grows the echo row into a band and
    /// draws [`Self::cmdline_full_text`] instead of the one-row line.
    pub cmdline_expanded: bool,
    /// MB.2: the full (possibly multi-line) `:` line text, for the
    /// expanded band. Empty / unused in tier 1 (the one-row `cmdline_text`
    /// carries the single line there).
    pub cmdline_full_text: std::sync::Arc<str>,
}

impl Default for ModelineRenderState {
    fn default() -> Self {
        Self {
            cmdline_text: std::sync::Arc::from(""),
            auto_submit_hint: false,
            search_pattern: None,
            search_direction: None,
            cmdline_expanded: false,
            cmdline_full_text: std::sync::Arc::from(""),
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
    /// D.5.b (2026-05-30): active buffer's minor modes,
    /// snapshotted at publish time and threaded into
    /// `TranslateContext` so chord bindings registered against
    /// `MinorMode(ModeId)` layers gate on per-buffer
    /// activation (K.1.c). Empty in headless / mid-boot when
    /// no buffer has been activated yet. One Arc bump per
    /// publish; the slice is typically 0–3 entries
    /// (diff-mode, completion-popup-mode,
    /// active-snippet-mode in the busiest realistic case).
    pub active_minor_modes: std::sync::Arc<[lattice_mode::ModeId]>,
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
    /// L4a.2 (lsp-architecture.md §15): the active inline end-of-line
    /// diagnostic summary, as `(line, summary)`, when the cursor-line
    /// idle gate has fired. `None` whenever the gate is disarmed
    /// (`ui.diagnostics.inline = off`, Insert/Replace, pre-idle, or
    /// the line carries no qualifying diagnostic). The renderer
    /// (L4a.3) splices `summary.text` as trailing virtual text on
    /// `line`, themed by `summary.severity_rank`. Recomputed each
    /// publish while the gate is visible, so diagnostics that land on
    /// the line after the gate fires refresh it for free.
    pub inline_summary: Option<(u32, lattice_lsp::InlineDiagnosticSummary)>,
}

#[cfg(test)]
mod tests {
    use crate::action::Action;
    use crate::editor::Editor;
    use lattice_lsp::DiagnosticEvent;
    use lattice_lsp::lsp_types::{
        Diagnostic, DiagnosticSeverity, Position, PublishDiagnosticsParams, Range, Uri,
    };
    use std::str::FromStr;
    use std::sync::Arc;

    /// Write `contents` to a uniquely-named temp file; the returned
    /// guard removes it on drop. Local to this module so the fold
    /// test can open a second on-disk buffer without depending on
    /// the `dispatch` test module's `write_temp`.
    struct TempFilePathRs(std::path::PathBuf);
    impl Drop for TempFilePathRs {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn write_temp_rs(contents: &str) -> TempFilePathRs {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "lattice-foldbuf-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::write(&path, contents).expect("write temp file");
        TempFilePathRs(path)
    }

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
        let mut editor = Editor::default();
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
        let mut editor = Editor::default();
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

    // --- L4a.2: inline cursor-line diagnostic-summary idle gate ---

    /// Build an editor whose active buffer maps to `uri` with one
    /// ERROR diagnostic on `line`, cursor parked there. Used by the
    /// inline-summary gate tests.
    fn editor_with_diag_on_line(uri: &Uri, line: u32, message: &str) -> Editor {
        use lattice_protocol::position::Position as ProtoPos;
        let mut editor = Editor::default();
        editor
            .buffer_uris
            .insert(editor.document_buffer_id, uri.clone());
        editor.cursor = ProtoPos::new(line, 0);
        let diag = Diagnostic {
            range: Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 5 },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: None,
            message: message.to_string(),
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
        editor
    }

    /// Landing on a new line arms the idle gate (deadline set,
    /// summary not yet visible). The eol summary only appears once the
    /// timer fires — never on the cursor-move publish itself.
    #[test]
    fn inline_summary_armed_but_hidden_until_idle_fires() {
        let uri = Uri::from_str("file:///tmp/gate.rs").unwrap();
        let mut editor = editor_with_diag_on_line(&uri, 4, "synthetic");
        editor.publish_render_state();
        assert_eq!(editor.inline_diag_line, Some(4));
        assert!(editor.inline_diag_deadline.is_some(), "gate armed");
        assert!(!editor.inline_diag_visible);
        assert!(
            editor
                .render_state
                .load_full()
                .diagnostics
                .inline_summary
                .is_none(),
            "no summary before the idle deadline fires"
        );

        // Simulate the editor actor's timer arm firing.
        editor.fire_inline_diag_gate();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        let (line, summary) = rs
            .diagnostics
            .inline_summary
            .clone()
            .expect("summary visible after fire");
        assert_eq!(line, 4);
        assert_eq!(summary.severity_rank, 0);
        assert!(summary.text.contains("synthetic"));
        // Firing cleared the deadline so the pinned sleep won't refire.
        assert!(editor.inline_diag_deadline.is_none());
    }

    /// Insert (active text entry) suppresses the summary: entering
    /// Insert hides any visible summary and disarms the gate, which
    /// re-arms on return to Normal.
    #[test]
    fn inline_summary_suppressed_in_insert() {
        use lattice_grammar::ModalState;
        let uri = Uri::from_str("file:///tmp/gate.rs").unwrap();
        let mut editor = editor_with_diag_on_line(&uri, 2, "boom");
        editor.publish_render_state();
        editor.fire_inline_diag_gate();
        editor.publish_render_state();
        assert!(
            editor
                .render_state
                .load_full()
                .diagnostics
                .inline_summary
                .is_some()
        );

        editor.modal = ModalState::Insert;
        editor.publish_render_state();
        assert!(!editor.inline_diag_visible);
        assert!(editor.inline_diag_deadline.is_none());
        assert!(
            editor.inline_diag_line.is_none(),
            "armed line cleared so the gate re-arms on leaving Insert"
        );
        assert!(
            editor
                .render_state
                .load_full()
                .diagnostics
                .inline_summary
                .is_none()
        );

        // Back to Normal re-arms (hidden again until idle).
        editor.modal = ModalState::Normal;
        editor.publish_render_state();
        assert_eq!(editor.inline_diag_line, Some(2));
        assert!(editor.inline_diag_deadline.is_some());
        assert!(!editor.inline_diag_visible);
    }

    /// Moving the cursor to a new line re-arms the gate and hides the
    /// previous line's summary immediately.
    #[test]
    fn inline_summary_hidden_when_cursor_leaves_line() {
        use lattice_protocol::position::Position as ProtoPos;
        let uri = Uri::from_str("file:///tmp/gate.rs").unwrap();
        let mut editor = editor_with_diag_on_line(&uri, 3, "err here");
        editor.publish_render_state();
        editor.fire_inline_diag_gate();
        editor.publish_render_state();
        assert!(
            editor
                .render_state
                .load_full()
                .diagnostics
                .inline_summary
                .is_some()
        );

        // Cursor moves to a clean line: re-arm, summary gone.
        editor.cursor = ProtoPos::new(0, 0);
        editor.publish_render_state();
        assert_eq!(editor.inline_diag_line, Some(0));
        assert!(!editor.inline_diag_visible);
        assert!(
            editor
                .render_state
                .load_full()
                .diagnostics
                .inline_summary
                .is_none()
        );
    }

    /// `ui.diagnostics.inline = off` disarms the gate entirely — even
    /// with the cursor on a diagnostic line and the timer force-fired,
    /// no summary is published.
    #[test]
    fn inline_summary_off_option_disarms() {
        let uri = Uri::from_str("file:///tmp/gate.rs").unwrap();
        let mut editor = editor_with_diag_on_line(&uri, 1, "nope");
        editor.config.init_from_linkme();
        editor
            .config
            .parse_and_set_command("ui.diagnostics.inline=off")
            .unwrap();
        // Force-fire, then publish: `update_inline_diag_gate` clears it
        // because the option is Off before `build_render_state` reads.
        editor.fire_inline_diag_gate();
        editor.publish_render_state();
        assert!(!editor.inline_diag_visible);
        assert!(
            editor
                .render_state
                .load_full()
                .diagnostics
                .inline_summary
                .is_none()
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
        assert_eq!(rs.active_document.load().cursor, Position::new(7, 3));
        assert_eq!(rs.active_document.load().scroll, 5);
        assert_eq!(rs.active_document.load().viewport_height, 30);
        assert_eq!(
            rs.active_document.load().modal,
            lattice_grammar::ModalState::Normal
        );
        assert_eq!(
            rs.active_document.load().buffer_kind,
            lattice_core::BufferKind::Document
        );
        // Snapshot is a fresh Arc clone from `editor.document`.
        // Identity isn't preserved across publications (naive
        // rebuild today); the value is what matters.
        assert_eq!(rs.active_document.load().snapshot.buffer.byte_len(), 0);
        // Slice 3c.atomic.J: translator-context mirror fields
        // default to zero/false when no count, no macro, no
        // picker, no completion, no snippet is active.
        assert_eq!(rs.active_document.load().pending_count, 0);
        assert_eq!(rs.active_document.load().op_count, 0);
        assert!(!rs.active_document.load().macro_recording);
        assert!(!rs.active_document.load().completion_open);
        assert!(!rs.active_document.load().picker_open);
        assert!(!rs.active_document.load().snippet_active);
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
        assert_eq!(rs.active_document.load().pending_count, 7);
        assert_eq!(rs.active_document.load().op_count, 3);
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
        let mut editor = Editor::default();
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
        let mut editor = Editor::default();
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
        let mut editor = Editor::default();
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
    /// memoisation for `lsp.progress` (DR.2 retired the sibling
    /// `syntax.pane_highlights` inner-Arc cache).
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
        let mut editor = Editor::default();
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
    /// - `lsp.progress` (inner progress HashMap Arc)
    #[test]
    fn cached_substates_preserve_arc_identity_on_no_op_publish() {
        let mut editor = Editor::default();
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
        // ML.3c retired the `lsp.progress` inner-Arc cache assertion with
        // the field; DR.2 retired the `syntax.pane_highlights` one.
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
        let mut editor = Editor::default();
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
        let mut editor = Editor::default();
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
    /// `rs.active_document.load().folds`.
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
        assert_eq!(rs.active_document.load().folds.len(), 2);
        assert_eq!(rs.active_document.load().folds[0].start_line, 5);
        assert!(rs.active_document.load().folds[0].closed);
        assert_eq!(rs.active_document.load().folds[1].end_line, 30);
        assert!(!rs.active_document.load().folds[1].closed);
    }

    /// Fold-bleed regression: with buffer A folded in one pane and a
    /// *different* buffer B in another split, each pane must resolve
    /// folds from ITS OWN buffer — not from `active_document.folds`.
    /// Both renderers previously read the active doc's folds for every
    /// pane, so folding A elided B's lines (GPUI) and A's inactive pane
    /// dropped its folds on focus-out (TUI). `folds_for_buffer` is the
    /// shared per-buffer source that closes the bug for both peers.
    #[test]
    fn folds_for_buffer_is_per_buffer_not_active_document() {
        use lattice_core::Fold;
        use lattice_core::ui::pane::SplitOrientation;

        // Buffer A (boot document): give it a closed fold.
        let document = lattice_core::Document::from_text(
            &(0..10).map(|i| format!("a{i}\n")).collect::<String>(),
        );
        let mut editor = Editor::boot(document);
        let a_id = editor.document_buffer_id;
        editor.folds.push(Fold {
            start_line: 0,
            end_line: 4,
            closed: true,
            identity: None,
        });

        // Split and open a DIFFERENT buffer B in the new pane. `do_edit`
        // snapshots A's folds into its `DocumentFolds` on switch-away,
        // so A's inactive pane keeps them in the published `cells.panes`.
        let path = write_temp_rs(&(0..10).map(|i| format!("b{i}\n")).collect::<String>());
        editor.do_split_pane(SplitOrientation::Vertical);
        let new_idx = editor.pane_tree.split_active(SplitOrientation::Vertical);
        editor.pane_tree.set_active(new_idx);
        let _ = editor.do_edit(Some(path.0.clone()), false);
        let b_id = editor.pane_tree.active().buffer_id;
        assert_ne!(a_id, b_id, "B must be a distinct buffer from A");

        editor.publish_render_state();
        let rs = editor.render_state.load_full();

        // A is now inactive, but its pane still resolves A's own folds.
        let (a_folds, a_foldenable) = rs.folds_for_buffer(a_id);
        assert!(a_foldenable, "foldenable defaults on");
        assert!(
            a_folds.iter().any(|f| f.closed && f.start_line == 0),
            "inactive buffer A keeps its own closed fold, got {a_folds:?}"
        );

        // B is active and has no folds — it must NOT inherit A's folds.
        let (b_folds, _) = rs.folds_for_buffer(b_id);
        assert!(
            b_folds.is_empty(),
            "active buffer B resolves to its own (empty) folds, got {b_folds:?}"
        );
    }

    /// PI.0: content-centring follows the buffer that carries
    /// `CenterContentWidth`, not the active-buffer identity. A centred
    /// buffer keeps its pad even when a DIFFERENT buffer is the active
    /// document — the picker-preview scenario that used to collapse the
    /// dashboard's centring to 0 the instant `document_buffer_id` pointed
    /// at the previewed file.
    #[test]
    fn content_left_pad_follows_rendered_buffer_not_active_identity() {
        use lattice_core::ui::pane::SplitOrientation;

        let document = lattice_core::Document::from_text("centered\n");
        let mut editor = Editor::boot(document);
        let a_id = editor.document_buffer_id;

        // Open B in a split and focus it, so A is no longer the active doc.
        let path = write_temp_rs("b0\nb1\n");
        let new_idx = editor.pane_tree.split_active(SplitOrientation::Vertical);
        editor.pane_tree.set_active(new_idx);
        let _ = editor.do_edit(Some(path.0.clone()), false);
        let b_id = editor.pane_tree.active().buffer_id;
        assert_ne!(a_id, b_id, "B must be distinct from A");
        assert_ne!(editor.document_buffer_id, a_id, "A is no longer active");

        // Mark A for centring within a 20-col block; give A's (now
        // inactive) pane an 80-col viewport.
        editor
            .buffer_locals
            .entry(a_id)
            .or_default()
            .insert(crate::modes::CenterContentWidth(20));
        for leaf in editor.pane_tree.leaves_mut() {
            if leaf.buffer_id == a_id {
                leaf.viewport_width = 80;
            }
        }

        editor.publish_render_state();
        let rs = editor.render_state.load_full();

        // A keeps its centring pad = (80 - 20) / 2 = 30 despite being
        // inactive; the fix reads A's own local, not `== document_buffer_id`.
        assert_eq!(
            rs.content_left_pad_for(a_id),
            30,
            "centred buffer A keeps its pad when a different buffer is active"
        );
        // B carries no CenterContentWidth → no pad.
        assert_eq!(rs.content_left_pad_for(b_id), 0);
    }

    /// Exact user repro (2026-06-30): A in pane1, B (folded) active in
    /// pane2, then `<C-w>w` focus back to A. B's folds must survive the
    /// switch-away — `folds_for_buffer(B)` must still report B's closed
    /// fold once B is inactive. Pane navigation goes through
    /// `activate_pane` (NOT `do_edit`/`activate_document`), so this guards
    /// the `sync_active_document_to_pane` → `snapshot_active_document`
    /// stash path specifically.
    #[test]
    fn fold_survives_switching_pane_focus_away_from_its_buffer() {
        use lattice_core::Fold;
        use lattice_core::ui::pane::SplitOrientation;

        let document = lattice_core::Document::from_text(
            &(0..10).map(|i| format!("a{i}\n")).collect::<String>(),
        );
        let mut editor = Editor::boot(document);
        let a_id = editor.document_buffer_id;
        let a_idx = editor.pane_tree.active_index();

        // Split, open B in the new pane → B active.
        let path = write_temp_rs(&(0..10).map(|i| format!("b{i}\n")).collect::<String>());
        let b_idx = editor.pane_tree.split_active(SplitOrientation::Vertical);
        editor.pane_tree.set_active(b_idx);
        let _ = editor.do_edit(Some(path.0.clone()), false);
        let b_id = editor.pane_tree.active().buffer_id;
        assert_ne!(a_id, b_id);

        // Fold B while it is the active buffer (simulates z<Space>).
        editor.folds.push(Fold {
            start_line: 1,
            end_line: 5,
            closed: true,
            identity: None,
        });
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(
            rs.folds_for_buffer(b_id).0.iter().any(|f| f.closed),
            "sanity: B is folded while active"
        );

        // <C-w>w back to A: pane navigation, NOT activate_document.
        editor.activate_pane(a_idx);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();

        let (b_folds, _) = rs.folds_for_buffer(b_id);
        assert!(
            b_folds.iter().any(|f| f.closed && f.start_line == 1),
            "B keeps its closed fold after focus moves away, got {b_folds:?}"
        );
    }

    /// `inlay_hints_for_buffer` routes the active buffer to the baked
    /// `syntax.inlay_hints` list and an unknown buffer to empty — the
    /// per-pane source that lets both renderers splice hints through one
    /// path regardless of focus.
    #[test]
    fn inlay_hints_for_buffer_routes_active_and_unknown() {
        use crate::render_state::InlayHintRow;
        let mut editor = Editor::default();
        let a_id = editor.document_buffer_id;
        editor.publish_render_state();
        let rs = editor.render_state.load_full();

        // Active buffer returns whatever `syntax.inlay_hints` holds —
        // Arc-identity equal to the published list.
        let active = rs.inlay_hints_for_buffer(a_id);
        assert!(Arc::ptr_eq(&active, &rs.syntax.inlay_hints));

        // An unknown buffer (no pane entry) routes to empty, never a panic.
        let _ = InlayHintRow {
            line: 0,
            byte: 0,
            text: String::new(),
        };
        let unknown = rs.inlay_hints_for_buffer(lattice_core::BufferId(999_999));
        assert!(unknown.is_empty());
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
        assert_eq!(rs.active_document.load().all_matches.len(), 1);
        assert_eq!(rs.active_document.load().all_matches[0], r);
        assert_eq!(rs.active_document.load().current_match, Some(r));
        assert!(rs.active_document.load().option_cache.show_whitespace);
        assert!(
            rs.active_document
                .load()
                .option_cache
                .current_line_highlight
        );
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

    // DR.2 (decoration-retention): the `pane_highlights_reflect_editor_state`
    // round-trip test was retired with the `pane_highlights` producer.
    // Inactive-pane styling now flows through the per-pane `DisplayMatrix`
    // (covered by the cells-worker tests).

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
        let mut editor = Editor::default();
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
        // MB.1: the `:` line is a real buffer, so `set_command_line_text`
        // needs a booted editor (mode registry + option defaults) to open
        // the synthetic `*command-line*` buffer.
        let mut editor = Editor::boot(lattice_core::Document::empty());

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
        editor.set_command_line_text("describe-key ");
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
        assert_eq!(rs.modeline.search_pattern.as_deref(), Some("needle"),);
        assert_eq!(
            rs.modeline.search_direction,
            Some(SearchDirection::Backward),
        );
    }

    /// Slice 3c.final.B (group 6): lifecycle flags round-trip
    /// through the published snapshot. (T.6.t removed the host
    /// `Theme` field assertion — the resolved table now carries
    /// all style state and `MatrixVersion::theme` carries its
    /// version.)
    #[test]
    fn lifecycle_reflects_editor_state() {
        let mut editor = Editor::default();
        editor.should_quit = true;
        editor.pending_redraw = true;
        editor.terminal_width = Some(120);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(rs.lifecycle.should_quit);
        assert!(rs.lifecycle.pending_redraw);
        assert_eq!(rs.lifecycle.terminal_width, Some(120));
    }

    // ML.3c: `lsp_progress_reflects_published_map` retired with the host
    // accumulator + `RenderState.lsp.progress`. The progress fold + badge
    // are now covered by `lattice_lsp::modeline`'s store tests.

    /// Slice 3c.final.B (group 4): popup_buffer / placement
    /// fields published into `PopupRenderState`. With no popup
    /// open the substate reports `is_open() == false` and
    /// `help` is `None`.
    #[test]
    fn popup_substate_defaults_closed() {
        let mut editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert!(!rs.popup.is_open());
        assert_eq!(rs.popup.buffer_id, None);
        assert_eq!(rs.popup.scroll, 0);
    }

    /// Slice 3c.final.B (group 3): picker + completion slots
    /// default to None when no overlay is open.
    #[test]
    fn picker_and_completion_substates_default_closed() {
        let mut editor = Editor::default();
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

    // ---- S2.1 (cell-grid renderer plumbing) ----

    /// `CellsRenderState` is populated by `publish_render_state`.
    /// The matrix Arc is published as the active pane's
    /// per-buffer matrix cell (via
    /// `Editor::cells_matrix_for(document_buffer_id)`); other
    /// fields aggregate active document + syntax inputs.
    ///
    /// D.4.d.1.b (2026-05-29): pre-d.1.b the assertion was
    /// `Arc::ptr_eq(&rs.cells.matrix, &editor.cells_matrix_cell)`.
    /// The worker now writes through `cells.panes[i].matrix`
    /// (each entry's cell comes from the registry), so the
    /// renderer's top-level read target switched to the
    /// registry's active-buffer cell to keep the renderer
    /// back-compat path landing on the worker's writes.
    #[test]
    fn cells_substate_is_populated_on_publish() {
        let mut editor = Editor::default();
        editor.viewport_height = 24;
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        // I.5.2: `cells` is an inner `ArcSwap`; load the snapshot once.
        let rsc = rs.cells.load();
        // Matrix Arc identity matches the registry's active
        // cell (so the worker's writes via that cell are
        // visible through the published RS without a republish
        // round-trip).
        let registry_cell = editor.cells_matrix_for(editor.document_buffer_id);
        assert!(
            std::sync::Arc::ptr_eq(&rsc.matrix, &registry_cell),
            "cells.matrix must come from cells_matrix_for(active_buffer)"
        );
        // No worker yet → matrix stays empty.
        let m = rsc.matrix.load();
        assert!(m.is_empty(), "matrix is empty until S2.2 worker lands");
        // viewport_height + text version surface through to the
        // worker via the same RS path.
        assert_eq!(rsc.viewport_height, 24);
        assert_eq!(rsc.version.text, editor.document.text_version());
        // Snapshot is populated (the cell-builder reads it
        // line-by-line in S2.2+).
        assert!(rsc.snapshot.is_some());
    }

    /// The matrix Arc identity persists across publishes. This is
    /// the load-bearing invariant for S2.2's worker: the worker
    /// holds its sibling Arc and writes via `store()`; subsequent
    /// publishes must NOT swap the cell out from under it. The
    /// registry's lazy-insert is idempotent so repeat lookups for
    /// the same buffer return the same Arc — d.1.b preserves the
    /// invariant by sourcing `cells.matrix` from the registry.
    #[test]
    fn cells_matrix_arc_identity_is_stable_across_publishes() {
        let mut editor = Editor::default();
        editor.publish_render_state();
        let rs1 = editor.render_state.load_full();
        let cell1 = rs1.cells.load().matrix.clone();
        editor.publish_render_state();
        editor.publish_render_state();
        let rs2 = editor.render_state.load_full();
        assert!(
            std::sync::Arc::ptr_eq(&cell1, &rs2.cells.load().matrix),
            "cells.matrix Arc identity must persist across publishes"
        );
    }

    // ---- D.4.d.1.a (per-pane cells inputs) ----

    /// D.4.d.1.a: the default editor's single Document leaf
    /// surfaces through `cells.panes` as exactly one entry whose
    /// `pane_id` matches the active leaf, `buffer_id` matches
    /// the active document, and `matrix` resolves through the
    /// registry's idempotent `cells_matrix_for` port (the worker
    /// will write through the same Arc in D.4.d.1.b).
    ///
    /// The d.0 boot invariant — active-doc registry entry
    /// shares Arc identity with `cells_matrix_cell` — is set up
    /// by `Editor::boot`. `Editor::default()` (used here)
    /// doesn't seed the registry, so we assert the weaker
    /// registry-port contract instead: every panes entry's
    /// `matrix` is the same Arc the registry returns for that
    /// `buffer_id`.
    #[test]
    fn cells_panes_populated_for_single_document_pane() {
        let mut editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        let rsc = rs.cells.load();
        assert_eq!(
            rsc.panes.len(),
            1,
            "default single Document leaf produces one panes entry"
        );
        let entry = &rsc.panes[0];
        assert_eq!(entry.buffer_id, editor.document_buffer_id);
        assert_eq!(entry.pane_id, editor.pane_tree.active().id);
        assert!(
            entry.snapshot.is_some(),
            "active pane entry must carry the document snapshot"
        );
        let registry_cell = editor.cells_matrix_for(entry.buffer_id);
        assert!(
            std::sync::Arc::ptr_eq(&entry.matrix, &registry_cell),
            "panes entry matrix must come from the registry's cells_matrix_for port"
        );
        // Active pane's version must match the top-level cells
        // version — same hashes, same inputs.
        assert_eq!(entry.version, rsc.version);
    }

    /// D.4.d.1.a: a vsplit produces two Document leaves; each
    /// surfaces with a distinct `pane_id`. With both leaves
    /// still pointing at the active buffer, both entries share
    /// `buffer_id` and the same registry matrix Arc — the
    /// registry hands out one cell per buffer, not per pane.
    #[test]
    fn cells_panes_populated_per_visible_document_leaf() {
        use lattice_core::ui::pane::SplitOrientation;
        let mut editor = Editor::default();
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(
            rs.cells.load().panes.len(),
            2,
            "two Document leaves expected"
        );
        assert_ne!(
            rs.cells.load().panes[0].pane_id,
            rs.cells.load().panes[1].pane_id,
            "leaves must surface with distinct pane ids"
        );
        let shared_cell = editor.cells_matrix_for(editor.document_buffer_id);
        for entry in rs.cells.load().panes.iter() {
            assert_eq!(entry.buffer_id, editor.document_buffer_id);
            assert!(
                std::sync::Arc::ptr_eq(&entry.matrix, &shared_cell),
                "panes showing the same buffer must share the registry's matrix Arc"
            );
        }
    }

    // ---- D.4.d.2.1.b (virtual_rows_matrix on PaneCellsInputs) ----

    /// D.4.d.2.1.b: the publish path attaches a per-buffer
    /// `virtual_rows_matrix` cell to each `PaneCellsInputs` so
    /// D.4.d.2.1.c's worker iteration can write through
    /// `pane.virtual_rows_matrix.store(...)`. Single-pane
    /// invariant: the entry's cell resolves through the same
    /// registry port (`virtual_rows_matrix_for`) the
    /// renderer-side lookup (D.4.d.2.1.d) will read.
    ///
    /// `Editor::default()` doesn't seed the registry (boot
    /// does), so we assert the registry-port equality here —
    /// not the boot-seeded Arc-identity against
    /// `virtual_rows_matrix_cell`, which the D.4.d.2.0
    /// `virtual_rows_matrix_for_active_doc_shares_field_arc`
    /// test in `dispatch::tests` already covers via
    /// `Editor::boot`.
    #[test]
    fn cells_panes_carry_virtual_rows_matrix_for_single_document_pane() {
        let mut editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        let rsc = rs.cells.load();
        assert_eq!(rsc.panes.len(), 1);
        let entry = &rsc.panes[0];
        let registry_cell = editor.virtual_rows_matrix_for(entry.buffer_id);
        assert!(
            std::sync::Arc::ptr_eq(&entry.virtual_rows_matrix, &registry_cell),
            "panes entry virtual_rows_matrix must come from the \
             registry's `virtual_rows_matrix_for` port"
        );
    }

    /// D.4.d.2.1.b: a vsplit produces two Document leaves; both
    /// surface with the same `virtual_rows_matrix` Arc because
    /// they share `buffer_id` (the registry hands out one cell
    /// per buffer, not per pane — same contract as the cells
    /// side). When `:diffsplit` lands (D.4.d.3), the second
    /// leaf will point at a different buffer and the two
    /// `virtual_rows_matrix` Arcs will diverge — that case is
    /// already covered by the D.4.d.2.0 distinct-buffers test;
    /// here we lock the same-buffer-shares-cell invariant the
    /// worker iteration in D.4.d.2.1.c will rely on.
    #[test]
    fn cells_panes_share_virtual_rows_matrix_when_buffers_match() {
        use lattice_core::ui::pane::SplitOrientation;
        let mut editor = Editor::default();
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(
            rs.cells.load().panes.len(),
            2,
            "two Document leaves expected"
        );
        let shared_cell = editor.virtual_rows_matrix_for(editor.document_buffer_id);
        for entry in rs.cells.load().panes.iter() {
            assert_eq!(entry.buffer_id, editor.document_buffer_id);
            assert!(
                std::sync::Arc::ptr_eq(&entry.virtual_rows_matrix, &shared_cell),
                "panes showing the same buffer must share the \
                 registry's virtual_rows_matrix Arc"
            );
        }
    }

    /// D.4.d.1.a: non-Document leaves (file tree / help /
    /// messages / oil / terminal) don't take the cells path —
    /// they're filtered out of `cells.panes` so the worker can
    /// iterate without a kind check per entry.
    #[test]
    fn cells_panes_skip_non_document_leaves() {
        use lattice_core::BufferKind;
        use lattice_core::ui::pane::SplitOrientation;
        let mut editor = Editor::default();
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        // Flip pane index 1 to a non-Document kind. The filter
        // keys on `leaf.buffer` directly, so the buffer_id
        // doesn't need to point at a real FileTreeBuffer for
        // this assertion.
        editor.pane_tree.leaves_mut()[1].buffer = BufferKind::FileTree;
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(
            rs.cells.load().panes.len(),
            1,
            "non-Document leaves must be filtered out of panes"
        );
        assert_eq!(
            rs.cells.load().panes[0].buffer_id,
            editor.document_buffer_id
        );
    }

    /// D.4.d.1.a: the active pane's entry carries the
    /// single-edit delta the publisher just `take()`d off
    /// `Editor::last_edit_for_cells`; other panes (even ones
    /// showing the same buffer) carry `None`. Prevents the
    /// worker from double-applying the same delta when
    /// iterating per pane.
    #[test]
    fn cells_panes_last_edit_routes_only_to_active_pane() {
        use lattice_core::ui::pane::SplitOrientation;
        let mut editor = Editor::default();
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        // Make pane 0 the active one; both leaves share the
        // active document so without the pane filter both would
        // get the delta.
        editor.pane_tree.set_active(0);
        let active_pane_id = editor.pane_tree.active().id;
        editor.last_edit_for_cells = Some(lattice_cells::EditDelta {
            start_line: 0,
            lines_removed: 0,
            lines_added: 1,
        });
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        let rsc = rs.cells.load();
        assert_eq!(rsc.panes.len(), 2);
        let active_entry = rsc
            .panes
            .iter()
            .find(|p| p.pane_id == active_pane_id)
            .expect("active pane must be present in panes");
        assert!(
            active_entry.last_edit.is_some(),
            "active pane entry must carry the consumed delta"
        );
        let other_entry = rsc
            .panes
            .iter()
            .find(|p| p.pane_id != active_pane_id)
            .expect("non-active pane must be present in panes");
        assert!(
            other_entry.last_edit.is_none(),
            "non-active pane entries must never carry the active delta"
        );
        // Slot drained — next publish sees None on every entry.
        editor.publish_render_state();
        let rs2 = editor.render_state.load_full();
        assert!(rs2.cells.load().panes.iter().all(|p| p.last_edit.is_none()));
    }

    // ---- D.4.d.1.c (per-pane matrix lookup) ----

    /// D.4.d.1.c: `cells.pane_matrices` carries one entry per
    /// visible Document leaf; each entry's matrix Arc identity
    /// matches the corresponding `panes[i].matrix` (the
    /// registry cell the worker writes through).
    #[test]
    fn cells_pane_matrices_mirror_panes_entries() {
        use lattice_core::ui::pane::SplitOrientation;
        let mut editor = Editor::default();
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        let rsc = rs.cells.load();
        assert_eq!(rsc.panes.len(), 2);
        assert_eq!(rsc.pane_matrices.len(), 2);
        for entry in rsc.panes.iter() {
            let lookup = rsc
                .pane_matrices
                .get(&entry.pane_id)
                .expect("every pane must appear in pane_matrices");
            assert!(
                std::sync::Arc::ptr_eq(lookup, &entry.matrix),
                "pane_matrices lookup must return the same Arc as panes[i].matrix"
            );
        }
    }

    /// D.4.d.1.c: `matrix_for_pane` is the typed read on top of
    /// `pane_matrices`. Returns the matching cell for visible
    /// Document panes and `None` for any other id (closed pane,
    /// non-Document leaf, unknown id).
    #[test]
    fn cells_matrix_for_pane_returns_matching_cell_or_none() {
        let mut editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        let rsc = rs.cells.load();
        let active_pane_id = editor.pane_tree.active().id;
        let cell = rsc
            .matrix_for_pane(active_pane_id)
            .expect("active Document pane must resolve through matrix_for_pane");
        assert!(
            std::sync::Arc::ptr_eq(cell, &rsc.matrix),
            "active pane's matrix_for_pane lookup must match top-level cells.matrix"
        );
        // Unknown id returns None.
        let unknown = lattice_core::ui::pane::PaneId(u32::MAX);
        assert!(rsc.matrix_for_pane(unknown).is_none());
    }

    /// D.4.d.1.c: non-Document leaves are absent from
    /// `pane_matrices` (the worker filters them out of `panes`).
    /// Renderers' per-kind dispatch already knows not to consult
    /// cells for those kinds.
    #[test]
    fn cells_pane_matrices_skip_non_document_leaves() {
        use lattice_core::BufferKind;
        use lattice_core::ui::pane::SplitOrientation;
        let mut editor = Editor::default();
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        let non_doc_pane_id = {
            let leaves = editor.pane_tree.leaves_mut();
            leaves[1].buffer = BufferKind::FileTree;
            leaves[1].id
        };
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(rs.cells.load().pane_matrices.len(), 1);
        assert!(rs.cells.load().matrix_for_pane(non_doc_pane_id).is_none());
    }

    // ---- D.4.d.2.1.d (per-pane virtual-rows matrix lookup) ----

    /// D.4.d.2.1.d: `virtual_rows.pane_matrices` carries one
    /// entry per visible Document leaf; each entry's matrix Arc
    /// identity matches the corresponding
    /// `cells.panes[i].virtual_rows_matrix` (the registry cell
    /// the worker writes through). Mirror of the cells
    /// `cells_pane_matrices_mirror_panes_entries` test for the
    /// virtual-rows pipeline.
    #[test]
    fn virtual_rows_pane_matrices_mirror_panes_entries() {
        use lattice_core::ui::pane::SplitOrientation;
        let mut editor = Editor::default();
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(rs.cells.load().panes.len(), 2);
        assert_eq!(rs.virtual_rows.pane_matrices.len(), 2);
        for entry in rs.cells.load().panes.iter() {
            let lookup = rs
                .virtual_rows
                .pane_matrices
                .get(&entry.pane_id)
                .expect("every pane must appear in virtual_rows.pane_matrices");
            assert!(
                std::sync::Arc::ptr_eq(lookup, &entry.virtual_rows_matrix),
                "pane_matrices lookup must return the same Arc as panes[i].virtual_rows_matrix"
            );
        }
    }

    /// D.4.d.2.1.d: `matrix_for_pane` is the typed read on top
    /// of `pane_matrices`. Returns the matching cell for visible
    /// Document panes and `None` for any other id (closed pane,
    /// non-Document leaf, unknown id).
    ///
    /// `Editor::default()` doesn't run the boot seed (which is
    /// what shares Arc identity with `virtual_rows_matrix_cell`),
    /// so we assert the registry-port equality here — the
    /// matrix the renderer would read through this lookup is
    /// the same one `virtual_rows_matrix_for(buffer_id)` returns.
    /// The D.4.d.2.0 boot Arc-identity invariant is covered in
    /// `dispatch::tests::virtual_rows_matrix_for_active_doc_shares_field_arc`.
    #[test]
    fn virtual_rows_matrix_for_pane_returns_matching_cell_or_none() {
        let mut editor = Editor::default();
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        let active_pane_id = editor.pane_tree.active().id;
        let cell = rs
            .virtual_rows
            .matrix_for_pane(active_pane_id)
            .expect("active Document pane must resolve through matrix_for_pane");
        let registry_cell = editor.virtual_rows_matrix_for(editor.document_buffer_id);
        assert!(
            std::sync::Arc::ptr_eq(cell, &registry_cell),
            "active pane's matrix_for_pane lookup must return the same Arc as the registry"
        );
        // Unknown id returns None.
        let unknown = lattice_core::ui::pane::PaneId(u32::MAX);
        assert!(rs.virtual_rows.matrix_for_pane(unknown).is_none());
    }

    /// D.4.d.2.1.d: non-Document leaves are absent from
    /// `virtual_rows.pane_matrices` (the publisher filters
    /// them out of `cells.panes` upstream, so the lookup
    /// derived from `panes` is automatically scoped to
    /// Document panes only). Mirror of
    /// `cells_pane_matrices_skip_non_document_leaves`.
    #[test]
    fn virtual_rows_pane_matrices_skip_non_document_leaves() {
        use lattice_core::BufferKind;
        use lattice_core::ui::pane::SplitOrientation;
        let mut editor = Editor::default();
        editor.pane_tree.split_active(SplitOrientation::Vertical);
        let non_doc_pane_id = {
            let leaves = editor.pane_tree.leaves_mut();
            leaves[1].buffer = BufferKind::FileTree;
            leaves[1].id
        };
        editor.publish_render_state();
        let rs = editor.render_state.load_full();
        assert_eq!(rs.virtual_rows.pane_matrices.len(), 1);
        assert!(rs.virtual_rows.matrix_for_pane(non_doc_pane_id).is_none());
    }

    /// `publish_render_state` fires `cells_wake.notify_one()` —
    /// the permit-style coalescing means a single `notified()`
    /// resolves after one or many publishes. Validates S2.2's
    /// future worker will see the wake.
    #[tokio::test]
    async fn publish_render_state_fires_cells_wake() {
        let mut editor = Editor::default();
        // Permit set by the publish call; subsequent
        // `notified().await` resolves immediately.
        editor.publish_render_state();
        let waker = editor.cells_wake.0.clone();
        // Borderline impossible to hit the timeout if the permit
        // is set — `tokio::time::timeout` returns Err only on
        // genuine miss.
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), async move {
            waker.notified().await
        })
        .await;
        assert!(
            result.is_ok(),
            "cells_wake permit must be set by publish_render_state"
        );
    }
}
